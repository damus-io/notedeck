//! Materializing sessions from their persisted kind-31988 state: background
//! restore, live discovery, reopening (reviving) a closed session, focusing
//! one from a clicked `agentium:` chip, and loading a resumed session's
//! history. [`hydrate_session_from_state`] is the one hydrator they share.

use crate::agent_status::AgentStatus;
use crate::backend::BackendType;
use crate::{
    focus_queue, get_backend, secret_key_bytes, session, session_converter, session_events,
    session_loader, session_restore_loader, update, AiMode, ChatSession, Dave, DaveOverlay,
    SessionId,
};
use nostrdb::{Subscription, Transaction};
use notedeck::{AppContext, Waker};
use std::path::PathBuf;

/// Per-frame time budget for draining background-restored sessions into the
/// manager, so a large restore fills in over several frames without stalling any
/// single one (mirrors `notedeck_columns`' `TIMELINE_LOADER_APPLY_BUDGET`).
const SESSION_RESTORE_APPLY_BUDGET: std::time::Duration = std::time::Duration::from_millis(2);

/// Subscription waiting for ndb to index 1988 conversation events.
pub(crate) struct PendingMessageLoad {
    /// ndb subscription for kind-1988 events matching the session
    sub: Subscription,
    /// Account that signed the archived conversation events.
    account: nostrdb_net::Pubkey,
    /// Dave's internal session ID
    dave_session_id: SessionId,
    /// Claude session ID (the `d` tag value)
    claude_session_id: String,
}

impl Dave {
    /// Act on a pending [`open`](Self::open): resolve the clicked kind-31988 note to
    /// one of this account's sessions (by its `claude_session_id` d-tag) and switch
    /// to it, revealing the chat.
    ///
    /// A materialized session is simply focused. A session that *isn't*
    /// materialized — a soft-deleted (tombstoned) session, or one not yet restored
    /// — is [reopened](Self::reopen_session): clicking a deleted `agentium:` chip
    /// revives and resumes a session we own (or surfaces history for a remote one)
    /// rather than doing nothing. A note that isn't a session state at all drops
    /// the request rather than retrying forever.
    pub(crate) fn process_pending_open(&mut self, ndb: &nostrdb::Ndb) {
        let Some(note_id) = self.pending_open.take() else {
            return;
        };
        // The session's stable event id (the kind-31988 `d` tag), if this note is a
        // session-state event. Scope the read txn so it drops before reopen below.
        let event_id: Option<String> = {
            let Ok(txn) = Transaction::new(ndb) else {
                // Couldn't open a read txn this frame; retry next frame.
                self.pending_open = Some(note_id);
                return;
            };
            ndb.get_note_by_id(&txn, note_id.bytes())
                .ok()
                .and_then(|note| session_events::get_tag_value(&note, "d").map(|s| s.to_string()))
        };
        let Some(event_id) = event_id else {
            // Not a session-state note we can route to.
            return;
        };

        // Already materialized — just focus it.
        if let Some(session_id) = self.session_id_for_event_id(&event_id) {
            if self.session_manager.switch_to(session_id) {
                // Reveal the chat: clear any overlay (directory/session picker) and
                // the mobile session-list drawer, and stop auto-steal fighting the
                // switch — dequeue this session's own entry and anchor auto-steal
                // here so it doesn't immediately yank onto a *different* session.
                self.active_overlay = DaveOverlay::None;
                self.show_session_list = false;
                self.focus_queue.dequeue(session_id);
                self.anchor_focus(session_id);
            }
            return;
        }

        // Already awaiting a remote resume for this session — focus the existing
        // "Connecting…" placeholder rather than queuing a second command and
        // stranding a duplicate placeholder (re-clicking the chip before its host
        // answers). The placeholder is agentic-less, so the check above misses it.
        if let Some(placeholder_id) = self.pending_placeholder_for(None, &event_id) {
            self.session_manager.switch_to(placeholder_id);
            self.active_overlay = DaveOverlay::None;
            self.show_session_list = false;
            self.anchor_focus(placeholder_id);
            return;
        }

        // Not materialized: a soft-deleted (or not-yet-restored) session. Reopen
        // it instead of dropping the click — the deleted-chip resume affordance.
        let Some(account) = self.pns_local_state.as_ref().map(|state| state.account) else {
            return;
        };

        // Resolve the target session's state (across live + tombstoned) so we can
        // route by host: a session on THIS host is revived + resumed locally; one
        // on ANOTHER host can't be (there's no local backend to `claude --resume`
        // it), so we publish a resume command asking its own host to reopen it.
        let state = {
            let Ok(txn) = Transaction::new(ndb) else {
                // Couldn't open a read txn this frame; retry next frame.
                self.pending_open = Some(note_id);
                return;
            };
            let live = session_loader::load_session_states_for_author(ndb, &txn, &account);
            let deleted =
                session_loader::load_deleted_session_states_for_author(ndb, &txn, &account);
            session_loader::resolve_session_including_deleted(&live, &deleted, &event_id)
                .ok()
                .cloned()
        };
        let Some(state) = state else {
            return;
        };

        if !state.hostname.is_empty() && state.hostname != self.hostname {
            // Remote session: ask its host to reopen + revive + resume it, and
            // stand up a "Connecting…" placeholder for immediate feedback (see
            // queue_resume_command). The revived kind-31988 state streams back
            // over the shared subscription and upgrades the placeholder in place.
            self.queue_resume_command(&state);
            self.show_session_list = false;
            return;
        }

        // Local session: revive + resume it in place.
        if let Some(session_id) = self.reopen_session(ndb, account, &event_id) {
            self.active_overlay = DaveOverlay::None;
            self.show_session_list = false;
            self.focus_queue.dequeue(session_id);
        }
    }

    /// The pending placeholder a just-discovered kind-31988 state should upgrade
    /// in place, or `None` to materialize a fresh session.
    ///
    /// A **spawn** placeholder has no session yet, so it is correlated by the
    /// `spawn_id` the receiving host echoes into the new session's state. A
    /// **resume** placeholder revives an *existing* session and is correlated by
    /// that session's d-tag (`claude_sid`) instead: the revived state always comes
    /// back on its original d-tag, whereas matching on a freshly-minted spawn_id
    /// is racy — an older revision of the same session (re-delivered over PNS) can
    /// materialize it before the spawn_id-carrying revision lands, stranding the
    /// placeholder until it times out.
    fn pending_placeholder_for(
        &self,
        spawn_id: Option<&str>,
        claude_sid: &str,
    ) -> Option<SessionId> {
        self.session_manager
            .iter()
            .find(|s| {
                if s.pending_created_at.is_none() {
                    return false;
                }
                let resume_match = s.pending_resume_target.as_deref() == Some(claude_sid);
                let spawn_match = spawn_id.is_some() && s.spawn_id.as_deref() == spawn_id;
                resume_match || spawn_match
            })
            .map(|s| s.id)
    }

    /// The [`SessionId`] of the materialized session whose stable event id
    /// (kind-1988/31988 d-tag) is `event_id`, if any. Matches an agentic session's
    /// [`event_session_id`](session::AgenticSessionData::event_session_id) — the same
    /// key kind-31988 state events carry.
    fn session_id_for_event_id(&self, event_id: &str) -> Option<SessionId> {
        self.session_manager
            .iter()
            .find(|s| {
                s.agentic
                    .as_ref()
                    .is_some_and(|a| a.event_session_id() == event_id)
            })
            .map(|s| s.id)
    }

    /// Drain finished sessions from the background restore worker (see
    /// [`session_restore_loader`]) into the session manager, a few per frame
    /// under [`SESSION_RESTORE_APPLY_BUDGET`]. The worker does the expensive ndb
    /// read + render off-thread; this only does the cheap `&mut self` work of
    /// creating + hydrating each session.
    ///
    /// Coexists with the live-discovery path
    /// ([`poll_session_state_events`](Self::poll_session_state_events)): both run
    /// on the render thread and dedup by `event_session_id`, so whichever
    /// materializes a session first wins and the other skips it.
    pub(crate) fn drain_session_restore(&mut self, waker: &Waker) {
        // Messages tagged with a different account are stale (the user switched
        // accounts while an in-flight restore was streaming); drop them.
        let current = self.pns_local_state.as_ref().map(|state| state.account);

        // Preserve the user's focus across restore. `new_resumed_session` sets
        // `active` per call, so without this the newest restored session would
        // yank focus every frame. Capture the active session *before* creating
        // anything, and re-assert it after. In Agentic mode the manager starts
        // empty (`None`), so focus lands once on the first restored session and
        // then stays there as the rest stream in.
        let keep_active = self.session_manager.active_id();
        let mut first_created: Option<SessionId> = None;
        let mut created_any = false;

        let start = std::time::Instant::now();
        let mut handled = 0usize;
        loop {
            if handled > 0 && start.elapsed() >= SESSION_RESTORE_APPLY_BUDGET {
                waker.wake();
                break;
            }
            let Some(msg) = self.session_restore_loader.try_recv() else {
                break;
            };
            handled += 1;

            match msg {
                session_restore_loader::SessionRestoreMsg::Started {
                    account,
                    live_count,
                    host_paths,
                } => {
                    if Some(account) != current {
                        continue;
                    }
                    self.directory_picker
                        .seed_host_paths(host_paths, &self.hostname);
                    // Skip the directory picker if this account has sessions to
                    // restore; leave it up otherwise (empty account = new user).
                    if live_count > 0 && matches!(self.active_overlay, DaveOverlay::DirectoryPicker)
                    {
                        self.active_overlay = DaveOverlay::None;
                    }
                }
                session_restore_loader::SessionRestoreMsg::Session {
                    account,
                    state,
                    loaded,
                } => {
                    if Some(account) != current {
                        continue;
                    }
                    // Dedup against the live manager (covers both a prior restore
                    // batch and the live-discovery poll path).
                    let exists = self.session_manager.iter().any(|session| {
                        session.agentic.as_ref().is_some_and(|agentic| {
                            agentic.event_session_id() == state.claude_session_id.as_str()
                        })
                    });
                    if exists {
                        continue;
                    }

                    let backend = state
                        .backend
                        .as_deref()
                        .and_then(BackendType::from_tag_str)
                        .unwrap_or(BackendType::Claude);
                    let cwd = std::path::PathBuf::from(&state.cwd);

                    // The d-tag is the event_id (Nostr identity). The cli_session
                    // tag holds the real CLI session ID for --resume. If there's
                    // no cli_session tag, this is a legacy event where d-tag was
                    // the CLI session ID.
                    let resume_id = match state.cli_session_id {
                        Some(ref cli) if !cli.is_empty() => cli.clone(),
                        // Empty cli_session — backend never started, nothing to resume.
                        Some(_) => String::new(),
                        // Legacy: d-tag IS the CLI session ID.
                        None => state.claude_session_id.clone(),
                    };

                    let dave_sid = self.session_manager.new_resumed_session(
                        cwd,
                        resume_id,
                        state.title.clone(),
                        AiMode::Agentic,
                        backend,
                    );
                    first_created.get_or_insert(dave_sid);

                    if let Some(session) = self.session_manager.get_mut(dave_sid) {
                        hydrate_session_from_state(session, &state, *loaded, &self.hostname);
                    }
                    created_any = true;
                }
                session_restore_loader::SessionRestoreMsg::Finished { account, restored } => {
                    if Some(account) != current {
                        continue;
                    }
                    tracing::info!("restored {restored} sessions from ndb");
                }
                session_restore_loader::SessionRestoreMsg::Failed { account, error } => {
                    if Some(account) == current {
                        tracing::error!("session restore failed: {error}");
                    }
                }
            }
        }

        if !created_any {
            return;
        }

        self.session_manager.rebuild_groups();

        // Re-assert focus: restoring sessions must never yank the user (see above).
        match keep_active {
            Some(id) => {
                self.session_manager.switch_to(id);
            }
            None => {
                if let Some(first) = first_created {
                    self.session_manager.switch_to(first);
                }
            }
        }
    }

    /// Advance the shared inline-session cache for the selected account so
    /// `agentium:<word-id>` chips drawn in notes/Dave-chat read the latest folded
    /// session state, requesting a repaint while state streams in.
    ///
    /// Read-only: Dave's PNS publish path (see [`Self::update`]) already syncs and
    /// fans out session-state events, so this only *advances* the fold — it never
    /// re-publishes, which would double-write. The cache is shared (cloned `Rc`)
    /// into the reference parser and renderer, so the fold happens once per account.
    #[profiling::function]
    pub(crate) fn pump_session_cache(&mut self, ctx: &mut AppContext<'_>) {
        let author = *ctx.accounts.selected_account_pubkey();
        let Ok(txn) = Transaction::new(ctx.ndb) else {
            return;
        };
        let changed = self
            .session_cache
            .borrow_mut()
            .poll(ctx.ndb, &txn, &author)
            .changed;
        if changed {
            ctx.wake();
        }
    }

    /// Poll for new kind-31988 session state events from the ndb subscription.
    ///
    /// When PNS events arrive from relays and get unwrapped, new session state
    /// events may appear. This detects them and creates sessions we don't already have.
    pub(crate) fn poll_session_state_events(&mut self, ctx: &mut AppContext<'_>) {
        let Some(sub) = self.session_state_sub else {
            return;
        };
        let Some(account) = self.pns_local_state.as_ref().map(|state| state.account) else {
            return;
        };

        // Defer materializing discovered sessions until the host's account-wide
        // private-note sync has settled (`AppContext::private_sync_settled`, driven
        // by `notedeck::HostPrivateSync`). Negentropy history reconciliation streams
        // events in over several rounds, so a mid-sync snapshot can hold a session's
        // `create` revision while its newer `deleted` revision is still pending —
        // draining now would materialize an already-deleted "litter" session that
        // vanishes a few frames later. We return *before* `poll_for_notes` so the
        // notes stay queued on the subscription; once settled, the drain sees the
        // netted head (ndb has collapsed each replaceable session-state event to its
        // latest revision, and the creation path re-queries that latest revision, so
        // deleted sessions never surface).
        //
        // `private_sync_settled` is `true` when local-only (no private relay ⇒
        // nothing to reconcile), so processing runs immediately in that case.
        if !ctx.private_sync_settled {
            return;
        }

        let note_keys = ctx.ndb.poll_for_notes(sub, 32);
        if note_keys.is_empty() {
            return;
        }

        let txn = match Transaction::new(ctx.ndb) {
            Ok(t) => t,
            Err(_) => return,
        };

        // Collect existing claude session IDs to avoid duplicates
        let mut existing_ids: std::collections::HashSet<String> = self
            .session_manager
            .iter()
            .filter_map(|s| s.agentic.as_ref().map(|a| a.event_session_id().to_string()))
            .collect();

        for key in note_keys {
            let Ok(note) = ctx.ndb.get_note_by_key(&txn, key) else {
                continue;
            };

            let Some(claude_sid) = session_events::get_tag_value(&note, "d") else {
                continue;
            };

            let status_str = session_events::get_tag_value(&note, "status").unwrap_or("idle");
            let backend_tag =
                session_events::get_tag_value(&note, "backend").and_then(BackendType::from_tag_str);

            // Skip deleted sessions entirely — don't create or keep them
            if status_str == "deleted" {
                // If we have this session locally, remove it (only if this
                // event is newer than the last state we applied).
                if existing_ids.contains(claude_sid) {
                    let ts = note.created_at();
                    let to_delete: Vec<SessionId> = self
                        .session_manager
                        .iter()
                        .filter(|s| {
                            s.agentic.as_ref().is_some_and(|a| {
                                a.event_session_id() == claude_sid && ts > a.remote_status_ts
                            })
                        })
                        .map(|s| s.id)
                        .collect();
                    for id in to_delete {
                        let bt = self
                            .session_manager
                            .get(id)
                            .map(|s| s.backend_type)
                            .unwrap_or(BackendType::Remote);
                        update::delete_session(
                            &mut self.session_manager,
                            &mut self.focus_queue,
                            get_backend(&self.backends, bt),
                            &mut self.directory_picker,
                            id,
                        );
                    }
                }
                continue;
            }

            // Update remote_status for existing remote sessions, but only
            // if this event is newer than the one we already applied.
            // Multiple revisions of the same replaceable event can arrive
            // out of order (e.g. after a relay reconnect).
            if existing_ids.contains(claude_sid) {
                let ts = note.created_at();
                let new_status = AgentStatus::from_status_str(status_str);
                let new_custom_title =
                    session_events::get_tag_value(&note, "custom_title").map(|s| s.to_string());
                let new_hostname = session_events::get_tag_value(&note, "hostname").unwrap_or("");
                for session in self.session_manager.iter_mut() {
                    let is_remote = session.is_remote();
                    if let Some(agentic) = &mut session.agentic {
                        if agentic.event_session_id() == claude_sid && ts > agentic.remote_status_ts
                        {
                            agentic.remote_status_ts = ts;
                            // A state event is a "host is alive" signal; feed
                            // the status-bar last-activity indicator. Set the
                            // field directly (keeping the newest) since
                            // `agentic` is borrowed and `mark_activity` would
                            // reborrow the whole session.
                            session.last_activity =
                                Some(session.last_activity.map_or(ts, |c| c.max(ts)));
                            // custom_title syncs for both local and remote
                            if new_custom_title.is_some() {
                                session.details.custom_title = new_custom_title.clone();
                            }
                            if let Some(backend) = backend_tag {
                                session.backend_type = backend;
                            }
                            // Hostname syncs for remote sessions from the event
                            if is_remote && !new_hostname.is_empty() {
                                session.details.hostname = new_hostname.to_string();
                            }
                            // Status, indicator, and permission mode only update
                            // for remote sessions (local sessions derive from
                            // the process)
                            if is_remote {
                                agentic.remote_status = new_status;
                                session.indicator =
                                    session_events::get_tag_value(&note, "indicator")
                                        .and_then(focus_queue::FocusPriority::from_indicator_str);
                                if let Some(pm) =
                                    session_events::get_tag_value(&note, "permission-mode")
                                {
                                    agentic.permission_mode =
                                        crate::session::permission_mode_from_str(pm);
                                }
                            }
                        }
                    }
                }
                self.session_manager.rebuild_groups();
                continue;
            }

            // Look up the latest revision of this session. PNS wrapping
            // causes old revisions (including pre-deletion) to arrive from
            // the relay. Only create a session if the latest revision is valid.
            let Some(state) = session_loader::latest_valid_session_for_author(
                ctx.ndb, &txn, &account, claude_sid,
            ) else {
                continue;
            };

            tracing::info!(
                "discovered new session from relay: '{}' ({}) on {}",
                state.title,
                claude_sid,
                state.hostname,
            );

            existing_ids.insert(claude_sid.to_string());

            // Track this host+cwd for the directory picker
            if !state.cwd.is_empty() {
                self.directory_picker
                    .add_host_path(&state.hostname, PathBuf::from(&state.cwd));
            }

            let backend = state
                .backend
                .as_deref()
                .and_then(BackendType::from_tag_str)
                .unwrap_or(BackendType::Claude);
            let cwd = std::path::PathBuf::from(&state.cwd);

            // Same event_id / cli_session logic as restore_sessions_from_ndb
            let resume_id = match state.cli_session_id {
                Some(ref cli) if !cli.is_empty() => cli.clone(),
                Some(_) => String::new(),       // backend never started
                None => claude_sid.to_string(), // legacy
            };

            // Check for a pending placeholder this state should upgrade in place
            // (spawn: by echoed spawn_id; resume: by target d-tag). If found,
            // upgrade it instead of creating a duplicate session.
            let pending_sid = self.pending_placeholder_for(state.spawn_id.as_deref(), claude_sid);

            let dave_sid = if let Some(sid) = pending_sid {
                tracing::info!("upgrading pending placeholder to real session");
                sid
            } else {
                self.session_manager.new_resumed_session(
                    cwd,
                    resume_id,
                    state.title.clone(),
                    AiMode::Agentic,
                    backend,
                )
            };

            // Load any conversation history that arrived with it
            let loaded = session_loader::load_session_messages_for_author(
                ctx.ndb, &txn, &account, claude_sid,
            );

            if let Some(session) = self.session_manager.get_mut(dave_sid) {
                // Clear pending state (upgrades placeholder to real session).
                session.pending_created_at = None;
                session.details.title = state.title.clone();

                // Initialize agentic data if absent (e.g. upgraded placeholder)
                // so the shared hydrator has something to populate.
                if session.agentic.is_none() {
                    session.agentic = Some(session::AgenticSessionData::new(
                        dave_sid,
                        PathBuf::from(&state.cwd),
                    ));
                }

                if !loaded.messages.is_empty() {
                    tracing::info!(
                        "loaded {} messages for discovered session",
                        loaded.messages.len()
                    );
                }

                hydrate_session_from_state(session, &state, loaded, &self.hostname);
            }

            self.session_manager.rebuild_groups();

            // If we were showing the directory picker, switch to showing sessions
            if matches!(self.active_overlay, DaveOverlay::DirectoryPicker) {
                self.active_overlay = DaveOverlay::None;
            }
        }
    }

    /// Reopen a closed (possibly soft-deleted) session so a new message drives
    /// its backend again — the host side of `agentium resume` and the
    /// deleted-chip resume button.
    ///
    /// Resolves `selector` (a d-tag, cli-session id, or `agentium:` word-id)
    /// across both the live and tombstoned sets, then materializes the session
    /// through the shared [`hydrate_session_from_state`] — which pins `event_id`
    /// back to the original d-tag, restores history + dedup, and derives
    /// `resume_session_id` from the `cli_session` tag so the backend resumes with
    /// `claude --resume`. Marking it `state_dirty` makes the next
    /// [`publish_dirty_session_states`](Self::publish_dirty_session_states) emit a
    /// newer active revision for that d-tag, which overwrites the tombstone in
    /// both folds — reviving the `agentium:` ref in place. An already-open
    /// session is simply refocused. Returns the reopened `SessionId`, or `None`
    /// if nothing matched.
    pub(crate) fn reopen_session(
        &mut self,
        ndb: &nostrdb::Ndb,
        account: nostrdb_net::Pubkey,
        selector: &str,
    ) -> Option<SessionId> {
        let txn = Transaction::new(ndb).ok()?;
        let live = session_loader::load_session_states_for_author(ndb, &txn, &account);
        let deleted = session_loader::load_deleted_session_states_for_author(ndb, &txn, &account);
        let state =
            match session_loader::resolve_session_including_deleted(&live, &deleted, selector) {
                Ok(state) => state.clone(),
                Err(err) => {
                    tracing::warn!("reopen_session: {}", err);
                    return None;
                }
            };

        // Already materialized (e.g. resuming a still-live session) — just focus.
        let already_open = self
            .session_manager
            .iter()
            .find(|session| {
                session.agentic.as_ref().map(|a| a.event_session_id())
                    == Some(state.claude_session_id.as_str())
            })
            .map(|session| session.id);
        if let Some(existing) = already_open {
            self.session_manager.switch_to(existing);
            return Some(existing);
        }

        let backend = state
            .backend
            .as_deref()
            .and_then(BackendType::from_tag_str)
            .unwrap_or(BackendType::Claude);

        // resume_session_id is (re)derived inside the hydrator from cli_session;
        // seed empty here.
        let dave_sid = self.session_manager.new_resumed_session(
            PathBuf::from(&state.cwd),
            String::new(),
            state.title.clone(),
            AiMode::Agentic,
            backend,
        );

        let loaded = session_loader::load_session_messages_for_author(
            ndb,
            &txn,
            &account,
            &state.claude_session_id,
        );

        if let Some(session) = self.session_manager.get_mut(dave_sid) {
            tracing::info!(
                "reopening session '{}' ({}): {} messages",
                state.title,
                state.claude_session_id,
                loaded.messages.len(),
            );
            hydrate_session_from_state(session, &state, loaded, &self.hostname);
            // Force a fresh active revision for the (possibly tombstoned) d-tag:
            // publish_dirty_session_states emits status != deleted at a created_at
            // above the tombstone, reviving it in both folds.
            session.state_dirty = true;
            session.focus_requested = true;
        }

        self.session_manager.rebuild_groups();
        if self.show_scene {
            self.scene.select(dave_sid);
        }
        self.session_manager.switch_to(dave_sid);
        Some(dave_sid)
    }

    /// Process pending archive conversion (JSONL to nostr events).
    ///
    /// When resuming a session, the JSONL archive needs to be converted to
    /// nostr events. If events already exist in ndb, load them directly.
    /// Restore a resumed session's history + identity once its kind-1988 events
    /// are in ndb (the session-picker resume path).
    ///
    /// Prefers the full [`hydrate_session_from_state`] when a kind-31988 state
    /// event exists for the session; otherwise (a fresh JSONL import that never
    /// had a dave state) it pins the Nostr identity to the d-tag the messages are
    /// keyed by and loads history + dedup directly. Either way it restores the
    /// three things the old picker path dropped: `event_id`, the `seen_note_ids`
    /// dedup set, and the `responded` permission map — so a resumed session keeps
    /// its `agentium:` identity and doesn't double-append its own history.
    fn load_resumed_session_history(
        &mut self,
        ndb: &nostrdb::Ndb,
        account: nostrdb_net::Pubkey,
        dave_sid: SessionId,
        claude_sid: &str,
    ) {
        let txn = Transaction::new(ndb).expect("txn");
        let loaded =
            session_loader::load_session_messages_for_author(ndb, &txn, &account, claude_sid);
        tracing::info!("loaded {} messages into chat UI", loaded.messages.len());

        if let Some(state) =
            session_loader::latest_valid_session_for_author(ndb, &txn, &account, claude_sid)
        {
            if let Some(session) = self.session_manager.get_mut(dave_sid) {
                hydrate_session_from_state(session, &state, loaded, &self.hostname);
            }
            return;
        }

        // No kind-31988 state yet: pin identity to the d-tag and load the
        // history/dedup subset the full hydrator does (the picker already set
        // title/cwd/hostname/resume id at session creation).
        let Some(session) = self.session_manager.get_mut(dave_sid) else {
            return;
        };
        session.chat = loaded.messages;
        if let Some(agentic) = &mut session.agentic {
            agentic.event_id = claude_sid.to_string();
            if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id) {
                agentic.live_threading.seed(root, last);
            }
            agentic.permissions.merge_loaded(loaded.permissions);
            agentic.seen_note_ids = loaded.note_ids;
        }
    }

    pub(crate) fn process_archive_conversion(&mut self, ctx: &mut AppContext<'_>) {
        let Some((file_path, dave_sid, claude_sid)) = self.pending_archive_convert.take() else {
            return;
        };

        let account = *ctx.accounts.selected_account_pubkey();
        let txn = Transaction::new(ctx.ndb).expect("txn");
        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
            .authors([account.bytes()])
            .tags([claude_sid.as_str()], 'd')
            .limit(1)
            .build();
        let already_exists = ctx
            .ndb
            .query(&txn, &[filter], 1)
            .map(|r| !r.is_empty())
            .unwrap_or(false);
        drop(txn);

        if already_exists {
            tracing::info!(
                "session {} already has events in ndb, skipping archive conversion",
                claude_sid
            );
            self.load_resumed_session_history(ctx.ndb, account, dave_sid, &claude_sid);
        } else if let Some(secret_bytes) =
            secret_key_bytes(ctx.accounts.get_selected_account().keypair())
        {
            let sub_filter = nostrdb::Filter::new()
                .kinds([session_events::AI_CONVERSATION_KIND as u64])
                .authors([account.bytes()])
                .tags([claude_sid.as_str()], 'd')
                .build();

            match ctx.ndb.subscribe(&[sub_filter]) {
                Ok(sub) => {
                    match session_converter::convert_session_to_events(
                        &file_path,
                        ctx.ndb,
                        &secret_bytes,
                    ) {
                        Ok(note_ids) => {
                            tracing::info!(
                                "archived session: {} events from {}, awaiting indexing",
                                note_ids.len(),
                                file_path.display()
                            );
                            self.pending_message_load = Some(PendingMessageLoad {
                                sub,
                                account,
                                dave_session_id: dave_sid,
                                claude_session_id: claude_sid,
                            });
                        }
                        Err(e) => {
                            tracing::error!("archive conversion failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("failed to subscribe for archive events: {:?}", e);
                }
            }
        } else {
            tracing::warn!("no secret key available for archive conversion");
        }
    }

    /// Poll for pending message load completion.
    ///
    /// After archive conversion, wait for ndb to index the kind-1988 events,
    /// then load them into the session's chat history.
    pub(crate) fn poll_pending_message_load(&mut self, ndb: &nostrdb::Ndb) {
        let Some(pending) = &self.pending_message_load else {
            return;
        };

        let notes = ndb.poll_for_notes(pending.sub, 4096);
        if notes.is_empty() {
            return;
        }

        // Copy out what we need, then drop the borrow so the shared hydrator can
        // take &mut self.
        let account = pending.account;
        let dave_sid = pending.dave_session_id;
        let claude_sid = pending.claude_session_id.clone();
        self.pending_message_load = None;

        self.load_resumed_session_history(ndb, account, dave_sid, &claude_sid);
    }
}

/// Check if a session state represents a remote session.
///
/// A session is remote if its hostname differs from the local hostname,
/// or (for old events without hostname) if the cwd doesn't exist locally.
fn is_session_remote(hostname: &str, cwd: &str, local_hostname: &str) -> bool {
    (!hostname.is_empty() && hostname != local_hostname)
        || (hostname.is_empty() && !std::path::PathBuf::from(cwd).exists())
}

/// Hydrate an already-created session from its persisted kind-31988
/// [`SessionState`](session_loader::SessionState) and loaded kind-1988 history.
///
/// This is the single place that turns "a state event + its conversation" into a
/// live [`ChatSession`], shared by startup restore, relay discovery, the session
/// picker resume, and `agentium resume`. Getting it right in one spot is what
/// keeps a resumed session's Nostr **identity** intact — `agentic.event_id` is
/// repointed at the d-tag so future state events keep the same `agentium:` ref
/// (and, for a tombstoned session, a later active publish revives it) — and its
/// **history** present (chat, threading seed, permission state, dedup set).
///
/// The caller owns session *creation* (`new_resumed_session`) and any
/// path-specific setup (placeholder upgrade, title) done before calling this.
/// `mark_activity` is issued here *before* the `agentic` borrow to avoid a double
/// mutable borrow of `session`.
fn hydrate_session_from_state(
    session: &mut ChatSession,
    state: &session_loader::SessionState,
    loaded: session_loader::LoadedSession,
    local_hostname: &str,
) {
    session.chat = loaded.messages;

    if is_session_remote(&state.hostname, &state.cwd, local_hostname) {
        session.source = session::SessionSource::Remote;
    }

    // Local sessions use the current machine's hostname; remote sessions use
    // what was stored in the event.
    session.details.hostname = if session.is_remote() {
        state.hostname.clone()
    } else {
        local_hostname.to_string()
    };

    session.details.custom_title = state.custom_title.clone();
    session.spawn_id = state.spawn_id.clone();

    // Restore focus indicator from the state event.
    session.indicator = state
        .indicator
        .as_deref()
        .and_then(focus_queue::FocusPriority::from_indicator_str);

    // Use home_dir from the event for remote abbreviation.
    if !state.home_dir.is_empty() {
        session.details.home_dir = state.home_dir.clone();
    }

    // Resolve the project the cwd belongs to (git repo grouping). Remote sessions
    // rely on the persisted tags since git isn't available for their cwd; local
    // sessions recompute from git, which stays authoritative even for old events
    // that predate the project tags. `hydrate` runs at load/open, not per frame,
    // so the `project_for` git spawn here is acceptable.
    if session.is_remote() {
        session.details.project_slug = state.project.clone();
        session.details.project_root = state.project_root.as_ref().map(PathBuf::from);
    } else if let Some(cwd) = session.details.cwd.clone() {
        let project = crate::worktree::project_for(&cwd);
        session.details.project_slug = Some(project.slug);
        session.details.project_root = Some(project.root);
    }

    // A state event is a "host is alive" signal; feed the status-bar
    // last-activity indicator (before borrowing agentic).
    session.mark_activity(state.created_at);

    if let Some(agentic) = &mut session.agentic {
        // Restore the event_id from the d-tag so published state events keep
        // using the same Nostr identity.
        agentic.event_id = state.claude_session_id.clone();

        // The cli_session tag holds the real CLI id for `claude --resume`. An
        // empty value means the backend never started (nothing to resume, so we
        // must not pass the event UUID as a session id); an absent tag is a
        // legacy event where the d-tag itself was the CLI id. Setting this here
        // (rather than only at session creation) keeps upgraded placeholders —
        // which are born without agentic data — correctly resumable.
        agentic.resume_session_id = match state.cli_session_id {
            Some(ref cli) if !cli.is_empty() => Some(cli.clone()),
            Some(_) => None,
            None => Some(state.claude_session_id.clone()),
        };

        if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id) {
            agentic.live_threading.seed(root, last);
        }
        // Load permission state and dedup set from events.
        agentic.permissions.merge_loaded(loaded.permissions);
        agentic.seen_note_ids = loaded.note_ids;
        // Set remote status and permission mode from the state event.
        agentic.remote_status = AgentStatus::from_status_str(&state.status);
        agentic.remote_status_ts = state.created_at;
        if let Some(ref pm) = state.permission_mode {
            agentic.permission_mode = crate::session::permission_mode_from_str(pm);
        }
        // Live conversation events flow through the shared per-account
        // subscription; no per-session subscription needed here.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiMode;
    use crate::session_events::{build_live_event, ThreadingState};
    use crate::tests::{test_config, test_dave, test_secret_key};
    use crate::{PnsLocalState, SessionManager};
    use nostrdb::{IngestMetadata, Ndb};
    use notedeck::DataPath;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// A `SessionState` with the fields the hydrator reads, defaulted so a test
    /// only sets what it cares about.
    fn hydrate_test_state(
        claude_session_id: &str,
        cli: Option<&str>,
    ) -> session_loader::SessionState {
        session_loader::SessionState {
            claude_session_id: claude_session_id.to_string(),
            title: "restored title".to_string(),
            custom_title: Some("custom".to_string()),
            cwd: "/tmp/proj".to_string(),
            status: "working".to_string(),
            indicator: None,
            hostname: "my-host".to_string(),
            home_dir: "/home/me".to_string(),
            backend: Some("claude".to_string()),
            permission_mode: None,
            created_at: 1_770_000_123,
            cli_session_id: cli.map(str::to_string),
            spawn_id: Some("spawn-xyz".to_string()),
            project: None,
            project_root: None,
        }
    }

    /// The shared hydrator restores a resumed session's Nostr identity (event_id
    /// = the d-tag), its resume id, and its dedup set — the fields the old
    /// SessionPicker resume path dropped. This is the regression guard for the
    /// "resume doesn't carry history/identity" bug.
    #[test]
    fn hydrator_restores_identity_and_resume_id() {
        let mut manager = SessionManager::new();
        // Born with a fresh random event_id and no resume id — as if just created.
        let sid = manager.new_resumed_session(
            PathBuf::from("/tmp/proj"),
            String::new(),
            "placeholder".to_string(),
            AiMode::Agentic,
            BackendType::Claude,
        );

        let state = hydrate_test_state("dead-dtag", Some("cli-uuid-123"));
        let mut note_ids = HashSet::new();
        note_ids.insert([7u8; 32]);
        let loaded = session_loader::LoadedSession {
            messages: Vec::new(),
            orders: Vec::new(),
            root_note_id: None,
            last_note_id: None,
            permissions: session::PermissionTracker::new(),
            note_ids: note_ids.clone(),
            max_order: None,
        };

        let session = manager.get_mut(sid).unwrap();
        let fresh_event_id = session.agentic.as_ref().unwrap().event_id.clone();
        hydrate_session_from_state(session, &state, loaded, "my-host");

        let agentic = manager.get_mut(sid).unwrap().agentic.as_ref().unwrap();
        // Identity is repointed off the fresh UUID onto the persisted d-tag.
        assert_ne!(agentic.event_id, fresh_event_id);
        assert_eq!(agentic.event_id, "dead-dtag");
        // The real CLI id is what `claude --resume` needs.
        assert_eq!(agentic.resume_session_id.as_deref(), Some("cli-uuid-123"));
        // Dedup set seeded so live polling won't double-append restored notes.
        assert_eq!(agentic.seen_note_ids, note_ids);
    }

    /// An empty `cli_session` means the backend never started: there is nothing
    /// to `--resume`, so the hydrator must leave `resume_session_id` cleared
    /// rather than pass the event UUID as a bogus CLI id.
    #[test]
    fn hydrator_clears_resume_id_when_backend_never_started() {
        let mut manager = SessionManager::new();
        let sid = manager.new_resumed_session(
            PathBuf::from("/tmp/proj"),
            "stale".to_string(),
            "placeholder".to_string(),
            AiMode::Agentic,
            BackendType::Claude,
        );

        let state = hydrate_test_state("dead-dtag", Some(""));
        let loaded = session_loader::LoadedSession {
            messages: Vec::new(),
            orders: Vec::new(),
            root_note_id: None,
            last_note_id: None,
            permissions: session::PermissionTracker::new(),
            note_ids: HashSet::new(),
            max_order: None,
        };

        let session = manager.get_mut(sid).unwrap();
        hydrate_session_from_state(session, &state, loaded, "my-host");

        let agentic = manager.get_mut(sid).unwrap().agentic.as_ref().unwrap();
        assert_eq!(agentic.event_id, "dead-dtag");
        assert_eq!(agentic.resume_session_id, None);
    }

    /// Reopening a soft-deleted session materializes it from ndb with its
    /// *original* identity (event_id = the d-tag), its full history, and its CLI
    /// resume id — and marks it dirty so the next state publish overwrites the
    /// tombstone. The end-to-end guard for `agentium resume` and the deleted-chip
    /// resume button.
    #[tokio::test]
    async fn reopen_revives_deleted_session_with_history() {
        let sk = test_secret_key();
        let account = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        let sid = "revive-me";

        let base_dir = TempDir::new().unwrap();
        let data_path = DataPath::new(base_dir.path());
        let mut dave = test_dave(&data_path);
        // Publish the tombstone under dave's own hostname so the reopened session
        // is treated as local (and thus publishes an active state to revive it).
        let host = dave.hostname.clone();

        let tmp = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp.path().to_str().unwrap(), &test_config()).unwrap();

        // Two conversation messages keyed by the session's d-tag.
        let mut threading = ThreadingState::new();
        let m1 =
            build_live_event("hello", "user", sid, None, None, None, &mut threading, &sk).unwrap();
        let m2 = build_live_event(
            "hi there",
            "assistant",
            sid,
            None,
            None,
            None,
            &mut threading,
            &sk,
        )
        .unwrap();
        // A tombstone (status=deleted) as the latest state revision, carrying the
        // real CLI session id for `claude --resume`.
        let tomb = session_events::build_session_state_event(
            sid,
            "Dead Session",
            None,
            "/tmp/proj",
            "deleted",
            None,
            &host,
            "/home/dev",
            "claude",
            "default",
            Some("cli-abc"),
            None,
            None,
            None,
            1_000,
            &sk,
        )
        .unwrap();

        let filter = nostrdb::Filter::new().build();
        let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
        for ev in [&m1, &m2, &tomb] {
            ndb.process_event_with(&ev.to_event_json(), IngestMetadata::new().client(true))
                .unwrap();
        }
        // `wait_for_notes` treats its count as a per-await *maximum* and returns
        // the first batch the stream yields, so it can hand back one note key
        // while the other two are still being ingested. Every assertion below
        // then reads a half-populated db. `wait_for_all_notes` accumulates until
        // all three have landed.
        ndb.wait_for_all_notes(sub, 3).await.unwrap();

        let reopened = dave
            .reopen_session(&ndb, account, sid)
            .expect("reopen resolves the tombstone");

        let session = dave
            .session_manager
            .get_mut(reopened)
            .expect("session materialized");
        let agentic = session.agentic.as_ref().unwrap();
        assert_eq!(
            agentic.event_id, sid,
            "keeps the original d-tag identity (revive in place)"
        );
        assert_eq!(
            agentic.resume_session_id.as_deref(),
            Some("cli-abc"),
            "resumes the real CLI session"
        );
        assert_eq!(session.chat.len(), 2, "history is rehydrated");
        assert!(
            session.state_dirty,
            "dirty so the next publish emits an active revision that overwrites the tombstone"
        );
    }

    /// Background restore ([`Dave::drain_session_restore`]) streams sessions in
    /// over many frames. It must never steal focus from the session the user is
    /// already on — a hazard the old *synchronous* restore sidestepped only by
    /// finishing within a single frame. Seed several persisted sessions, pin an
    /// existing active session, drive the drain to completion, and assert focus
    /// never moves while every seeded session still materializes.
    #[tokio::test]
    async fn background_restore_preserves_active_session() {
        let sk = test_secret_key();
        let account = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;

        let base_dir = TempDir::new().unwrap();
        let data_path = DataPath::new(base_dir.path());
        let mut dave = test_dave(&data_path);
        let host = dave.hostname.clone();

        // Seed several persisted (kind-31988) sessions in a local ndb, authored
        // by the account — the store the restore worker reads.
        let ndb_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(ndb_dir.path().to_str().unwrap(), &test_config()).unwrap();
        const SEEDED: usize = 6;
        let filter = nostrdb::Filter::new().build();
        let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
        for i in 0..SEEDED {
            let state = session_events::build_session_state_event(
                &format!("restore-{i}"),
                &format!("Restored {i}"),
                None,
                "/tmp/proj",
                "idle",
                None,
                &host,
                "/home/dev",
                "claude",
                "default",
                Some(&format!("cli-{i}")),
                None,
                None,
                None,
                1_000 + i as u64,
                &sk,
            )
            .unwrap();
            ndb.process_event_with(&state.to_event_json(), IngestMetadata::new().client(true))
                .unwrap();
        }
        ndb.wait_for_all_notes(sub, SEEDED as u32).await.unwrap();

        // The session the user is on before restore begins.
        let user_sid = dave.session_manager.new_resumed_session(
            PathBuf::from("/tmp/user"),
            String::new(),
            "User".to_string(),
            AiMode::Agentic,
            BackendType::Claude,
        );
        assert_eq!(dave.session_manager.active_id(), Some(user_sid));

        // Restore reads `pns_local_state.account` to drop stale cross-account
        // results, so it must be set for the drain to apply anything.
        dave.pns_local_state = Some(PnsLocalState {
            account,
            has_secret_key: true,
        });

        // A noop waker: this test drives the drain itself rather than
        // waiting to be woken.
        let waker = notedeck::Waker::noop();
        dave.session_restore_loader
            .start(waker.clone(), ndb.clone());
        dave.session_restore_loader.restore_account(account);

        // Drive the drain one "frame" at a time until every seeded session has
        // materialized, asserting focus stays put on each frame.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while dave.session_manager.len() < SEEDED + 1 {
            dave.drain_session_restore(&waker);
            assert_eq!(
                dave.session_manager.active_id(),
                Some(user_sid),
                "background restore must not steal focus from the user's active session"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "restore did not materialize all sessions in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert_eq!(
            dave.session_manager.len(),
            SEEDED + 1,
            "every seeded session is restored alongside the user's session"
        );
        assert_eq!(
            dave.session_manager.active_id(),
            Some(user_sid),
            "focus remains on the user's session after restore completes"
        );
    }

    /// Clicking a deleted `agentium:` chip routes by the session's host: a
    /// tombstone owned by *another* host queues a kind-31989 resume command
    /// (asking that host to reopen it) plus a "Connecting…" placeholder keyed by
    /// that command's spawn_id, while one owned by *this* host is revived +
    /// resumed in place with no command emitted. The end-to-end guard for "resume
    /// a chip from any remote on the correct host".
    #[tokio::test]
    async fn deleted_chip_routes_remote_resume_but_revives_local() {
        let sk = test_secret_key();
        let account = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;

        let base_dir = TempDir::new().unwrap();
        let data_path = DataPath::new(base_dir.path());
        let mut dave = test_dave(&data_path);
        dave.pns_local_state = Some(PnsLocalState {
            account,
            has_secret_key: true,
        });
        let local_host = dave.hostname.clone();

        let tmp = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp.path().to_str().unwrap(), &test_config()).unwrap();

        // Build a tombstone (status=deleted) for a session on `host`, carrying the
        // CLI session id for `claude --resume`.
        let tombstone = |sid: &str, host: &str| {
            session_events::build_session_state_event(
                sid,
                "Dead Session",
                None,
                "/tmp/proj",
                "deleted",
                None,
                host,
                "/home/dev",
                "claude",
                "default",
                Some("cli-abc"),
                None,
                None,
                None,
                1_000,
                &sk,
            )
            .unwrap()
        };
        let remote_tomb = tombstone("remote-sess", "other-host");
        let local_tomb = tombstone("local-sess", &local_host);

        let filter = nostrdb::Filter::new().build();
        let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
        for ev in [&remote_tomb, &local_tomb] {
            ndb.process_event_with(&ev.to_event_json(), IngestMetadata::new().client(true))
                .unwrap();
        }
        // `wait_for_all_notes` accumulates to the full count; `wait_for_notes`
        // can return after the first tombstone, leaving the second invisible to
        // the `process_pending_open` lookups below.
        ndb.wait_for_all_notes(sub, 2).await.unwrap();

        // Click the remote chip: a resume command is queued for its host, and a
        // "Connecting…" placeholder stands in until the owning host revives it.
        dave.pending_open = Some(nostrdb_net::NoteId::new(remote_tomb.note_id));
        dave.process_pending_open(&ndb);
        assert_eq!(
            dave.pending_resume_commands.len(),
            1,
            "remote chip queues exactly one resume command",
        );
        let cmd = &dave.pending_resume_commands[0];
        assert_eq!(cmd.target_host, "other-host", "targets the session's host");
        assert_eq!(cmd.target_session_id, "remote-sess", "names the session");
        assert_eq!(cmd.cli_session_id, "cli-abc", "carries the CLI resume id");

        // The only materialized session is a pending placeholder correlated by the
        // target session's d-tag — no session identity yet (agentic is None), so
        // the revived kind-31988 (which comes back on that d-tag) upgrades it in
        // place rather than adding a duplicate chip.
        assert_eq!(
            dave.session_manager.iter().count(),
            1,
            "a remote resume materializes only the Connecting… placeholder",
        );
        let placeholder = dave
            .session_manager
            .iter()
            .find(|s| s.pending_created_at.is_some())
            .expect("a pending placeholder for the remote resume");
        assert_eq!(
            placeholder.pending_resume_target.as_deref(),
            Some("remote-sess"),
            "placeholder is correlated by the target session's d-tag",
        );
        assert!(
            placeholder.agentic.is_none(),
            "placeholder carries no session identity until the host revives it",
        );
        // pending_placeholder_for finds it by that d-tag (spawn_id absent), the
        // exact lookup the discovery fold uses to upgrade in place.
        let placeholder_id = placeholder.id;
        assert_eq!(
            dave.pending_placeholder_for(None, "remote-sess"),
            Some(placeholder_id),
            "the discovery fold correlates the revived state to this placeholder",
        );

        // Re-clicking the same deleted chip before the host answers focuses the
        // existing placeholder instead of queuing a second command / stranding a
        // duplicate placeholder.
        dave.pending_open = Some(nostrdb_net::NoteId::new(remote_tomb.note_id));
        dave.process_pending_open(&ndb);
        assert_eq!(
            dave.pending_resume_commands.len(),
            1,
            "re-click does not queue a second resume command",
        );
        assert_eq!(
            dave.session_manager
                .iter()
                .filter(|s| s.pending_created_at.is_some())
                .count(),
            1,
            "re-click does not create a second placeholder",
        );
        assert_eq!(
            dave.session_manager.active_id(),
            Some(placeholder_id),
            "re-click focuses the existing placeholder",
        );

        // Click the local chip: it is revived in place (materialized, dirty) with
        // no additional resume command emitted.
        dave.pending_open = Some(nostrdb_net::NoteId::new(local_tomb.note_id));
        dave.process_pending_open(&ndb);
        assert_eq!(
            dave.pending_resume_commands.len(),
            1,
            "a local resume revives in place, emitting no command",
        );
        let revived = dave
            .session_manager
            .iter()
            .find(|s| {
                s.agentic
                    .as_ref()
                    .is_some_and(|a| a.event_session_id() == "local-sess")
            })
            .expect("local session materialized in place");
        assert!(revived.state_dirty, "dirty so the tombstone is overwritten");
    }

    /// The discovery fold correlates a spawn placeholder by its echoed spawn_id
    /// and a resume placeholder by the target session's d-tag. The resume case
    /// must hold *regardless* of the revived state's spawn_id — an older revision
    /// re-delivered over PNS carries a stale/absent spawn_id but the same d-tag,
    /// and it must still upgrade the placeholder rather than strand it (the bug
    /// where "Connecting…" lingered until the pending timeout).
    #[test]
    fn pending_placeholder_for_matches_spawn_by_id_and_resume_by_dtag() {
        let base_dir = TempDir::new().unwrap();
        let data_path = DataPath::new(base_dir.path());
        let mut dave = test_dave(&data_path);

        let spawn_id = dave.session_manager.new_pending_placeholder(
            PathBuf::from("/tmp/proj"),
            "host-a".to_string(),
            BackendType::Claude,
            "spawn-Y".to_string(),
            None,
        );
        let resume_id = dave.session_manager.new_pending_placeholder(
            PathBuf::from("/tmp/proj"),
            "host-a".to_string(),
            BackendType::Claude,
            "spawn-X".to_string(),
            Some("sess-D".to_string()),
        );

        // Spawn placeholder: matched by the echoed spawn_id, not by d-tag.
        assert_eq!(
            dave.pending_placeholder_for(Some("spawn-Y"), "sess-unrelated"),
            Some(spawn_id),
        );
        // Resume placeholder: matched by d-tag even when no spawn_id is offered...
        assert_eq!(
            dave.pending_placeholder_for(None, "sess-D"),
            Some(resume_id),
        );
        // ...and even when the revived state carries a *different* spawn_id (the
        // PNS-reordering case the d-tag correlation is meant to survive).
        assert_eq!(
            dave.pending_placeholder_for(Some("some-stale-spawn"), "sess-D"),
            Some(resume_id),
        );
        // A d-tag no placeholder is waiting on materializes a fresh session.
        assert_eq!(
            dave.pending_placeholder_for(Some("nope"), "sess-none"),
            None
        );

        // Once a placeholder is upgraded (no longer pending), it stops matching.
        dave.session_manager
            .get_mut(resume_id)
            .unwrap()
            .pending_created_at = None;
        assert_eq!(dave.pending_placeholder_for(None, "sess-D"), None);
    }
}
