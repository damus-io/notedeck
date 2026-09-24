//! Dave's outbound private notes: the PNS ingest path every built event takes,
//! the kind-1988 live/user events, and the kind-31988 session-state publish
//! (whose one builder is [`session_state_snapshot`] +
//! [`SessionState::build_event`](session_loader::SessionState::build_event)),
//! plus the per-frame drains of queued permission, mode, interrupt and
//! deletion events.

use crate::{
    embedded_engine, file_update, secret_key_bytes, session, session_events, session_loader,
    ChatSession, Dave,
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
pub(crate) fn pns_ingest(ndb: &nostrdb::Ndb, event_json: &str, secret_key: &[u8; 32]) {
    if let Err(e) = notedeck::write_private_note(ndb, secret_key, event_json) {
        tracing::warn!("failed to write private note: {e}");
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

/// Wire safety cap for a tool result's `output`, in bytes.
///
/// Truncation is otherwise a display concern — the host keeps the full output in
/// memory and [`ui::dave::DaveUi::tool_output_ui`] truncates it for display —
/// but the tool_result note is PNS-wrapped and published, so the serialized
/// event must stay under typical relay limits (~64KB). This mirrors the
/// permission event's tool-input budget: ~40KB output plus summary, inner-event,
/// and PNS overhead, times the ~1.33x base64 expansion, lands under 64KB.
pub(crate) const MAX_TOOL_OUTPUT_WIRE_BYTES: usize = 40_000;

/// The file update to publish on a tool_result note, or `None` when it would
/// push the note over the wire budget.
///
/// Shares [`MAX_TOOL_OUTPUT_WIRE_BYTES`] with the (already capped) output so
/// the two together stay under relay limits. An oversized edit is dropped
/// whole rather than truncated: a clipped old/new string would render a
/// diff that never happened. Reloaded/remote sessions then show the summary
/// only, as before.
pub(crate) fn wire_file_update(
    file_update: Option<&file_update::FileUpdate>,
    output_len: usize,
) -> Option<&file_update::FileUpdate> {
    let budget = MAX_TOOL_OUTPUT_WIRE_BYTES.saturating_sub(output_len);
    file_update.filter(|update| update.payload_len() <= budget)
}

/// Build and ingest a live kind-1988 event into ndb (via PNS wrapping).
///
/// Extracts cwd and session ID from the session's agentic data,
/// builds the event, PNS-wraps and ingests it, and returns the event
/// for relay publishing.
pub(crate) fn ingest_live_event(
    session: &mut ChatSession,
    ndb: &nostrdb::Ndb,
    secret_key: &[u8; 32],
    content: &str,
    role: &str,
    tool_id: Option<&str>,
    tool_name: Option<&str>,
) -> Option<session_events::BuiltEvent> {
    let agentic = session.agentic.as_mut()?;
    let session_id = agentic.event_session_id().to_string();
    let cwd = agentic.cwd.to_str();

    match session_events::build_live_event(
        content,
        role,
        &session_id,
        cwd,
        tool_id,
        tool_name,
        &mut agentic.live_threading,
        secret_key,
    ) {
        Ok(event) => {
            // Mark as seen so we don't double-process when it echoes back from the relay
            agentic.seen_note_ids.insert(event.note_id);
            pns_ingest(ndb, &event.note_json, secret_key);
            Some(event)
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
) -> Option<session_events::BuiltEvent> {
    let agentic = session.agentic.as_mut()?;
    let session_id = agentic.event_session_id().to_string();
    let engine = embedded_engine(ndb, secret_key)?;
    match engine.prepare_message(&session_id, text) {
        Ok(event) => {
            agentic.seen_note_ids.insert(event.note_id);
            Some(event)
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
pub(crate) fn build_user_send_event(
    session: &mut ChatSession,
    ndb: &nostrdb::Ndb,
    secret_key: &[u8; 32],
    text: &str,
) -> Option<session_events::BuiltEvent> {
    if session.is_remote() {
        ingest_remote_user_message(session, ndb, secret_key, text)
    } else {
        ingest_live_event(session, ndb, secret_key, text, "user", None, None)
    }
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
            match engine.prepare_permission_response(
                &resp.event_session_id,
                &resp.perm_id.to_string(),
                resp.allowed,
                resp.message.as_deref(),
                resp.cancel_turn,
            ) {
                Ok(_) => {
                    tracing::info!(
                        "queued permission response for {} ({})",
                        resp.perm_id,
                        if resp.allowed { "allow" } else { "deny" }
                    );
                }
                Err(e) => tracing::error!(
                    "failed to build permission response for {}: {:?}",
                    resp.perm_id,
                    e
                ),
            }
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

    /// An edit's diff is published with its tool_result only while it fits the
    /// wire budget left after the output. Past that it is dropped whole, never
    /// truncated into a diff that didn't happen.
    #[test]
    fn wire_file_update_drops_oversized_edits() {
        let edit = |len: usize| {
            file_update::FileUpdate::new(
                "a.rs".to_string(),
                file_update::FileUpdateType::Write {
                    content: "x".repeat(len),
                },
            )
        };
        let small = edit(100);
        assert!(wire_file_update(Some(&small), 0).is_some());
        assert!(wire_file_update(None, 0).is_none());

        let huge = edit(MAX_TOOL_OUTPUT_WIRE_BYTES);
        assert!(
            wire_file_update(Some(&huge), 0).is_none(),
            "an edit past the budget is dropped"
        );

        // The output's share of the budget counts against the edit.
        assert!(wire_file_update(Some(&small), MAX_TOOL_OUTPUT_WIRE_BYTES - 50).is_none());
    }

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
}
