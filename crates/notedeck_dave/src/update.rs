//! Helper functions for the Dave update loop.
//!
//! These are standalone functions with explicit inputs to reduce the complexity
//! of the main Dave struct and make the code more testable and reusable.

use crate::backend::{AiBackend, BackendType};
use crate::config::AiMode;
use crate::focus_queue::{FocusPriority, FocusQueue};
use crate::messages::{
    AnswerSummary, AnswerSummaryEntry, AskUserQuestionInput, Message, PermissionResponse,
    QuestionAnswer,
};
use crate::session::{ChatSession, EditorJob, PermissionMessageState, SessionId, SessionManager};
use crate::ui::{AgentScene, DirectoryPicker};
use claude_agent_sdk_rs::PermissionMode;
use std::path::PathBuf;
use std::time::Instant;

/// Timeout for confirming interrupt (in seconds)
pub const INTERRUPT_CONFIRM_TIMEOUT_SECS: f32 = 1.5;

// =============================================================================
// Interrupt Handling
// =============================================================================

/// Handle an interrupt request - requires double-Escape to confirm.
/// Returns the new pending_since state.
pub fn handle_interrupt_request(
    session_manager: &SessionManager,
    backend: &dyn AiBackend,
    pending_since: Option<Instant>,
    ctx: &egui::Context,
) -> Option<Instant> {
    // Only allow interrupt if there's an active AI operation
    let has_active_operation = session_manager
        .get_active()
        .map(|s| s.incoming_tokens.is_some())
        .unwrap_or(false);

    if !has_active_operation {
        return None;
    }

    let now = Instant::now();

    if let Some(pending) = pending_since {
        if now.duration_since(pending).as_secs_f32() < INTERRUPT_CONFIRM_TIMEOUT_SECS {
            // Second Escape within timeout - confirm interrupt
            if let Some(session) = session_manager.get_active() {
                let session_id = format!("dave-session-{}", session.id);
                backend.interrupt_session(session_id, ctx.clone());
            }
            None
        } else {
            // Timeout expired, treat as new first press
            Some(now)
        }
    } else {
        // First Escape press
        Some(now)
    }
}

/// Execute the actual interrupt on the active session.
pub fn execute_interrupt(
    session_manager: &mut SessionManager,
    backend: &dyn AiBackend,
    ctx: &egui::Context,
) {
    if let Some(session) = session_manager.get_active_mut() {
        let session_id = format!("dave-session-{}", session.id);
        backend.interrupt_session(session_id, ctx.clone());
        session.incoming_tokens = None;
        if let Some(agentic) = &mut session.agentic {
            agentic.permissions.pending.clear();
        }
        tracing::debug!("Interrupted session {}", session.id);
    }
}

/// Check if interrupt confirmation has timed out.
/// Returns None if timed out, otherwise returns the original value.
pub fn check_interrupt_timeout(pending_since: Option<Instant>) -> Option<Instant> {
    pending_since.filter(|pending| {
        Instant::now().duration_since(*pending).as_secs_f32() < INTERRUPT_CONFIRM_TIMEOUT_SECS
    })
}

// =============================================================================
// Plan Mode
// =============================================================================

/// Add the current pending permission's tool to the session's runtime allowlist.
/// Returns the key that was added (for logging), or None if no pending permission.
pub fn allow_always(session_manager: &mut SessionManager) -> Option<String> {
    let session = session_manager.get_active_mut()?;
    let agentic = session.agentic.as_mut()?;

    // Find the last pending (unresponded) permission request
    let (tool_name, tool_input) = session.chat.iter().rev().find_map(|msg| {
        if let crate::messages::Message::PermissionRequest(req) = msg {
            if req.response.is_none() {
                return Some((req.tool_name.clone(), req.tool_input.clone()));
            }
        }
        None
    })?;

    let key = agentic.add_runtime_allow(&tool_name, &tool_input);
    if let Some(ref k) = key {
        tracing::info!("allow_always: added runtime allow for '{}'", k);
    }
    key
}

/// Cycle permission mode for the active session: Default → Plan → AcceptEdits → Default.
/// Info needed to publish a permission mode command to a remote host.
pub struct ModeCommandPublish {
    pub session_id: String,
    pub mode: &'static str,
}

pub fn cycle_permission_mode(
    session_manager: &mut SessionManager,
    backend: &dyn AiBackend,
    ctx: &egui::Context,
) -> Option<ModeCommandPublish> {
    let session = session_manager.get_active_mut()?;
    let is_remote = session.is_remote();
    let session_id = session.id;
    let agentic = session.agentic.as_mut()?;

    let new_mode = match agentic.permission_mode {
        PermissionMode::Default => PermissionMode::Plan,
        PermissionMode::Plan => PermissionMode::AcceptEdits,
        _ => PermissionMode::Default,
    };
    agentic.permission_mode = new_mode;

    let mode_str = crate::session::permission_mode_to_str(new_mode);

    let result = if is_remote {
        // Remote session: return info for caller to publish command event
        let event_sid = agentic.event_session_id().to_string();
        Some(ModeCommandPublish {
            session_id: event_sid,
            mode: mode_str,
        })
    } else {
        // Local session: apply directly and mark dirty for state event publish
        let backend_sid = format!("dave-session-{}", session_id);
        backend.set_permission_mode(backend_sid, new_mode, ctx.clone());
        session.state_dirty = true;
        None
    };

    tracing::debug!(
        "Cycled permission mode for session {} to {:?} (remote={})",
        session_id,
        new_mode,
        is_remote,
    );

    result
}

/// Exit plan mode for the active session (switch to Default mode).
pub fn exit_plan_mode(
    session_manager: &mut SessionManager,
    backend: &dyn AiBackend,
    ctx: &egui::Context,
) {
    if let Some(session) = session_manager.get_active_mut() {
        if let Some(agentic) = &mut session.agentic {
            agentic.permission_mode = PermissionMode::Default;
            let session_id = format!("dave-session-{}", session.id);
            backend.set_permission_mode(session_id, PermissionMode::Default, ctx.clone());
            tracing::debug!("Exited plan mode for session {}", session.id);
        }
    }
}

// =============================================================================
// Permission Handling
// =============================================================================

/// Get the first pending permission request ID for the active session.
pub fn first_pending_permission(session_manager: &SessionManager) -> Option<uuid::Uuid> {
    let session = session_manager.get_active()?;
    if session.is_remote() {
        // Remote: find first unresponded PermissionRequest in chat
        let responded = session.agentic.as_ref().map(|a| &a.permissions.responded);
        for msg in &session.chat {
            if let Message::PermissionRequest(req) = msg {
                if req.response.is_none() && responded.is_none_or(|ids| !ids.contains(&req.id)) {
                    return Some(req.id);
                }
            }
        }
        None
    } else {
        // Local: check oneshot senders
        session
            .agentic
            .as_ref()
            .and_then(|a| a.permissions.pending.keys().next().copied())
    }
}

/// Get the tool name of the first pending permission request.
pub fn pending_permission_tool_name(session_manager: &SessionManager) -> Option<&str> {
    let request_id = first_pending_permission(session_manager)?;
    let session = session_manager.get_active()?;

    for msg in &session.chat {
        if let Message::PermissionRequest(req) = msg {
            if req.id == request_id {
                return Some(&req.tool_name);
            }
        }
    }

    None
}

/// Check if the first pending permission is an AskUserQuestion tool call.
pub fn has_pending_question(session_manager: &SessionManager) -> bool {
    pending_permission_tool_name(session_manager) == Some("AskUserQuestion")
}

/// Check if the first pending permission is an ExitPlanMode tool call.
pub fn has_pending_exit_plan_mode(session_manager: &SessionManager) -> bool {
    pending_permission_tool_name(session_manager) == Some("ExitPlanMode")
}

/// Data needed to publish a permission response to relays.
pub struct PermissionPublish {
    pub perm_id: uuid::Uuid,
    pub allowed: bool,
    pub message: Option<String>,
}

/// Handle a permission response (from UI button or keybinding).
pub fn handle_permission_response(
    session_manager: &mut SessionManager,
    request_id: uuid::Uuid,
    response: PermissionResponse,
) -> Option<PermissionPublish> {
    let session = session_manager.get_active_mut()?;

    let is_remote = session.is_remote();

    let response_type = match &response {
        PermissionResponse::Allow { .. } => crate::messages::PermissionResponseType::Allowed,
        PermissionResponse::Deny { .. } => crate::messages::PermissionResponseType::Denied,
    };

    // Extract relay-publish info before we move `response`.
    let allowed = matches!(&response, PermissionResponse::Allow { .. });
    let message = match &response {
        PermissionResponse::Allow { message } => message.clone(),
        PermissionResponse::Deny { reason } => Some(reason.clone()),
    };

    // If Allow has a message, add it as a User message to the chat
    if let PermissionResponse::Allow { message: Some(msg) } = &response {
        if !msg.is_empty() {
            session.chat.push(Message::User(msg.clone()));
        }
    }

    // Clear permission message state (agentic only)
    if let Some(agentic) = &mut session.agentic {
        agentic.permission_message_state = PermissionMessageState::None;
    }

    // Resolve through the single unified path
    if let Some(agentic) = &mut session.agentic {
        agentic.permissions.resolve(
            &mut session.chat,
            request_id,
            response_type,
            None,
            is_remote,
            Some(response),
        );

        // Optimistically set remote status to Working so the phone doesn't
        // have to wait for the full round-trip (phone→relay→desktop→relay→phone)
        // before auto-steal can move on. The desktop will publish the real
        // status once it processes the permission response.
        if is_remote {
            agentic.remote_status = Some(crate::agent_status::AgentStatus::Working);
        }
    }

    Some(PermissionPublish {
        perm_id: request_id,
        allowed,
        message,
    })
}

/// Handle a user's response to an AskUserQuestion tool call.
pub fn handle_question_response(
    session_manager: &mut SessionManager,
    request_id: uuid::Uuid,
    answers: Vec<QuestionAnswer>,
) -> Option<PermissionPublish> {
    let session = session_manager.get_active_mut()?;

    let is_remote = session.is_remote();

    // Find the original AskUserQuestion request to get the question labels
    let questions_input = session.chat.iter().find_map(|msg| {
        if let Message::PermissionRequest(req) = msg {
            if req.id == request_id && req.tool_name == "AskUserQuestion" {
                serde_json::from_value::<AskUserQuestionInput>(req.tool_input.clone()).ok()
            } else {
                None
            }
        } else {
            None
        }
    });

    // Format answers as JSON for the tool response, and build summary for display
    let (formatted_response, answer_summary) = if let Some(ref questions) = questions_input {
        let mut answers_obj = serde_json::Map::new();
        let mut summary_entries = Vec::with_capacity(questions.questions.len());

        for (q_idx, (question, answer)) in
            questions.questions.iter().zip(answers.iter()).enumerate()
        {
            let mut answer_obj = serde_json::Map::new();

            // Map selected indices to option labels
            let selected_labels: Vec<String> = answer
                .selected
                .iter()
                .filter_map(|&idx| question.options.get(idx).map(|o| o.label.clone()))
                .collect();

            answer_obj.insert(
                "selected".to_string(),
                serde_json::Value::Array(
                    selected_labels
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );

            // Build display text for summary
            let mut display_parts = selected_labels;
            if let Some(ref other) = answer.other_text {
                if !other.is_empty() {
                    answer_obj.insert(
                        "other".to_string(),
                        serde_json::Value::String(other.clone()),
                    );
                    display_parts.push(format!("Other: {}", other));
                }
            }

            // Use header as the key, fall back to question index
            let key = if !question.header.is_empty() {
                question.header.clone()
            } else {
                format!("question_{}", q_idx)
            };
            answers_obj.insert(key.clone(), serde_json::Value::Object(answer_obj));

            summary_entries.push(AnswerSummaryEntry {
                header: key,
                answer: display_parts.join(", "),
            });
        }

        (
            serde_json::json!({ "answers": answers_obj }).to_string(),
            Some(AnswerSummary {
                entries: summary_entries,
            }),
        )
    } else {
        // Fallback: just serialize the answers directly
        (
            serde_json::to_string(&answers).unwrap_or_else(|_| "{}".to_string()),
            None,
        )
    };

    // Clean up transient answer state
    if let Some(agentic) = &mut session.agentic {
        agentic.question_answers.remove(&request_id);
        agentic.question_index.remove(&request_id);

        // Resolve through the single unified path
        let oneshot_response = PermissionResponse::Allow {
            message: Some(formatted_response.clone()),
        };
        agentic.permissions.resolve(
            &mut session.chat,
            request_id,
            crate::messages::PermissionResponseType::Allowed,
            answer_summary,
            is_remote,
            Some(oneshot_response),
        );

        // Optimistically set remote status to Working (same as permission response)
        if is_remote {
            agentic.remote_status = Some(crate::agent_status::AgentStatus::Working);
        }
    }

    Some(PermissionPublish {
        perm_id: request_id,
        allowed: true,
        message: Some(formatted_response),
    })
}

// =============================================================================
// Agent Navigation
// =============================================================================

/// Switch to a session and optionally focus it in the scene.
///
/// Handles the common pattern of: switch_to → scene.select → scene.focus_on → focus_requested.
/// Used by navigation, focus queue, and auto-steal-focus operations.
pub fn switch_and_focus_session(
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    show_scene: bool,
    id: SessionId,
) {
    session_manager.switch_to(id);
    if show_scene {
        scene.select(id);
        if let Some(session) = session_manager.get(id) {
            if let Some(agentic) = &session.agentic {
                scene.focus_on(agentic.scene_position);
            }
        }
    }
    if let Some(session) = session_manager.get_mut(id) {
        if !session.has_pending_permissions() {
            session.focus_requested = true;
        }
    }
}

/// Switch to agent by index in the visual display order (0-indexed).
pub fn switch_to_agent_by_index(
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    show_scene: bool,
    index: usize,
) {
    let ids = session_manager.visual_order();
    if let Some(&id) = ids.get(index) {
        switch_and_focus_session(session_manager, scene, show_scene, id);
    }
}

/// Cycle agents using a direction function that computes the next index.
fn cycle_agent(
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    show_scene: bool,
    index_fn: impl FnOnce(usize, usize) -> usize,
) {
    let ids = session_manager.visual_order();
    if ids.is_empty() {
        return;
    }
    let current_idx = session_manager
        .active_id()
        .and_then(|active| ids.iter().position(|&id| id == active))
        .unwrap_or(0);
    let next_idx = index_fn(current_idx, ids.len());
    if let Some(&id) = ids.get(next_idx) {
        switch_and_focus_session(session_manager, scene, show_scene, id);
    }
}

/// Cycle to the next agent.
pub fn cycle_next_agent(
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    cycle_agent(session_manager, scene, show_scene, |idx, len| {
        (idx + 1) % len
    });
}

/// Cycle to the previous agent.
pub fn cycle_prev_agent(
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    cycle_agent(session_manager, scene, show_scene, |idx, len| {
        if idx == 0 {
            len - 1
        } else {
            idx - 1
        }
    });
}

// =============================================================================
// Focus Queue Operations
// =============================================================================

/// Navigate to the next item in the focus queue.
/// Done items are automatically dismissed after switching to them.
pub fn focus_queue_next(
    session_manager: &mut SessionManager,
    focus_queue: &mut FocusQueue,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    if let Some(session_id) = focus_queue.next() {
        switch_and_focus_session(session_manager, scene, show_scene, session_id);
        dismiss_done(session_manager, focus_queue, session_id);
    }
}

/// Navigate to the previous item in the focus queue.
/// Done items are automatically dismissed after switching to them.
pub fn focus_queue_prev(
    session_manager: &mut SessionManager,
    focus_queue: &mut FocusQueue,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    if let Some(session_id) = focus_queue.prev() {
        switch_and_focus_session(session_manager, scene, show_scene, session_id);
        dismiss_done(session_manager, focus_queue, session_id);
    }
}

/// Dismiss a Done session from the focus queue and clear its indicator.
fn dismiss_done(
    session_manager: &mut SessionManager,
    focus_queue: &mut FocusQueue,
    session_id: SessionId,
) {
    if focus_queue.get_session_priority(session_id) == Some(FocusPriority::Done) {
        focus_queue.dequeue_done(session_id);
        if let Some(session) = session_manager.get_mut(session_id) {
            if session.indicator == Some(FocusPriority::Done) {
                session.indicator = None;
                session.state_dirty = true;
            }
        }
    }
}

/// Toggle Done status for the current focus queue item.
pub fn focus_queue_toggle_done(focus_queue: &mut FocusQueue) {
    if let Some(entry) = focus_queue.current() {
        if entry.priority == FocusPriority::Done {
            focus_queue.dequeue(entry.session_id);
        }
    }
}

/// Toggle auto-steal focus mode.
/// Returns the new auto_steal_focus state.
pub fn toggle_auto_steal(
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    show_scene: bool,
    auto_steal_focus: bool,
    home_session: &mut Option<SessionId>,
) -> bool {
    let new_state = !auto_steal_focus;

    if new_state {
        // Enabling: record current session as home
        *home_session = session_manager.active_id();
        tracing::debug!("Auto-steal focus enabled, home session: {:?}", home_session);
    } else {
        // Disabling: switch back to home session if set
        if let Some(home_id) = home_session.take() {
            switch_and_focus_session(session_manager, scene, show_scene, home_id);
            tracing::debug!("Auto-steal focus disabled, returned to home session");
        }
    }

    // Request focus on input after toggle
    if let Some(session) = session_manager.get_active_mut() {
        session.focus_requested = true;
    }

    new_state
}

/// Process auto-steal focus logic: switch to focus queue items as needed.
/// Returns true if focus was stolen (switched to a NeedsInput or Done session),
/// which can be used to raise the OS window.
pub fn process_auto_steal_focus(
    session_manager: &mut SessionManager,
    focus_queue: &mut FocusQueue,
    scene: &mut AgentScene,
    show_scene: bool,
    auto_steal_focus: bool,
    home_session: &mut Option<SessionId>,
) -> bool {
    if !auto_steal_focus {
        return false;
    }

    let has_needs_input = focus_queue.has_needs_input();
    let has_done = focus_queue.has_done();

    if has_needs_input {
        // There are NeedsInput items - check if we need to steal focus
        let current_session = session_manager.active_id();
        let current_priority = current_session.and_then(|id| focus_queue.get_session_priority(id));
        let already_on_needs_input = current_priority == Some(FocusPriority::NeedsInput);

        if !already_on_needs_input {
            // Save current session before stealing (only if we haven't saved yet)
            if home_session.is_none() {
                *home_session = current_session;
                tracing::debug!("Auto-steal: saved home session {:?}", home_session);
            }

            // Jump to first NeedsInput item
            if let Some(idx) = focus_queue.first_needs_input_index() {
                focus_queue.set_cursor(idx);
                if let Some(entry) = focus_queue.current() {
                    switch_and_focus_session(session_manager, scene, show_scene, entry.session_id);
                    tracing::debug!("Auto-steal: switched to session {:?}", entry.session_id);
                    return true;
                }
            }
        }
    } else if has_done {
        // No NeedsInput but there are Done items - auto-focus those
        let current_session = session_manager.active_id();
        let current_priority = current_session.and_then(|id| focus_queue.get_session_priority(id));
        let already_on_done = current_priority == Some(FocusPriority::Done);

        if !already_on_done {
            // Save current session before stealing (only if we haven't saved yet)
            if home_session.is_none() {
                *home_session = current_session;
                tracing::debug!("Auto-steal: saved home session {:?}", home_session);
            }

            // Jump to first Done item (keep in queue; cleared externally
            // when the session's clearing condition is met)
            if let Some(idx) = focus_queue.first_done_index() {
                focus_queue.set_cursor(idx);
                if let Some(entry) = focus_queue.current() {
                    let sid = entry.session_id;
                    switch_and_focus_session(session_manager, scene, show_scene, sid);
                    tracing::debug!("Auto-steal: switched to Done session {:?}", sid);
                    return true;
                }
            }
        }
    } else if let Some(home_id) = home_session.take() {
        // No more NeedsInput or Done items - return to saved session
        switch_and_focus_session(session_manager, scene, show_scene, home_id);
        tracing::debug!("Auto-steal: returned to home session {:?}", home_id);
    }

    false
}

// =============================================================================
// External Editor
// =============================================================================

/// Open an external editor for composing the input text (non-blocking).
///
/// Launches `$VISUAL` or `$EDITOR` (default: vim) in a **new** terminal
/// window so it never hijacks the terminal notedeck was launched from.
/// On macOS, uses `$TERM_PROGRAM` to detect the user's terminal; on
/// Linux, checks `$TERMINAL` then probes common emulators.
pub fn open_external_editor(session_manager: &mut SessionManager) {
    // Don't spawn another editor if one is already pending
    if session_manager.pending_editor.is_some() {
        tracing::warn!("External editor already in progress");
        return;
    }

    let Some(session) = session_manager.get_active_mut() else {
        return;
    };
    let session_id = session.id;
    let input_content = session.input.clone();

    // Create temp file with a unique name to avoid vim swap file conflicts
    let temp_path = std::env::temp_dir().join(format!(
        "notedeck_input_{}.txt",
        std::process::id()
            ^ (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u32)
                .unwrap_or(0))
    ));
    if let Err(e) = std::fs::write(&temp_path, &input_content) {
        tracing::error!("Failed to write temp file for external editor: {}", e);
        return;
    }

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vim".to_string());

    // Always open in a new terminal window so we never steal the
    // launching terminal's tty (which breaks when the app is disowned).
    let spawn_result = if cfg!(target_os = "macos") {
        spawn_macos_editor(&editor, &temp_path)
    } else {
        spawn_linux_editor(&editor, &temp_path)
    };

    match spawn_result {
        Ok(child) => {
            session_manager.pending_editor = Some(EditorJob {
                child,
                temp_path,
                session_id,
            });
            tracing::debug!("External editor spawned for session {}", session_id);
        }
        Err(e) => {
            tracing::error!("Failed to spawn external editor: {}", e);
            let _ = std::fs::remove_file(&temp_path);
        }
    }
}

/// macOS: open the editor in a new terminal window.
///
/// Uses `$TERM_PROGRAM` to detect the running terminal and launch a new
/// window with the right CLI invocation. Falls back to `open -W -t`
/// (system default text editor) if the terminal is unknown.
fn spawn_macos_editor(
    editor: &str,
    file: &std::path::Path,
) -> std::io::Result<std::process::Child> {
    use std::process::{Command, Stdio};

    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    tracing::debug!("macOS TERM_PROGRAM={}, editor={}", term_program, editor);

    match term_program.as_str() {
        "WezTerm" => {
            let bin = find_macos_bin("wezterm", "WezTerm");
            Command::new(&bin)
                .args(["start", "--always-new-process", "--"])
                .arg(editor)
                .arg(file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
        "kitty" => {
            let bin = find_macos_bin("kitty", "kitty");
            Command::new(&bin)
                .arg(editor)
                .arg(file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
        "Alacritty" | "alacritty" => {
            let bin = find_macos_bin("alacritty", "Alacritty");
            Command::new(&bin)
                .arg("-e")
                .arg(editor)
                .arg(file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
        _ => {
            // Unknown terminal — open in system default text editor
            tracing::debug!(
                "Unknown TERM_PROGRAM '{}', using `open -W -t`",
                term_program
            );
            Command::new("open")
                .arg("-W")
                .arg("-t")
                .arg(file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
    }
}

/// Find a binary on PATH or inside /Applications/<app>.app/Contents/MacOS/.
fn find_macos_bin(bin_name: &str, app_name: &str) -> String {
    use std::process::Command;

    // Try PATH first
    if let Ok(output) = Command::new("which").arg(bin_name).output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return path;
            }
        }
    }

    // Check app bundle
    let bundle = format!("/Applications/{}.app/Contents/MacOS/{}", app_name, bin_name);
    if std::path::Path::new(&bundle).exists() {
        return bundle;
    }

    bin_name.to_string()
}

/// Linux: spawn a terminal emulator with the editor.
fn spawn_linux_editor(
    editor: &str,
    file: &std::path::Path,
) -> std::io::Result<std::process::Child> {
    use std::process::Command;

    if let Ok(terminal) = std::env::var("TERMINAL") {
        return Command::new(&terminal)
            .arg("-e")
            .arg(editor)
            .arg(file)
            .spawn();
    }

    // Auto-detect. Each terminal has different exec syntax.
    let terminals: &[(&str, &[&str])] = &[
        ("wezterm", &["start", "--always-new-process", "--"]),
        ("alacritty", &["-e"]),
        ("kitty", &[]),
        ("gnome-terminal", &["--"]),
        ("konsole", &["-e"]),
        ("urxvtc", &["-e"]),
        ("urxvt", &["-e"]),
        ("xterm", &["-e"]),
    ];

    for (name, prefix_args) in terminals {
        let found = Command::new("which")
            .arg(name)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if found {
            tracing::debug!("Opening editor via {}: {} {}", name, editor, file.display());
            let mut cmd = Command::new(name);
            for arg in *prefix_args {
                cmd.arg(arg);
            }
            cmd.arg(editor).arg(file);
            return cmd.spawn();
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "No terminal emulator found. Set $TERMINAL or $VISUAL.",
    ))
}

/// Poll for external editor completion (called each frame).
pub fn poll_editor_job(session_manager: &mut SessionManager) {
    let Some(ref mut job) = session_manager.pending_editor else {
        return;
    };

    // Non-blocking check if child has exited
    match job.child.try_wait() {
        Ok(Some(status)) => {
            let session_id = job.session_id;
            let temp_path = job.temp_path.clone();

            if status.success() {
                match std::fs::read_to_string(&temp_path) {
                    Ok(content) => {
                        if let Some(session) = session_manager.get_mut(session_id) {
                            session.input = content;
                            session.focus_requested = true;
                            tracing::debug!(
                                "External editor completed, updated input for session {}",
                                session_id
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to read temp file after editing: {}", e);
                    }
                }
            } else {
                tracing::warn!("External editor exited with status: {}", status);
            }

            if let Err(e) = std::fs::remove_file(&temp_path) {
                tracing::error!("Failed to remove temp file: {}", e);
            }

            session_manager.pending_editor = None;
        }
        Ok(None) => {
            // Editor still running
        }
        Err(e) => {
            tracing::error!("Failed to poll editor process: {}", e);
            let temp_path = job.temp_path.clone();
            let _ = std::fs::remove_file(&temp_path);
            session_manager.pending_editor = None;
        }
    }
}

// =============================================================================
// Session Management
// =============================================================================

/// Create a new session with the given cwd.
#[allow(clippy::too_many_arguments)]
pub fn create_session_with_cwd(
    session_manager: &mut SessionManager,
    directory_picker: &mut DirectoryPicker,
    scene: &mut AgentScene,
    show_scene: bool,
    ai_mode: AiMode,
    cwd: PathBuf,
    hostname: &str,
    backend_type: BackendType,
    ndb: Option<&nostrdb::Ndb>,
) -> SessionId {
    directory_picker.add_recent(cwd.clone());

    let id = session_manager.new_session(cwd, ai_mode, backend_type);
    if let Some(session) = session_manager.get_mut(id) {
        session.details.hostname = hostname.to_string();
        session.focus_requested = true;
        if show_scene {
            scene.select(id);
            if let Some(agentic) = &session.agentic {
                scene.focus_on(agentic.scene_position);
            }
        }

        // Set up ndb subscriptions so remote clients can send messages
        // to this session (e.g. to kickstart the backend remotely).
        if let (Some(ndb), Some(agentic)) = (ndb, &mut session.agentic) {
            let event_id = agentic.event_session_id().to_string();
            crate::setup_conversation_subscription(agentic, &event_id, ndb);
            crate::setup_conversation_action_subscription(agentic, &event_id, ndb);
        }
    }
    session_manager.rebuild_host_groups();
    id
}

/// Create a new session that resumes an existing Claude conversation.
#[allow(clippy::too_many_arguments)]
pub fn create_resumed_session_with_cwd(
    session_manager: &mut SessionManager,
    directory_picker: &mut DirectoryPicker,
    scene: &mut AgentScene,
    show_scene: bool,
    ai_mode: AiMode,
    cwd: PathBuf,
    resume_session_id: String,
    title: String,
    hostname: &str,
    backend_type: BackendType,
) -> SessionId {
    directory_picker.add_recent(cwd.clone());

    let id =
        session_manager.new_resumed_session(cwd, resume_session_id, title, ai_mode, backend_type);
    if let Some(session) = session_manager.get_mut(id) {
        session.details.hostname = hostname.to_string();
        session.focus_requested = true;
        if show_scene {
            scene.select(id);
            if let Some(agentic) = &session.agentic {
                scene.focus_on(agentic.scene_position);
            }
        }
    }
    session_manager.rebuild_host_groups();
    id
}

/// Clone the active agent, creating a new session with the same working directory.
pub fn clone_active_agent(
    session_manager: &mut SessionManager,
    directory_picker: &mut DirectoryPicker,
    scene: &mut AgentScene,
    show_scene: bool,
    ai_mode: AiMode,
    hostname: &str,
) -> Option<SessionId> {
    let active = session_manager.get_active()?;
    let cwd = active.cwd().cloned()?;
    let backend_type = active.backend_type;
    Some(create_session_with_cwd(
        session_manager,
        directory_picker,
        scene,
        show_scene,
        ai_mode,
        cwd,
        hostname,
        backend_type,
        None,
    ))
}

/// Delete a session and clean up backend resources.
pub fn delete_session(
    session_manager: &mut SessionManager,
    focus_queue: &mut FocusQueue,
    backend: &dyn AiBackend,
    directory_picker: &mut DirectoryPicker,
    id: SessionId,
) -> bool {
    focus_queue.remove_session(id);
    if session_manager.delete_session(id) {
        let session_id = format!("dave-session-{}", id);
        backend.cleanup_session(session_id);

        if session_manager.is_empty() {
            directory_picker.open();
        }
        true
    } else {
        false
    }
}

// =============================================================================
// Send Action Handling
// =============================================================================

/// Handle the /cd command if present in input.
/// Returns Some(Ok(path)) if cd succeeded, Some(Err(())) if cd failed, None if not a cd command.
pub fn handle_cd_command(session: &mut ChatSession) -> Option<Result<PathBuf, ()>> {
    let input = session.input.trim().to_string();
    if !input.starts_with("/cd ") {
        return None;
    }

    let path_str = input.strip_prefix("/cd ").unwrap().trim();
    let path = PathBuf::from(path_str);
    session.input.clear();

    if path.exists() && path.is_dir() {
        if let Some(agentic) = &mut session.agentic {
            agentic.cwd = path.clone();
        }
        session.chat.push(Message::System(format!(
            "Working directory set to: {}",
            path.display()
        )));
        Some(Ok(path))
    } else {
        session
            .chat
            .push(Message::Error(format!("Invalid directory: {}", path_str)));
        Some(Err(()))
    }
}
