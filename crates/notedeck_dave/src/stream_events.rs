//! Handlers for the events an AI backend streams into a session: tool
//! calls and results, permission requests, subagents, compaction, usage and
//! stream end, plus the dispatches that start a turn: a user turn's and the
//! compact the compact-and-proceed flow drives. [`Dave::process_events`] drains
//! every session's stream through them.

use crate::backend::{AiBackend, BackendType};
use crate::backend_profiles::{missing_profile_error, BackendProfiles};
use crate::publish::{
    build_user_send_event, ingest_live_event, ingest_live_event_within,
    publish_auto_accept_response, publish_permission_request, record_dispatch, record_published,
    Ingest, Place,
};
use crate::session_events::{LiveEventTags, MAX_WIRE_EVENT_BYTES};
use crate::tool_round::{AnsweredRound, RoundCall, StartedCall, ToolRound};
use crate::tools::Tool;
use crate::{
    backend, get_backend, messages, reconcile, secret_key_bytes, session, session_events,
    session_loader, Dave, DaveApiResponse, ExecutedTool, Message, PermissionResponse, SessionId,
    SessionInfo, SubagentInfo, ToolCall, ToolCalls, ToolResponses,
};
use nostrdb::Transaction;
use nostrdb_net::KeypairUnowned;
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

/// What Dave's per-frame work needs from the host: where events go, who signs
/// them, and how to wake.
///
/// [`Dave::update`](notedeck::App::update) builds one from its
/// [`AppContext`] each frame and hands it down; a test builds one over its own
/// ndb and key ([`HostCtx::new`]) and drives the same code with no
/// `AppContext` at all.
pub struct HostCtx<'a> {
    /// Where published events are PNS-ingested (the host fans them out).
    pub(crate) ndb: &'a nostrdb::Ndb,
    /// The account's signing key, copied out (an `AppContext` only lends it);
    /// `None` publishes nothing.
    pub(crate) secret_key: Option<[u8; 32]>,
    /// The account whose notes Dave folds and reconciles against.
    pub(crate) author: nostrdb_net::Pubkey,
    /// How a backend's worker asks for another pass when it has output.
    pub(crate) waker: &'a Waker,
}

impl<'a> HostCtx<'a> {
    /// A host acting as `keypair`'s account over `ndb`.
    pub fn new(ndb: &'a nostrdb::Ndb, keypair: KeypairUnowned<'_>, waker: &'a Waker) -> Self {
        let author = *keypair.pubkey;
        HostCtx {
            ndb,
            secret_key: secret_key_bytes(keypair),
            author,
            waker,
        }
    }

    /// The app host's view: its ndb and waker, acting as the selected account.
    pub fn from_app(app: &'a AppContext<'_>) -> Self {
        Self::new(
            app.ndb,
            app.accounts.get_selected_account().keypair(),
            app.waker,
        )
    }

    /// A host signing as `secret_key`'s account, for tests that drive Dave's
    /// per-frame work over their own ndb.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn signing(ndb: &'a nostrdb::Ndb, secret_key: &[u8; 32], waker: &'a Waker) -> Self {
        let keypair = nostrdb_net::FullKeypair::from_secret_bytes(secret_key)
            .expect("a valid secret key")
            .to_keypair();
        Self::new(ndb, (&keypair).into(), waker)
    }

    /// The signing key, if the account has one.
    pub(crate) fn sk(&self) -> Option<&[u8; 32]> {
        self.secret_key.as_ref()
    }

    /// The account's anonymous id, which backends pass to the provider: a
    /// hash of the secret key, or of the pubkey for a watch-only account.
    pub(crate) fn user_id(&self) -> String {
        use sha2::{Digest, Sha256};
        // pubkeys have degraded privacy, don't do that
        let key_input = self
            .secret_key
            .as_ref()
            .map(|sk| sk.as_slice())
            .unwrap_or(self.author.bytes());
        let hex_key = hex::encode(key_input);
        let input = format!("{hex_key}notedeck_dave_user_id");
        hex::encode(Sha256::digest(input))
    }
}

impl Dave {
    /// Process incoming tokens from the ai backend for ALL sessions.
    pub(crate) fn process_events(&mut self, host: &HostCtx<'_>) -> ProcessEventsResult {
        let mut needs_send: HashSet<SessionId> = HashSet::new();
        let mut needs_compact: HashSet<SessionId> = HashSet::new();
        // Sessions whose turn ended this drain, to reconcile once it's over.
        let mut ended: Vec<SessionId> = Vec::new();
        let active_id = self.session_manager.active_id();

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
                host,
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

                if let DaveApiResponse::RateLimit(info) = &res {
                    self.rate_limit = Some(info.merged_onto(self.rate_limit.as_ref()));
                }
                let outcome = apply_response(session, session_id, res, &ctx);
                turn_ended |= outcome.turn_ended;
            }

            // Decide the turn boundary. A disconnected channel means the backend
            // dropped its sender (per-query turn end, or the persistent actor
            // died); an explicit `QueryComplete` (`turn_ended`) ends a turn on a
            // persistent channel that stays open for the next turn / wake-up.
            match recvr.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if let Some(session) = self.session_manager.get_mut(session_id) {
                        // The backend is gone, and nothing it held will
                        // resume the session now.
                        session.wakeup_turn = false;
                        session.waiting_on = Default::default();
                        session.inspector.background.set(Vec::new());
                        if end_turn_unless_tools_run(
                            session,
                            session_id,
                            host,
                            &mut needs_send,
                            &mut needs_compact,
                        ) {
                            ended.push(session_id);
                        }
                    }
                    // Receiver intentionally dropped — the stream is over.
                }
                _ => {
                    // Persistent channel: run stream-end handling for the turn
                    // that just completed, but keep the receiver installed so the
                    // next turn (including a spontaneous wake-up) still flows.
                    if turn_ended {
                        if let Some(session) = self.session_manager.get_mut(session_id) {
                            if end_turn_unless_tools_run(
                                session,
                                session_id,
                                host,
                                &mut needs_send,
                                &mut needs_compact,
                            ) {
                                ended.push(session_id);
                            }
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

        // A turn held open by its tool calls ends on the drain where the
        // round's last answer lands, which is this one for a round of fast
        // calls whose stream just ended.
        for session in self.session_manager.iter_mut() {
            let session_id = session.id;
            if finish_tool_round(
                session,
                session_id,
                host,
                &mut needs_send,
                &mut needs_compact,
            ) {
                ended.push(session_id);
            }
        }

        reconcile_ended_turns(
            &mut self.session_manager,
            &ended,
            &needs_send,
            &needs_compact,
            host,
        );
        crate::child_report::report_child_turns(
            &mut self.session_manager,
            &ended,
            &needs_send,
            &needs_compact,
            host,
        );

        ProcessEventsResult {
            needs_send,
            needs_compact,
        }
    }

    /// The status bar's COMPACT: compact the active session's context.
    ///
    /// A Claude or remote session gets [`COMPACT_COMMAND`] as a user turn,
    /// the same as if it were typed. Asking the backend directly
    /// ([`AiBackend::compact_session`]) needs a live CLI process, so it did
    /// nothing for a restored session that hadn't sent a turn in this
    /// process, and nothing at all for a remote session, which has no local
    /// backend. A turn starts the CLI if there is none, waits behind a turn
    /// already running, and for a remote session is published for the host
    /// to run. The turn's dispatch shows at once; the CLI's
    /// `compaction_started` then shows the session compacting.
    ///
    /// Codex has no slash commands, so a local Codex session asks its
    /// backend instead. A remote Codex session can't be compacted from here.
    pub(crate) fn compact_active(&mut self, host: &HostCtx<'_>) {
        let Some(sid) = self.session_manager.active_id() else {
            return;
        };
        // A subagent's session runs no backend of its own.
        if self.session_manager.backing_id(sid) != sid {
            return;
        }
        let Some(session) = self.session_manager.get(sid) else {
            return;
        };
        if session.backend_type == BackendType::Codex {
            if session.is_remote() {
                tracing::warn!("Compact: a remote Codex session can't be compacted");
                return;
            }
            dispatch_compact_to_backend(&mut self.session_manager, &self.backends, sid, host.waker);
            return;
        }
        if self.add_user_message_for_session(sid, host, COMPACT_COMMAND.to_owned(), Vec::new()) {
            self.send_user_message_for(sid, host);
        }
    }
}

/// The turn the status bar's COMPACT sends a Claude session
/// ([`Dave::compact_active`]).
pub(crate) const COMPACT_COMMAND: &str = "/compact";

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
    host: &HostCtx<'_>,
) {
    for &session_id in ended {
        if needs_send.contains(&session_id) || needs_compact.contains(&session_id) {
            continue;
        }
        if let Some(session) = sessions.get_mut(session_id) {
            reconcile::maybe_reconcile_at_rest(session, host.ndb, &host.author);
        }
    }
}

/// What [`apply_response`] needs from outside the session: the host its live
/// events are ingested into and signed by, and how the backend ends a turn.
pub(crate) struct ApplyCtx<'a> {
    pub(crate) host: &'a HostCtx<'a>,
    /// Whether the backend keeps one channel for the whole session, so a turn
    /// ends on an explicit `QueryComplete` rather than a disconnect.
    pub(crate) persistent_stream: bool,
}

/// What applying one backend response asks the caller to do next.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApplyOutcome {
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

    // The inspector reads every response before the match below consumes it.
    session.inspector.apply(&res);

    // A response that adds a row ends the assistant segment before it. Publish
    // that segment first, so it is stamped ahead of this response's own note
    // and sorts in its place in the turn.
    if closes_assistant_segment(session, &res) {
        flush_open_assistant(session, ctx.host.ndb, &ctx.host.secret_key);
    }

    // Publish the live event for a response whose note isn't a row's: a
    // running tool, a tool result, a compaction starting. A response that adds
    // a row it publishes (an error, a command reply, a todo list, a finished
    // compaction) does both through `push_published` in the match below.
    if let Some(sk) = ctx.host.sk() {
        publish_response(session, &res, ctx.host.ndb, sk);
    }

    // Backend produced real content — transition dispatch
    // state so redispatch knows the backend consumed our
    // messages (AwaitingResponse → Streaming).
    if !matches!(
        res,
        DaveApiResponse::SessionInfo(_)
            | DaveApiResponse::ServerInfo(_)
            | DaveApiResponse::CompactionStarted
            | DaveApiResponse::CompactionComplete(_)
            | DaveApiResponse::QueryComplete(_)
            | DaveApiResponse::WaitingOn(_)
            | DaveApiResponse::BackgroundWork(_)
            | DaveApiResponse::WakeupTurnStarted
            | DaveApiResponse::CliState(_)
            | DaveApiResponse::RateLimit(_)
            | DaveApiResponse::SubagentProgress(_)
    ) {
        session.dispatch_state.backend_responded();
    }

    match res {
        DaveApiResponse::Failed(err) => {
            session.push_published(
                Message::Error(err),
                Place::Turn,
                ctx.host.ndb,
                ctx.host.sk(),
            );
        }
        DaveApiResponse::Token(token) => {
            session.append_token(&token);
        }
        DaveApiResponse::ToolCalls(toolcalls) => {
            handle_tool_calls(session, &toolcalls, ctx.host.ndb, ctx.host.waker);
        }
        DaveApiResponse::ToolRunning(running) => {
            session.push_running_tool(running);
        }
        DaveApiResponse::PermissionRequest(pending) => {
            handle_permission_request(session, pending, &ctx.host.secret_key, ctx.host.ndb);
        }
        DaveApiResponse::ToolResult(result) => {
            handle_tool_result(session, result);
        }
        DaveApiResponse::SessionInfo(info) => {
            handle_session_info(session, info);
        }
        DaveApiResponse::ServerInfo(info) => {
            let catalog = crate::slash::SlashCatalog::from_server_info(&info);
            session.slash_catalog = Some(std::rc::Rc::new(catalog));
        }
        DaveApiResponse::CommandReply(text) => {
            let reply = Message::System(text.into());
            session.push_published(reply, Place::Turn, ctx.host.ndb, ctx.host.sk());
        }
        DaveApiResponse::SubagentSpawned(subagent) => {
            let task_id = match session.resume_subagent(&subagent) {
                Some(task_id) => task_id,
                None => {
                    let task_id = subagent.task_id.clone();
                    handle_subagent_spawned(session, subagent);
                    task_id
                }
            };
            publish_subagent(session, &task_id, &ctx.host.secret_key, ctx.host.ndb);
        }
        DaveApiResponse::SubagentOutput { task_id, output } => {
            session.update_subagent_output(&task_id, &output);
        }
        // Neither publishes: the next lifecycle note (the completion) carries
        // the agent id and transcript, so relays see one note per transition,
        // not one per block.
        DaveApiResponse::SubagentText {
            task_id,
            kind,
            text,
        } => {
            session.push_subagent_text(&task_id, kind, &text);
        }
        DaveApiResponse::SubagentIdentified { task_id, agent_id } => {
            session.set_subagent_agent_id(&task_id, agent_id);
        }
        DaveApiResponse::SubagentCompleted { task_id, result } => {
            session.complete_subagent(&task_id, &result);
            publish_subagent(session, &task_id, &ctx.host.secret_key, ctx.host.ndb);
        }
        DaveApiResponse::SubagentFailed { task_id, error } => {
            session.fail_subagent(&task_id, &error);
            publish_subagent(session, &task_id, &ctx.host.secret_key, ctx.host.ndb);
        }
        DaveApiResponse::CompactionStarted => {
            if let Some(agentic) = &mut session.agentic {
                if agentic.compact_intent.is_none() {
                    agentic.compact_intent = Some(session::CompactIntent::Manual);
                }
            }
        }
        DaveApiResponse::CompactionComplete(info) => {
            handle_compaction_complete(session, session_id, info, ctx.host);
        }
        // The inspector's usage history (folded above) is the one record
        // of a session's tokens and cost; nothing else keeps a copy.
        DaveApiResponse::UsageUpdate(_) => {}
        DaveApiResponse::QueryComplete(_) => {
            session.wakeup_turn = false;
            // For a persistent-stream backend this is the turn
            // boundary — the channel stays open, so run stream-end
            // handling after the drain instead of on disconnect.
            outcome.turn_ended = ctx.persistent_stream;
        }

        DaveApiResponse::TodoUpdate(todos) => {
            tracing::debug!("Todo update for session {}", session_id);
            let todos = Message::TodoUpdate(todos);
            session.push_published(todos, Place::Turn, ctx.host.ndb, ctx.host.sk());
        }
        DaveApiResponse::WaitingOn(waiting_on) => {
            session.waiting_on = waiting_on;
        }
        DaveApiResponse::WakeupTurnStarted => {
            session.wakeup_turn = true;
            session.start_turn(session::now_unix());
        }
        // Only the inspector keeps these, folded above (and the account's
        // rate limits on `Dave`, by the drain loop).
        DaveApiResponse::CliState(_)
        | DaveApiResponse::RateLimit(_)
        | DaveApiResponse::BackgroundWork(_)
        | DaveApiResponse::SubagentProgress(_) => {}
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
/// A failure, a command reply, a todo list and a finished compaction publish
/// with their rows, through [`session::ChatSession::push_published`] in
/// [`apply_response`]. PermissionRequest and the subagent lifecycle
/// (spawned/completed/failed) have their own event builders. Token, ToolCalls,
/// SessionInfo and streamed SubagentOutput don't publish.
fn publish_response(
    session: &mut session::ChatSession,
    res: &DaveApiResponse,
    ndb: &nostrdb::Ndb,
    sk: &[u8; 32],
) {
    match res {
        DaveApiResponse::ToolRunning(running) => {
            let tags = LiveEventTags {
                tool_id: Some(&running.tool_use_id),
                tool_name: Some(&running.tool_name),
                tool_desc: running.description.as_deref(),
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
        _ => {}
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
/// never cut into a diff that didn't happen. The tool name, tool use id,
/// description, failure and parent task travel as tags.
fn publish_tool_result(
    session: &mut session::ChatSession,
    result: &ExecutedTool,
    ndb: &nostrdb::Ndb,
    sk: &[u8; 32],
) {
    let tags = LiveEventTags {
        tool_id: result.tool_use_id.as_deref(),
        tool_name: Some(&result.tool_name),
        tool_desc: result.description.as_deref(),
        tool_error: result.is_error,
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
        | DaveApiResponse::CommandReply(_)
        | DaveApiResponse::CompactionComplete(_) => true,
        DaveApiResponse::ToolResult(result) => session.tool_result_inserts_row(result),
        DaveApiResponse::Token(_)
        | DaveApiResponse::SessionInfo(_)
        | DaveApiResponse::ServerInfo(_)
        | DaveApiResponse::UsageUpdate(_)
        | DaveApiResponse::SubagentOutput { .. }
        | DaveApiResponse::SubagentText { .. }
        | DaveApiResponse::SubagentIdentified { .. }
        | DaveApiResponse::SubagentCompleted { .. }
        | DaveApiResponse::SubagentFailed { .. }
        | DaveApiResponse::CompactionStarted
        | DaveApiResponse::QueryComplete(_)
        | DaveApiResponse::WaitingOn(_)
        | DaveApiResponse::BackgroundWork(_)
        | DaveApiResponse::WakeupTurnStarted
        | DaveApiResponse::CliState(_)
        | DaveApiResponse::RateLimit(_)
        | DaveApiResponse::SubagentProgress(_) => false,
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

/// Handle a round of tool calls from the AI backend: show the calls, then
/// start each one as a [`ToolRound`] the session holds until every call is
/// answered.
///
/// Nothing here waits on a tool. A fast call answers on the spot and a slow
/// one runs on a thread of its own; [`finish_tool_round`] puts the answers in
/// the chat and asks the model to go on once the round is answered and the
/// backend's stream has ended.
fn handle_tool_calls(
    session: &mut session::ChatSession,
    toolcalls: &[ToolCall],
    ndb: &nostrdb::Ndb,
    waker: &Waker,
) {
    tracing::info!("got tool calls: {:?}", toolcalls);
    // A backend asks for its next round only with this one's answers, so a
    // round still here is a stale one: answer it (cancelled) before the new
    // calls, so each round's answers stay under its own calls.
    if let Some(stale) = session.tool_round.take() {
        tracing::warn!(
            "session {}: new tool calls while a round was outstanding",
            session.id
        );
        stale.cancel();
        for response in stale.into_answers().responses {
            session.insert_turn_content(response);
        }
    }
    // Route through `insert_turn_content` so a message queued during this turn
    // stays the trailing run and is redispatched afterwards.
    session.insert_turn_content(Message::ToolCalls(toolcalls.to_vec()));

    let calls = toolcalls
        .iter()
        .map(|call| RoundCall {
            id: call.id().to_owned(),
            started: start_call(call, ndb),
        })
        .collect();
    session.tool_round = Some(ToolRound::start(calls, waker));
}

/// Start one tool call. Every tool Dave runs itself today is a fast host
/// tool, so each answers on the spot; a slow tool would hand back a
/// [`StartedCall::Pending`] job instead.
fn start_call(call: &ToolCall, ndb: &nostrdb::Ndb) -> StartedCall {
    let response = match call.calls() {
        ToolCalls::PresentNotes(present) => {
            ToolResponses::PresentNotes(present.note_ids.len() as i32)
        }
        ToolCalls::Invalid(invalid) => ToolResponses::Error(invalid.error.clone()),
        ToolCalls::Query(search_call) => match Transaction::new(ndb) {
            Ok(txn) => ToolResponses::Query(search_call.execute(&txn, ndb)),
            Err(err) => ToolResponses::Error(format!("failed to open db: {err}")),
        },
    };
    StartedCall::Done(Box::new(response))
}

/// Finish the session's round of tool calls if it is done: every call is
/// answered (or the round was cancelled) and the backend has ended its
/// turn. The answers go into the chat in call order, the model is asked to
/// go on unless the round was cancelled, and the turn the round held open
/// ends. Returns whether it did, so the caller counts the turn as ended.
pub(crate) fn finish_tool_round(
    session: &mut session::ChatSession,
    session_id: SessionId,
    host: &HostCtx<'_>,
    needs_send: &mut HashSet<SessionId>,
    needs_compact: &mut HashSet<SessionId>,
) -> bool {
    let Some(round) = session.tool_round.as_mut() else {
        return false;
    };
    if !round.turn_over() || !round.poll() {
        return false;
    }
    let Some(round) = session.tool_round.take() else {
        return false;
    };
    let AnsweredRound {
        responses,
        cancelled,
    } = round.into_answers();
    for response in responses {
        session.insert_turn_content(response);
    }
    if !cancelled {
        needs_send.insert(session_id);
    }
    handle_stream_end(session, session_id, host, needs_send, needs_compact);
    true
}

/// End the turn the backend just finished, unless Dave is still running its
/// tool calls: then the round holds the turn open, and
/// [`finish_tool_round`] ends it. Returns whether the turn ended here.
fn end_turn_unless_tools_run(
    session: &mut session::ChatSession,
    session_id: SessionId,
    host: &HostCtx<'_>,
    needs_send: &mut HashSet<SessionId>,
    needs_compact: &mut HashSet<SessionId>,
) -> bool {
    if let Some(round) = session.tool_round.as_mut() {
        round.end_turn();
        return false;
    }
    handle_stream_end(session, session_id, host, needs_send, needs_compact);
    true
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
    // Insert before queued user messages, keeping them trailing; this records
    // the row's position for the subagent's output and completion.
    session.insert_turn_content(Message::Subagent(subagent));
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
    let idx = session.turn_rows().subagent(task_id)?;
    let agentic = session.agentic.as_mut()?;
    let Some(Message::Subagent(info)) = session.chat.get(idx) else {
        return None;
    };
    let session_id = agentic.event_session_id().to_string();
    match session_events::build_subagent_event(info, &session_id, &mut agentic.live_threading, sk) {
        Ok(event) => {
            record_published(
                agentic,
                std::slice::from_ref(&event),
                Ingest::Pns { ndb, sk },
            );
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
/// and shows and publishes the compaction info.
fn handle_compaction_complete(
    session: &mut session::ChatSession,
    session_id: SessionId,
    info: messages::CompactionInfo,
    host: &HostCtx<'_>,
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
    let complete = Message::CompactionComplete(info);
    session.push_published(complete, Place::Turn, host.ndb, host.sk());
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
    host: &HostCtx<'_>,
    needs_send: &mut HashSet<SessionId>,
    needs_compact: &mut HashSet<SessionId>,
) {
    let (ndb, secret_key) = (host.ndb, &host.secret_key);
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
        let error = Message::Error(NO_RESPONSE_ERROR.to_string());
        session.push_published(error, Place::Turn, ndb, secret_key.as_ref());
    }

    // The turn is over: count its time and mark where it ended, before the
    // dispatch state resets (the turn's last row is found from it) and before
    // a queued message is redispatched as the next turn.
    if let Some(span) = session.end_turn(session::now_unix()) {
        publish_turn_end(session, span, ndb, secret_key);
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

/// Publish a turn's end as a [`TURN_END_ROLE`](session_events::TURN_END_ROLE)
/// marker (locally ingested; the host fans it out), so observers and a restart
/// fold the same span onto the turn's last row.
fn publish_turn_end(
    session: &mut session::ChatSession,
    span: session::TurnSpan,
    ndb: &nostrdb::Ndb,
    secret_key: &Option<[u8; 32]>,
) {
    let Some(sk) = secret_key else {
        return;
    };
    ingest_live_event(
        session,
        ndb,
        sk,
        "",
        session_events::TURN_END_ROLE,
        LiveEventTags {
            turn_start: Some(span.start),
            ..Default::default()
        },
    );
}

/// The error a turn shows when the backend ended it without producing anything.
const NO_RESPONSE_ERROR: &str = "No response from backend";

/// Ask session `sid`'s backend to compact it, for a backend with no slash
/// commands ([`Dave::compact_active`]). Does nothing when the backend has no
/// live session to compact.
fn dispatch_compact_to_backend(
    session_manager: &mut session::SessionManager,
    backends: &HashMap<BackendType, Box<dyn AiBackend>>,
    sid: SessionId,
    waker: &Waker,
) {
    let Some(session) = session_manager.get_mut(sid) else {
        return;
    };
    let session_id = format!("dave-session-{}", sid);
    let backend = get_backend(backends, session.backend_type);
    let Some(rx) = backend.compact_session(session_id.clone(), waker.clone()) else {
        tracing::warn!("Compact failed: no backend session for {}", session_id);
        return;
    };
    tracing::info!("Compact dispatched for session {}", session_id);
    session.incoming_tokens = Some(rx);
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

/// What starting a turn takes from outside the session: the host its dispatch
/// markers go to (and whose account and waker the backend is handed), and
/// what else the backend gets alongside the chat.
pub(crate) struct DispatchCtx<'a> {
    pub(crate) host: &'a HostCtx<'a>,
    pub(crate) tools: Arc<HashMap<String, Tool>>,
    /// The configured extra environment for subprocess backends; the
    /// session's agentium identity is layered over it
    /// ([`session_env`](crate::backend::shared::session_env)).
    pub(crate) session_env: &'a BTreeMap<String, String>,
    /// The account's backend profiles, for a session that runs under one.
    pub(crate) profiles: &'a BackendProfiles,
}

/// Start a turn: hand `session`'s trailing user message(s) to `backend`.
///
/// Marks them dispatched and publishes their dispatch markers
/// ([`record_dispatch`]), so every fold places a queued message where the
/// host dispatched it, then starts the backend's stream over the chat.
///
/// A session that runs under a backend profile gets the profile's env
/// ([`session_env`](crate::backend::shared::session_env)) and, when it hasn't
/// picked a model, the profile's default. When this host has no profile by
/// that name the turn fails instead ([`fail_turn`]): the backend is never
/// called, since running on the plain backend would send the work to the
/// wrong account.
///
/// [`Dave::send_user_message_for`] picks the backend and calls this; the
/// convergence harness dispatches through it too, so a dispatch that stopped
/// publishing its markers fails the queued scenarios there.
pub(crate) fn dispatch_turn(
    session: &mut session::ChatSession,
    backend: &dyn AiBackend,
    ctx: &DispatchCtx<'_>,
) {
    record_dispatch(session, ctx.host.ndb, ctx.host.sk());

    let session_id = format!("dave-session-{}", session.id);
    let profile = match session.details.profile.as_deref() {
        None => None,
        Some(slug) => match ctx.profiles.get(slug) {
            Some(profile) => Some(profile),
            None => {
                let error = missing_profile_error(slug, &session.details.hostname);
                tracing::warn!("session {}: backend profile {slug:?} missing", session.id);
                fail_turn(session, backend, session_id, error, ctx.host.waker);
                return;
            }
        },
    };
    // The stable kind-31988 d-tag (UUID), distinct from the ephemeral
    // `dave-session-{n}` routing key above, goes into the session env as the
    // agentium identity so an in-session agent reads its OWN ref. Only
    // agentic sessions have one.
    let agentium_session_id = session.agentic.as_ref().map(|a| a.event_session_id());
    let session_env =
        crate::backend::shared::session_env(agentium_session_id, ctx.session_env, profile);
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
    // A model picked for the session wins; otherwise a profile starts on its
    // own default, since the backend's default names a model its endpoint
    // may not serve.
    let model_name = session
        .details
        .resolve_model()
        .or_else(|| profile.and_then(|p| p.default_model.clone()));
    // `rx` is `None` for persistent-stream backends on subsequent turns — the
    // session already owns a long-lived channel we must keep, so only replace
    // `incoming_tokens` when a new receiver was minted.
    let stream = backend.stream_request(
        messages,
        ctx.tools.clone(),
        model_name,
        ctx.host.user_id(),
        session_id,
        session_env,
        cwd,
        resume_session_id,
        permission_mode,
        ctx.host.waker.clone(),
    );
    if let Some(rx) = stream.rx {
        session.incoming_tokens = Some(rx);
    }
    session.task_handle = stream.task;
}

/// End the turn just dispatched on `session` with `error`, without the backend.
///
/// The error arrives the way a backend's own failure does: one
/// [`DaveApiResponse::Failed`] on a channel whose sender is already dropped,
/// so the next poll shows it in the chat, publishes it, and runs the usual
/// stream end. A session the backend still has a live CLI for (a persistent
/// channel from an earlier turn) has it shut down first, since the turn's
/// receiver replaces that channel.
fn fail_turn(
    session: &mut session::ChatSession,
    backend: &dyn AiBackend,
    session_id: String,
    error: String,
    waker: &Waker,
) {
    if session.incoming_tokens.is_some() {
        backend.cleanup_session(session_id);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(DaveApiResponse::Failed(error));
    session.incoming_tokens = Some(rx);
    session.task_handle = None;
    waker.wake();
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

    /// An agentic session and the host it runs on (its ndb, signing as
    /// [`test_secret_key`]), for driving responses through [`apply_response`].
    struct Fixture {
        session: session::ChatSession,
        ndb: Ndb,
        waker: Waker,
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
                waker: Waker::noop(),
                _dir: dir,
            }
        }

        /// The host the session runs on.
        fn host<'a>(ndb: &'a Ndb, waker: &'a Waker) -> HostCtx<'a> {
            HostCtx::signing(ndb, &test_secret_key(), waker)
        }

        fn apply(&mut self, res: DaveApiResponse) {
            let host = Self::host(&self.ndb, &self.waker);
            let ctx = ApplyCtx {
                host: &host,
                persistent_stream: true,
            };
            apply_response(&mut self.session, 1, res, &ctx);
        }

        fn stream_end(&mut self) {
            self.stream_end_into(&mut HashSet::new());
        }

        /// End the turn, collecting a redispatch into `needs_send`.
        fn stream_end_into(&mut self, needs_send: &mut HashSet<SessionId>) {
            let host = Self::host(&self.ndb, &self.waker);
            handle_stream_end(&mut self.session, 1, &host, needs_send, &mut HashSet::new());
        }

        /// How many notes the session has published so far.
        fn published(&self) -> usize {
            self.session.agentic.as_ref().unwrap().ledger.own().len()
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
            description: None,
            is_error: false,
        }
    }

    /// The anonymous user id as Dave derived it from the selected account's
    /// keypair before [`HostCtx`], kept here to pin the id providers see.
    fn calculate_user_id(keypair: KeypairUnowned) -> String {
        use sha2::{Digest, Sha256};
        let key_input = keypair
            .secret_key
            .map(|sk| sk.as_secret_bytes())
            .unwrap_or(keypair.pubkey.bytes());
        let hex_key = hex::encode(key_input);
        let input = format!("{hex_key}notedeck_dave_user_id");
        hex::encode(Sha256::digest(input))
    }

    /// A host hands backends the same user id the account's keypair always
    /// gave: the secret key's hash, or the pubkey's for a watch-only account.
    #[test]
    fn host_user_id_is_the_keypairs() {
        let f = Fixture::new();
        let full = nostrdb_net::FullKeypair::from_secret_bytes(&test_secret_key())
            .unwrap()
            .to_keypair();
        let watch_only = nostrdb_net::Keypair::only_pubkey(full.pubkey);

        for keypair in [&full, &watch_only] {
            let host = HostCtx::new(&f.ndb, keypair.into(), &f.waker);
            assert_eq!(host.user_id(), calculate_user_id(keypair.into()));
        }
        assert_ne!(
            HostCtx::new(&f.ndb, (&full).into(), &f.waker).user_id(),
            HostCtx::new(&f.ndb, (&watch_only).into(), &f.waker).user_id(),
            "a watch-only account hashes its pubkey, not a key it lacks"
        );
    }

    /// A turn that writes no text publishes no assistant note — before the
    /// segment flush, stream end re-published the previous turn's text.
    #[test]
    fn textless_turn_publishes_no_assistant_note() {
        let mut f = Fixture::new();
        f.session.mark_dispatched();
        f.apply(DaveApiResponse::Token("first answer".to_string()));
        f.stream_end();
        assert_eq!(
            f.published(),
            2,
            "the text turn publishes its segment and its turn-end marker"
        );

        f.session.mark_dispatched();
        f.apply(DaveApiResponse::ToolResult(executed(None, None)));
        f.stream_end();
        assert_eq!(
            f.published(),
            4,
            "the tool-only turn publishes its tool result and its marker, and no text"
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
            agent_id: None,
            transcript: Default::default(),
        }));
        f.apply(DaveApiResponse::ToolRunning(crate::messages::RunningTool {
            tool_use_id: "t1".to_string(),
            tool_name: "Grep".to_string(),
            summary: "loader".to_string(),
            description: None,
        }));
        f.apply(DaveApiResponse::Token("while ".to_string()));
        f.apply(DaveApiResponse::ToolResult(executed(None, Some("s1"))));
        f.apply(DaveApiResponse::Token("that runs, ".to_string()));
        f.apply(DaveApiResponse::ToolResult(executed(Some("t1"), None)));
        f.apply(DaveApiResponse::Token("one block".to_string()));
        f.stream_end();

        assert_eq!(f.assistant_texts(), ["while that runs, one block"]);
    }

    /// Forwarded subagent text fills its row's transcript and adds no chat
    /// row, and the agent id lands on the row; neither publishes until the
    /// completion does.
    #[test]
    fn subagent_text_and_agent_id_land_on_its_row() {
        use crate::messages::TranscriptKind;
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
            background: false,
            agent_id: None,
            transcript: Default::default(),
        }));
        let after_spawn = f.published();
        f.apply(DaveApiResponse::SubagentText {
            task_id: "s1".to_string(),
            kind: TranscriptKind::Text,
            text: "the loader lives in session_loader.rs".to_string(),
        });
        f.apply(DaveApiResponse::SubagentIdentified {
            task_id: "s1".to_string(),
            agent_id: "a1".to_string(),
        });
        assert_eq!(
            f.published(),
            after_spawn,
            "text and id wait for completion"
        );
        f.apply(DaveApiResponse::SubagentCompleted {
            task_id: "s1".to_string(),
            result: "done".to_string(),
        });
        assert_eq!(f.published(), after_spawn + 1);

        assert!(f.assistant_texts().is_empty(), "no subagent prose in chat");
        let Some(Message::Subagent(row)) = f.session.chat.last() else {
            panic!("the subagent row is the last row");
        };
        assert_eq!(row.agent_id.as_deref(), Some("a1"));
        let texts: Vec<_> = row.transcript.runs().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["the loader lives in session_loader.rs"]);
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
        f.stream_end_into(&mut needs_send);

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
        assert_eq!(
            f.published(),
            2,
            "the error is published, and the turn-end marker"
        );
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
                agent_id: None,
                transcript: Default::default(),
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

        let seen = &session.agentic.as_ref().unwrap().ledger.own();
        assert!(seen.contains(&spawned.note_id) && seen.contains(&completed.note_id));

        assert!(
            publish_subagent(&mut session, "unknown-task", &sk, &ndb).is_none(),
            "a task id with no row publishes nothing"
        );
    }

    /// A local stream's usage lands in the inspector's history, the one
    /// record the status bar's meter and the Usage card both read.
    #[test]
    fn stream_usage_lands_in_the_inspector() {
        use crate::messages::UsageInfo;

        let mut f = Fixture::new();
        f.apply(DaveApiResponse::UsageUpdate(UsageInfo {
            input_tokens: 1_500,
            cache_creation_input_tokens: 9_000,
            cache_read_input_tokens: 24_000,
            output_tokens: 1_100,
            ..Default::default()
        }));
        f.apply(DaveApiResponse::QueryComplete(UsageInfo {
            cost_usd: Some(1.34),
            num_turns: 2,
            ..Default::default()
        }));

        let usage = &f.session.inspector.usage;
        assert_eq!(usage.current().map(|t| t.context()), Some(34_500));
        assert_eq!(usage.total_cost(), Some(1.34));
        assert_eq!(usage.turns_recorded(), 1);
    }

    /// A session whose turn launched a background task and ended is parked,
    /// not done: Waiting. The task's notification wakes it into a turn the
    /// backend began itself, which shows Working, and once that turn ends
    /// with nothing left outstanding the session is Done.
    #[test]
    fn background_task_parks_the_session_waiting_until_it_wakes() {
        use crate::agent_status::AgentStatus;
        use crate::messages::{UsageInfo, WaitingOn};

        let mut f = Fixture::new();
        f.session.chat.push(Message::User("watch the card".into()));
        f.session.mark_dispatched();
        f.apply(DaveApiResponse::Token("watching in the background".into()));
        f.apply(DaveApiResponse::WaitingOn(WaitingOn {
            background_tasks: 1,
            ..Default::default()
        }));
        f.apply(DaveApiResponse::QueryComplete(UsageInfo::default()));
        f.stream_end();
        f.session.update_status();
        assert_eq!(f.session.status(), AgentStatus::Waiting);
        assert_eq!(
            crate::focus_queue::FocusPriority::from_status(f.session.status()),
            None,
            "Waiting neither raises the indicator nor steals focus"
        );

        // The task finishes: its notification ends the wait and wakes the
        // session into a turn no query began.
        f.apply(DaveApiResponse::WaitingOn(WaitingOn::default()));
        f.apply(DaveApiResponse::WakeupTurnStarted);
        f.session.update_status();
        assert_eq!(f.session.status(), AgentStatus::Working);

        f.apply(DaveApiResponse::Token("the card moved".into()));
        f.apply(DaveApiResponse::QueryComplete(UsageInfo::default()));
        f.stream_end();
        f.session.update_status();
        assert_eq!(f.session.status(), AgentStatus::Done);
    }

    /// A turn counts its time from dispatch to its end once, and publishes one
    /// turn-end marker, however many times its stream ends.
    #[test]
    fn a_turn_counts_its_time_once() {
        use crate::messages::UsageInfo;

        let mut f = Fixture::new();
        f.session.chat.push(Message::User("go".into()));
        f.session.mark_dispatched();
        let start = session::now_unix() - 90;
        // The clock, moved back rather than slept past: the turn started 90s ago.
        f.session.turn_started_at = Some(start);
        f.apply(DaveApiResponse::Token("done".into()));
        f.apply(DaveApiResponse::QueryComplete(UsageInfo::default()));
        let before = f.published();
        f.stream_end();

        assert_eq!(f.session.turn_started_at, None);
        let worked = f.session.walltime().worked_secs;
        assert!((90..=91).contains(&worked), "worked {worked}s");
        assert_eq!(f.session.turns().len(), 1);
        assert_eq!(f.session.turns()[0].start, start);
        assert_eq!(f.session.turns()[0].last_row, 1, "on the reply");
        // The reply's last segment and the turn-end marker.
        assert_eq!(f.published(), before + 2);

        // The channel then drops: the same turn ends again.
        let before = f.published();
        f.stream_end();
        assert_eq!(f.session.walltime().worked_secs, worked, "counted once");
        assert_eq!(f.session.turns().len(), 1);
        assert_eq!(f.published(), before, "no second marker");
    }

    /// A wake-up turn starts its own clock, and ends once.
    #[test]
    fn a_wakeup_turn_counts_once() {
        use crate::messages::UsageInfo;

        let mut f = Fixture::new();
        f.apply(DaveApiResponse::WakeupTurnStarted);
        let start = f
            .session
            .turn_started_at
            .expect("the wake-up started a turn");
        f.apply(DaveApiResponse::WakeupTurnStarted);
        assert_eq!(f.session.turn_started_at, Some(start), "the same turn");
        f.session.turn_started_at = Some(start - 30);
        f.apply(DaveApiResponse::Token("the card moved".into()));
        f.apply(DaveApiResponse::QueryComplete(UsageInfo::default()));
        f.stream_end();
        f.stream_end();

        let worked = f.session.walltime().worked_secs;
        assert!((30..=31).contains(&worked), "worked {worked}s");
        assert_eq!(f.session.turns().len(), 1);
    }

    /// A finished turn's span reaches the wire: a fold of the session's notes,
    /// as a restart or an observer makes, puts it on the row it ended on.
    #[tokio::test]
    async fn a_fold_restores_the_turn_spans() {
        use crate::messages::UsageInfo;

        let mut f = Fixture::new();
        let sk = test_secret_key();
        assert!(f.ndb.add_key(&sk), "ndb must accept the PNS key");
        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
            .build();
        let sub = f.ndb.subscribe(std::slice::from_ref(&filter)).unwrap();

        crate::publish::record_user_message(&mut f.session, &f.ndb, Some(&sk), "go".into(), vec![]);
        f.session.mark_dispatched();
        f.session.turn_started_at = Some(session::now_unix() - 60);
        f.apply(DaveApiResponse::Token("done".into()));
        f.apply(DaveApiResponse::QueryComplete(UsageInfo::default()));
        f.stream_end();
        // The user message, the reply and the turn-end marker.
        f.ndb
            .wait_for_all_notes_within(sub, 3, std::time::Duration::from_secs(5))
            .await
            .unwrap();

        let local = f.session.turns()[0];
        let author = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        let txn = Transaction::new(&f.ndb).unwrap();
        let claude_sid = f.session.agentic.as_ref().unwrap().event_session_id();
        let loaded = agentium_core::session_loader::load_session_messages_for_author(
            &f.ndb, &txn, &author, claude_sid,
        );
        assert_eq!(loaded.messages.len(), 2);
        let [folded] = loaded.turns[..] else {
            panic!("one turn, got {:?}", loaded.turns);
        };
        assert_eq!(folded.last_row, local.last_row);
        assert_eq!(folded.start, local.start);
        // The marker's `created_at` is stamped as it is built, a moment after.
        assert!((local.end..=local.end + 1).contains(&folded.end));
    }

    /// A scheduled wake-up holds the session Waiting only until it is due.
    #[test]
    fn a_due_wakeup_no_longer_holds_the_session() {
        use crate::agent_status::AgentStatus;
        use crate::messages::{UsageInfo, WaitingOn};

        let mut f = Fixture::new();
        f.session
            .chat
            .push(Message::User("check back later".into()));
        f.session.mark_dispatched();
        f.apply(DaveApiResponse::Token("scheduled".into()));
        f.apply(DaveApiResponse::WaitingOn(WaitingOn {
            wakeup_at: Some(session::now_unix() + 600),
            ..Default::default()
        }));
        f.apply(DaveApiResponse::QueryComplete(UsageInfo::default()));
        f.stream_end();
        f.session.update_status();
        assert_eq!(f.session.status(), AgentStatus::Waiting);

        f.session.waiting_on.wakeup_at = Some(session::now_unix().saturating_sub(1));
        f.session.update_status();
        assert_eq!(f.session.status(), AgentStatus::Done);
    }

    /// A backend that records what the status bar's COMPACT asks of it: the
    /// last user message of each turn it is handed, and how many times it
    /// was asked to compact directly.
    struct CompactProbe {
        turns: std::sync::mpsc::Sender<String>,
        compacts: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl AiBackend for CompactProbe {
        fn stream_request(
            &self,
            messages: Vec<Message>,
            _tools: Arc<HashMap<String, Tool>>,
            _model: Option<String>,
            _user_id: String,
            _session_id: String,
            _session_env: BTreeMap<String, String>,
            _cwd: Option<std::path::PathBuf>,
            _resume_session_id: Option<String>,
            _permission_mode: claude_agent_sdk_rs::PermissionMode,
            _waker: Waker,
        ) -> crate::backend::ResponseStream {
            let last_user = messages.iter().rev().find_map(|m| match m {
                Message::User(u) => Some(u.text.clone()),
                _ => None,
            });
            let _ = self.turns.send(last_user.unwrap_or_default());
            crate::backend::ResponseStream {
                rx: None,
                task: None,
            }
        }

        fn cleanup_session(&self, _session_id: String) {}

        fn interrupt_session(&self, _session_id: String, _waker: Waker) {}

        fn set_permission_mode(
            &self,
            _session_id: String,
            _mode: claude_agent_sdk_rs::PermissionMode,
            _waker: Waker,
        ) {
        }

        fn compact_session(
            &self,
            _session_id: String,
            _waker: Waker,
        ) -> Option<std::sync::mpsc::Receiver<DaveApiResponse>> {
            self.compacts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            None
        }
    }

    /// What one COMPACT press did: the turns the backend was handed and the
    /// direct compacts it was asked for.
    struct CompactRun {
        turns: Vec<String>,
        compacts: usize,
        /// The session's last chat row, when it is a user message.
        last_user: Option<UserMessageRow>,
    }

    /// The user row a COMPACT press left at the end of the chat.
    struct UserMessageRow {
        text: String,
        published: bool,
    }

    /// Press COMPACT on one fresh session on `backend`, made remote when
    /// `remote`, with a [`CompactProbe`] standing in for that backend.
    fn press_compact(backend: BackendType, remote: bool) -> CompactRun {
        let mut test = crate::tests::TestDave::new();
        let [sid] = test.add_sessions(["/tmp/compact"], backend);
        if remote {
            test.dave.session_manager.get_mut(sid).unwrap().source = session::SessionSource::Remote;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let compacts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        test.dave.backends.insert(
            backend,
            Box::new(CompactProbe {
                turns: tx,
                compacts: compacts.clone(),
            }),
        );
        let parts = test.parts();
        parts.dave.compact_active(&parts.host);
        let last_user = match test.dave.session_manager.get(sid).unwrap().chat.last() {
            Some(Message::User(u)) => Some(UserMessageRow {
                text: u.text.clone(),
                published: u.note_id.is_some(),
            }),
            _ => None,
        };
        CompactRun {
            turns: rx.try_iter().collect(),
            compacts: compacts.load(std::sync::atomic::Ordering::SeqCst),
            last_user,
        }
    }

    /// A local Claude session with no live CLI (restored, no turn sent in
    /// this process) is compacted by a `/compact` turn, which starts one;
    /// asking the backend directly would find no session and do nothing.
    #[test]
    fn compact_on_a_claude_session_with_no_live_cli_sends_a_compact_turn() {
        let run = press_compact(BackendType::Claude, false);
        assert_eq!(run.turns, vec![COMPACT_COMMAND.to_owned()]);
        assert_eq!(run.compacts, 0);
        let row = run.last_user.expect("the compact turn shows in the chat");
        assert_eq!(row.text, COMPACT_COMMAND);
        assert!(row.published, "the turn is published for observers");
    }

    /// A remote session has no local backend: COMPACT publishes `/compact`
    /// as a user message for the host to run, and touches no backend here.
    #[test]
    fn compact_on_a_remote_session_publishes_a_compact_message() {
        let run = press_compact(BackendType::Claude, true);
        assert!(run.turns.is_empty(), "no local turn: {:?}", run.turns);
        assert_eq!(run.compacts, 0);
        let row = run
            .last_user
            .expect("the compact message shows in the chat");
        assert_eq!(row.text, COMPACT_COMMAND);
        assert!(row.published, "the host only sees a published message");
    }

    /// Codex has no slash commands, so a local Codex session asks its
    /// backend to compact rather than sending `/compact` to the model.
    #[test]
    fn compact_on_a_codex_session_asks_the_backend() {
        let run = press_compact(BackendType::Codex, false);
        assert!(
            run.turns.is_empty(),
            "no turn to the model: {:?}",
            run.turns
        );
        assert_eq!(run.compacts, 1);
        assert!(run.last_user.is_none());
    }

    /// What one probed turn handed the backend.
    struct ProbedTurn {
        model: Option<String>,
        env: BTreeMap<String, String>,
    }

    /// A backend that records the model and env of each turn it is handed.
    struct TurnProbe {
        turns: std::sync::mpsc::Sender<ProbedTurn>,
    }

    impl AiBackend for TurnProbe {
        fn stream_request(
            &self,
            _messages: Vec<Message>,
            _tools: Arc<HashMap<String, Tool>>,
            model: Option<String>,
            _user_id: String,
            _session_id: String,
            session_env: BTreeMap<String, String>,
            _cwd: Option<std::path::PathBuf>,
            _resume_session_id: Option<String>,
            _permission_mode: claude_agent_sdk_rs::PermissionMode,
            _waker: Waker,
        ) -> crate::backend::ResponseStream {
            let _ = self.turns.send(ProbedTurn {
                model,
                env: session_env,
            });
            crate::backend::ResponseStream {
                rx: None,
                task: None,
            }
        }

        fn cleanup_session(&self, _session_id: String) {}

        fn interrupt_session(&self, _session_id: String, _waker: Waker) {}

        fn set_permission_mode(
            &self,
            _session_id: String,
            _mode: claude_agent_sdk_rs::PermissionMode,
            _waker: Waker,
        ) {
        }
    }

    /// What sending one message on a Claude session under profile `hydra`
    /// did, with `profiles` as the account's profiles.
    struct ProfileRun {
        turns: Vec<ProbedTurn>,
        chat: Vec<Message>,
        status: crate::agent_status::AgentStatus,
    }

    fn send_under_hydra(profiles: crate::backend_profiles::BackendProfiles) -> ProfileRun {
        let mut test = crate::tests::TestDave::new();
        let [sid] = test.add_sessions(["/tmp/profile"], BackendType::Claude);
        {
            let session = test.dave.session_manager.get_mut(sid).unwrap();
            session.details.profile = Some("hydra".to_string());
            session.details.hostname = "monad".to_string();
        }
        test.dave.backend_profiles = profiles;
        let (tx, rx) = std::sync::mpsc::channel();
        test.dave
            .backends
            .insert(BackendType::Claude, Box::new(TurnProbe { turns: tx }));

        let parts = test.parts();
        assert!(parts.dave.add_user_message_for_session(
            sid,
            &parts.host,
            "ship it".to_string(),
            Vec::new()
        ));
        parts.dave.send_user_message_for(sid, &parts.host);
        parts.dave.process_events(&parts.host);

        let session = test.dave.session_manager.get_mut(sid).unwrap();
        session.update_status();
        ProfileRun {
            turns: rx.try_iter().collect(),
            chat: session.chat.clone(),
            status: session.status(),
        }
    }

    /// A session under a profile this host doesn't have fails its turn with
    /// an error naming the profile, the host and the fix, and never reaches
    /// the backend: running it on the plain backend would send the work to
    /// the wrong account.
    #[test]
    fn a_missing_profile_fails_the_turn_without_the_backend() {
        let run = send_under_hydra(Default::default());
        assert!(
            run.turns.is_empty(),
            "the backend must not be called without its profile"
        );
        let error = run
            .chat
            .iter()
            .find_map(|m| match m {
                Message::Error(e) => Some(e.as_str()),
                _ => None,
            })
            .expect("the turn shows an error");
        assert!(error.contains("`hydra`"), "error: {error}");
        assert!(error.contains("monad"), "error: {error}");
        assert!(
            error.contains("agentium profile set hydra"),
            "error: {error}"
        );
        assert_ne!(
            run.status,
            crate::agent_status::AgentStatus::Working,
            "the failed turn is over"
        );
    }

    /// A session under a profile this host has runs with the profile's env
    /// and, with no model picked, on the profile's default model.
    #[test]
    fn a_profile_session_runs_with_its_env_and_default_model() {
        let mut hydra = agentium_core::config::BackendProfile::new("hydra", "Hydra");
        hydra.env.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            "https://llm.hydra.host".to_string(),
        );
        hydra.default_model = Some("glm-5.3-flash".to_string());
        let run = send_under_hydra(crate::backend_profiles::BackendProfiles::from_profiles([
            hydra,
        ]));

        let [turn] = run.turns.as_slice() else {
            panic!("expected one turn, got {}", run.turns.len());
        };
        assert_eq!(turn.model.as_deref(), Some("glm-5.3-flash"));
        assert_eq!(turn.env["ANTHROPIC_BASE_URL"], "https://llm.hydra.host");
        assert!(turn.env.contains_key("AGENTIUM_SESSION"));
        assert!(!run.chat.iter().any(|m| matches!(m, Message::Error(_))));
    }

    /// Tool rounds run through [`Dave::process_events`]: the turn a round
    /// holds open, its answers' order, and Stop.
    mod tool_rounds {
        use super::*;
        use crate::agent_status::AgentStatus;
        use crate::tool_round::{ToolJob, CANCELLED};
        use std::sync::mpsc;
        use std::sync::Mutex;
        use std::time::{Duration, Instant};

        /// A backend whose next turn streams what the test sends, ending
        /// when the test drops its sender.
        struct HandedStream {
            next: Mutex<Option<mpsc::Receiver<DaveApiResponse>>>,
        }

        impl AiBackend for HandedStream {
            fn stream_request(
                &self,
                _messages: Vec<Message>,
                _tools: Arc<HashMap<String, Tool>>,
                _model: Option<String>,
                _user_id: String,
                _session_id: String,
                _session_env: BTreeMap<String, String>,
                _cwd: Option<std::path::PathBuf>,
                _resume_session_id: Option<String>,
                _permission_mode: claude_agent_sdk_rs::PermissionMode,
                _waker: Waker,
            ) -> crate::backend::ResponseStream {
                crate::backend::ResponseStream {
                    rx: self.next.lock().unwrap().take(),
                    task: None,
                }
            }

            fn cleanup_session(&self, _session_id: String) {}

            fn interrupt_session(&self, _session_id: String, _waker: Waker) {}

            fn set_permission_mode(
                &self,
                _session_id: String,
                _mode: claude_agent_sdk_rs::PermissionMode,
                _waker: Waker,
            ) {
            }
        }

        /// A Dave whose one OpenAI session `sid` has sent "go" and is
        /// streaming the turn `stream` feeds.
        struct MidTurn {
            test: crate::tests::TestDave,
            sid: SessionId,
            stream: mpsc::Sender<DaveApiResponse>,
        }

        fn mid_turn() -> MidTurn {
            let mut test = crate::tests::TestDave::new();
            let [sid] = test.add_sessions(["/tmp/tools"], BackendType::OpenAI);
            let (stream, rx) = mpsc::channel();
            test.dave.backends.insert(
                BackendType::OpenAI,
                Box::new(HandedStream {
                    next: Mutex::new(Some(rx)),
                }),
            );
            let parts = test.parts();
            assert!(parts.dave.add_user_message_for_session(
                sid,
                &parts.host,
                "go".into(),
                Vec::new()
            ));
            parts.dave.send_user_message_for(sid, &parts.host);
            MidTurn { test, sid, stream }
        }

        fn invalid(id: &str) -> ToolCall {
            ToolCall::invalid(id.into(), Some("nope".into()), None, format!("bad {id}"))
        }

        /// Show the round's calls and hand the session a round of `calls`
        /// started by hand, as `handle_tool_calls` would with a slow tool.
        fn hand_round(test: &mut crate::tests::TestDave, sid: SessionId, calls: Vec<RoundCall>) {
            let session = test.dave.session_manager.get_mut(sid).unwrap();
            let shown = calls.iter().map(|c| invalid(&c.id)).collect();
            session.insert_turn_content(Message::ToolCalls(shown));
            session.tool_round = Some(ToolRound::start(calls, &Waker::noop()));
        }

        /// The chat's rows past the user's "go", named.
        fn rows(test: &mut crate::tests::TestDave, sid: SessionId) -> Vec<String> {
            let session = test.dave.session_manager.get(sid).unwrap();
            session
                .chat
                .iter()
                .skip(1)
                .map(|m| match m {
                    Message::ToolCalls(calls) => format!(
                        "calls[{}]",
                        calls.iter().map(|c| c.id()).collect::<Vec<_>>().join(",")
                    ),
                    Message::ToolResponse(r) => match r.responses() {
                        ToolResponses::Error(e) => format!("{}: {e}", r.id()),
                        other => format!("{}: {other:?}", r.id()),
                    },
                    Message::User(u) => format!("user: {}", u.text),
                    other => format!("{other:?}"),
                })
                .collect()
        }

        fn status(test: &mut crate::tests::TestDave, sid: SessionId) -> AgentStatus {
            let session = test.dave.session_manager.get_mut(sid).unwrap();
            session.update_status();
            session.status()
        }

        /// A call whose job waits until the test sends on `go`.
        struct Gated {
            call: RoundCall,
            go: mpsc::Sender<()>,
        }

        fn gated(id: &str) -> Gated {
            let (go, wait) = mpsc::channel::<()>();
            let job: ToolJob = Box::new(move |_| {
                wait.recv().map_err(|e| e.to_string())?;
                Ok(ToolResponses::PresentNotes(7))
            });
            let call = RoundCall {
                id: id.into(),
                started: StartedCall::Pending(job),
            };
            Gated { call, go }
        }

        /// Drain until `sid` asks to send, failing after a few seconds.
        fn drain_until_send(test: &mut crate::tests::TestDave, sid: SessionId) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let parts = test.parts();
                if parts
                    .dave
                    .process_events(&parts.host)
                    .needs_send
                    .contains(&sid)
                {
                    return;
                }
                assert!(Instant::now() < deadline, "the round never finished");
                std::thread::yield_now();
            }
        }

        /// A round of fast calls is answered, and the model asked to go on,
        /// on the drain its stream ends in: no frame of latency.
        #[test]
        fn a_fast_round_is_answered_on_the_drain_its_stream_ends() {
            let MidTurn {
                mut test,
                sid,
                stream,
            } = mid_turn();
            stream
                .send(DaveApiResponse::ToolCalls(vec![invalid("a"), invalid("b")]))
                .unwrap();
            drop(stream);

            let parts = test.parts();
            let result = parts.dave.process_events(&parts.host);
            assert!(
                result.needs_send.contains(&sid),
                "the model is asked to go on"
            );
            assert_eq!(rows(&mut test, sid), ["calls[a,b]", "a: bad a", "b: bad b"]);
            let session = test.dave.session_manager.get(sid).unwrap();
            assert!(session.tool_round.is_none());
            assert!(!session.is_dispatched(), "the turn ended");
        }

        /// A slow call holds the turn open past its stream's end: the
        /// session stays Working, a message sent meanwhile queues, and the
        /// answers land, in call order and ahead of that message, on the
        /// drain the slow call finishes in.
        #[test]
        fn a_slow_round_holds_the_turn_open_until_its_answers_land() {
            let MidTurn {
                mut test,
                sid,
                stream,
            } = mid_turn();
            stream
                .send(DaveApiResponse::Token("looking".into()))
                .unwrap();
            {
                let parts = test.parts();
                parts.dave.process_events(&parts.host);
            }
            let Gated { call: slow, go } = gated("slow");
            let fast = RoundCall {
                id: "fast".into(),
                started: StartedCall::Done(Box::new(ToolResponses::PresentNotes(1))),
            };
            hand_round(&mut test, sid, vec![slow, fast]);
            drop(stream);

            let parts = test.parts();
            let result = parts.dave.process_events(&parts.host);
            assert!(!result.needs_send.contains(&sid), "nothing to send yet");
            assert_eq!(status(&mut test, sid), AgentStatus::Working);
            let parts = test.parts();
            assert!(
                !parts.dave.add_user_message_for_session(
                    sid,
                    &parts.host,
                    "meanwhile".into(),
                    Vec::new()
                ),
                "a message sent while the tools run queues"
            );

            go.send(()).unwrap();
            drain_until_send(&mut test, sid);
            let rows = rows(&mut test, sid);
            assert_eq!(
                rows[rows.len() - 4..],
                [
                    "calls[slow,fast]",
                    "slow: PresentNotes(7)",
                    "fast: PresentNotes(1)",
                    "user: meanwhile"
                ]
            );
            assert!(test
                .dave
                .session_manager
                .get(sid)
                .unwrap()
                .tool_round
                .is_none());
        }

        /// Stop reaches a running tool: the round answers it cancelled, the
        /// turn ends, and the model isn't asked to go on.
        #[test]
        fn stop_cancels_a_running_round_and_ends_the_turn() {
            let MidTurn {
                mut test,
                sid,
                stream,
            } = mid_turn();
            let (saw_tx, saw_cancel) = mpsc::channel();
            // Held until the test is done, so the job's own answer can't
            // land before the drain and be kept.
            let (_hold, held) = mpsc::channel::<()>();
            let job: ToolJob = Box::new(move |cancel| {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !cancel.is_cancelled() && Instant::now() < deadline {
                    std::thread::yield_now();
                }
                let _ = saw_tx.send(cancel.is_cancelled());
                let _ = held.recv();
                Err("stopped".into())
            });
            let call = RoundCall {
                id: "build".into(),
                started: StartedCall::Pending(job),
            };
            hand_round(&mut test, sid, vec![call]);
            drop(stream);
            {
                let parts = test.parts();
                parts.dave.process_events(&parts.host);
            }
            assert_eq!(status(&mut test, sid), AgentStatus::Working);

            let backend = get_backend(&test.dave.backends, BackendType::OpenAI);
            crate::update::execute_interrupt(
                &test.dave.session_manager,
                backend,
                &egui::Context::default(),
            );
            let parts = test.parts();
            let result = parts.dave.process_events(&parts.host);
            assert!(
                !result.needs_send.contains(&sid),
                "a stopped round goes no further"
            );
            assert_eq!(
                rows(&mut test, sid),
                ["calls[build]".to_owned(), format!("build: {CANCELLED}")]
            );
            assert!(!test.dave.session_manager.get(sid).unwrap().is_dispatched());
            assert_ne!(status(&mut test, sid), AgentStatus::Working);
            assert_eq!(saw_cancel.recv_timeout(Duration::from_secs(5)), Ok(true));
        }
    }
}
