//! Dave's outbound private notes: the PNS ingest path every built event takes,
//! the kind-1988 live/user events, and the kind-31988 session-state publish
//! (whose one builder is [`session_state_snapshot`] +
//! [`SessionState::build_event`](session_loader::SessionState::build_event)),
//! plus the per-frame drains of queued permission, mode, interrupt and
//! deletion events.

use crate::update::PermissionPublish;
use crate::{
    embedded_engine, secret_key_bytes, session, session_events, session_loader, ChatSession, Dave,
    ImageAttachment, Message, UserMessage,
};
use nostrdb::Transaction;
use notedeck::AppContext;

/// Hand a freshly-built inner event to the host's private-note write path
/// ([`notedeck::write_private_note`]).
///
/// The host PNS-wraps the event into a kind-1080 envelope, ingests it locally
/// (nostrdb unwraps the inner event so dave can query it at once), and fans the
/// envelope out to the account's private relays. dave authors the inner event and
/// no longer wraps 1080 envelopes or runs a publish queue itself.
///
/// Returns whether the note was handed to nostrdb. One that wasn't never
/// comes back through the conversation subscription, so it must not be
/// recorded as waiting for that (see
/// [`AgenticSessionData::record_self_note`](session::AgenticSessionData::record_self_note)).
pub(crate) fn pns_ingest(ndb: &nostrdb::Ndb, event_json: &str, secret_key: &[u8; 32]) -> bool {
    match notedeck::write_private_note(ndb, secret_key, event_json) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("failed to write private note: {e}");
            false
        }
    }
}

/// Ingest a freshly-built event: PNS-wrap into local ndb and push to the
/// relay publish queue. Logs on success with `event_desc` and on failure.
/// Returns `true` if the event was queued successfully.
pub(crate) fn ingest_built_event(
    result: Result<session_events::BuiltEvent, session_events::EventBuildError>,
    event_desc: &str,
    ndb: &nostrdb::Ndb,
    sk: &[u8; 32],
) -> bool {
    match result {
        Ok(evt) => {
            tracing::info!("{}", event_desc);
            pns_ingest(ndb, &evt.note_json, sk);
            true
        }
        Err(e) => {
            tracing::error!("failed to build event ({}): {}", event_desc, e);
            false
        }
    }
}

/// The status, hostname, and monotonic `created_at` to stamp on a kind-31988
/// state-event publish for a dirty session.
struct SessionStatePublish {
    status: String,
    hostname: String,
    created_at: u64,
}

/// Decide what to publish for a dirty session's kind-31988 state event, or
/// `None` to skip it.
///
/// A **local** session publishes on any dirty change, using this machine's
/// hostname and its derived status. A **remote** session is owned by another
/// machine, so we publish one ONLY to persist a `custom_title` change we own (a
/// rename): status-only dirties are skipped (the owner is authoritative for
/// status) by comparing the in-memory title to the latest persisted one, and we
/// re-assert the last-known remote status + the owner's hostname so we don't
/// rewrite them. The owner's next publish overrides with a newer `created_at`
/// and adopts the title via its own ingest handler.
///
/// `created_at` is `max(now, latest_persisted + 1)` so the newest revision
/// always wins nostrdb's replaceable resolution and the receive-side guard. The
/// ndb lookups use a short read txn dropped before returning, since the caller
/// ingests afterward and a nested read/write txn deadlocks LMDB.
fn session_state_publish_params(
    session: &session::ChatSession,
    event_sid: &str,
    local_hostname: &str,
    ndb: &nostrdb::Ndb,
    account: &nostrdb_net::Pubkey,
) -> Option<SessionStatePublish> {
    let now = session_events::now_secs();

    if !session.is_remote() {
        let latest = Transaction::new(ndb)
            .ok()
            .and_then(|txn| session_loader::latest_state_created_at(ndb, &txn, account, event_sid));
        return Some(SessionStatePublish {
            status: session.status().as_str().to_string(),
            hostname: local_hostname.to_string(),
            created_at: session_events::next_state_created_at(now, latest.unwrap_or(0)),
        });
    }

    // Remote: only publish to persist a custom_title change (rename). The latest
    // persisted revision gives both the title to compare against and the
    // monotonic baseline.
    let persisted = Transaction::new(ndb).ok().and_then(|txn| {
        session_loader::latest_valid_session_for_author(ndb, &txn, account, event_sid)
    });
    if persisted.as_ref().and_then(|s| s.custom_title.clone()) == session.details.custom_title {
        return None;
    }
    let status = session
        .agentic
        .as_ref()
        .and_then(|a| a.remote_status.as_ref())
        .map(|s| s.as_str().to_string())
        .unwrap_or_else(|| "idle".to_string());
    Some(SessionStatePublish {
        status,
        hostname: session.details.hostname.clone(),
        created_at: session_events::next_state_created_at(
            now,
            persisted.as_ref().map(|s| s.created_at).unwrap_or(0),
        ),
    })
}

/// Snapshot a live [`ChatSession`] into the persistable [`SessionState`] — the
/// single, total mapping from an in-memory session to its kind-31988 tag set.
///
/// The publish-decision fields (`status`, `hostname`, `created_at`) are supplied
/// by the caller ([`session_state_publish_params`] for a live status publish, or
/// the delete path for a tombstone); every other field is read straight off the
/// session. Both publish paths funnel through here and [`SessionState::build_event`]
/// so no tag can be silently dropped by one site — the hazard that repeatedly
/// stranded fields (hostname, cli_session_id, custom_title, spawn_id) back when
/// each site hand-built its own event from a bespoke snapshot.
///
/// Returns `None` for a non-agentic session (nothing to persist).
pub(crate) fn session_state_snapshot(
    session: &session::ChatSession,
    status: String,
    hostname: String,
    created_at: u64,
) -> Option<session_loader::SessionState> {
    let agentic = session.agentic.as_ref()?;
    Some(session_loader::SessionState {
        claude_session_id: agentic.event_session_id().to_string(),
        title: session.details.title.clone(),
        custom_title: session.details.custom_title.clone(),
        cwd: agentic.cwd.to_string_lossy().to_string(),
        status,
        indicator: session.indicator.as_ref().map(|i| i.as_str().to_string()),
        hostname,
        home_dir: session.details.home_dir.clone(),
        backend: Some(session.backend_type.as_str().to_string()),
        permission_mode: Some(
            crate::session::permission_mode_to_str(agentic.permission_mode).to_string(),
        ),
        created_at,
        cli_session_id: agentic.cli_resume_id().map(|s| s.to_string()),
        spawn_id: session.spawn_id.clone(),
        project: session.details.project_slug.clone(),
        project_root: session
            .details
            .project_root
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
    })
}

/// Build and ingest a live kind-1988 event into ndb (via PNS wrapping).
///
/// Extracts cwd and session ID from the session's agentic data,
/// builds the event, PNS-wraps and ingests it, and returns the event.
///
/// A message too big for one wire note is ingested as several parts that a
/// reader joins back into one (see [`session_events::build_live_events`]);
/// the first part is returned, since its id is the message's.
pub(crate) fn ingest_live_event(
    session: &mut ChatSession,
    ndb: &nostrdb::Ndb,
    secret_key: &[u8; 32],
    content: &str,
    role: &str,
    tags: session_events::LiveEventTags<'_>,
) -> Option<session_events::BuiltEvent> {
    ingest_built_live_events(session, ndb, secret_key, |session_id, cwd, threading| {
        session_events::build_live_events(
            content, role, session_id, cwd, tags, threading, secret_key,
        )
    })
}

/// [`ingest_live_event`] for a note whose payload is cut to fit the wire:
/// `content(cap)` is its content with the payload cut to `cap` bytes, and the
/// note keeps the largest cap up to `max_cap` whose built event fits
/// [`MAX_WIRE_EVENT_BYTES`](session_events::MAX_WIRE_EVENT_BYTES).
///
/// `None`, with nothing ingested, when not even `content(0)` fits. A payload
/// that can't be cut passes a `max_cap` of 0, so it goes whole or not at all.
pub(crate) fn ingest_live_event_within(
    session: &mut ChatSession,
    ndb: &nostrdb::Ndb,
    secret_key: &[u8; 32],
    max_cap: usize,
    content: impl Fn(usize) -> String,
    role: &str,
    tags: session_events::LiveEventTags<'_>,
) -> Option<session_events::BuiltEvent> {
    ingest_built_live_events(session, ndb, secret_key, |session_id, cwd, threading| {
        session_events::build_live_event_within(
            max_cap, content, role, session_id, cwd, tags, threading, secret_key,
        )
        .map(|event| event.into_iter().collect())
    })
}

/// Ingest the live events `build` makes from the session's id, cwd and
/// threading, and record each as waiting to come back through ndb. Returns the
/// first: the note itself, or the first part of a split message. `build`
/// returns no events for a note it declined to build.
fn ingest_built_live_events(
    session: &mut ChatSession,
    ndb: &nostrdb::Ndb,
    secret_key: &[u8; 32],
    build: impl FnOnce(
        &str,
        Option<&str>,
        &mut session_events::ThreadingState,
    ) -> Result<Vec<session_events::BuiltEvent>, session_events::EventBuildError>,
) -> Option<session_events::BuiltEvent> {
    let agentic = session.agentic.as_mut()?;
    let session_id = agentic.event_session_id().to_string();
    let cwd = agentic.cwd.to_str();

    match build(&session_id, cwd, &mut agentic.live_threading) {
        Ok(events) => {
            for event in &events {
                if pns_ingest(ndb, &event.note_json, secret_key) {
                    agentic.record_self_note(event.note_id);
                }
            }
            events.into_iter().next()
        }
        Err(e) => {
            tracing::warn!("failed to build live event: {}", e);
            None
        }
    }
}

/// Build a *remote* session's user message through the engine — the
/// [`agentium_core::Engine::send_message`] equivalent for a controller sending
/// input to a remote host.
///
/// Mirrors [`ingest_live_event`]'s local-ingest + echo-back tracking (marking
/// the note seen so the relay round-trip isn't reprocessed) and returns the
/// event for dave's batched relay-publish queue, but derives conversation
/// threading from ndb (via the engine) rather than the session's in-memory
/// [`live_threading`](crate::session::AgenticSessionData::live_threading). The
/// local host-archival path stays on [`ingest_live_event`]; only remote
/// controller sends route here.
fn ingest_remote_user_message(
    session: &mut ChatSession,
    ndb: &nostrdb::Ndb,
    secret_key: &[u8; 32],
    text: &str,
    queued: bool,
) -> Option<session_events::BuiltEvent> {
    let agentic = session.agentic.as_mut()?;
    let session_id = agentic.event_session_id().to_string();
    let engine = embedded_engine(ndb, secret_key)?;
    match engine.prepare_message(&session_id, text, queued) {
        Ok(events) => {
            // The engine ingested them already.
            for event in &events {
                agentic.record_self_note(event.note_id);
            }
            // The first part's id is the message's.
            events.into_iter().next()
        }
        Err(e) => {
            tracing::warn!("failed to build remote user message: {:?}", e);
            None
        }
    }
}

/// Build the kind-1988 `user` event for a send, PNS-ingested locally and ready
/// for dave's relay-publish queue. A remote session is a controller send routed
/// through the engine ([`ingest_remote_user_message`]); a local session archives
/// the host's own turn via the in-memory threading path ([`ingest_live_event`]).
/// Shared by the interactive send ([`Dave::handle_user_send`]) and the
/// programmatic one ([`Dave::add_user_message_for_session`]).
///
/// `queued` tags the note as sent while a turn was in flight (see
/// [`record_dispatch`] and [`record_user_message`]).
pub(crate) fn build_user_send_event(
    session: &mut ChatSession,
    ndb: &nostrdb::Ndb,
    secret_key: &[u8; 32],
    text: &str,
    queued: bool,
) -> Option<session_events::BuiltEvent> {
    if session.is_remote() {
        ingest_remote_user_message(session, ndb, secret_key, text, queued)
    } else {
        ingest_live_event(
            session,
            ndb,
            secret_key,
            text,
            "user",
            session_events::LiveEventTags {
                queued,
                ..Default::default()
            },
        )
    }
}

/// Record a user-authored message on a session — the one path every user send
/// takes, interactive or programmatic.
///
/// Publishes its kind-1988 `user` note when a signing key is available (see
/// [`build_user_send_event`]), appends it to chat, and retitles the session.
/// Whether to dispatch it is the caller's call. A message sent while a turn is
/// in flight is queued: it waits at the end of the chat, and its note says so.
///
/// A local session knows whether it has dispatched a turn. A remote one only
/// knows the host's status, so a send to a session that is working or waiting
/// on input is queued. The host queues every remote message anyway and marks
/// where it dispatched it; the tag keeps this device and every observer from
/// showing it inside the reply until that marker arrives.
pub(crate) fn record_user_message(
    session: &mut ChatSession,
    ndb: &nostrdb::Ndb,
    secret_key: Option<&[u8; 32]>,
    text: String,
    images: Vec<ImageAttachment>,
) {
    let queued = if session.is_remote() {
        session_loader::status_in_turn(session.status().as_str())
    } else {
        session.is_dispatched()
    };
    let note_id = secret_key
        .and_then(|sk| build_user_send_event(session, ndb, sk, &text, queued))
        .map(|event| event.note_id);
    session.chat.push(Message::User(UserMessage {
        note_id,
        queued,
        ..UserMessage::new(text, images)
    }));
    session.update_title_from_last_message();
}

/// Hand a session's trailing user message(s) to the backend: mark them
/// dispatched, and publish a [`DISPATCHED_ROLE`] marker for the first queued
/// one and every one after it.
///
/// A queued message's note is stamped when it was typed, mid-turn, but the
/// host keeps it at the end of the chat until this moment. The marker records
/// where it really joined the conversation, so the fold over the notes puts it
/// in the same place (see `session_loader::display_order`). A message behind
/// it in the run needs a marker too even if it was not queued (sent once the
/// session was idle, with the queued one still waiting): by its own stamp it
/// would sort before the marker, above the message the host shows first.
///
/// Every dispatch goes through here, by way of
/// [`dispatch_turn`](crate::stream_events::dispatch_turn).
///
/// [`DISPATCHED_ROLE`]: session_events::DISPATCHED_ROLE
pub(crate) fn record_dispatch(
    session: &mut ChatSession,
    ndb: &nostrdb::Ndb,
    secret_key: Option<&[u8; 32]>,
) {
    session.mark_dispatched();
    let first = session.chat.len() - session.trailing_user_count();
    let mut marking = false;
    for idx in first..session.chat.len() {
        let Some(Message::User(user)) = session.chat.get_mut(idx) else {
            continue;
        };
        marking |= user.queued;
        if !marking {
            continue;
        }
        user.queued = false;
        let (Some(note_id), Some(sk)) = (user.note_id, secret_key) else {
            continue;
        };
        ingest_live_event(
            session,
            ndb,
            sk,
            "",
            session_events::DISPATCHED_ROLE,
            session_events::LiveEventTags {
                refs: Some(&note_id),
                ..Default::default()
            },
        );
    }
}

/// Publish a permission response the user gave (see
/// [`publish_permission_response`]) and, when it answers a session this host
/// runs, record the note on that session.
///
/// Only a local session records it: a remote issuer renders the response's
/// reply row from the note's echo, which a recorded note would skip.
pub(crate) fn publish_user_permission_response(
    sessions: &mut session::SessionManager,
    engine: &agentium_core::Engine,
    resp: &PermissionPublish,
) {
    let Some(event) = publish_permission_response(engine, resp) else {
        return;
    };
    let local = sessions.iter_mut().find(|s| {
        !s.is_remote()
            && s.agentic
                .as_ref()
                .is_some_and(|a| a.event_session_id() == resp.event_session_id)
    });
    if let Some(agentic) = local.and_then(|s| s.agentic.as_mut()) {
        agentic.record_self_note(event.note_id);
    }
}

/// Build and locally ingest one permission response through the engine (which
/// resolves the request's note id from ndb); the host's private-sync Session
/// fans it out. Returns the built event, or `None` after logging a failure.
pub(crate) fn publish_permission_response(
    engine: &agentium_core::Engine,
    resp: &PermissionPublish,
) -> Option<session_events::BuiltEvent> {
    match engine.prepare_permission_response(
        &resp.event_session_id,
        &resp.perm_id.to_string(),
        resp.allowed,
        resp.message.as_deref(),
        resp.cancel_turn,
    ) {
        Ok(event) => {
            tracing::info!(
                "queued permission response for {} ({})",
                resp.perm_id,
                if resp.allowed { "allow" } else { "deny" }
            );
            Some(event)
        }
        Err(e) => {
            tracing::error!(
                "failed to build permission response for {}: {:?}",
                resp.perm_id,
                e
            );
            None
        }
    }
}

/// Ingest an event this host built for one of its own sessions and record it
/// (see [`AgenticSessionData::record_self_note`]). Returns the note id, or
/// `None` after logging a build or ingest failure.
///
/// [`AgenticSessionData::record_self_note`]: session::AgenticSessionData::record_self_note
pub(crate) fn ingest_session_event(
    agentic: &mut session::AgenticSessionData,
    result: Result<session_events::BuiltEvent, session_events::EventBuildError>,
    event_desc: &str,
    ndb: &nostrdb::Ndb,
    sk: &[u8; 32],
) -> Option<[u8; 32]> {
    match result {
        Ok(evt) => {
            if !pns_ingest(ndb, &evt.note_json, sk) {
                return None;
            }
            agentic.record_self_note(evt.note_id);
            Some(evt.note_id)
        }
        Err(e) => {
            tracing::warn!("failed to build {}: {}", event_desc, e);
            None
        }
    }
}

/// Publish a local session's `permission_request` note and record its note id
/// under the request's perm id, which a later response links to.
pub(crate) fn publish_permission_request(
    session: &mut ChatSession,
    request: &crate::messages::PermissionRequest,
    ndb: &nostrdb::Ndb,
    sk: &[u8; 32],
) -> Option<[u8; 32]> {
    let agentic = session.agentic.as_mut()?;
    let sid = agentic.event_session_id().to_string();
    let built = session_events::build_permission_request_event(
        &request.id,
        &request.tool_name,
        &request.tool_input,
        &sid,
        &mut agentic.live_threading,
        sk,
    );
    let note_id = ingest_session_event(agentic, built, "permission request event", ndb, sk)?;
    agentic
        .permissions
        .request_note_ids
        .insert(request.id, note_id);
    Some(note_id)
}

/// Publish the `permission_response{auto}` a local session's runtime allowlist
/// gave a request without a user click, so observers, a restart and the CLI
/// show it resolved (and auto-accepted) rather than pending.
///
/// An observer auto-accepting a remote session's request publishes through
/// here too (see `conversation::auto_accept_remote_request`). Skipped when the
/// request's note id was never recorded: there is no note to answer.
pub(crate) fn publish_auto_accept_response(
    session: &mut ChatSession,
    perm_id: uuid::Uuid,
    ndb: &nostrdb::Ndb,
    sk: &[u8; 32],
) {
    let Some(agentic) = session.agentic.as_mut() else {
        return;
    };
    let Some(request_note_id) = agentic.permissions.request_note_ids.get(&perm_id).copied() else {
        tracing::warn!("auto-accepted {perm_id} has no published request; not publishing");
        return;
    };
    let sid = agentic.event_session_id().to_string();
    let built = session_events::build_permission_response_event(
        &perm_id,
        &request_note_id,
        true,
        None,
        false,
        true,
        &sid,
        &mut agentic.live_threading,
        sk,
    );
    ingest_session_event(agentic, built, "auto-accept response event", ndb, sk);
}

/// Update every session's status, then publish the auto-accept responses for
/// the permissions the runtime allowlist resolved on the way (see
/// [`SessionManager::update_all_statuses`]), so observers stop showing them
/// pending. Without a key the statuses still update and nothing is published.
/// Returns what was resolved.
///
/// The per-frame pass in `Dave::update` calls this; so do tests, which then
/// exercise the path the app runs rather than a copy of its loop.
///
/// [`SessionManager::update_all_statuses`]: session::SessionManager::update_all_statuses
pub(crate) fn update_statuses_and_publish_auto_resolved(
    sessions: &mut session::SessionManager,
    ndb: &nostrdb::Ndb,
    sk: Option<&[u8; 32]>,
) -> Vec<session::AutoResolved> {
    let resolved = sessions.update_all_statuses();
    let Some(sk) = sk else {
        return resolved;
    };
    for auto in &resolved {
        let Some(session) = sessions.get_mut(auto.session) else {
            continue;
        };
        publish_auto_accept_response(session, auto.perm_id, ndb, sk);
    }
    resolved
}

impl Dave {
    /// Publish kind-31988 state events for sessions whose status changed.
    pub(crate) fn publish_dirty_session_states(&mut self, ctx: &mut AppContext<'_>) {
        let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) else {
            return;
        };
        let account = *ctx.accounts.selected_account_pubkey();

        for session in self.session_manager.iter_mut() {
            if !session.state_dirty {
                continue;
            }

            let Some(agentic) = &session.agentic else {
                continue;
            };
            let event_sid = agentic.event_session_id().to_string();

            // What to publish for this dirty session, or `None` to skip it —
            // see `session_state_publish_params`.
            let Some(publish) = session_state_publish_params(
                session,
                &event_sid,
                &self.hostname,
                ctx.ndb,
                &account,
            ) else {
                session.state_dirty = false;
                continue;
            };

            let Some(state) = session_state_snapshot(
                session,
                publish.status.clone(),
                publish.hostname,
                publish.created_at,
            ) else {
                session.state_dirty = false;
                continue;
            };

            ingest_built_event(
                state.build_event(&sk),
                &format!(
                    "publishing session state: {} -> {}",
                    event_sid, publish.status
                ),
                ctx.ndb,
                &sk,
            );

            session.state_dirty = false;
        }
    }

    /// Publish "deleted" state events for sessions that were deleted.
    /// Called in the update loop where AppContext is available.
    pub(crate) fn publish_pending_deletions(&mut self, ctx: &mut AppContext<'_>) {
        if self.pending_deletions.is_empty() {
            return;
        }

        let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) else {
            return;
        };
        let account = *ctx.accounts.selected_account_pubkey();

        for mut state in std::mem::take(&mut self.pending_deletions) {
            // Keep the "deleted" revision strictly newest so it wins replaceable
            // resolution over the session's last status event (same-second safe).
            state.created_at = {
                let latest = Transaction::new(ctx.ndb).ok().and_then(|txn| {
                    session_loader::latest_state_created_at(
                        ctx.ndb,
                        &txn,
                        &account,
                        &state.claude_session_id,
                    )
                });
                session_events::next_state_created_at(
                    session_events::now_secs(),
                    latest.unwrap_or(0),
                )
            };
            ingest_built_event(
                state.build_event(&sk),
                &format!(
                    "publishing deleted session state: {}",
                    state.claude_session_id
                ),
                ctx.ndb,
                &sk,
            );
        }
    }

    /// Build and queue permission response events through the engine.
    /// Called in the update loop where AppContext is available.
    ///
    /// The engine builds + locally-ingests each response (resolving the request's
    /// note id from ndb); the host's private-sync Session fans it out.
    pub(crate) fn publish_pending_perm_responses(&mut self, ctx: &AppContext<'_>) {
        if self.pending_perm_responses.is_empty() {
            return;
        }

        let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) else {
            tracing::warn!("no secret key for publishing permission responses");
            self.pending_perm_responses.clear();
            return;
        };
        let Some(engine) = embedded_engine(ctx.ndb, &sk) else {
            self.pending_perm_responses.clear();
            return;
        };

        for resp in std::mem::take(&mut self.pending_perm_responses) {
            publish_user_permission_response(&mut self.session_manager, &engine, &resp);
        }
    }

    /// Publish permission mode command events for remote sessions.
    /// Called in the update loop where AppContext is available.
    pub(crate) fn publish_pending_mode_commands(&mut self, ctx: &AppContext<'_>) {
        if self.pending_mode_commands.is_empty() {
            return;
        }

        let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) else {
            tracing::warn!("no secret key for publishing mode commands");
            self.pending_mode_commands.clear();
            return;
        };
        let Some(engine) = embedded_engine(ctx.ndb, &sk) else {
            self.pending_mode_commands.clear();
            return;
        };

        for cmd in std::mem::take(&mut self.pending_mode_commands) {
            match engine.prepare_set_permission_mode(&cmd.session_id, cmd.mode) {
                Ok(_) => {
                    tracing::info!(
                        "publishing permission mode command: {} -> {}",
                        cmd.session_id,
                        cmd.mode
                    );
                }
                Err(e) => tracing::error!(
                    "failed to build mode command for {}: {:?}",
                    cmd.session_id,
                    e
                ),
            }
        }
    }

    /// Publish interrupt command events for remote sessions.
    /// Called in the update loop where AppContext is available.
    pub(crate) fn publish_pending_interrupt_commands(&mut self, ctx: &AppContext<'_>) {
        if self.pending_interrupt_commands.is_empty() {
            return;
        }

        let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) else {
            tracing::warn!("no secret key for publishing interrupt commands");
            self.pending_interrupt_commands.clear();
            return;
        };
        let Some(engine) = embedded_engine(ctx.ndb, &sk) else {
            self.pending_interrupt_commands.clear();
            return;
        };

        for cmd in std::mem::take(&mut self.pending_interrupt_commands) {
            match engine.prepare_interrupt(&cmd.session_id) {
                Ok(_) => {
                    tracing::info!("publishing interrupt command for {}", cmd.session_id);
                }
                Err(e) => tracing::error!(
                    "failed to build interrupt command for {}: {:?}",
                    cmd.session_id,
                    e
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_status::AgentStatus;
    use crate::backend::BackendType;
    use crate::config::AiMode;
    use crate::session::SessionSource;
    use crate::tests::{test_config, test_secret_key};
    use nostrdb::{IngestMetadata, Ndb};
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// A remote-session rename must publish a kind-31988 (so it persists across
    /// restart), carrying the owner's hostname + last-known status — but a
    /// status-only dirty on a remote session must NOT publish. This is the gate
    /// in `session_state_publish_params` that lets the phone persist a
    /// custom_title it owns without clobbering the owner's authoritative status.
    #[tokio::test]
    async fn remote_rename_publishes_only_on_custom_title_change() {
        let sk = test_secret_key();
        let account = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        let sid = "rename-persist-test";

        let tmp = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp.path().to_str().unwrap(), &test_config()).unwrap();
        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_SESSION_STATE_KIND as u64])
            .build();

        // The owner published a state with NO custom_title. Seed it in the
        // FUTURE: against a 1970 timestamp any wall-clock implementation wins,
        // so the `now.max(persisted + 1)` monotonicity rule — what makes a
        // replaceable event beat a revision whose clock ran ahead — is never
        // exercised.
        let persisted_created_at = session_events::now_secs() + 100;
        let seed = session_events::build_session_state_event(
            sid,
            "Auto Title",
            None,
            "/home/dev/proj",
            "working",
            None,
            "build-server",
            "/home/dev",
            "claude",
            "default",
            Some(sid),
            None,
            None,
            None,
            persisted_created_at,
            &sk,
        )
        .unwrap();
        let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
        ndb.process_event_with(
            &format!("[\"EVENT\",{}]", seed.note_json),
            IngestMetadata::new().client(true),
        )
        .unwrap();
        let _ = ndb.wait_for_notes(sub, 1).await.unwrap();

        // A remote session renamed in memory to "My Title".
        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/home/dev/proj"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        session.source = SessionSource::Remote;
        session.details.hostname = "build-server".to_string();
        session.details.custom_title = Some("My Title".to_string());
        {
            let a = session.agentic.as_mut().unwrap();
            a.event_id = sid.to_string();
            a.remote_status = Some(AgentStatus::Working);
        }

        // custom_title differs from persisted (None) -> publish, faithfully.
        let publish = session_state_publish_params(&session, sid, "phone-host", &ndb, &account)
            .expect("a remote rename must publish");
        assert_eq!(
            publish.status, "working",
            "re-asserts the last-known remote status, not a derived one"
        );
        assert_eq!(
            publish.hostname, "build-server",
            "keeps the owner's hostname, not the phone's"
        );
        assert!(
            publish.created_at > persisted_created_at,
            "created_at must strictly beat the persisted revision, even one \
             stamped ahead of this machine's clock"
        );

        // Now the in-memory title matches what's persisted -> a status-only
        // dirty must be skipped (the owner is authoritative for status).
        session.details.custom_title = None;
        assert!(
            session_state_publish_params(&session, sid, "phone-host", &ndb, &account).is_none(),
            "a remote status-only dirty must not publish"
        );

        // A local session always publishes, using this machine's hostname.
        let mut local = session::ChatSession::new(
            2,
            PathBuf::from("/home/dev/proj"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        local.agentic.as_mut().unwrap().event_id = "local-sid".to_string();
        let lp = session_state_publish_params(&local, "local-sid", "phone-host", &ndb, &account)
            .expect("a local session publishes on any dirty");
        assert_eq!(lp.hostname, "phone-host", "local publish uses this machine");
    }

    /// A note nostrdb never took never comes back through the conversation
    /// subscription, so neither publish path records it as waiting to: the
    /// reconcile at rest, which waits for every recorded note, would wait on
    /// it forever, and the dedup set would skip it if it ever did arrive.
    ///
    /// The note here is an inner event too big for NIP-44 to carry, which
    /// [`pns_ingest`] can't wrap.
    #[test]
    fn a_note_nostrdb_refused_is_not_recorded() {
        let tmp = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp.path().to_str().unwrap(), &test_config()).unwrap();
        let sk = test_secret_key();
        let refused = || session_events::BuiltEvent {
            note_json: "x".repeat(70_000),
            note_id: [7; 32],
            kind: session_events::AI_CONVERSATION_KIND,
        };
        assert!(
            !pns_ingest(&ndb, &refused().note_json, &sk),
            "NIP-44 can't carry the note"
        );

        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        let agentic = session.agentic.as_mut().unwrap();
        assert_eq!(
            ingest_session_event(agentic, Ok(refused()), "refused note", &ndb, &sk),
            None
        );
        let first =
            ingest_built_live_events(&mut session, &ndb, &sk, |_, _, _| Ok(vec![refused()]));
        assert!(first.is_some(), "the built note is still handed back");

        let agentic = session.agentic.as_ref().unwrap();
        assert!(agentic.unindexed_self_notes.is_empty());
        assert!(!agentic.seen_note_ids.contains(&[7; 32]));
        assert!(!agentic.fold_dirty, "the chat gained no published row");
    }
}
