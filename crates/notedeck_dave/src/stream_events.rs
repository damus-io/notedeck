//! Handlers for the events an AI backend streams into a session: tool
//! calls and results, permission requests, subagents, compaction, usage and
//! stream end, plus the dispatches that start a turn: a user turn's and the
//! compact the compact-and-proceed flow drives. [`Dave::process_events`] drains
//! every session's stream through them.

use crate::backend::{AiBackend, BackendType};
use crate::publish::{
    build_user_send_event, ingest_live_event, ingest_live_event_within, pns_ingest,
    publish_auto_accept_response, publish_permission_request, record_dispatch,
};
use crate::session_events::{LiveEventTags, MAX_WIRE_EVENT_BYTES};
use crate::tools::Tool;
use crate::{
    backend, get_backend, messages, reconcile, secret_key_bytes, session, session_events,
    session_loader, Dave, DaveApiResponse, ExecutedTool, Message, PermissionResponse, SessionId,
    SessionInfo, SubagentInfo, ToolCall, ToolCalls, ToolResponse, ToolResponses,
};
use nostrdb::Transaction;
use notedeck::{AppContext, Waker};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Result from processing incoming AI backend tokens for all sessions.
pub(crate) struct ProcessEventsResult {
    /// Sessions that need to dispatch queued user messages.
    pub(crate) needs_send: HashSet<SessionId>,
    /// Sessions that need a compact query dispatched (compact-and-proceed).
    pub(crate) needs_compact: HashSet<SessionId>,
}

impl Dave {
    /// Process incoming tokens from the ai backend for ALL sessions.
    pub(crate) fn process_events(&mut self, app_ctx: &AppContext) -> ProcessEventsResult {
        let mut needs_send: HashSet<SessionId> = HashSet::new();
        let mut needs_compact: HashSet<SessionId> = HashSet::new();
        // Sessions whose turn ended this drain, to reconcile once it's over.
        let mut ended: Vec<SessionId> = Vec::new();
        let active_id = self.session_manager.active_id();

        // Extract secret key once for live event generation
        let secret_key = secret_key_bytes(app_ctx.accounts.get_selected_account().keypair());

        // Get all session IDs to process
        let session_ids = self.session_manager.session_ids();

        for session_id in session_ids {
            // Take the receiver out to avoid borrow conflicts
            let (recvr, backend_type) = {
                let Some(session) = self.session_manager.get_mut(session_id) else {
                    continue;
                };
                (session.incoming_tokens.take(), session.backend_type)
            };

            let Some(recvr) = recvr else {
                continue;
            };

            // Persistent-stream backends (Claude) keep one channel for the whole
            // session, so a turn ends via an explicit `QueryComplete` rather than
            // the channel disconnecting. Non-persistent backends end a turn by
            // dropping the sender (see the disconnect branch below).
            let persistent_stream = self
                .backends
                .get(&backend_type)
                .map(|b| b.persistent_stream())
                .unwrap_or(false);
            let ctx = ApplyCtx {
                ndb: app_ctx.ndb,
                secret_key: &secret_key,
                persistent_stream,
            };
            let mut turn_ended = false;

            while let Ok(res) = recvr.try_recv() {
                // Nudge avatar only for active session
                if active_id == Some(session_id) {
                    if let Some(avatar) = &mut self.avatar {
                        avatar.random_nudge();
                    }
                }

                let Some(session) = self.session_manager.get_mut(session_id) else {
                    break;
                };

                let outcome = apply_response(session, session_id, res, &ctx);
                if outcome.needs_send {
                    needs_send.insert(session_id);
                }
                turn_ended |= outcome.turn_ended;
            }

            // Decide the turn boundary. A disconnected channel means the backend
            // dropped its sender (per-query turn end, or the persistent actor
            // died); an explicit `QueryComplete` (`turn_ended`) ends a turn on a
            // persistent channel that stays open for the next turn / wake-up.
            match recvr.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    ended.push(session_id);
                    if let Some(session) = self.session_manager.get_mut(session_id) {
                        handle_stream_end(
                            session,
                            session_id,
                            &secret_key,
                            app_ctx.ndb,
                            &mut needs_send,
                            &mut needs_compact,
                        );
                    }
                    // Receiver intentionally dropped — the stream is over.
                }
                _ => {
                    // Persistent channel: run stream-end handling for the turn
                    // that just completed, but keep the receiver installed so the
                    // next turn (including a spontaneous wake-up) still flows.
                    if turn_ended {
                        ended.push(session_id);
                        if let Some(session) = self.session_manager.get_mut(session_id) {
                            handle_stream_end(
                                session,
                                session_id,
                                &secret_key,
                                app_ctx.ndb,
                                &mut needs_send,
                                &mut needs_compact,
                            );
                        }
                    }

                    // Channel still open, put receiver back. Waiting on the
                    // backend is intentionally stateless — a session blocked on
                    // user input (a pending permission / NeedsInput) or a slow
                    // provider must never be timed out from here. The backends
                    // themselves carry per-operation timeouts for genuine
                    // network/RPC hangs.
                    if let Some(session) = self.session_manager.get_mut(session_id) {
                        session.incoming_tokens = Some(recvr);
                    }
                }
            }
        }

        let author = *app_ctx.accounts.selected_account_pubkey();
        reconcile_ended_turns(
            &mut self.session_manager,
            &ended,
            &needs_send,
            &needs_compact,
            app_ctx.ndb,
            &author,
        );

        ProcessEventsResult {
            needs_send,
            needs_compact,
        }
    }

    /// Dispatch a compact request to the backend for the active session.
    pub(crate) fn dispatch_compact(&mut self, bt: BackendType, ui: &egui::Ui) {
        dispatch_compact_for_active(&mut self.session_manager, &self.backends, bt, ui.ctx());
    }
}

/// Reconcile each session whose turn ended this drain
/// ([`reconcile::maybe_reconcile_at_rest`]).
///
/// A turn that published nothing at its end has every note indexed already,
/// so no conversation poll will come along to reconcile it. One about to
/// dispatch again (`needs_send`) or compact (`needs_compact`) is not at rest,
/// so it is skipped.
pub(crate) fn reconcile_ended_turns(
    sessions: &mut session::SessionManager,
    ended: &[SessionId],
    needs_send: &HashSet<SessionId>,
    needs_compact: &HashSet<SessionId>,
    ndb: &nostrdb::Ndb,
    author: &nostrdb_net::Pubkey,
) {
    for &session_id in ended {
        if needs_send.contains(&session_id) || needs_compact.contains(&session_id) {
            continue;
        }
        if let Some(session) = sessions.get_mut(session_id) {
            reconcile::maybe_reconcile_at_rest(session, ndb, author);
        }
    }
}

/// What [`apply_response`] needs from outside the session: the database the
/// live events are ingested into, the key they are signed with, and how the
/// backend ends a turn.
pub(crate) struct ApplyCtx<'a> {
    /// Where published live events are PNS-ingested (the host fans them out).
    pub(crate) ndb: &'a nostrdb::Ndb,
    /// The account's signing key; `None` publishes nothing.
    pub(crate) secret_key: &'a Option<[u8; 32]>,
    /// Whether the backend keeps one channel for the whole session, so a turn
    /// ends on an explicit `QueryComplete` rather than a disconnect.
    pub(crate) persistent_stream: bool,
}

/// What applying one backend response asks the caller to do next.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApplyOutcome {
    /// A tool produced a response that has to be sent back to the backend.
    pub(crate) needs_send: bool,
    /// The response closed the turn on a persistent stream; the caller runs
    /// [`handle_stream_end`] after the drain.
    pub(crate) turn_ended: bool,
}

/// Apply one backend response to a session: publish its live event, advance
/// the dispatch state, and update the chat.
///
/// This is the body of [`Dave::process_events`]'s drain loop, lifted out so it
/// needs no `AppContext` — the convergence tests drive a session through it
/// and compare the host's chat with the fold over what it published.
pub(crate) fn apply_response(
    session: &mut session::ChatSession,
    session_id: SessionId,
    res: DaveApiResponse,
    ctx: &ApplyCtx<'_>,
) -> ApplyOutcome {
    let mut outcome = ApplyOutcome::default();

    // A response that adds a row ends the assistant segment before it. Publish
    // that segment first, so it is stamped ahead of this response's own note
    // and sorts in its place in the turn.
    if closes_assistant_segment(session, &res) {
        flush_open_assistant(session, ctx.ndb, ctx.secret_key);
    }

    // Publish the live event for this response. Centralised here so every
    // response type that needs relay propagation is handled in one place.
    if let Some(sk) = ctx.secret_key {
        publish_response(session, &res, ctx.ndb, sk);
    }

    // Backend produced real content — transition dispatch
    // state so redispatch knows the backend consumed our
    // messages (AwaitingResponse → Streaming).
    if !matches!(
        res,
        DaveApiResponse::SessionInfo(_)
            | DaveApiResponse::CompactionStarted
            | DaveApiResponse::CompactionComplete(_)
            | DaveApiResponse::QueryComplete(_)
    ) {
        session.dispatch_state.backend_responded();
    }

    match res {
        DaveApiResponse::Failed(ref err) => {
            session.insert_turn_content(Message::Error(err.to_string()));
        }
        DaveApiResponse::Token(token) => {
            session.append_token(&token);
        }
        DaveApiResponse::ToolCalls(toolcalls) => {
            outcome.needs_send = handle_tool_calls(session, &toolcalls, ctx.ndb);
        }
        DaveApiResponse::ToolRunning(running) => {
            session.push_running_tool(running);
        }
        DaveApiResponse::PermissionRequest(pending) => {
            handle_permission_request(session, pending, ctx.secret_key, ctx.ndb);
        }
        DaveApiResponse::ToolResult(result) => {
            handle_tool_result(session, result);
        }
        DaveApiResponse::SessionInfo(info) => {
            handle_session_info(session, info);
        }
        DaveApiResponse::SubagentSpawned(subagent) => {
            let task_id = subagent.task_id.clone();
            handle_subagent_spawned(session, subagent);
            publish_subagent(session, &task_id, ctx.secret_key, ctx.ndb);
        }
        DaveApiResponse::SubagentOutput { task_id, output } => {
            session.update_subagent_output(&task_id, &output);
        }
        DaveApiResponse::SubagentCompleted { task_id, result } => {
            session.complete_subagent(&task_id, &result);
            publish_subagent(session, &task_id, ctx.secret_key, ctx.ndb);
        }
        DaveApiResponse::SubagentFailed { task_id, error } => {
            session.fail_subagent(&task_id, &error);
            publish_subagent(session, &task_id, ctx.secret_key, ctx.ndb);
        }
        DaveApiResponse::CompactionStarted => {
            if let Some(agentic) = &mut session.agentic {
                if agentic.compact_intent.is_none() {
                    agentic.compact_intent = Some(session::CompactIntent::Manual);
                }
            }
        }
        DaveApiResponse::CompactionComplete(info) => {
            handle_compaction_complete(session, session_id, info);
        }
        DaveApiResponse::UsageUpdate(info) => {
            handle_usage_update(session, info);
        }
        DaveApiResponse::QueryComplete(info) => {
            handle_query_complete(session, info);
            // For a persistent-stream backend this is the turn
            // boundary — the channel stays open, so run stream-end
            // handling after the drain instead of on disconnect.
            outcome.turn_ended = ctx.persistent_stream;
        }

        DaveApiResponse::TodoUpdate(todos) => {
            tracing::debug!("Todo update for session {}", session_id);
            session.insert_turn_content(Message::TodoUpdate(todos));
        }
    }

    outcome
}

/// Publish the live event a backend response carries, if it has one (locally
/// ingested; the host fans it out).
///
/// A running tool publishes a `tool_call` whose content is its plain summary
/// line, so an older observer that renders `tool_call` as assistant text still
/// shows something readable. Its `tool_result` carries the same `tool-id`, and
/// a subagent-internal result its `parent-task`, so the fold can pair the two
/// into one row and nest the result the way the host does.
///
/// A failure publishes an `error` note and a todo list a `todo` note (see
/// [`publish_todo`]).
///
/// PermissionRequest and the subagent lifecycle (spawned/completed/failed)
/// have their own event builders. Token, ToolCalls, SessionInfo and streamed
/// SubagentOutput don't publish.
fn publish_response(
    session: &mut session::ChatSession,
    res: &DaveApiResponse,
    ndb: &nostrdb::Ndb,
    sk: &[u8; 32],
) {
    match res {
        DaveApiResponse::Failed(err) => {
            ingest_live_event(session, ndb, sk, err, "error", LiveEventTags::default());
        }
        DaveApiResponse::ToolRunning(running) => {
            let tags = LiveEventTags {
                tool_id: Some(&running.tool_use_id),
                tool_name: Some(&running.tool_name),
                ..Default::default()
            };
            ingest_live_event(session, ndb, sk, &running.summary, "tool_call", tags);
        }
        DaveApiResponse::ToolResult(result) => {
            publish_tool_result(session, result, ndb, sk);
        }
        DaveApiResponse::CompactionStarted => {
            ingest_live_event(
                session,
                ndb,
                sk,
                "",
                "compaction_started",
                LiveEventTags::default(),
            );
        }
        DaveApiResponse::CompactionComplete(info) => {
            let content = info.pre_tokens.to_string();
            ingest_live_event(
                session,
                ndb,
                sk,
                &content,
                "compaction_complete",
                LiveEventTags::default(),
            );
        }
        DaveApiResponse::TodoUpdate(todos) => {
            publish_todo(session, todos, ndb, sk);
        }
        _ => {}
    }
}

/// Publish a todo list as a `role=todo` live event whose content is the
/// TodoWrite JSON.
///
/// A list over the wire budget is not published at all: cutting JSON short
/// leaves something the fold can't parse, so observers keep the previous list
/// instead of a broken one.
fn publish_todo(
    session: &mut session::ChatSession,
    todos: &serde_json::Value,
    ndb: &nostrdb::Ndb,
    sk: &[u8; 32],
) {
    let content = todos.to_string();
    let whole = |_cap| content.clone();
    if ingest_live_event_within(session, ndb, sk, 0, whole, "todo", LiveEventTags::default())
        .is_none()
    {
        tracing::warn!(
            "todo list is {} bytes, over the {MAX_WIRE_EVENT_BYTES} byte wire budget once \
             built; not publishing it",
            content.len(),
        );
    }
}

/// Publish a finished tool as a `tool_result` live event.
///
/// Encodes summary + raw output so a remote observer can reconstruct the full
/// result, not just the one-line summary (headway:dave/sting-february-sausage).
/// The output keeps its tail, cut so the built note fits
/// [`MAX_WIRE_EVENT_BYTES`] (the host keeps the full copy in memory; the UI
/// truncates for display). An edit's diff rides along when the note fits
/// with the whole output: an edit the CLI auto-approved has no
/// permission_request note to rebuild it from. Past that it is dropped whole,
/// never cut into a diff that didn't happen. The tool name, tool use id and
/// parent task travel as tags.
fn publish_tool_result(
    session: &mut session::ChatSession,
    result: &ExecutedTool,
    ndb: &nostrdb::Ndb,
    sk: &[u8; 32],
) {
    let tags = LiveEventTags {
        tool_id: result.tool_use_id.as_deref(),
        tool_name: Some(&result.tool_name),
        parent_task: result.parent_task_id.as_deref(),
        ..Default::default()
    };
    let output_len = result.output.as_deref().map_or(0, str::len);
    let content = |cap: usize, file_update| {
        let output = result
            .output
            .as_deref()
            .map(|o| backend::truncate_output(o, cap));
        session_loader::ToolResultContent::encode(&result.summary, output.as_deref(), file_update)
    };

    // Escaping only grows a payload, so skip building a diff that can't fit.
    let diff = result
        .file_update
        .as_ref()
        .filter(|update| output_len + update.payload_len() <= MAX_WIRE_EVENT_BYTES);
    if let Some(diff) = diff {
        let with_diff = |_cap| content(output_len, Some(diff));
        if ingest_live_event_within(session, ndb, sk, 0, with_diff, "tool_result", tags).is_some() {
            return;
        }
    }

    let max_cap = output_len.min(MAX_WIRE_EVENT_BYTES);
    let cut = |cap| content(cap, None);
    if ingest_live_event_within(session, ndb, sk, max_cap, cut, "tool_result", tags).is_none() {
        tracing::warn!(
            "{} tool_result is over the {MAX_WIRE_EVENT_BYTES} byte wire budget even \
             without its output; not publishing it",
            result.tool_name
        );
    }
}

/// Whether applying `res` inserts a chat row, ending the open assistant
/// segment: tokens after it start a new segment below that row.
///
/// Responses that only update state, or update a row in place, leave the
/// segment open — flushing there would split one text block into two rows.
fn closes_assistant_segment(session: &session::ChatSession, res: &DaveApiResponse) -> bool {
    match res {
        DaveApiResponse::ToolCalls(_)
        | DaveApiResponse::ToolRunning(_)
        | DaveApiResponse::PermissionRequest(_)
        | DaveApiResponse::SubagentSpawned(_)
        | DaveApiResponse::TodoUpdate(_)
        | DaveApiResponse::Failed(_)
        | DaveApiResponse::CompactionComplete(_) => true,
        DaveApiResponse::ToolResult(result) => session.tool_result_inserts_row(result),
        DaveApiResponse::Token(_)
        | DaveApiResponse::SessionInfo(_)
        | DaveApiResponse::UsageUpdate(_)
        | DaveApiResponse::SubagentOutput { .. }
        | DaveApiResponse::SubagentCompleted { .. }
        | DaveApiResponse::SubagentFailed { .. }
        | DaveApiResponse::CompactionStarted
        | DaveApiResponse::QueryComplete(_) => false,
    }
}

/// Close the session's open assistant segment and publish it as a
/// `role=assistant` live event (locally ingested; the host fans it out).
///
/// Every segment is published once, when it ends, so text written before a
/// tool call reaches observers and survives a restart, in its place in the
/// turn. Publishes nothing without a signing key or an open, non-empty segment.
fn flush_open_assistant(
    session: &mut session::ChatSession,
    ndb: &nostrdb::Ndb,
    secret_key: &Option<[u8; 32]>,
) {
    let Some(text) = session.close_open_assistant() else {
        return;
    };
    let Some(sk) = secret_key else {
        return;
    };
    ingest_live_event(
        session,
        ndb,
        sk,
        &text,
        "assistant",
        LiveEventTags::default(),
    );
}

/// Handle tool calls from the AI backend.
///
/// Pushes the tool calls to chat, executes each one, and pushes the
/// responses. Returns `true` if any tool produced a response that
/// needs to be sent back to the backend.
fn handle_tool_calls(
    session: &mut session::ChatSession,
    toolcalls: &[ToolCall],
    ndb: &nostrdb::Ndb,
) -> bool {
    tracing::info!("got tool calls: {:?}", toolcalls);
    // Route through `insert_turn_content` so a message queued during this turn
    // stays the trailing run and is redispatched afterwards.
    session.insert_turn_content(Message::ToolCalls(toolcalls.to_vec()));

    let txn = Transaction::new(ndb).unwrap();
    let mut needs_send = false;

    for call in toolcalls {
        match call.calls() {
            ToolCalls::PresentNotes(present) => {
                session.insert_turn_content(Message::ToolResponse(ToolResponse::new(
                    call.id().to_owned(),
                    ToolResponses::PresentNotes(present.note_ids.len() as i32),
                )));
                needs_send = true;
            }
            ToolCalls::Invalid(invalid) => {
                session.insert_turn_content(Message::tool_error(
                    call.id().to_string(),
                    invalid.error.clone(),
                ));
                needs_send = true;
            }
            ToolCalls::Query(search_call) => {
                let resp = search_call.execute(&txn, ndb);
                session.insert_turn_content(Message::ToolResponse(ToolResponse::new(
                    call.id().to_owned(),
                    ToolResponses::Query(resp),
                )));
                needs_send = true;
            }
        }
    }

    needs_send
}

/// Handle a permission request from the AI backend.
///
/// Builds and locally-ingests a permission request event for remote clients
/// (the host fans it out), stores the response sender for later, and adds the
/// request to chat.
fn handle_permission_request(
    session: &mut session::ChatSession,
    pending: messages::PendingPermission,
    secret_key: &Option<[u8; 32]>,
    ndb: &nostrdb::Ndb,
) {
    tracing::info!(
        "Permission request for tool '{}': {:?}",
        pending.request.tool_name,
        pending.request.tool_input
    );

    // Publish the request (perm-id, tool-name tags) for remote clients — an
    // auto-accepted one too, so the fold shows the same resolved row the host
    // does.
    if let Some(sk) = secret_key {
        publish_permission_request(session, &pending.request, ndb, sk);
    }

    // Check runtime allowlist — auto-accept, publish the auto response, and
    // show as already-allowed in chat
    if session.agentic.as_ref().is_some_and(|agentic| {
        agentic.should_runtime_allow(&pending.request.tool_name, &pending.request.tool_input)
    }) {
        tracing::info!(
            "runtime allow: auto-accepting '{}' for this session",
            pending.request.tool_name,
        );
        let _ = pending
            .response_tx
            .send(PermissionResponse::Allow { message: None });
        if let Some(sk) = secret_key {
            publish_auto_accept_response(session, pending.request.id, ndb, sk);
        }
        let request = pending.request.auto_accept();
        session.insert_turn_content(Message::PermissionRequest(request));
        return;
    }

    // Store the response sender for later (agentic only)
    if let Some(agentic) = &mut session.agentic {
        agentic
            .permissions
            .pending
            .insert(pending.request.id, pending.response_tx);
    }

    // Add the request to chat for UI display
    session.insert_turn_content(Message::PermissionRequest(pending.request));
}

/// Handle a tool result (execution metadata) from the AI backend.
///
/// Invalidates git status after file-modifying tools, then either folds
/// the result into a subagent or pushes it as a standalone tool response.
fn handle_tool_result(session: &mut session::ChatSession, result: ExecutedTool) {
    tracing::debug!("Tool result: {} - {}", result.tool_name, result.summary);

    if matches!(result.tool_name.as_str(), "Bash" | "Write" | "Edit") {
        if let Some(agentic) = &mut session.agentic {
            agentic.git_status.invalidate();
        }
    }
    // A subagent-internal result folds into its subagent's tool list; a
    // foreground result upgrades its in-flight running row in place (or is
    // appended when there is none).
    if let Some(result) = session.fold_tool_result(result) {
        session.place_tool_result(result);
    }
}

/// Handle a subagent spawn event from the AI backend.
fn handle_subagent_spawned(session: &mut session::ChatSession, subagent: SubagentInfo) {
    tracing::debug!(
        "Subagent spawned: {} ({}) - {}",
        subagent.task_id,
        subagent.subagent_type,
        subagent.description
    );
    let task_id = subagent.task_id.clone();
    // Insert before queued user messages (keeping them trailing) and record the
    // position the subagent row actually landed at.
    let idx = session.insert_turn_content(Message::Subagent(subagent));
    if let Some(agentic) = &mut session.agentic {
        agentic.subagent_indices.insert(task_id, idx);
    }
}

/// Publish a subagent's current lifecycle state (after a spawn, completion or
/// failure has been applied to its chat row) as a `role=subagent` kind-1988
/// note, so remote Dave observers and `agentium` readers see subagents.
///
/// Locally ingested like the other live roles; the host fans it out. The note
/// is built from the row itself, so it carries the full description, type and
/// background flag on every transition. Returns `None` (publishing nothing)
/// without a signing key, for a chat-only session, or for a task id with no
/// row — e.g. a completion for a subagent spawned before a restart.
fn publish_subagent(
    session: &mut session::ChatSession,
    task_id: &str,
    secret_key: &Option<[u8; 32]>,
    ndb: &nostrdb::Ndb,
) -> Option<session_events::BuiltEvent> {
    let sk = secret_key.as_ref()?;
    let agentic = session.agentic.as_mut()?;
    let idx = *agentic.subagent_indices.get(task_id)?;
    let Some(Message::Subagent(info)) = session.chat.get(idx) else {
        return None;
    };
    let session_id = agentic.event_session_id().to_string();
    match session_events::build_subagent_event(info, &session_id, &mut agentic.live_threading, sk) {
        Ok(event) => {
            if pns_ingest(ndb, &event.note_json, sk) {
                agentic.record_self_note(event.note_id);
            }
            Some(event)
        }
        Err(e) => {
            tracing::warn!("failed to build subagent event: {}", e);
            None
        }
    }
}

/// Handle compaction completion from the AI backend.
///
/// Updates agentic state, advances compact-and-proceed if waiting,
/// and pushes the compaction info to chat.
fn handle_compaction_complete(
    session: &mut session::ChatSession,
    session_id: SessionId,
    info: messages::CompactionInfo,
) {
    tracing::debug!(
        "Compaction completed for session {}: pre_tokens={}",
        session_id,
        info.pre_tokens
    );
    if let Some(agentic) = &mut session.agentic {
        agentic.last_compaction = Some(info.clone());

        match agentic.compact_intent {
            Some(session::CompactIntent::ProceedAfterCompaction) => {
                agentic.compact_intent = Some(session::CompactIntent::ReadyToProceed);
            }
            _ => {
                agentic.compact_intent = None;
            }
        }
    }
    session.insert_turn_content(Message::CompactionComplete(info));
}

/// Handle a per-turn usage update from an AssistantMessage.
/// This gives the accurate current context window snapshot since it reflects
/// a single API call's token counts (not the cumulative session total).
fn handle_usage_update(session: &mut session::ChatSession, info: messages::UsageInfo) {
    if let Some(agentic) = &mut session.agentic {
        agentic.usage.input_tokens = info.input_tokens;
        agentic.usage.cache_creation_input_tokens = info.cache_creation_input_tokens;
        agentic.usage.cache_read_input_tokens = info.cache_read_input_tokens;
        agentic.usage.output_tokens = info.output_tokens;
    }
}

/// Handle query completion (usage metrics) from the AI backend.
/// Updates cost and turn count from the final Result message.
fn handle_query_complete(session: &mut session::ChatSession, info: messages::UsageInfo) {
    if let Some(agentic) = &mut session.agentic {
        agentic.usage.num_turns = info.num_turns;
        if let Some(cost) = info.cost_usd {
            agentic.usage.cost_usd = Some(cost);
        }
    }
}

/// Handle a SessionInfo response from the AI backend.
fn handle_session_info(session: &mut session::ChatSession, info: SessionInfo) {
    // Propagate the runtime model for header display only.
    // Keep the original requested override intact so duplicate/clear
    // can reuse the user's intent instead of the backend's resolved model.
    if info.model.is_some() {
        session.details.model.clone_from(&info.model);
    }

    if let Some(agentic) = &mut session.agentic {
        // Live conversation and action events flow through the shared
        // per-account subscriptions (see `subscribe_conversation_events`); no
        // per-session subscription is created here.
        agentic.session_info = Some(info);
    }
    // Persist initial session state now that we know the claude_session_id
    session.state_dirty = true;
}

/// Handle stream-end for a session after the AI backend disconnects.
///
/// Publishes the turn's last open assistant segment (the host fans it out),
/// finalizes the turn's rows, and checks whether queued messages need
/// redispatch.
pub(crate) fn handle_stream_end(
    session: &mut session::ChatSession,
    session_id: SessionId,
    secret_key: &Option<[u8; 32]>,
    ndb: &nostrdb::Ndb,
    needs_send: &mut HashSet<SessionId>,
    needs_compact: &mut HashSet<SessionId>,
) {
    // Publish the segment the turn ended on. A turn that wrote no text since
    // its last row publishes nothing here — never an earlier turn's text.
    flush_open_assistant(session, ndb, secret_key);
    session.finalize_last_assistant();

    // Stop any tool row still spinning: an interrupted turn can end without a
    // result for a tool that had already started. Publish each one's result so
    // the fold stops its spinner too.
    let finalized = session.finalize_running_tools();
    if let Some(sk) = secret_key {
        for result in &finalized {
            publish_tool_result(session, result, ndb, sk);
        }
    }

    session.task_handle = None;

    // If the backend returned nothing this turn (dispatch_state never left
    // AwaitingResponse and no row was added — a compaction adds one without
    // leaving it), show an error so the user isn't left staring at silence.
    //
    // Check redispatch BEFORE adding that error and BEFORE resetting
    // dispatch_state: the check counts the trailing user run against the
    // dispatched count, and the error lands between the dispatched message and
    // any queued one, which would hide the queued one from the count.
    let redispatch = session.needs_redispatch_after_stream_end();
    if matches!(
        session.dispatch_state,
        session::DispatchState::AwaitingResponse { .. }
    ) && !session.turn_has_content()
    {
        tracing::warn!("Session {}: backend returned empty response", session_id);
        publish_error(session, NO_RESPONSE_ERROR, ndb, secret_key);
    }

    if redispatch {
        tracing::info!(
            "Session {}: redispatching queued user message after stream end",
            session_id
        );
        needs_send.insert(session_id);
    }

    session.dispatch_state.stream_ended();

    // Compact-and-proceed: if we were waiting for the stream to end
    // before dispatching the compact query, signal the caller now.
    if let Some(agentic) = &session.agentic {
        if agentic.compact_intent == Some(session::CompactIntent::ProceedAfterStreamEnd) {
            needs_compact.insert(session_id);
        }
    }

    // After compact & approve: compaction must have completed
    // (ReadyToProceed) before we send "Proceed". It is a user turn like any
    // other, so it is published for observers and a restart.
    if session.take_compact_and_proceed() {
        if let Some(sk) = secret_key {
            build_user_send_event(session, ndb, sk, session::PROCEED_MESSAGE, false);
        }
        needs_send.insert(session_id);
    }
}

/// The error a turn shows when the backend ended it without producing anything.
const NO_RESPONSE_ERROR: &str = "No response from backend";

/// Show `text` as this turn's error row and publish it as a `role=error` live
/// event (locally ingested; the host fans it out), so observers and a restart
/// show it too.
///
/// The row goes through [`session::ChatSession::insert_turn_content`], so a
/// message queued during the turn stays the trailing run and is still
/// redispatched.
fn publish_error(
    session: &mut session::ChatSession,
    text: &str,
    ndb: &nostrdb::Ndb,
    secret_key: &Option<[u8; 32]>,
) {
    session.insert_turn_content(Message::Error(text.to_string()));
    let Some(sk) = secret_key else {
        return;
    };
    ingest_live_event(session, ndb, sk, text, "error", LiveEventTags::default());
}

/// Dispatch a compact request to the backend for the active session.
fn dispatch_compact_for_active(
    session_manager: &mut session::SessionManager,
    backends: &HashMap<BackendType, Box<dyn AiBackend>>,
    bt: BackendType,
    ctx: &egui::Context,
) {
    let Some(session) = session_manager.get_active() else {
        return;
    };
    let session_id = format!("dave-session-{}", session.id);
    tracing::info!("Compact requested for session {}", session_id);
    let backend = get_backend(backends, bt);
    let persistent = backend.persistent_stream();
    if let Some(rx) = backend.compact_session(session_id.clone(), notedeck::Waker::egui(ctx)) {
        tracing::info!("Compact dispatched for session {}", session_id);
        if let Some(session) = session_manager.get_active_mut() {
            session.incoming_tokens = Some(rx);
        }
    } else if persistent {
        // Persistent-stream backend: compaction responses flow on the session's
        // existing channel, so there's no new receiver to install.
        tracing::info!(
            "Compact dispatched on persistent channel for session {}",
            session_id
        );
    } else {
        tracing::warn!("Compact failed: no backend session for {}", session_id);
    }
}

/// Dispatch a compact query for a specific session (compact-and-proceed flow).
pub(crate) fn dispatch_compact_for_session(
    session_manager: &mut session::SessionManager,
    backends: &HashMap<BackendType, Box<dyn AiBackend>>,
    session_id: SessionId,
    waker: &Waker,
) {
    let Some(session) = session_manager.get(session_id) else {
        return;
    };
    let bt = session.backend_type;
    let backend_session_id = format!("dave-session-{}", session_id);
    tracing::info!(
        "Session {}: dispatching compact for compact-and-proceed",
        session_id
    );
    let backend = get_backend(backends, bt);
    let persistent = backend.persistent_stream();
    let compact_rx = backend.compact_session(backend_session_id, waker.clone());
    // A non-persistent backend that returned no receiver has no live session to
    // compact — nothing to do. A persistent backend reuses its existing channel
    // (None) and must still record the compact-and-proceed intent.
    if compact_rx.is_none() && !persistent {
        return;
    }
    if let Some(session) = session_manager.get_mut(session_id) {
        if let Some(rx) = compact_rx {
            session.incoming_tokens = Some(rx);
        }
        if let Some(agentic) = &mut session.agentic {
            agentic.compact_intent = Some(session::CompactIntent::ProceedAfterCompaction);
        }
    }
}

/// What starting a turn takes from outside the session: where its dispatch
/// markers go, and what the backend is handed alongside the chat.
pub(crate) struct DispatchCtx<'a> {
    pub(crate) ndb: &'a nostrdb::Ndb,
    /// Signs the dispatch markers; `None` publishes none.
    pub(crate) secret_key: Option<&'a [u8; 32]>,
    /// The account's hashed id, which backends pass to the provider.
    pub(crate) user_id: String,
    pub(crate) tools: Arc<HashMap<String, Tool>>,
    /// The configured extra environment for subprocess backends; the
    /// session's agentium identity is layered over it
    /// ([`session_env`](crate::backend::shared::session_env)).
    pub(crate) session_env: &'a BTreeMap<String, String>,
    pub(crate) waker: &'a Waker,
}

/// Start a turn: hand `session`'s trailing user message(s) to `backend`.
///
/// Marks them dispatched and publishes their dispatch markers
/// ([`record_dispatch`]), so every fold places a queued message where the
/// host dispatched it, then starts the backend's stream over the chat.
///
/// [`Dave::send_user_message_for`] picks the backend and calls this; the
/// convergence harness dispatches through it too, so a dispatch that stopped
/// publishing its markers fails the queued scenarios there.
pub(crate) fn dispatch_turn(
    session: &mut session::ChatSession,
    backend: &dyn AiBackend,
    ctx: &DispatchCtx<'_>,
) {
    record_dispatch(session, ctx.ndb, ctx.secret_key);

    let session_id = format!("dave-session-{}", session.id);
    // The stable kind-31988 d-tag (UUID), distinct from the ephemeral
    // `dave-session-{n}` routing key above, goes into the session env as the
    // agentium identity so an in-session agent reads its OWN ref. Only
    // agentic sessions have one.
    let agentium_session_id = session.agentic.as_ref().map(|a| a.event_session_id());
    let session_env = crate::backend::shared::session_env(agentium_session_id, ctx.session_env);
    let messages = session.chat.clone();
    let cwd = session.agentic.as_ref().map(|a| a.cwd.clone());
    let resume_session_id = session
        .agentic
        .as_ref()
        .and_then(|a| a.cli_resume_id().map(|s| s.to_string()));
    // The session's initial permission mode, so a subprocess backend spawns
    // its CLI in the mode the UI already shows (e.g. Auto) rather than
    // Default. Only the turn that creates the session actor consumes it;
    // later changes go through backend.set_permission_mode. Non-agentic
    // sessions have no mode and fall back to Default.
    let permission_mode = session
        .agentic
        .as_ref()
        .map(|a| a.permission_mode)
        .unwrap_or(claude_agent_sdk_rs::PermissionMode::Default);
    let model_name = session.details.resolve_model();
    // `rx` is `None` for persistent-stream backends on subsequent turns — the
    // session already owns a long-lived channel we must keep, so only replace
    // `incoming_tokens` when a new receiver was minted.
    let (rx, task_handle) = backend.stream_request(
        messages,
        ctx.tools.clone(),
        model_name,
        ctx.user_id.clone(),
        session_id,
        session_env,
        cwd,
        resume_session_id,
        permission_mode,
        ctx.waker.clone(),
    );
    if let Some(rx) = rx {
        session.incoming_tokens = Some(rx);
    }
    session.task_handle = task_handle;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiMode;
    use crate::messages::SubagentStatus;
    use crate::tests::{test_config, test_secret_key};
    use nostrdb::Ndb;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Read a tag's value out of a built event's JSON.
    fn tag<'a>(event: &'a serde_json::Value, name: &str) -> Option<&'a str> {
        event["tags"]
            .as_array()?
            .iter()
            .find(|t| t[0] == name)
            .and_then(|t| t[1].as_str())
    }

    /// An agentic session, its ndb and a signing key, for driving responses
    /// through [`apply_response`].
    struct Fixture {
        session: session::ChatSession,
        ndb: Ndb,
        secret_key: Option<[u8; 32]>,
        _dir: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
            let session = session::ChatSession::new(
                1,
                PathBuf::from("/tmp"),
                AiMode::Agentic,
                BackendType::Claude,
            );
            Fixture {
                session,
                ndb,
                secret_key: Some(test_secret_key()),
                _dir: dir,
            }
        }

        fn apply(&mut self, res: DaveApiResponse) {
            let ctx = ApplyCtx {
                ndb: &self.ndb,
                secret_key: &self.secret_key,
                persistent_stream: true,
            };
            apply_response(&mut self.session, 1, res, &ctx);
        }

        fn stream_end(&mut self) {
            handle_stream_end(
                &mut self.session,
                1,
                &self.secret_key,
                &self.ndb,
                &mut HashSet::new(),
                &mut HashSet::new(),
            );
        }

        /// How many notes the session has published so far.
        fn published(&self) -> usize {
            self.session.agentic.as_ref().unwrap().seen_note_ids.len()
        }

        /// The session's assistant rows, in chat order.
        fn assistant_texts(&self) -> Vec<&str> {
            self.session
                .chat
                .iter()
                .filter_map(|m| match m {
                    Message::Assistant(msg) => Some(msg.text()),
                    _ => None,
                })
                .collect()
        }
    }

    fn executed(tool_use_id: Option<&str>, parent_task_id: Option<&str>) -> ExecutedTool {
        ExecutedTool {
            tool_name: "Grep".to_string(),
            summary: "3 matches".to_string(),
            output: None,
            parent_task_id: parent_task_id.map(str::to_string),
            file_update: None,
            tool_use_id: tool_use_id.map(str::to_string),
        }
    }

    /// A turn that writes no text publishes no assistant note — before the
    /// segment flush, stream end re-published the previous turn's text.
    #[test]
    fn textless_turn_publishes_no_assistant_note() {
        let mut f = Fixture::new();
        f.session.mark_dispatched();
        f.apply(DaveApiResponse::Token("first answer".to_string()));
        f.stream_end();
        assert_eq!(f.published(), 1, "the text turn publishes its segment");

        f.session.mark_dispatched();
        f.apply(DaveApiResponse::ToolResult(executed(None, None)));
        f.stream_end();
        assert_eq!(
            f.published(),
            2,
            "the tool-only turn publishes its tool result and nothing else"
        );
    }

    /// A subagent-internal result, or a result upgrading its running row in
    /// place, adds no row, so it must not split the text streaming around it.
    #[test]
    fn in_place_results_keep_the_segment_open() {
        let mut f = Fixture::new();
        f.session.mark_dispatched();
        f.apply(DaveApiResponse::SubagentSpawned(SubagentInfo {
            task_id: "s1".to_string(),
            description: "Map the loader".to_string(),
            subagent_type: "Explore".to_string(),
            status: SubagentStatus::Running,
            output: String::new(),
            max_output_size: 4000,
            tool_results: Vec::new(),
            background: true,
        }));
        f.apply(DaveApiResponse::ToolRunning(crate::messages::RunningTool {
            tool_use_id: "t1".to_string(),
            tool_name: "Grep".to_string(),
            summary: "loader".to_string(),
        }));
        f.apply(DaveApiResponse::Token("while ".to_string()));
        f.apply(DaveApiResponse::ToolResult(executed(None, Some("s1"))));
        f.apply(DaveApiResponse::Token("that runs, ".to_string()));
        f.apply(DaveApiResponse::ToolResult(executed(Some("t1"), None)));
        f.apply(DaveApiResponse::Token("one block".to_string()));
        f.stream_end();

        assert_eq!(f.assistant_texts(), ["while that runs, one block"]);
    }

    /// An empty response shows the error on any turn, not only before the
    /// session's first assistant text.
    #[test]
    fn empty_response_after_a_text_turn_shows_error() {
        let mut f = Fixture::new();
        f.session.mark_dispatched();
        f.apply(DaveApiResponse::Token("first answer".to_string()));
        f.stream_end();

        f.session.mark_dispatched();
        f.stream_end();
        assert!(matches!(
            f.session.chat.last(),
            Some(Message::Error(e)) if e == "No response from backend"
        ));
    }

    /// An empty response's error lands between the dispatched message and one
    /// queued during the turn, and the queued message is still redispatched:
    /// the redispatch check runs before the error row splits the trailing
    /// user run.
    #[test]
    fn empty_response_error_keeps_queued_message_redispatched() {
        let mut f = Fixture::new();
        f.session
            .chat
            .push(Message::User("first".to_string().into()));
        f.session.mark_dispatched();
        f.session
            .chat
            .push(Message::User("queued".to_string().into()));

        let mut needs_send = HashSet::new();
        handle_stream_end(
            &mut f.session,
            1,
            &f.secret_key,
            &f.ndb,
            &mut needs_send,
            &mut HashSet::new(),
        );

        assert!(
            needs_send.contains(&1),
            "the queued message must be redispatched"
        );
        let rows: Vec<&str> = f
            .session
            .chat
            .iter()
            .map(|m| match m {
                Message::User(u) => u.text.as_str(),
                Message::Error(e) => e.as_str(),
                _ => "other",
            })
            .collect();
        assert_eq!(rows, ["first", NO_RESPONSE_ERROR, "queued"]);
        assert_eq!(f.published(), 1, "the error is published");
    }

    /// The host publishes each subagent lifecycle transition as a
    /// `role=subagent` kind-1988 note built from the chat row — spawned while
    /// running, then completed with its result — and marks each note seen so
    /// its relay echo isn't reprocessed (headway:dave/good-note-salute).
    #[test]
    fn subagent_lifecycle_publishes_role_subagent_notes() {
        let tmp = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp.path().to_str().unwrap(), &test_config()).unwrap();
        let sk = Some(test_secret_key());
        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        session.agentic.as_mut().unwrap().event_id = "subagent-publish".to_string();

        handle_subagent_spawned(
            &mut session,
            SubagentInfo {
                task_id: "toolu_1".to_string(),
                description: "Map the loader".to_string(),
                subagent_type: "Explore".to_string(),
                status: SubagentStatus::Running,
                output: String::new(),
                max_output_size: 4000,
                tool_results: Vec::new(),
                background: true,
            },
        );
        let spawned =
            publish_subagent(&mut session, "toolu_1", &sk, &ndb).expect("a spawn publishes a note");
        session.complete_subagent("toolu_1", "found 3 call sites");
        let completed = publish_subagent(&mut session, "toolu_1", &sk, &ndb)
            .expect("a completion publishes a note");

        for (event, status) in [(&spawned, "running"), (&completed, "completed")] {
            let v: serde_json::Value = serde_json::from_str(&event.note_json).unwrap();
            assert_eq!(v["kind"], session_events::AI_CONVERSATION_KIND);
            assert_eq!(tag(&v, "role"), Some("subagent"));
            assert_eq!(tag(&v, "d"), Some("subagent-publish"));
            assert_eq!(tag(&v, "task-id"), Some("toolu_1"));
            assert_eq!(tag(&v, "subagent-type"), Some("Explore"));
            assert_eq!(tag(&v, "background"), Some("true"));
            assert_eq!(tag(&v, "status"), Some(status));
        }
        let v: serde_json::Value = serde_json::from_str(&completed.note_json).unwrap();
        let content: session_events::SubagentContent =
            serde_json::from_str(v["content"].as_str().unwrap()).unwrap();
        assert_eq!(content.output.as_deref(), Some("found 3 call sites"));

        let seen = &session.agentic.as_ref().unwrap().seen_note_ids;
        assert!(seen.contains(&spawned.note_id) && seen.contains(&completed.note_id));

        assert!(
            publish_subagent(&mut session, "unknown-task", &sk, &ndb).is_none(),
            "a task id with no row publishes nothing"
        );
    }
}
