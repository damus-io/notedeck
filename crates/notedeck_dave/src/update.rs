//! Helper functions for the Dave update loop.
//!
//! These are standalone functions with explicit inputs to reduce the complexity
//! of the main Dave struct and make the code more testable and reusable.

use crate::backend::AiBackend;
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
            agentic.pending_permissions.clear();
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

/// Toggle plan mode for the active session.
pub fn toggle_plan_mode(
    session_manager: &mut SessionManager,
    backend: &dyn AiBackend,
    ctx: &egui::Context,
) {
    if let Some(session) = session_manager.get_active_mut() {
        if let Some(agentic) = &mut session.agentic {
            let new_mode = match agentic.permission_mode {
                PermissionMode::Plan => PermissionMode::Default,
                _ => PermissionMode::Plan,
            };
            agentic.permission_mode = new_mode;

            let session_id = format!("dave-session-{}", session.id);
            backend.set_permission_mode(session_id, new_mode, ctx.clone());

            tracing::debug!(
                "Toggled plan mode for session {} to {:?}",
                session.id,
                new_mode
            );
        }
    }
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
    session_manager
        .get_active()
        .and_then(|session| session.agentic.as_ref())
        .and_then(|agentic| agentic.pending_permissions.keys().next().copied())
}

/// Get the tool name of the first pending permission request.
pub fn pending_permission_tool_name(session_manager: &SessionManager) -> Option<&str> {
    let session = session_manager.get_active()?;
    let agentic = session.agentic.as_ref()?;
    let request_id = agentic.pending_permissions.keys().next()?;

    for msg in &session.chat {
        if let Message::PermissionRequest(req) = msg {
            if &req.id == request_id {
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

/// Handle a permission response (from UI button or keybinding).
pub fn handle_permission_response(
    session_manager: &mut SessionManager,
    request_id: uuid::Uuid,
    response: PermissionResponse,
) {
    let Some(session) = session_manager.get_active_mut() else {
        return;
    };

    // Record the response type in the message for UI display
    let response_type = match &response {
        PermissionResponse::Allow { .. } => crate::messages::PermissionResponseType::Allowed,
        PermissionResponse::Deny { .. } => crate::messages::PermissionResponseType::Denied,
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

    for msg in &mut session.chat {
        if let Message::PermissionRequest(req) = msg {
            if req.id == request_id {
                req.response = Some(response_type);
                break;
            }
        }
    }

    if let Some(agentic) = &mut session.agentic {
        if let Some(sender) = agentic.pending_permissions.remove(&request_id) {
            if sender.send(response).is_err() {
                tracing::error!(
                    "Failed to send permission response for request {}",
                    request_id
                );
            }
        } else {
            tracing::warn!("No pending permission found for request {}", request_id);
        }
    }
}

/// Handle a user's response to an AskUserQuestion tool call.
pub fn handle_question_response(
    session_manager: &mut SessionManager,
    request_id: uuid::Uuid,
    answers: Vec<QuestionAnswer>,
) {
    let Some(session) = session_manager.get_active_mut() else {
        return;
    };

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

    // Mark the request as allowed in the UI and store the summary for display
    for msg in &mut session.chat {
        if let Message::PermissionRequest(req) = msg {
            if req.id == request_id {
                req.response = Some(crate::messages::PermissionResponseType::Allowed);
                req.answer_summary = answer_summary.clone();
                break;
            }
        }
    }

    // Clean up transient answer state and send response (agentic only)
    if let Some(agentic) = &mut session.agentic {
        agentic.question_answers.remove(&request_id);
        agentic.question_index.remove(&request_id);

        // Send the response through the permission channel
        if let Some(sender) = agentic.pending_permissions.remove(&request_id) {
            let response = PermissionResponse::Allow {
                message: Some(formatted_response),
            };
            if sender.send(response).is_err() {
                tracing::error!(
                    "Failed to send question response for request {}",
                    request_id
                );
            }
        } else {
            tracing::warn!("No pending permission found for request {}", request_id);
        }
    }
}

// =============================================================================
// Agent Navigation
// =============================================================================

/// Switch to agent by index in the ordered list (0-indexed).
pub fn switch_to_agent_by_index(
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    show_scene: bool,
    index: usize,
) {
    let ids = session_manager.session_ids();
    if let Some(&id) = ids.get(index) {
        session_manager.switch_to(id);
        if show_scene {
            scene.select(id);
        }
        if let Some(session) = session_manager.get_mut(id) {
            if !session.has_pending_permissions() {
                session.focus_requested = true;
            }
        }
    }
}

/// Cycle to the next agent.
pub fn cycle_next_agent(
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    let ids = session_manager.session_ids();
    if ids.is_empty() {
        return;
    }
    let current_idx = session_manager
        .active_id()
        .and_then(|active| ids.iter().position(|&id| id == active))
        .unwrap_or(0);
    let next_idx = (current_idx + 1) % ids.len();
    if let Some(&id) = ids.get(next_idx) {
        session_manager.switch_to(id);
        if show_scene {
            scene.select(id);
        }
        if let Some(session) = session_manager.get_mut(id) {
            if !session.has_pending_permissions() {
                session.focus_requested = true;
            }
        }
    }
}

/// Cycle to the previous agent.
pub fn cycle_prev_agent(
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    let ids = session_manager.session_ids();
    if ids.is_empty() {
        return;
    }
    let current_idx = session_manager
        .active_id()
        .and_then(|active| ids.iter().position(|&id| id == active))
        .unwrap_or(0);
    let prev_idx = if current_idx == 0 {
        ids.len() - 1
    } else {
        current_idx - 1
    };
    if let Some(&id) = ids.get(prev_idx) {
        session_manager.switch_to(id);
        if show_scene {
            scene.select(id);
        }
        if let Some(session) = session_manager.get_mut(id) {
            if !session.has_pending_permissions() {
                session.focus_requested = true;
            }
        }
    }
}

// =============================================================================
// Focus Queue Operations
// =============================================================================

/// Navigate to the next item in the focus queue.
pub fn focus_queue_next(
    session_manager: &mut SessionManager,
    focus_queue: &mut FocusQueue,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    if let Some(session_id) = focus_queue.next() {
        session_manager.switch_to(session_id);
        if show_scene {
            scene.select(session_id);
            if let Some(session) = session_manager.get(session_id) {
                if let Some(agentic) = &session.agentic {
                    scene.focus_on(agentic.scene_position);
                }
            }
        }
        if let Some(session) = session_manager.get_mut(session_id) {
            if !session.has_pending_permissions() {
                session.focus_requested = true;
            }
        }
    }
}

/// Navigate to the previous item in the focus queue.
pub fn focus_queue_prev(
    session_manager: &mut SessionManager,
    focus_queue: &mut FocusQueue,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    if let Some(session_id) = focus_queue.prev() {
        session_manager.switch_to(session_id);
        if show_scene {
            scene.select(session_id);
            if let Some(session) = session_manager.get(session_id) {
                if let Some(agentic) = &session.agentic {
                    scene.focus_on(agentic.scene_position);
                }
            }
        }
        if let Some(session) = session_manager.get_mut(session_id) {
            if !session.has_pending_permissions() {
                session.focus_requested = true;
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
            session_manager.switch_to(home_id);
            if show_scene {
                scene.select(home_id);
                if let Some(session) = session_manager.get(home_id) {
                    if let Some(agentic) = &session.agentic {
                        scene.focus_on(agentic.scene_position);
                    }
                }
            }
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
pub fn process_auto_steal_focus(
    session_manager: &mut SessionManager,
    focus_queue: &mut FocusQueue,
    scene: &mut AgentScene,
    show_scene: bool,
    auto_steal_focus: bool,
    home_session: &mut Option<SessionId>,
) {
    if !auto_steal_focus {
        return;
    }

    let has_needs_input = focus_queue.has_needs_input();

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
                    session_manager.switch_to(entry.session_id);
                    if show_scene {
                        scene.select(entry.session_id);
                        if let Some(session) = session_manager.get(entry.session_id) {
                            if let Some(agentic) = &session.agentic {
                                scene.focus_on(agentic.scene_position);
                            }
                        }
                    }
                    tracing::debug!("Auto-steal: switched to session {:?}", entry.session_id);
                }
            }
        }
    } else if let Some(home_id) = home_session.take() {
        // No more NeedsInput items - return to saved session
        session_manager.switch_to(home_id);
        if show_scene {
            scene.select(home_id);
            if let Some(session) = session_manager.get(home_id) {
                if let Some(agentic) = &session.agentic {
                    scene.focus_on(agentic.scene_position);
                }
            }
        }
        tracing::debug!("Auto-steal: returned to home session {:?}", home_id);
    }
}

// =============================================================================
// External Editor
// =============================================================================

/// Try to find a common terminal emulator.
pub fn find_terminal() -> Option<String> {
    use std::process::Command;
    let terminals = [
        "alacritty",
        "kitty",
        "gnome-terminal",
        "konsole",
        "urxvtc",
        "urxvt",
        "xterm",
    ];
    for term in terminals {
        if Command::new("which")
            .arg(term)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(term.to_string());
        }
    }
    None
}

/// Open an external editor for composing the input text (non-blocking).
pub fn open_external_editor(session_manager: &mut SessionManager) {
    use std::process::Command;

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

    // Create temp file with current input content
    let temp_path = std::env::temp_dir().join("notedeck_input.txt");
    if let Err(e) = std::fs::write(&temp_path, &input_content) {
        tracing::error!("Failed to write temp file for external editor: {}", e);
        return;
    }

    // Try $VISUAL first (GUI editors), then fall back to terminal + $EDITOR
    let visual = std::env::var("VISUAL").ok();
    let editor = std::env::var("EDITOR").ok();

    let spawn_result = if let Some(visual_editor) = visual {
        // $VISUAL is set - use it directly (assumes GUI editor)
        tracing::debug!("Opening external editor via $VISUAL: {}", visual_editor);
        Command::new(&visual_editor).arg(&temp_path).spawn()
    } else {
        // Fall back to terminal + $EDITOR
        let editor_cmd = editor.unwrap_or_else(|| "vim".to_string());
        let terminal = std::env::var("TERMINAL")
            .ok()
            .or_else(find_terminal)
            .unwrap_or_else(|| "xterm".to_string());

        tracing::debug!(
            "Opening external editor via terminal: {} -e {} {}",
            terminal,
            editor_cmd,
            temp_path.display()
        );
        Command::new(&terminal)
            .arg("-e")
            .arg(&editor_cmd)
            .arg(&temp_path)
            .spawn()
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
pub fn create_session_with_cwd(
    session_manager: &mut SessionManager,
    directory_picker: &mut DirectoryPicker,
    scene: &mut AgentScene,
    show_scene: bool,
    ai_mode: AiMode,
    cwd: PathBuf,
) -> SessionId {
    directory_picker.add_recent(cwd.clone());

    let id = session_manager.new_session(cwd, ai_mode);
    if let Some(session) = session_manager.get_mut(id) {
        session.focus_requested = true;
        if show_scene {
            scene.select(id);
            if let Some(agentic) = &session.agentic {
                scene.focus_on(agentic.scene_position);
            }
        }
    }
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
) -> SessionId {
    directory_picker.add_recent(cwd.clone());

    let id = session_manager.new_resumed_session(cwd, resume_session_id, title, ai_mode);
    if let Some(session) = session_manager.get_mut(id) {
        session.focus_requested = true;
        if show_scene {
            scene.select(id);
            if let Some(agentic) = &session.agentic {
                scene.focus_on(agentic.scene_position);
            }
        }
    }
    id
}

/// Clone the active agent, creating a new session with the same working directory.
pub fn clone_active_agent(
    session_manager: &mut SessionManager,
    directory_picker: &mut DirectoryPicker,
    scene: &mut AgentScene,
    show_scene: bool,
    ai_mode: AiMode,
) -> Option<SessionId> {
    let cwd = session_manager
        .get_active()
        .and_then(|s| s.cwd().cloned())?;
    Some(create_session_with_cwd(
        session_manager,
        directory_picker,
        scene,
        show_scene,
        ai_mode,
        cwd,
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
