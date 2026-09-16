//! Helper functions for the Dave update loop.
//!
//! These are standalone functions with explicit inputs to reduce the complexity
//! of the main Dave struct and make the code more testable and reusable.

use crate::agent_status::AgentStatus;
use crate::backend::{AiBackend, BackendType, Model};
use crate::config::AiMode;
use crate::focus_queue::{AutoStealState, FocusPriority, FocusQueue};
use crate::messages::{
    AnswerSummary, AnswerSummaryEntry, Message, PermissionRequest, PermissionResponse,
    PermissionView, QuestionAnswer,
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

/// Info needed to publish an interrupt command to a remote host.
///
/// A remote session's turn runs on the host, so the client can't abort it
/// locally (its [`RemoteOnlyBackend`](crate::backend::RemoteOnlyBackend) interrupt
/// is a no-op). Instead the interrupt is published as a kind-1988 command the
/// host applies to its local backend — the caller turns this into that event.
pub struct InterruptPublish {
    /// The session's live-event `d`-tag (its `event_session_id`).
    pub session_id: String,
}

/// The result of an interrupt request: the new double-Escape confirmation state
/// plus, when the interrupt fired against a remote session, the command to
/// publish to its host.
pub struct InterruptOutcome {
    pub pending_since: Option<Instant>,
    pub publish: Option<InterruptPublish>,
}

/// Whether the active session has an in-flight turn that Escape can interrupt.
///
/// Local sessions stream tokens into `incoming_tokens`; remote sessions have no
/// local stream, so their liveness comes from the host's status (`Working`).
fn session_is_interruptible(session: &ChatSession) -> bool {
    if session.is_remote() {
        session.status() == AgentStatus::Working
    } else {
        session.incoming_tokens.is_some()
    }
}

/// The interrupt command to publish for a remote session, if it is one.
fn remote_interrupt_publish(session: &ChatSession) -> Option<InterruptPublish> {
    if !session.is_remote() {
        return None;
    }
    let session_id = session.agentic.as_ref()?.event_session_id().to_string();
    Some(InterruptPublish { session_id })
}

/// Handle an interrupt request - requires double-Escape to confirm.
///
/// Returns the new confirmation state and, for a confirmed interrupt on a remote
/// session, the [`InterruptPublish`] the caller must forward to the host. A local
/// session is interrupted directly on its backend and yields no publish.
pub fn handle_interrupt_request(
    session_manager: &SessionManager,
    backend: &dyn AiBackend,
    pending_since: Option<Instant>,
    ctx: &egui::Context,
) -> InterruptOutcome {
    // Only allow interrupt if there's an active AI operation
    let has_active_operation = session_manager
        .get_active()
        .map(session_is_interruptible)
        .unwrap_or(false);

    if !has_active_operation {
        return InterruptOutcome {
            pending_since: None,
            publish: None,
        };
    }

    let now = Instant::now();

    let Some(pending) = pending_since else {
        // First Escape press — arm the confirmation window.
        return InterruptOutcome {
            pending_since: Some(now),
            publish: None,
        };
    };

    if now.duration_since(pending).as_secs_f32() >= INTERRUPT_CONFIRM_TIMEOUT_SECS {
        // Timeout expired, treat as new first press.
        return InterruptOutcome {
            pending_since: Some(now),
            publish: None,
        };
    }

    // Second Escape within timeout — confirm, then take the one interrupt path.
    InterruptOutcome {
        pending_since: None,
        publish: execute_interrupt(session_manager, backend, ctx),
    }
}

/// Execute the actual interrupt on the active session.
///
/// The single place both interrupt gestures land — the Stop button directly, and
/// Escape once its double-press is confirmed — so the two cannot drift apart.
///
/// Interrupting asks the backend to abort the in-flight turn and does nothing
/// else. In particular it must NOT tear down local session state: on a
/// persistent-stream backend (Claude) `incoming_tokens` is the session's one
/// long-lived channel rather than a per-turn one, and the session actor outlives
/// an interrupt, so `stream_request` hands back no replacement receiver on the
/// next turn. Dropping it here left the session permanently deaf —
/// `process_events` skips a session with no receiver, so the aborted turn's
/// `QueryComplete` never arrived and `handle_stream_end` never ran: the partial
/// assistant message was never finalized or archived, `task_handle` was never
/// cleared, and the kind-31988 state carrying the `cli_session` tag that
/// `claude --resume` needs was never republished. Likewise the pending
/// permission map holds the oneshot senders answering the CLI's `can_use_tool`
/// RPCs; clearing it cancelled those while their request rows stayed unanswered
/// in chat. Winding the turn down is the stream's job, not the interrupt's.
///
/// For a remote session there is nothing local to abort, so this returns the
/// [`InterruptPublish`] the caller forwards to the host as a command; the host
/// applies it the same way (see `Dave::poll_remote_conversation_actions`).
pub fn execute_interrupt(
    session_manager: &SessionManager,
    backend: &dyn AiBackend,
    ctx: &egui::Context,
) -> Option<InterruptPublish> {
    let session = session_manager.get_active()?;
    if let Some(publish) = remote_interrupt_publish(session) {
        tracing::debug!("Interrupting remote session {}", session.id);
        return Some(publish);
    }
    let session_id = format!("dave-session-{}", session.id);
    backend.interrupt_session(session_id, notedeck::Waker::egui(ctx));
    tracing::debug!("Interrupted session {}", session.id);
    None
}

/// Exit a tool call by denying it and cancelling the current turn.
pub fn exit_tool_call(
    session_manager: &mut SessionManager,
    request_id: uuid::Uuid,
) -> Option<PermissionPublish> {
    handle_permission_response(
        session_manager,
        request_id,
        PermissionResponse::Cancel {
            reason: crate::messages::DEFAULT_EXIT_REASON.into(),
        },
    )
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

/// The next mode in the click / Ctrl+M cycle: Manual (Default) → Plan →
/// AcceptEdits → Auto → Manual. These are the modes the CLI honors as a runtime
/// switch. `Auto` is Claude Code's classifier-gated auto-execution mode: safe
/// tool calls run without prompting while flagged/dangerous ones still route to
/// dave's permission UI. `BypassPermissions` is deliberately never in the cycle
/// — dave can't enter it mid-session and it does no safety checking.
fn next_cycle_permission_mode(mode: PermissionMode) -> PermissionMode {
    match mode {
        PermissionMode::Default => PermissionMode::Plan,
        PermissionMode::Plan => PermissionMode::AcceptEdits,
        PermissionMode::AcceptEdits => PermissionMode::Auto,
        _ => PermissionMode::Default,
    }
}

pub fn cycle_permission_mode(
    session_manager: &mut SessionManager,
    backend: &dyn AiBackend,
    ctx: &egui::Context,
) -> Option<ModeCommandPublish> {
    let current = session_manager
        .get_active()?
        .agentic
        .as_ref()?
        .permission_mode;

    set_permission_mode(
        session_manager,
        backend,
        next_cycle_permission_mode(current),
        ctx,
    )
}

/// Apply an explicit permission mode (Default / Plan / AcceptEdits) to the
/// active session.
///
/// Shared by [`cycle_permission_mode`] and by deliberate selection from the mode
/// menu. Local sessions apply on the backend and mark state dirty; remote
/// sessions return a command for the caller to publish to the host.
pub fn set_permission_mode(
    session_manager: &mut SessionManager,
    backend: &dyn AiBackend,
    new_mode: PermissionMode,
    ctx: &egui::Context,
) -> Option<ModeCommandPublish> {
    let session = session_manager.get_active_mut()?;
    let is_remote = session.is_remote();
    let session_id = session.id;
    let agentic = session.agentic.as_mut()?;

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
        backend.set_permission_mode(backend_sid, new_mode, notedeck::Waker::egui(ctx));
        session.state_dirty = true;
        None
    };

    tracing::debug!(
        "Set permission mode for session {} to {:?} (remote={})",
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
            backend.set_permission_mode(
                session_id,
                PermissionMode::Default,
                notedeck::Waker::egui(ctx),
            );
            tracing::debug!("Exited plan mode for session {}", session.id);
        }
    }
}

// =============================================================================
// Permission Handling
// =============================================================================

fn first_remote_pending_permission(
    session: &ChatSession,
) -> Option<&crate::messages::PermissionRequest> {
    let agentic = session.agentic.as_ref();
    let responded = agentic.map(|a| &a.permissions.responded);
    session.chat.iter().find_map(|msg| {
        let Message::PermissionRequest(req) = msg else {
            return None;
        };
        if req.response.is_some() {
            return None;
        }
        if responded.is_some_and(|ids| ids.contains_key(&req.id)) {
            return None;
        }
        if agentic.is_some_and(|a| a.should_runtime_allow(&req.tool_name, &req.tool_input)) {
            return None;
        }
        Some(req)
    })
}

/// Get the first pending permission request ID for the active session.
pub fn first_pending_permission(session_manager: &SessionManager) -> Option<uuid::Uuid> {
    let session = session_manager.get_active()?;
    if session.is_remote() {
        first_remote_pending_permission(session).map(|req| req.id)
    } else {
        // Local: check oneshot senders
        session
            .agentic
            .as_ref()
            .and_then(|a| a.permissions.pending.keys().next().copied())
    }
}

/// Get the first pending permission request for the active session.
pub fn pending_permission(session_manager: &SessionManager) -> Option<&PermissionRequest> {
    let session = session_manager.get_active()?;

    if session.is_remote() {
        return first_remote_pending_permission(session);
    }

    let request_id = first_pending_permission(session_manager)?;
    session.chat.iter().find_map(|msg| {
        if let Message::PermissionRequest(req) = msg {
            if req.id == request_id {
                return Some(req);
            }
        }
        None
    })
}

/// Check if the first pending permission is a shared question-set prompt.
pub fn has_pending_question(session_manager: &SessionManager) -> bool {
    pending_permission(session_manager)
        .is_some_and(|request| matches!(request.view, PermissionView::QuestionSet(_)))
}

/// Check if the first pending permission is an ExitPlanMode tool call.
pub fn has_pending_exit_plan_mode(session_manager: &SessionManager) -> bool {
    pending_permission(session_manager).is_some_and(|request| request.view.is_plan_review())
}

/// Data needed to publish a permission response event.
pub struct PermissionPublish {
    pub perm_id: uuid::Uuid,
    pub event_session_id: String,
    pub request_note_id: [u8; 32],
    pub allowed: bool,
    pub message: Option<String>,
    pub cancel_turn: bool,
}

/// Handle a permission response (from UI button or keybinding).
pub fn handle_permission_response(
    session_manager: &mut SessionManager,
    request_id: uuid::Uuid,
    response: PermissionResponse,
) -> Option<PermissionPublish> {
    let session = session_manager.get_active_mut()?;

    let is_remote = session.is_remote();
    let cancels_turn = response.cancels_turn();
    let publish_metadata = session.agentic.as_ref().and_then(|agentic| {
        let request_note_id = agentic
            .permissions
            .request_note_ids
            .get(&request_id)
            .copied()?;
        Some((agentic.event_session_id().to_string(), request_note_id))
    });
    if is_remote && publish_metadata.is_none() {
        tracing::warn!(
            "missing permission publish metadata for remote request {}",
            request_id
        );
        return None;
    }

    let response_type = match &response {
        PermissionResponse::Allow { .. } => crate::messages::PermissionResponseType::Allowed,
        PermissionResponse::Deny { .. } | PermissionResponse::Cancel { .. } => {
            crate::messages::PermissionResponseType::Denied
        }
    };

    // Extract relay-publish info before we move `response`.
    let allowed = matches!(&response, PermissionResponse::Allow { .. });
    let message = match &response {
        PermissionResponse::Allow { message } => message.clone(),
        PermissionResponse::Deny { reason } | PermissionResponse::Cancel { reason } => {
            Some(reason.clone())
        }
    };

    // Surface the user's approve/deny reply text inline as a user message so
    // there's a visible record of what was said, matching the note-render path
    // that reconstructs it on reload / for a remote observer (see
    // `session_loader::render_conversation_note`). `permission_reply_message`
    // drops empty and canned-placeholder reasons so a plain allow/deny adds no
    // bubble.
    //
    // Gate this optimistic push on `!is_remote`. A local host never renders its
    // own `permission_response` note live (`process_conversation_notes` takes
    // the `!is_remote` early-return), so it needs the push. A remote issuer,
    // however, gets the reply appended when the echoed-back note ingests via
    // `process_conversation_notes`; pushing here too would render it twice.
    if !is_remote {
        if let Some(reply) = crate::messages::permission_reply_message(message.as_deref()) {
            session.chat.push(Message::User(reply.into()));
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
        if is_remote && !cancels_turn {
            agentic.remote_status = Some(crate::agent_status::AgentStatus::Working);
        }
    }

    publish_metadata.map(|(event_session_id, request_note_id)| PermissionPublish {
        perm_id: request_id,
        event_session_id,
        request_note_id,
        allowed,
        message,
        cancel_turn: cancels_turn,
    })
}

/// Handle a user's response to a shared question-set prompt.
pub fn handle_question_response(
    session_manager: &mut SessionManager,
    request_id: uuid::Uuid,
    answers: Vec<QuestionAnswer>,
) -> Option<PermissionPublish> {
    let session = session_manager.get_active_mut()?;

    let is_remote = session.is_remote();
    let publish_metadata = session.agentic.as_ref().and_then(|agentic| {
        let request_note_id = agentic
            .permissions
            .request_note_ids
            .get(&request_id)
            .copied()?;
        Some((agentic.event_session_id().to_string(), request_note_id))
    });
    if is_remote && publish_metadata.is_none() {
        tracing::warn!(
            "missing question publish metadata for remote request {}",
            request_id
        );
        return None;
    }

    // Find the original shared question-set request to get the option labels.
    let questions_input = session.chat.iter().find_map(|msg| {
        if let Message::PermissionRequest(req) = msg {
            if req.id == request_id {
                req.view.question_set()
            } else {
                None
            }
        } else {
            None
        }
    });

    // The model-facing payload is plain prose (`Header: label, …` per line), not
    // JSON: it is injected verbatim as user text and never re-parsed, so it must
    // read as prose. Produce it via the shared agentium_core formatter so this
    // local answer path agrees byte-for-byte with the engine's remote
    // `respond_question`. See `crate::messages::format_question_answers`.
    let formatted_response = crate::messages::format_question_answers(questions_input, &answers);

    // The display summary (one collapsible `Header: answer` entry per question)
    // is UI-only and needs the option labels, so it's built here where the
    // question metadata is in hand rather than in the shared formatter.
    let answer_summary = questions_input.map(|questions| {
        let entries = questions
            .questions
            .iter()
            .zip(answers.iter())
            .enumerate()
            .map(|(q_idx, (question, answer))| {
                let mut display_parts: Vec<String> = answer
                    .selected
                    .iter()
                    .filter_map(|&idx| question.options.get(idx).map(|o| o.label.clone()))
                    .collect();
                if let Some(other) = answer.other_text.as_ref().filter(|other| !other.is_empty()) {
                    display_parts.push(format!("Other: {}", other));
                }

                let header = if !question.header.is_empty() {
                    question.header.clone()
                } else {
                    format!("question_{}", q_idx)
                };

                AnswerSummaryEntry {
                    header,
                    answer: display_parts.join(", "),
                }
            })
            .collect();
        AnswerSummary { entries }
    });

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

    publish_metadata.map(|(event_session_id, request_note_id)| PermissionPublish {
        perm_id: request_id,
        event_session_id,
        request_note_id,
        allowed: true,
        message: Some(formatted_response),
        cancel_turn: false,
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
                scene.focus_on(agentic.scene_position.into());
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
    collapse: &crate::collapse_state::CollapseState,
    scene: &mut AgentScene,
    show_scene: bool,
    index: usize,
) {
    let ids = session_manager.visual_order(collapse);
    if let Some(&id) = ids.get(index) {
        switch_and_focus_session(session_manager, scene, show_scene, id);
    }
}

/// Cycle agents using a direction function that computes the next index.
fn cycle_agent(
    session_manager: &mut SessionManager,
    collapse: &crate::collapse_state::CollapseState,
    scene: &mut AgentScene,
    show_scene: bool,
    index_fn: impl FnOnce(usize, usize) -> usize,
) {
    let ids = session_manager.visual_order(collapse);
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
    collapse: &crate::collapse_state::CollapseState,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    cycle_agent(session_manager, collapse, scene, show_scene, |idx, len| {
        (idx + 1) % len
    });
}

/// Cycle to the previous agent.
pub fn cycle_prev_agent(
    session_manager: &mut SessionManager,
    collapse: &crate::collapse_state::CollapseState,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    cycle_agent(session_manager, collapse, scene, show_scene, |idx, len| {
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

/// Navigate to the next visible item in the focus queue.
/// Skips sessions inside collapsed folders.
/// Done items are automatically dismissed after switching to them.
pub fn focus_queue_next(
    session_manager: &mut SessionManager,
    focus_queue: &mut FocusQueue,
    collapse: &crate::collapse_state::CollapseState,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    let visible = session_manager.visual_order(collapse);
    let saved_cursor = focus_queue.cursor_index();
    let max_attempts = focus_queue.len();
    for _ in 0..max_attempts {
        if let Some(session_id) = focus_queue.next() {
            if visible.contains(&session_id) {
                switch_and_focus_session(session_manager, scene, show_scene, session_id);
                dismiss_done(session_manager, focus_queue, session_id);
                return;
            }
        } else {
            return;
        }
    }
    // All skipped — restore cursor to original position.
    if let Some(idx) = saved_cursor {
        focus_queue.set_cursor(idx);
    }
}

/// Navigate to the previous visible item in the focus queue.
/// Skips sessions inside collapsed folders.
/// Done items are automatically dismissed after switching to them.
pub fn focus_queue_prev(
    session_manager: &mut SessionManager,
    focus_queue: &mut FocusQueue,
    collapse: &crate::collapse_state::CollapseState,
    scene: &mut AgentScene,
    show_scene: bool,
) {
    let visible = session_manager.visual_order(collapse);
    let saved_cursor = focus_queue.cursor_index();
    let max_attempts = focus_queue.len();
    for _ in 0..max_attempts {
        if let Some(session_id) = focus_queue.prev() {
            if visible.contains(&session_id) {
                switch_and_focus_session(session_manager, scene, show_scene, session_id);
                dismiss_done(session_manager, focus_queue, session_id);
                return;
            }
        } else {
            return;
        }
    }
    // All skipped — restore cursor to original position.
    if let Some(idx) = saved_cursor {
        focus_queue.set_cursor(idx);
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

/// Anchor auto-steal focus to a session the user just deliberately opened.
///
/// Records `id` as the home session so auto-steal returns there once any urgent
/// (NeedsInput/Done) session is handled, and cancels a `Pending` steal so it
/// doesn't fire on top of this navigation and immediately yank focus onto a
/// *different* session. No-op while auto-steal is `Disabled` (the default),
/// where `home_session` and the pending state are unused.
pub fn anchor_auto_steal(
    auto_steal: &mut AutoStealState,
    home_session: &mut Option<SessionId>,
    id: SessionId,
) {
    if !auto_steal.is_enabled() {
        return;
    }
    *home_session = Some(id);
    if *auto_steal == AutoStealState::Pending {
        *auto_steal = AutoStealState::Idle;
    }
}

/// Process auto-steal focus logic: switch to focus queue items as needed.
/// Returns true if focus was stolen (switched to a NeedsInput or Done session),
/// which can be used to raise the OS window.
///
/// Sessions inside collapsed directories are skipped — auto-steal only
/// targets sessions the user can currently see.
pub fn process_auto_steal_focus(
    session_manager: &mut SessionManager,
    focus_queue: &mut FocusQueue,
    collapse: &crate::collapse_state::CollapseState,
    scene: &mut AgentScene,
    show_scene: bool,
    auto_steal_focus: bool,
    home_session: &mut Option<SessionId>,
) -> bool {
    if !auto_steal_focus {
        return false;
    }

    let visible = session_manager.visual_order(collapse);

    let first_visible_needs_input =
        focus_queue.first_visible_index(FocusPriority::NeedsInput, &visible);
    let first_visible_done = focus_queue.first_visible_index(FocusPriority::Done, &visible);

    if let Some(idx) = first_visible_needs_input {
        // There are visible NeedsInput items - check if we need to steal focus
        let current_session = session_manager.active_id();
        let current_priority = current_session.and_then(|id| focus_queue.get_session_priority(id));
        let already_on_needs_input = current_priority == Some(FocusPriority::NeedsInput);

        if !already_on_needs_input {
            // Save current session before stealing (only if we haven't saved yet)
            if home_session.is_none() {
                *home_session = current_session;
                tracing::debug!("Auto-steal: saved home session {:?}", home_session);
            }

            focus_queue.set_cursor(idx);
            if let Some(entry) = focus_queue.current() {
                switch_and_focus_session(session_manager, scene, show_scene, entry.session_id);
                tracing::debug!("Auto-steal: switched to session {:?}", entry.session_id);
                return true;
            }
        }
    } else if let Some(idx) = first_visible_done {
        // No visible NeedsInput but there are visible Done items - auto-focus those
        let current_session = session_manager.active_id();
        let current_priority = current_session.and_then(|id| focus_queue.get_session_priority(id));
        let already_on_done = current_priority == Some(FocusPriority::Done);

        if !already_on_done {
            // Save current session before stealing (only if we haven't saved yet)
            if home_session.is_none() {
                *home_session = current_session;
                tracing::debug!("Auto-steal: saved home session {:?}", home_session);
            }

            focus_queue.set_cursor(idx);
            if let Some(entry) = focus_queue.current() {
                let sid = entry.session_id;
                switch_and_focus_session(session_manager, scene, show_scene, sid);
                tracing::debug!("Auto-steal: switched to Done session {:?}", sid);
                return true;
            }
        }
    } else if let Some(home_id) = home_session.take() {
        // No more visible NeedsInput or Done items - return to saved session
        // only if it is still visible (not inside a collapsed group).
        if visible.contains(&home_id) {
            switch_and_focus_session(session_manager, scene, show_scene, home_id);
            tracing::debug!("Auto-steal: returned to home session {:?}", home_id);
        } else {
            tracing::debug!(
                "Auto-steal: home session {:?} is collapsed, staying on current",
                home_id
            );
        }
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
            let _ = std::fs::remove_file(temp_path.with_extension("sh"));
            let _ = std::fs::remove_file(temp_path.with_extension("done"));
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
///
/// Many Linux terminals (gnome-terminal, konsole, etc.) daemonize: the
/// spawned process exits immediately while the actual window runs as a
/// child of a separate daemon. This means we cannot rely on the child
/// process exit to know when the user is done editing.
///
/// Instead we wrap the editor invocation in a small shell script that
/// creates a sentinel `.done` file when the editor exits.
/// `poll_editor_job` watches for that file.
fn spawn_linux_editor(
    editor: &str,
    file: &std::path::Path,
) -> std::io::Result<std::process::Child> {
    use std::process::Command;

    // Write a helper script that runs the editor then creates a sentinel.
    let script_path = file.with_extension("sh");
    let done_path = file.with_extension("done");
    // Remove stale sentinel from a previous run, if any.
    let _ = std::fs::remove_file(&done_path);
    std::fs::write(
        &script_path,
        format!("#!/bin/sh\n\"$@\"\ntouch '{}'\n", done_path.display()),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;
    }

    let spawn_via = |name: &str, prefix_args: &[&str]| -> std::io::Result<std::process::Child> {
        tracing::debug!("Opening editor via {}: {} {}", name, editor, file.display());
        let mut cmd = Command::new(name);
        for arg in prefix_args {
            cmd.arg(arg);
        }
        cmd.arg(&script_path).arg(editor).arg(file);
        cmd.spawn()
    };

    if let Ok(terminal) = std::env::var("TERMINAL") {
        return spawn_via(&terminal, &["-e"]);
    }

    // Auto-detect. Each terminal has different exec syntax.
    let terminals: &[(&str, &[&str])] = &[
        ("wezterm", &["start", "--always-new-process", "--"]),
        ("alacritty", &["-e"]),
        ("kitty", &[]),
        ("gnome-terminal", &["--"]),
        ("konsole", &["-e"]),
        ("foot", &[]),
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
            return spawn_via(name, prefix_args);
        }
    }

    // Clean up the script on failure.
    let _ = std::fs::remove_file(&script_path);
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "No terminal emulator found. Set $TERMINAL or $VISUAL.",
    ))
}

/// Open a new terminal window.
///
/// Uses `$TERMINAL` if set, otherwise falls back to the platform default
/// (Terminal.app on macOS, `x-terminal-emulator` on Linux).
pub fn open_terminal(cwd: &std::path::Path) {
    let terminal = std::env::var("TERMINAL").ok();
    let _ = open_terminal_with_terminal(cwd, terminal.as_deref());
}

/// Open a new terminal window, optionally overriding the terminal executable.
///
/// Returns `true` if a terminal process was successfully started.
fn open_terminal_with_terminal(cwd: &std::path::Path, terminal: Option<&str>) -> bool {
    use std::process::{Command, Stdio};

    if let Some(terminal) = terminal {
        match Command::new(terminal)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return true;
            }
            Err(e) => tracing::warn!("$TERMINAL='{}' failed: {}", terminal, e),
        }
    }

    let result = if cfg!(target_os = "macos") {
        Command::new("open")
            .arg("-a")
            .arg("Terminal")
            .arg(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    } else if cfg!(target_os = "linux") {
        Command::new("x-terminal-emulator")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    } else {
        tracing::warn!("Open terminal not supported on this platform. Set $TERMINAL.");
        return false;
    };

    match result {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            tracing::debug!("Opened new terminal window in {:?}", cwd);
            true
        }
        Err(e) => {
            tracing::error!("Failed to open terminal: {}. Set $TERMINAL.", e);
            false
        }
    }
}

/// Poll for external editor completion (called each frame).
///
/// On Linux, many terminals daemonize so the child process exits before
/// the editor is done. We use a sentinel `.done` file (created by the
/// wrapper script in `spawn_linux_editor`) to detect actual completion.
/// The child exit is still checked so we can reap the process, but we
/// only read the temp file once the sentinel exists.
pub fn poll_editor_job(session_manager: &mut SessionManager) {
    let Some(ref mut job) = session_manager.pending_editor else {
        return;
    };

    // Reap child if it has exited (non-blocking).
    let child_done = match job.child.try_wait() {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(e) => {
            tracing::error!("Failed to poll editor process: {}", e);
            true
        }
    };

    // Check for sentinel file produced by the wrapper script.
    // On Linux, spawn_linux_editor always creates a wrapper .sh script
    // that touches a .done sentinel when the editor exits. We trust only
    // the sentinel on Linux because many terminals daemonize (the child
    // exits immediately even though the editor is still open).
    // On macOS (no wrapper script), we fall back to child process exit.
    let done_path = job.temp_path.with_extension("done");
    let script_path = job.temp_path.with_extension("sh");
    let uses_sentinel = script_path.exists();
    let sentinel_exists = done_path.exists();

    let editor_finished = if uses_sentinel {
        sentinel_exists
    } else {
        child_done
    };

    if !editor_finished {
        return;
    }

    let session_id = job.session_id;
    let temp_path = job.temp_path.clone();
    let script_path = temp_path.with_extension("sh");

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

    // Clean up temp files.
    let _ = std::fs::remove_file(&temp_path);
    let _ = std::fs::remove_file(&done_path);
    let _ = std::fs::remove_file(&script_path);

    session_manager.pending_editor = None;
}

// =============================================================================
// Session Management
// =============================================================================

/// Create a new session with the given cwd and optional model override.
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
    model: Model,
) -> SessionId {
    directory_picker.add_recent(cwd.clone());

    let id = session_manager.new_session(cwd, ai_mode, backend_type);
    if let Some(session) = session_manager.get_mut(id) {
        let model_id = model.to_model_id().map(str::to_string);
        session.details.hostname = hostname.to_string();
        session.details.requested_model = model_id.clone();
        session.details.model = model_id;
        session.focus_requested = true;
        if show_scene {
            scene.select(id);
            if let Some(agentic) = &session.agentic {
                scene.focus_on(agentic.scene_position.into());
            }
        }
        // Remote clients reach this session's live conversation events through
        // the shared per-account subscription; nothing per-session to wire up.
    }
    session_manager.rebuild_groups();
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
                scene.focus_on(agentic.scene_position.into());
            }
        }
    }
    session_manager.rebuild_groups();
    id
}

/// Clone the active agent, creating a new session with the same working directory.
/// Info needed to spawn a session on a remote host.
pub struct RemoteSpawn {
    pub host: String,
    pub cwd: PathBuf,
    pub backend: BackendType,
}

/// Clone a session by ID. For local sessions, creates the new session directly
/// and returns `None`. For remote sessions, returns `Some(RemoteSpawn)` so the
/// caller can dispatch it to the remote host.
pub fn clone_session(
    session_manager: &mut SessionManager,
    directory_picker: &mut DirectoryPicker,
    scene: &mut AgentScene,
    show_scene: bool,
    ai_mode: AiMode,
    hostname: &str,
    id: SessionId,
) -> Option<RemoteSpawn> {
    let session = session_manager.get(id)?;
    let cwd = session.cwd().cloned()?;
    let backend_type = session.backend_type;
    let model = session
        .details
        .resolve_model()
        .map(|id| Model::from_model_id(&id))
        .unwrap_or(Model::Default);

    if session.is_remote() {
        return Some(RemoteSpawn {
            host: session.details.hostname.clone(),
            cwd,
            backend: backend_type,
        });
    }

    create_session_with_cwd(
        session_manager,
        directory_picker,
        scene,
        show_scene,
        ai_mode,
        cwd,
        hostname,
        backend_type,
        model,
    );
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collapse_state::CollapseState;
    use crate::focus_queue::{FocusPriority, FocusQueue};
    use crate::session::{SessionId, SessionSource};

    #[test]
    fn cycle_walks_manual_plan_edits_auto() {
        // The click / Ctrl+M cycle is a closed 4-cycle over the modes the CLI
        // honors at runtime: Manual (Default) → Plan → AcceptEdits → Auto →
        // Manual.
        assert_eq!(
            next_cycle_permission_mode(PermissionMode::Default),
            PermissionMode::Plan
        );
        assert_eq!(
            next_cycle_permission_mode(PermissionMode::Plan),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            next_cycle_permission_mode(PermissionMode::AcceptEdits),
            PermissionMode::Auto
        );
        assert_eq!(
            next_cycle_permission_mode(PermissionMode::Auto),
            PermissionMode::Default
        );
    }

    #[test]
    fn cycle_never_reaches_bypass_permissions() {
        // BypassPermissions does no safety checking and dave can't enter it
        // mid-session, so it must never appear in the click / Ctrl+M cycle —
        // not from any starting mode.
        for start in [
            PermissionMode::Default,
            PermissionMode::Plan,
            PermissionMode::AcceptEdits,
            PermissionMode::Auto,
            PermissionMode::BypassPermissions,
        ] {
            let mut mode = start;
            for _ in 0..8 {
                mode = next_cycle_permission_mode(mode);
                assert_ne!(
                    mode,
                    PermissionMode::BypassPermissions,
                    "cycling from {start:?} reached BypassPermissions"
                );
            }
        }

        // Cycling out of BypassPermissions (should it ever be set) exits to
        // Manual (Default).
        assert_eq!(
            next_cycle_permission_mode(PermissionMode::BypassPermissions),
            PermissionMode::Default
        );
    }

    fn create_named_agent_session(
        sm: &mut SessionManager,
        picker: &mut DirectoryPicker,
        scene: &mut AgentScene,
        hostname: &str,
        cwd: &str,
        title: &str,
    ) -> SessionId {
        let id = create_session_with_cwd(
            sm,
            picker,
            scene,
            false,
            AiMode::Agentic,
            PathBuf::from(cwd),
            hostname,
            BackendType::Claude,
            Model::Default,
        );

        let session = sm.get_mut(id).expect("session should exist");
        session.details.title = title.to_string();
        session.details.custom_title = None;
        session.details.home_dir = "/home/tester".to_string();

        sm.rebuild_groups();
        id
    }

    fn create_remote_agent_session(
        sm: &mut SessionManager,
        picker: &mut DirectoryPicker,
        scene: &mut AgentScene,
    ) -> SessionId {
        let id = create_session_with_cwd(
            sm,
            picker,
            scene,
            false,
            AiMode::Agentic,
            PathBuf::from("/tmp"),
            "remote-a",
            BackendType::Claude,
            Model::Default,
        );
        let session = sm.get_mut(id).expect("session should exist");
        session.source = SessionSource::Remote;
        id
    }

    fn find_permission_request(
        session: &crate::session::ChatSession,
        request_id: uuid::Uuid,
    ) -> &PermissionRequest {
        session
            .chat
            .iter()
            .find_map(|msg| match msg {
                Message::PermissionRequest(req) if req.id == request_id => Some(req),
                _ => None,
            })
            .expect("permission request should exist")
    }

    #[test]
    fn pending_permission_skips_runtime_allowed_remote_requests() {
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let id = create_remote_agent_session(&mut sm, &mut picker, &mut scene);
        let allowed_id = uuid::Uuid::new_v4();
        let next_id = uuid::Uuid::new_v4();

        let session = sm.get_mut(id).expect("session should exist");
        session
            .chat
            .push(Message::PermissionRequest(PermissionRequest::new(
                allowed_id,
                "Bash".to_owned(),
                serde_json::json!({"command": "echo hi"}),
                Some(PermissionView::RawFallback),
                None,
                None,
            )));
        session
            .chat
            .push(Message::PermissionRequest(PermissionRequest::new(
                next_id,
                "AskUserQuestion".to_owned(),
                serde_json::json!({}),
                Some(PermissionView::QuestionSet(
                    crate::messages::QuestionSetInput {
                        questions: vec![crate::messages::UserQuestion {
                            question: "Pick one".to_owned(),
                            header: "choice".to_owned(),
                            multi_select: false,
                            options: vec![crate::messages::QuestionOption {
                                label: "A".to_owned(),
                                description: "Option A".to_owned(),
                            }],
                        }],
                    },
                )),
                None,
                None,
            )));

        if let Some(agentic) = &mut session.agentic {
            let _ = agentic.add_runtime_allow("Bash", &serde_json::json!({"command": "echo hi"}));
        }

        assert_eq!(first_pending_permission(&sm), Some(next_id));
        assert_eq!(pending_permission(&sm).map(|req| req.id), Some(next_id));
        assert!(has_pending_question(&sm));
    }

    /// clone_session must preserve the source session's model override
    /// instead of hardcoding Model::Default.
    #[test]
    fn clone_session_preserves_model() {
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();

        // Create a session with Model::Opus
        let orig_id = create_session_with_cwd(
            &mut sm,
            &mut picker,
            &mut scene,
            false,
            AiMode::Agentic,
            PathBuf::from("/tmp"),
            "localhost",
            BackendType::Claude,
            Model::Opus,
        );

        // Verify the original session has the model set
        let orig_model = sm.get(orig_id).unwrap().details.requested_model.clone();
        assert!(orig_model.is_some(), "original session should have a model");

        // Clone it
        let spawn = clone_session(
            &mut sm,
            &mut picker,
            &mut scene,
            false,
            AiMode::Agentic,
            "localhost",
            orig_id,
        );
        assert!(spawn.is_none(), "local clone should not return RemoteSpawn");

        // The new session should be the active one (most recently created)
        let new_id = sm.active_id().unwrap();
        assert_ne!(new_id, orig_id);

        let new_model = sm.get(new_id).unwrap().details.requested_model.clone();
        assert_eq!(
            new_model, orig_model,
            "cloned session should preserve the model from the original"
        );
    }

    /// clone_session with Model::Default should keep model as None.
    #[test]
    fn clone_session_preserves_default_model() {
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();

        let orig_id = create_session_with_cwd(
            &mut sm,
            &mut picker,
            &mut scene,
            false,
            AiMode::Agentic,
            PathBuf::from("/tmp"),
            "localhost",
            BackendType::Claude,
            Model::Default,
        );

        assert!(sm.get(orig_id).unwrap().details.requested_model.is_none());

        clone_session(
            &mut sm,
            &mut picker,
            &mut scene,
            false,
            AiMode::Agentic,
            "localhost",
            orig_id,
        );

        let new_id = sm.active_id().unwrap();
        // Without this the test is vacuous: a clone that creates nothing leaves
        // the original active, and the original's requested_model is None too.
        assert_ne!(new_id, orig_id, "clone must create a new session");
        assert!(
            sm.get(new_id).unwrap().details.requested_model.is_none(),
            "cloned default-model session should also have no model"
        );
    }

    /// clone_session must preserve the original requested model override,
    /// not the backend-reported runtime model shown in the UI.
    #[test]
    fn clone_session_uses_requested_model_not_runtime_model() {
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();

        let orig_id = create_session_with_cwd(
            &mut sm,
            &mut picker,
            &mut scene,
            false,
            AiMode::Agentic,
            PathBuf::from("/tmp"),
            "localhost",
            BackendType::Codex,
            Model::Custom("gpt-5.2-codex".to_string()),
        );

        let session = sm.get_mut(orig_id).unwrap();
        session.details.model = Some("gpt-5.2-codex-2026-03-01".to_string());

        clone_session(
            &mut sm,
            &mut picker,
            &mut scene,
            false,
            AiMode::Agentic,
            "localhost",
            orig_id,
        );

        let new_id = sm.active_id().unwrap();
        let new_session = sm.get(new_id).unwrap();
        assert_eq!(
            new_session.details.requested_model.as_deref(),
            Some("gpt-5.2-codex"),
            "clone should preserve the original requested override"
        );
        assert_eq!(
            new_session.details.model.as_deref(),
            Some("gpt-5.2-codex"),
            "new session should start from the requested override until the backend reports otherwise"
        );
    }

    #[test]
    fn focus_queue_next_skips_sessions_hidden_by_collapsed_cwd() {
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let mut focus_queue = FocusQueue::new();

        let visible_id = create_named_agent_session(
            &mut sm,
            &mut picker,
            &mut scene,
            "remote-a",
            "/srv/visible",
            "Visible",
        );
        let hidden_id = create_named_agent_session(
            &mut sm,
            &mut picker,
            &mut scene,
            "remote-a",
            "/srv/hidden",
            "Hidden",
        );
        // Second session in /srv/hidden so its cwd is collapsible
        // (single-session cwds skip the folder header and are always visible).
        let _hidden_sibling = create_named_agent_session(
            &mut sm,
            &mut picker,
            &mut scene,
            "remote-a",
            "/srv/hidden",
            "Hidden sibling",
        );

        let mut collapse = CollapseState::new();
        collapse.toggle_cwd("remote-a", std::path::Path::new("/srv/hidden"));

        sm.switch_to(hidden_id);

        focus_queue.enqueue(hidden_id, FocusPriority::NeedsInput);
        focus_queue.enqueue(visible_id, FocusPriority::Error);
        focus_queue.set_cursor(1);

        assert_eq!(sm.active_id(), Some(hidden_id));

        focus_queue_next(&mut sm, &mut focus_queue, &collapse, &mut scene, false);

        assert_eq!(sm.active_id(), Some(visible_id));
        assert_eq!(
            focus_queue.current().map(|entry| entry.session_id),
            Some(visible_id)
        );
    }

    #[test]
    fn auto_steal_ignores_collapsed_needs_input_and_uses_visible_done() {
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let mut focus_queue = FocusQueue::new();

        let home_id =
            create_named_agent_session(&mut sm, &mut picker, &mut scene, "", "/work/home", "Home");
        let hidden_needs_input = create_named_agent_session(
            &mut sm,
            &mut picker,
            &mut scene,
            "remote-a",
            "/srv/hidden",
            "Needs input",
        );
        let visible_done = create_named_agent_session(
            &mut sm,
            &mut picker,
            &mut scene,
            "remote-b",
            "/srv/done",
            "Done",
        );

        sm.switch_to(home_id);

        let mut collapse = CollapseState::new();
        collapse.toggle_host("remote-a");

        focus_queue.enqueue(hidden_needs_input, FocusPriority::NeedsInput);
        focus_queue.enqueue(visible_done, FocusPriority::Done);

        let mut home_session = None;
        let stole_focus = process_auto_steal_focus(
            &mut sm,
            &mut focus_queue,
            &collapse,
            &mut scene,
            false,
            true,
            &mut home_session,
        );

        assert!(stole_focus);
        assert_eq!(sm.active_id(), Some(visible_done));
        assert_eq!(home_session, Some(home_id));
        assert_eq!(
            focus_queue.current().map(|entry| entry.session_id),
            Some(visible_done)
        );
    }

    #[test]
    fn anchor_auto_steal_cancels_pending_and_sets_home() {
        // A deliberate open while a steal is pending: cancel the steal (so it
        // doesn't yank onto another session) and make the opened session home.
        let mut auto_steal = AutoStealState::Pending;
        let mut home_session = Some(1);
        anchor_auto_steal(&mut auto_steal, &mut home_session, 42);
        assert_eq!(auto_steal, AutoStealState::Idle);
        assert_eq!(home_session, Some(42));
    }

    #[test]
    fn anchor_auto_steal_when_idle_keeps_idle() {
        // Idle (enabled, nothing pending) stays Idle; home still updates so a
        // later steal returns to the deliberately-opened session.
        let mut auto_steal = AutoStealState::Idle;
        let mut home_session = None;
        anchor_auto_steal(&mut auto_steal, &mut home_session, 42);
        assert_eq!(auto_steal, AutoStealState::Idle);
        assert_eq!(home_session, Some(42));
    }

    #[test]
    fn anchor_auto_steal_disabled_is_noop() {
        // With auto-steal off (the default) there's nothing to fight, so neither
        // the state nor the (unused) home session is touched.
        let mut auto_steal = AutoStealState::Disabled;
        let mut home_session = None;
        anchor_auto_steal(&mut auto_steal, &mut home_session, 42);
        assert_eq!(auto_steal, AutoStealState::Disabled);
        assert_eq!(home_session, None);
    }

    #[test]
    fn remote_permission_response_without_publish_metadata_keeps_request_pending() {
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let id = create_remote_agent_session(&mut sm, &mut picker, &mut scene);
        let request_id = uuid::Uuid::new_v4();

        let session = sm.get_mut(id).expect("session should exist");
        session
            .chat
            .push(Message::PermissionRequest(PermissionRequest::new(
                request_id,
                "Bash".to_owned(),
                serde_json::json!({"command": "echo hi"}),
                Some(PermissionView::RawFallback),
                None,
                None,
            )));

        let publish = handle_permission_response(
            &mut sm,
            request_id,
            PermissionResponse::Allow {
                message: Some("ack".to_owned()),
            },
        );
        assert!(
            publish.is_none(),
            "remote response without metadata should not publish"
        );

        let session = sm.get(id).expect("session should exist");
        let req = find_permission_request(session, request_id);
        assert_eq!(
            req.response, None,
            "missing metadata must not resolve the request locally"
        );
        assert!(
            !session
                .agentic
                .as_ref()
                .expect("agentic session")
                .permissions
                .responded
                .contains_key(&request_id),
            "missing metadata must not mark request as responded"
        );
    }

    #[test]
    fn remote_permission_response_does_not_optimistically_push_reply() {
        // Regression: on the client that *issues* the response to a remote
        // session, the reply must not render twice. The echoed-back
        // `permission_response` note appends via `process_conversation_notes`,
        // so `handle_permission_response` must NOT also push it locally.
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let id = create_remote_agent_session(&mut sm, &mut picker, &mut scene);
        let request_id = uuid::Uuid::new_v4();
        let request_note_id = [9_u8; 32];

        let session = sm.get_mut(id).expect("session should exist");
        session
            .chat
            .push(Message::PermissionRequest(PermissionRequest::new(
                request_id,
                "Bash".to_owned(),
                serde_json::json!({"command": "echo hi"}),
                Some(PermissionView::RawFallback),
                None,
                None,
            )));
        session
            .agentic
            .as_mut()
            .expect("agentic session")
            .permissions
            .request_note_ids
            .insert(request_id, request_note_id);

        let user_messages_before = sm
            .get(id)
            .expect("session should exist")
            .chat
            .iter()
            .filter(|msg| matches!(msg, Message::User(_)))
            .count();

        let publish = handle_permission_response(
            &mut sm,
            request_id,
            PermissionResponse::Allow {
                message: Some("looks good, go ahead".to_owned()),
            },
        )
        .expect("remote response with metadata should return publish payload");
        assert_eq!(
            publish.message.as_deref(),
            Some("looks good, go ahead"),
            "the reply text still rides the publish payload onto the wire"
        );

        let user_messages_after = sm
            .get(id)
            .expect("session should exist")
            .chat
            .iter()
            .filter(|msg| matches!(msg, Message::User(_)))
            .count();
        assert_eq!(
            user_messages_after, user_messages_before,
            "remote issuer must not optimistically push the reply (the echoed-back \
             note appends it instead), else it renders twice"
        );
    }

    #[test]
    fn local_permission_response_with_publish_metadata_returns_publish() {
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let id = create_named_agent_session(
            &mut sm,
            &mut picker,
            &mut scene,
            "localhost",
            "/tmp",
            "Local",
        );
        let request_id = uuid::Uuid::new_v4();
        let request_note_id = [7_u8; 32];

        let session = sm.get_mut(id).expect("session should exist");
        session
            .chat
            .push(Message::PermissionRequest(PermissionRequest::new(
                request_id,
                "Bash".to_owned(),
                serde_json::json!({"command": "echo hi"}),
                Some(PermissionView::RawFallback),
                None,
                None,
            )));
        let expected_event_session_id = session
            .agentic
            .as_ref()
            .expect("agentic session")
            .event_session_id()
            .to_owned();
        session
            .agentic
            .as_mut()
            .expect("agentic session")
            .permissions
            .request_note_ids
            .insert(request_id, request_note_id);

        let publish = handle_permission_response(
            &mut sm,
            request_id,
            PermissionResponse::Allow { message: None },
        )
        .expect("local response with metadata should return publish payload");
        assert_eq!(publish.perm_id, request_id);
        assert_eq!(publish.event_session_id, expected_event_session_id);
        assert_eq!(publish.request_note_id, request_note_id);
        assert!(publish.allowed);
        assert_eq!(publish.message, None);
        assert!(!publish.cancel_turn);

        let session = sm.get(id).expect("session should exist");
        let req = find_permission_request(session, request_id);
        assert_eq!(
            req.response,
            Some(crate::messages::PermissionResponseType::Allowed)
        );
    }

    #[test]
    fn remote_question_response_without_publish_metadata_keeps_request_pending() {
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let id = create_remote_agent_session(&mut sm, &mut picker, &mut scene);
        let request_id = uuid::Uuid::new_v4();

        let session = sm.get_mut(id).expect("session should exist");
        session
            .chat
            .push(Message::PermissionRequest(PermissionRequest::new(
                request_id,
                "AskUserQuestion".to_owned(),
                serde_json::json!({}),
                Some(PermissionView::QuestionSet(
                    crate::messages::QuestionSetInput {
                        questions: vec![crate::messages::UserQuestion {
                            question: "Pick one".to_owned(),
                            header: "choice".to_owned(),
                            multi_select: false,
                            options: vec![crate::messages::QuestionOption {
                                label: "A".to_owned(),
                                description: "Option A".to_owned(),
                            }],
                        }],
                    },
                )),
                None,
                None,
            )));
        if let Some(agentic) = &mut session.agentic {
            agentic.question_answers.insert(
                request_id,
                vec![QuestionAnswer {
                    selected: vec![0],
                    other_text: None,
                }],
            );
            agentic.question_index.insert(request_id, 0);
        }

        let publish = handle_question_response(
            &mut sm,
            request_id,
            vec![QuestionAnswer {
                selected: vec![0],
                other_text: None,
            }],
        );
        assert!(
            publish.is_none(),
            "remote question response without metadata should not publish"
        );

        let session = sm.get(id).expect("session should exist");
        let req = find_permission_request(session, request_id);
        assert_eq!(
            req.response, None,
            "missing metadata must not resolve question request locally"
        );
        let agentic = session.agentic.as_ref().expect("agentic session");
        assert!(
            !agentic.permissions.responded.contains_key(&request_id),
            "missing metadata must not mark question request as responded"
        );
        assert!(
            agentic.question_answers.contains_key(&request_id),
            "missing metadata should keep staged answers for retry"
        );
        assert!(
            agentic.question_index.contains_key(&request_id),
            "missing metadata should keep staged question index for retry"
        );
    }

    #[test]
    fn local_question_response_with_publish_metadata_returns_publish() {
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let id = create_named_agent_session(
            &mut sm,
            &mut picker,
            &mut scene,
            "localhost",
            "/tmp",
            "Local Question",
        );
        let request_id = uuid::Uuid::new_v4();
        let request_note_id = [9_u8; 32];

        let session = sm.get_mut(id).expect("session should exist");
        session
            .chat
            .push(Message::PermissionRequest(PermissionRequest::new(
                request_id,
                "AskUserQuestion".to_owned(),
                serde_json::json!({}),
                Some(PermissionView::QuestionSet(
                    crate::messages::QuestionSetInput {
                        questions: vec![crate::messages::UserQuestion {
                            question: "Pick one".to_owned(),
                            header: "choice".to_owned(),
                            multi_select: false,
                            options: vec![crate::messages::QuestionOption {
                                label: "A".to_owned(),
                                description: "Option A".to_owned(),
                            }],
                        }],
                    },
                )),
                None,
                None,
            )));
        let expected_event_session_id = session
            .agentic
            .as_ref()
            .expect("agentic session")
            .event_session_id()
            .to_owned();
        session
            .agentic
            .as_mut()
            .expect("agentic session")
            .permissions
            .request_note_ids
            .insert(request_id, request_note_id);

        let publish = handle_question_response(
            &mut sm,
            request_id,
            vec![QuestionAnswer {
                selected: vec![0],
                other_text: None,
            }],
        )
        .expect("local question response with metadata should return publish payload");
        assert_eq!(publish.perm_id, request_id);
        assert_eq!(publish.event_session_id, expected_event_session_id);
        assert_eq!(publish.request_note_id, request_note_id);
        assert!(publish.allowed);
        assert!(!publish.cancel_turn);

        // The payload is plain prose (`Header: label`), not JSON: it's injected
        // to the model verbatim and never re-parsed. Selected index 0 resolves
        // to option label "A" under the "choice" header.
        let payload = publish
            .message
            .expect("question response should have payload");
        assert_eq!(payload, "choice: A");
        assert!(
            !payload.contains('{') && !payload.contains('\\'),
            "answers must be prose, not JSON escaped into the message: {payload:?}"
        );

        let session = sm.get(id).expect("session should exist");
        let req = find_permission_request(session, request_id);
        assert_eq!(
            req.response,
            Some(crate::messages::PermissionResponseType::Allowed)
        );
    }

    #[test]
    fn open_terminal_uses_override_and_launches_in_requested_cwd() {
        use std::io::ErrorKind;

        let tempdir = tempfile::TempDir::new().unwrap();
        let cwd = tempdir.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();

        let output_path = tempdir.path().join("pwd.txt");
        #[cfg(windows)]
        let script_path = tempdir.path().join("terminal.cmd");
        #[cfg(not(windows))]
        let script_path = tempdir.path().join("terminal.sh");

        #[cfg(windows)]
        std::fs::write(
            &script_path,
            format!("@echo off\r\ncd > \"{}\"\r\n", output_path.display()),
        )
        .unwrap();

        #[cfg(not(windows))]
        std::fs::write(
            &script_path,
            format!("#!/bin/sh\npwd > \"{}\"\n", output_path.display()),
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        assert!(
            open_terminal_with_terminal(&cwd, script_path.to_str()),
            "terminal override should launch successfully"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let launched_cwd = loop {
            match std::fs::read_to_string(&output_path) {
                Ok(contents) => break contents,
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "terminal script did not write its cwd before timeout"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(err) => panic!("failed reading terminal output: {err}"),
            }
        };

        let launched_cwd = std::path::PathBuf::from(launched_cwd.trim_end());
        assert_eq!(
            launched_cwd.canonicalize().unwrap(),
            cwd.canonicalize().unwrap()
        );
    }

    /// Helper: create a SessionManager with one session and set up an
    /// EditorJob backed by a real temp file and a trivially-exited child.
    fn editor_test_setup(
        test_name: &str,
        temp_content: &str,
        spawn_done_sentinel: bool,
    ) -> (SessionManager, PathBuf) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);

        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();

        let id = create_session_with_cwd(
            &mut sm,
            &mut picker,
            &mut scene,
            false,
            AiMode::Agentic,
            PathBuf::from("/tmp"),
            "localhost",
            BackendType::Claude,
            Model::Default,
        );

        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = std::env::temp_dir().join(format!(
            "notedeck_editor_test_{}_{}_{}.txt",
            test_name, id, unique
        ));
        std::fs::write(&temp_path, temp_content).unwrap();

        if spawn_done_sentinel {
            let done_path = temp_path.with_extension("done");
            std::fs::write(&done_path, "").unwrap();
        }

        // Spawn a child that exits immediately (simulates a daemonizing terminal).
        let child = std::process::Command::new("true").spawn().unwrap();

        sm.pending_editor = Some(crate::session::EditorJob {
            child,
            temp_path: temp_path.clone(),
            session_id: id,
        });

        (sm, temp_path)
    }

    /// When the sentinel .done file exists, poll_editor_job should read
    /// the temp file content into the session input and clean up.
    #[test]
    fn poll_editor_job_reads_content_on_sentinel() {
        // Create the wrapper .sh script so poll_editor_job uses sentinel mode
        let (mut sm, temp_path) = editor_test_setup("sentinel", "hello from editor", true);
        let script_path = temp_path.with_extension("sh");
        std::fs::write(&script_path, "#!/bin/sh\n").unwrap();

        poll_editor_job(&mut sm);

        // Input should be updated
        let session = sm.get(sm.active_id().unwrap()).unwrap();
        assert_eq!(session.input, "hello from editor");

        // Pending editor should be cleared
        assert!(sm.pending_editor.is_none());

        // Temp files should be cleaned up
        assert!(!temp_path.exists());
        assert!(!temp_path.with_extension("done").exists());
        assert!(!script_path.exists());
    }

    /// When using sentinel mode (wrapper .sh script exists) and the sentinel
    /// .done file does NOT exist yet, poll_editor_job should NOT read the
    /// file, even if the child process has exited. This is the daemonizing
    /// terminal case.
    #[test]
    fn poll_editor_job_waits_for_sentinel_on_linux() {
        let (mut sm, temp_path) = editor_test_setup("waits", "user text", false);

        // Create the .sh script so sentinel mode is active
        let script_path = temp_path.with_extension("sh");
        std::fs::write(&script_path, "#!/bin/sh\n").unwrap();

        // Wait for the child to exit (it's `true`, so basically instant)
        std::thread::sleep(std::time::Duration::from_millis(50));

        poll_editor_job(&mut sm);

        // Editor should still be pending — no sentinel yet
        assert!(
            sm.pending_editor.is_some(),
            "should NOT finish without sentinel even though child exited"
        );

        // Now create the sentinel
        std::fs::write(temp_path.with_extension("done"), "").unwrap();

        poll_editor_job(&mut sm);

        // Now it should have read the file
        let session = sm.get(sm.active_id().unwrap()).unwrap();
        assert_eq!(session.input, "user text");
        assert!(sm.pending_editor.is_none());

        // Cleanup
        let _ = std::fs::remove_file(&script_path);
    }

    /// Without a wrapper script (macOS path), poll_editor_job should
    /// fall back to child exit for completion detection.
    #[test]
    fn poll_editor_job_falls_back_to_child_exit_without_script() {
        let (mut sm, temp_path) = editor_test_setup("fallback", "macos content", false);
        // No .sh script — simulates macOS path

        // Verify no .sh script exists (sentinel mode should be off)
        let script_path = temp_path.with_extension("sh");
        assert!(
            !script_path.exists(),
            "no .sh script should exist for fallback test"
        );

        // Ensure the child process has exited before polling.
        if let Some(ref mut job) = sm.pending_editor {
            let _ = job.child.wait();
        }

        poll_editor_job(&mut sm);

        // Should complete based on child exit alone
        assert!(
            sm.pending_editor.is_none(),
            "pending_editor should be cleared after child exited"
        );
        let session = sm.get(sm.active_id().unwrap()).unwrap();
        assert_eq!(session.input, "macos content");
        assert!(!temp_path.exists());
    }

    /// A remote session running a turn (host status `Working`) is interruptible;
    /// a local session's interruptibility instead follows its token stream.
    #[test]
    fn remote_working_session_is_interruptible() {
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let id = create_remote_agent_session(&mut sm, &mut picker, &mut scene);

        let session = sm.get_mut(id).expect("session should exist");
        // No local token stream and idle host status → not interruptible.
        assert!(!session_is_interruptible(session));

        session.agentic.as_mut().unwrap().remote_status = Some(AgentStatus::Working);
        session.update_status();
        assert!(session_is_interruptible(session));
    }

    /// `execute_interrupt` on a remote session publishes an interrupt command to
    /// the host (there is nothing local to abort) keyed by the session's
    /// live-event id.
    #[test]
    fn execute_interrupt_remote_yields_publish() {
        let ctx = egui::Context::default();
        let backend = crate::backend::RemoteOnlyBackend;
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let id = create_remote_agent_session(&mut sm, &mut picker, &mut scene);
        sm.switch_to(id);

        let expected = sm
            .get(id)
            .unwrap()
            .agentic
            .as_ref()
            .unwrap()
            .event_session_id()
            .to_string();

        let publish = execute_interrupt(&sm, &backend, &ctx);
        assert_eq!(publish.map(|p| p.session_id), Some(expected));
    }

    /// `execute_interrupt` on a local session aborts on the backend directly and
    /// yields no publish.
    #[test]
    fn execute_interrupt_local_yields_no_publish() {
        let ctx = egui::Context::default();
        let backend = crate::backend::RemoteOnlyBackend;
        let mut sm = SessionManager::new();
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let id = create_named_agent_session(
            &mut sm,
            &mut picker,
            &mut scene,
            "local-host",
            "/tmp/project",
            "local",
        );
        sm.switch_to(id);

        let publish = execute_interrupt(&sm, &backend, &ctx);
        assert!(publish.is_none());
    }

    // =========================================================================
    // Interrupt: Escape and the Stop button must be the same thing
    // =========================================================================

    /// A fake backend with Claude's persistent-stream semantics — the property
    /// that makes the interrupt path's local teardown destructive.
    ///
    /// The session actor owns ONE response channel for the whole session, so
    /// `stream_request` hands back a receiver only on the turn that spawns the
    /// actor and `None` on every turn after. An interrupt aborts the in-flight
    /// turn but leaves the actor (and its channel) alive, so a caller that drops
    /// its receiver never gets another one.
    struct PersistentStreamFake {
        /// The one receiver this backend will ever hand out, taken on the first
        /// `stream_request` (the turn that "spawns the actor").
        rx: std::sync::Mutex<Option<std::sync::mpsc::Receiver<crate::DaveApiResponse>>>,
        interrupts: std::sync::atomic::AtomicUsize,
    }

    impl PersistentStreamFake {
        /// Returns the backend plus the actor-side sender, so a test can push a
        /// response the way a live actor would and prove the channel still works.
        fn new() -> (Self, std::sync::mpsc::Sender<crate::DaveApiResponse>) {
            let (tx, rx) = std::sync::mpsc::channel();
            (
                Self {
                    rx: std::sync::Mutex::new(Some(rx)),
                    interrupts: std::sync::atomic::AtomicUsize::new(0),
                },
                tx,
            )
        }

        fn interrupt_count(&self) -> usize {
            self.interrupts.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl crate::backend::AiBackend for PersistentStreamFake {
        fn stream_request(
            &self,
            _messages: Vec<crate::Message>,
            _tools: std::sync::Arc<std::collections::HashMap<String, crate::tools::Tool>>,
            _model: Option<String>,
            _user_id: String,
            _session_id: String,
            _agentium_session_id: Option<String>,
            _cwd: Option<PathBuf>,
            _resume_session_id: Option<String>,
            _permission_mode: PermissionMode,
            _waker: notedeck::Waker,
        ) -> (
            Option<std::sync::mpsc::Receiver<crate::DaveApiResponse>>,
            Option<tokio::task::JoinHandle<()>>,
        ) {
            (self.rx.lock().unwrap().take(), None)
        }

        fn persistent_stream(&self) -> bool {
            true
        }

        fn cleanup_session(&self, _session_id: String) {}

        fn interrupt_session(&self, _session_id: String, _waker: notedeck::Waker) {
            self.interrupts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        fn set_permission_mode(
            &self,
            _session_id: String,
            _mode: PermissionMode,
            _waker: notedeck::Waker,
        ) {
        }
    }

    /// Dispatch a turn exactly the way `Dave::send_user_message_for` does: only
    /// install a receiver when the backend minted one, because a persistent
    /// backend's `None` means "you already hold the session's channel".
    fn dispatch_turn(session: &mut ChatSession, backend: &dyn crate::backend::AiBackend) {
        let (rx, _handle) = backend.stream_request(
            vec![],
            std::sync::Arc::new(std::collections::HashMap::new()),
            None,
            String::new(),
            format!("dave-session-{}", session.id),
            None,
            None,
            None,
            PermissionMode::Default,
            notedeck::Waker::noop(),
        );
        if let Some(rx) = rx {
            session.incoming_tokens = Some(rx);
        }
    }

    /// Set up a local agentic session mid-turn on a persistent-stream backend.
    fn session_mid_turn(
        sm: &mut SessionManager,
        backend: &dyn crate::backend::AiBackend,
    ) -> SessionId {
        let mut picker = DirectoryPicker::new();
        let mut scene = AgentScene::new();
        let id = create_named_agent_session(
            sm,
            &mut picker,
            &mut scene,
            "local-host",
            "/tmp/project",
            "local",
        );
        sm.switch_to(id);
        dispatch_turn(sm.get_mut(id).expect("session"), backend);
        assert!(
            sm.get(id).unwrap().incoming_tokens.is_some(),
            "precondition: the turn installed the session's stream"
        );
        id
    }

    /// The regression: interrupting must not cost the session its stream.
    ///
    /// On a persistent-stream backend `incoming_tokens` is the session's ONE
    /// long-lived channel, not a per-turn one. Dropping it on interrupt leaves
    /// the session permanently deaf — `process_events` skips a session with no
    /// receiver, and the next turn's `stream_request` returns `None` because the
    /// actor is still alive, so nothing ever reinstalls it. The turn's
    /// `QueryComplete` never lands, so `handle_stream_end` never finalizes and
    /// archives the assistant message, never clears `task_handle`, and never
    /// republishes the kind-31988 state carrying the `cli_session` tag that
    /// `claude --resume` needs.
    #[test]
    fn interrupt_keeps_the_session_stream_alive() {
        let ctx = egui::Context::default();
        let (backend, actor_tx) = PersistentStreamFake::new();
        let mut sm = SessionManager::new();
        let id = session_mid_turn(&mut sm, &backend);

        execute_interrupt(&sm, &backend, &ctx);
        assert_eq!(
            backend.interrupt_count(),
            1,
            "the backend was asked to abort"
        );

        // The next turn: a persistent backend hands back no receiver.
        dispatch_turn(sm.get_mut(id).expect("session"), &backend);

        let session = sm.get(id).expect("session");
        let recvr = session
            .incoming_tokens
            .as_ref()
            .expect("session still owns its stream after an interrupt");

        // ...and it is a live channel, not just a `Some`: the actor can still
        // reach the session.
        actor_tx
            .send(crate::DaveApiResponse::Token("after interrupt".into()))
            .expect("actor's sender is still connected");
        assert!(
            matches!(recvr.try_recv(), Ok(crate::DaveApiResponse::Token(t)) if t == "after interrupt"),
            "the post-interrupt turn's output must still reach the session"
        );
    }

    /// Escape (confirmed) and the Stop button must leave a session in the same
    /// state. They are the same gesture; only the confirmation differs, and the
    /// confirmation gates *whether* the interrupt fires, never *what* it does.
    #[test]
    fn esc_and_stop_interrupts_leave_the_same_state() {
        let ctx = egui::Context::default();

        // Stop button.
        let (stop_backend, _stop_tx) = PersistentStreamFake::new();
        let mut stop_sm = SessionManager::new();
        let stop_id = session_mid_turn(&mut stop_sm, &stop_backend);
        execute_interrupt(&stop_sm, &stop_backend, &ctx);

        // Escape, confirmed by a second press inside the window.
        let (esc_backend, _esc_tx) = PersistentStreamFake::new();
        let mut esc_sm = SessionManager::new();
        let esc_id = session_mid_turn(&mut esc_sm, &esc_backend);
        let first = handle_interrupt_request(&esc_sm, &esc_backend, None, &ctx);
        assert!(
            first.pending_since.is_some(),
            "the first Escape only arms the confirmation"
        );
        assert_eq!(
            esc_backend.interrupt_count(),
            0,
            "the first Escape must not interrupt"
        );
        let second = handle_interrupt_request(&esc_sm, &esc_backend, first.pending_since, &ctx);
        assert!(second.pending_since.is_none(), "confirmation is consumed");

        assert_eq!(
            esc_backend.interrupt_count(),
            stop_backend.interrupt_count()
        );
        let esc = esc_sm.get(esc_id).expect("session");
        let stop = stop_sm.get(stop_id).expect("session");
        // Agreeing is not enough — they must agree on the *correct* behaviour,
        // or converging the two paths onto the broken one would satisfy this.
        assert!(
            esc.incoming_tokens.is_some() && stop.incoming_tokens.is_some(),
            "Escape and Stop must agree about the session's stream, and keep it"
        );
        assert_eq!(
            esc.has_pending_permissions(),
            stop.has_pending_permissions(),
            "Escape and Stop must agree about pending permissions"
        );
        assert_eq!(
            esc.status(),
            stop.status(),
            "Escape and Stop must agree about status"
        );
    }

    /// An interrupt must not silently drop the tool permission the user is
    /// being asked about. Dropping the pending oneshot answers the CLI's
    /// `can_use_tool` RPC with a cancellation while the request row in chat
    /// stays unanswered, so the UI and the CLI disagree about what happened.
    #[test]
    fn interrupt_keeps_a_pending_permission_answerable() {
        let ctx = egui::Context::default();
        let (backend, _tx) = PersistentStreamFake::new();
        let mut sm = SessionManager::new();
        let id = session_mid_turn(&mut sm, &backend);

        let (perm_tx, mut perm_rx) = tokio::sync::oneshot::channel();
        let perm_id = uuid::Uuid::new_v4();
        sm.get_mut(id)
            .unwrap()
            .agentic
            .as_mut()
            .unwrap()
            .permissions
            .pending
            .insert(perm_id, perm_tx);

        execute_interrupt(&sm, &backend, &ctx);

        assert!(
            sm.get(id)
                .unwrap()
                .agentic
                .as_ref()
                .unwrap()
                .permissions
                .pending
                .contains_key(&perm_id),
            "the pending permission survives the interrupt so the user can still answer it"
        );
        assert!(
            !matches!(
                perm_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ),
            "the CLI's can_use_tool RPC must not be cancelled out from under the request row"
        );
    }

    // =========================================================================
    // The two gestures, driven through the real widgets
    // =========================================================================
    //
    // The tests above start at `execute_interrupt` / `handle_interrupt_request`
    // and so take the wiring on faith. These drive the actual Stop button and
    // the actual Escape key through an egui harness, so "Escape and Stop are
    // the same thing" is asserted over the whole path a user travels rather
    // than from the seam inward.

    /// Click the real Stop button in a real `InputboxLayout` and return the
    /// action it raises.
    fn click_stop_button() -> Option<crate::ui::DaveAction> {
        use egui_kittest::kittest::Queryable;

        let mut harness = egui_kittest::Harness::new_ui_state(
            |ui, raised: &mut Option<crate::ui::DaveAction>| {
                let mut text = String::new();
                // `show_stop(true)` is the IsWorking state — the only state the
                // Stop button is drawn in.
                let result = crate::ui::InputboxLayout::new_default(&mut text)
                    .show_stop(true)
                    .show(ui);
                if let Some(action) = result.action().action {
                    *raised = Some(action);
                }
            },
            None,
        );
        harness.run();
        assert!(
            harness.state().is_none(),
            "no action before anyone clicks anything"
        );
        harness.get_by_label("Stop").click();
        harness.run();
        harness.state_mut().take()
    }

    /// Press Escape in a real egui frame and return the keybinding it triggers.
    fn press_escape() -> Option<crate::ui::keybindings::KeyAction> {
        let mut harness = egui_kittest::Harness::new_ui_state(
            |ui, action: &mut Option<crate::ui::keybindings::KeyAction>| {
                // Accumulate: `press_key` runs a key-down frame and then a
                // key-up frame, whose `None` would otherwise clobber the hit.
                if let Some(a) = crate::ui::keybindings::check_keybindings(
                    ui.ctx(),
                    false,
                    false,
                    false,
                    AiMode::Agentic,
                ) {
                    *action = Some(a);
                }
            },
            None,
        );
        harness.run();
        harness.press_key_modifiers(egui::Modifiers::NONE, egui::Key::Escape);
        harness.state().clone()
    }

    /// Both gestures reach the interrupt, and both leave the session usable.
    ///
    /// End-to-end over the real widgets: a click on the rendered Stop button
    /// and a real Escape keypress each arrive at the shared interrupt path, and
    /// the session still owns a live stream afterwards either way.
    #[test]
    fn stop_button_and_escape_key_both_interrupt_without_breaking_the_session() {
        let ctx = egui::Context::default();

        // --- Stop button: click the real widget, follow the action it raises.
        let action = click_stop_button();
        assert!(
            matches!(action, Some(crate::ui::DaveAction::Interrupt)),
            "clicking Stop must raise Interrupt, got {action:?}"
        );

        let (stop_backend, stop_tx) = PersistentStreamFake::new();
        let mut stop_sm = SessionManager::new();
        let stop_id = session_mid_turn(&mut stop_sm, &stop_backend);
        let mut overlay = crate::DaveOverlay::None;
        let mut show_list = false;
        crate::ui::handle_ui_action(
            action.expect("Stop raised an action"),
            &mut stop_sm,
            &stop_backend,
            &mut overlay,
            &mut show_list,
            &ctx,
        );
        assert_eq!(stop_backend.interrupt_count(), 1, "Stop aborted the turn");

        // --- Escape: press the real key, follow the keybinding it triggers.
        let key_action = press_escape();
        assert!(
            matches!(
                key_action,
                Some(crate::ui::keybindings::KeyAction::Interrupt)
            ),
            "Escape must trigger Interrupt, got {key_action:?}"
        );

        let (esc_backend, esc_tx) = PersistentStreamFake::new();
        let mut esc_sm = SessionManager::new();
        let esc_id = session_mid_turn(&mut esc_sm, &esc_backend);
        let first = handle_interrupt_request(&esc_sm, &esc_backend, None, &ctx);
        let second = handle_interrupt_request(&esc_sm, &esc_backend, first.pending_since, &ctx);
        assert!(second.pending_since.is_none(), "confirmation is consumed");
        assert_eq!(esc_backend.interrupt_count(), 1, "Escape aborted the turn");

        // --- Both sessions are still reachable by their actor.
        for (label, sm, id, tx) in [
            ("Stop", &stop_sm, stop_id, &stop_tx),
            ("Escape", &esc_sm, esc_id, &esc_tx),
        ] {
            let session = sm.get(id).expect("session");
            let recvr = session
                .incoming_tokens
                .as_ref()
                .unwrap_or_else(|| panic!("{label} left the session without a stream"));
            tx.send(crate::DaveApiResponse::Token("still here".into()))
                .unwrap_or_else(|_| panic!("{label} disconnected the actor's sender"));
            assert!(
                matches!(recvr.try_recv(), Ok(crate::DaveApiResponse::Token(t)) if t == "still here"),
                "{label} left the session unable to receive"
            );
        }
    }
}
