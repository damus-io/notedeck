//! Materializing sessions from their persisted kind-31988 state: background
//! restore, live discovery, reopening (reviving) a closed session, focusing
//! one from a clicked `agentium:` chip, and loading a resumed session's
//! history. [`hydrate_session_from_state`] is the one hydrator they share.

use crate::agent_status::AgentStatus;
use crate::backend::BackendType;
use crate::conversation_feed::{self, ConversationFeed};
use crate::{
    focus_queue, get_backend, secret_key_bytes, session, session_converter, session_events,
    session_loader, session_restore_loader, update, AiMode, ChatSession, Dave, DaveOverlay,
    SessionId,
};
use nostrdb::{NoteKey, Subscription, Transaction};
use notedeck::{AppContext, Waker};
use std::collections::HashMap;
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

/// A request to focus a session raised from elsewhere in the app — a click on
/// its inline `agentium:` chip, or an open by URI — waiting for the next
/// [`update`](notedeck::App::update). See [`Dave::open_with_message`].
pub struct PendingOpen {
    /// The kind-31988 session-state note to focus.
    pub note: nostrdb_net::NoteId,
    /// Text to send into the session once it's focused, as if typed and
    /// submitted (`OpenUri.msg`). `None` for a plain chip click.
    pub msg: Option<String>,
}

/// The session a [`PendingOpen`] focused, handed back to
/// [`update`](notedeck::App::update) so it can deliver the request's message
/// (see [`Dave::deliver_open_message`]).
pub(crate) struct OpenedSession {
    /// The focused session.
    pub(crate) session: SessionId,
    /// Whether it was already materialized when the open landed. A session that
    /// was only just reopened from its tombstone, or is still a "Connecting…"
    /// placeholder for a remote resume, isn't ready to take a message yet.
    pub(crate) was_live: bool,
    /// The request's message, if it carried one.
    pub(crate) msg: Option<String>,
}

/// What [`Dave::focus_session_note`] did with a session-state note.
enum FocusOutcome {
    /// It focused this session.
    Focused {
        /// The focused session.
        session: SessionId,
        /// See [`OpenedSession::was_live`].
        was_live: bool,
    },
    /// No read transaction this frame; try again next frame.
    Retry,
    /// The note doesn't resolve to a session we can open.
    Unresolved,
}

/// Where [`Dave::deliver_open_message`] puts an open request's message.
#[derive(Debug, PartialEq, Eq)]
enum OpenMessageDelivery {
    /// Submit it, exactly as the input box's Enter does.
    Send,
    /// Leave it in the session's input draft, for this reason, so nothing is
    /// lost when the session can't take input yet.
    Draft(&'static str),
}

impl Dave {
    /// Act on a pending [`open`](Self::open): resolve the clicked kind-31988 note to
    /// one of this account's sessions (by its `claude_session_id` d-tag) and switch
    /// to it, revealing the chat. Returns the focused session, with the request's
    /// message, for [`deliver_open_message`](Self::deliver_open_message).
    ///
    /// The request is consumed exactly once: it is put back only when no read
    /// transaction could be opened this frame, and an unresolvable note drops it
    /// (logging any message it carried) rather than retrying forever.
    pub(crate) fn process_pending_open(&mut self, ndb: &nostrdb::Ndb) -> Option<OpenedSession> {
        let pending = self.pending_open.take()?;
        match self.focus_session_note(ndb, pending.note) {
            FocusOutcome::Focused { session, was_live } => Some(OpenedSession {
                session,
                was_live,
                msg: pending.msg,
            }),
            FocusOutcome::Retry => {
                self.pending_open = Some(pending);
                None
            }
            FocusOutcome::Unresolved => {
                if pending.msg.is_some() {
                    tracing::warn!(
                        "open: {} isn't a session we can open; dropping its message",
                        pending.note.hex()
                    );
                }
                None
            }
        }
    }

    /// Focus the session whose kind-31988 state note is `note_id`.
    ///
    /// A materialized session is simply focused. A session that *isn't*
    /// materialized — a soft-deleted (tombstoned) session, or one not yet restored
    /// — is [reopened](Self::reopen_session): clicking a deleted `agentium:` chip
    /// revives and resumes a session we own (or surfaces history for a remote one)
    /// rather than doing nothing.
    fn focus_session_note(
        &mut self,
        ndb: &nostrdb::Ndb,
        note_id: nostrdb_net::NoteId,
    ) -> FocusOutcome {
        // The session's stable event id (the kind-31988 `d` tag), if this note is a
        // session-state event. Scope the read txn so it drops before reopen below.
        let event_id: Option<String> = {
            let Ok(txn) = Transaction::new(ndb) else {
                return FocusOutcome::Retry;
            };
            ndb.get_note_by_id(&txn, note_id.bytes())
                .ok()
                .and_then(|note| session_events::get_tag_value(&note, "d").map(|s| s.to_string()))
        };
        let Some(event_id) = event_id else {
            // Not a session-state note we can route to.
            return FocusOutcome::Unresolved;
        };

        // Already materialized — just focus it.
        if let Some(session_id) = self.session_id_for_event_id(&event_id) {
            if !self.session_manager.switch_to(session_id) {
                return FocusOutcome::Unresolved;
            }
            // Reveal the chat: clear any overlay (directory/session picker) and
            // the mobile session-list drawer, and stop auto-steal fighting the
            // switch — dequeue this session's own entry and anchor auto-steal
            // here so it doesn't immediately yank onto a *different* session.
            self.active_overlay = DaveOverlay::None;
            self.show_session_list = false;
            self.focus_queue.dequeue(session_id);
            self.anchor_focus(session_id);
            return FocusOutcome::Focused {
                session: session_id,
                was_live: true,
            };
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
            return FocusOutcome::Focused {
                session: placeholder_id,
                was_live: false,
            };
        }

        // Not materialized: a soft-deleted (or not-yet-restored) session. Reopen
        // it instead of dropping the click — the deleted-chip resume affordance.
        let Some(account) = self.pns_local_state.as_ref().map(|state| state.account) else {
            return FocusOutcome::Unresolved;
        };

        // Resolve the target session's state (across live + tombstoned) so we can
        // route by host: a session on THIS host is revived + resumed locally; one
        // on ANOTHER host can't be (there's no local backend to `claude --resume`
        // it), so we publish a resume command asking its own host to reopen it.
        let state = {
            let Ok(txn) = Transaction::new(ndb) else {
                return FocusOutcome::Retry;
            };
            let live = session_loader::load_session_states_for_author(ndb, &txn, &account);
            let deleted =
                session_loader::load_deleted_session_states_for_author(ndb, &txn, &account);
            session_loader::resolve_session_including_deleted(&live, &deleted, &event_id)
                .ok()
                .cloned()
        };
        let Some(state) = state else {
            return FocusOutcome::Unresolved;
        };

        if !state.hostname.is_empty() && state.hostname != self.hostname {
            // Remote session: ask its host to reopen + revive + resume it, and
            // stand up a "Connecting…" placeholder for immediate feedback (see
            // queue_resume_command). The revived kind-31988 state streams back
            // over the shared subscription and upgrades the placeholder in place.
            self.queue_resume_command(&state);
            self.show_session_list = false;
            return match self.pending_placeholder_for(None, &event_id) {
                Some(placeholder_id) => FocusOutcome::Focused {
                    session: placeholder_id,
                    was_live: false,
                },
                None => FocusOutcome::Unresolved,
            };
        }

        // Local session: revive + resume it in place.
        let Some(session_id) = self.reopen_session(ndb, account, &event_id) else {
            return FocusOutcome::Unresolved;
        };
        self.active_overlay = DaveOverlay::None;
        self.show_session_list = false;
        self.focus_queue.dequeue(session_id);
        FocusOutcome::Focused {
            session: session_id,
            was_live: false,
        }
    }

    /// Deliver an open request's message into the session it focused: submit it
    /// through the same path as the input box's Enter
    /// ([`add_user_message_for_session`](Self::add_user_message_for_session) then
    /// [`send_user_message_for`](Self::send_user_message_for)), so a local session
    /// dispatches it to its backend and a remote one publishes it to its host.
    ///
    /// A session that can't take input yet (see
    /// [`open_message_delivery`](Self::open_message_delivery)) gets the message in
    /// its input draft instead, so it is never lost silently.
    pub(crate) fn deliver_open_message(&mut self, opened: OpenedSession, app_ctx: &AppContext) {
        let Some(msg) = opened.msg else {
            return;
        };
        let sid = opened.session;
        match self.open_message_delivery(sid, opened.was_live) {
            OpenMessageDelivery::Send => {
                if self.add_user_message_for_session(sid, app_ctx, msg, Vec::new()) {
                    self.send_user_message_for(sid, app_ctx, app_ctx.waker);
                }
            }
            OpenMessageDelivery::Draft(reason) => {
                let Some(session) = self.session_manager.get_mut(sid) else {
                    tracing::warn!("open: session {sid} vanished; dropping its message");
                    return;
                };
                tracing::info!("open: {reason}; leaving the message in the draft");
                if !session.input.is_empty() {
                    session.input.push_str("\n\n");
                }
                session.input.push_str(&msg);
            }
        }
    }

    /// Whether session `sid`, just focused by an open request, can take that
    /// request's message now or should get it in its draft.
    fn open_message_delivery(&self, sid: SessionId, was_live: bool) -> OpenMessageDelivery {
        if !was_live {
            return OpenMessageDelivery::Draft("the session is still being reopened");
        }
        let Some(session) = self.session_manager.get(sid) else {
            return OpenMessageDelivery::Draft("the session is gone");
        };
        if session.pending_created_at.is_some() {
            return OpenMessageDelivery::Draft("the session is still connecting");
        }
        if !session.is_remote() && !self.backends.contains_key(&session.backend_type) {
            return OpenMessageDelivery::Draft("no local backend runs this session");
        }
        OpenMessageDelivery::Send
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
    ///
    /// The worker's history is from a snapshot older than the conversation
    /// poll, which may have delivered, and dropped, some of the session's
    /// notes before it existed here. Each session is installed with the
    /// history before the first of those only, and they are then handed to it
    /// as the poll would have (see
    /// [`ConversationFeed`](crate::conversation_feed::ConversationFeed)):
    /// every note is history or goes through the poll's processing, once.
    /// Returns the remote user messages that replay produced, for the caller
    /// to dispatch as it does the poll's.
    pub(crate) fn drain_session_restore(
        &mut self,
        ndb: &nostrdb::Ndb,
        secret_key: Option<&[u8; 32]>,
        waker: &Waker,
    ) -> Vec<(SessionId, String)> {
        // Messages tagged with a different account are stale (the user switched
        // accounts while an in-flight restore was streaming); drop them.
        let current = self.pns_local_state.as_ref().map(|state| state.account);
        // Notes the poll dropped for each session installed below, replayed
        // once the drain is done.
        let mut replays: HashMap<SessionId, Vec<NoteKey>> = HashMap::new();

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

                    let drops = self
                        .conversation_feed
                        .as_mut()
                        .map(|feed| feed.take_drops(&state.claude_session_id))
                        .unwrap_or_default();
                    let loaded =
                        self.restored_history(ndb, &account, &state, *loaded, drops.first());
                    if let Some(session) = self.session_manager.get_mut(dave_sid) {
                        hydrate_session_from_state(session, &state, loaded, &self.hostname);
                    }
                    if !drops.is_empty() {
                        replays.insert(dave_sid, drops);
                    }
                    created_any = true;
                }
                session_restore_loader::SessionRestoreMsg::Finished { account, restored } => {
                    if Some(account) != current {
                        continue;
                    }
                    tracing::info!("restored {restored} sessions from ndb");
                    if let Some(feed) = &mut self.conversation_feed {
                        feed.end_restore();
                    }
                }
                session_restore_loader::SessionRestoreMsg::Failed { account, error } => {
                    if Some(account) == current {
                        tracing::error!("session restore failed: {error}");
                        if let Some(feed) = &mut self.conversation_feed {
                            feed.end_restore();
                        }
                    }
                }
            }
        }

        let remote_user_messages = match current {
            Some(account) if !replays.is_empty() => {
                self.deliver_conversation_notes(ndb, secret_key, &account, replays)
            }
            _ => Vec::new(),
        };

        if !created_any {
            return remote_user_messages;
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

        remote_user_messages
    }

    /// The history a background-restored session is installed with: the
    /// worker's fold, cut back to the notes before `first_drop`, the first one
    /// the poll dropped for the session (or to everything the poll has
    /// passed, when it dropped none; see
    /// [`restored_history_bound`](conversation_feed::restored_history_bound)).
    ///
    /// Only a snapshot that reaches past the bound is folded again, here on
    /// the render thread: one that took in a note the poll then delivered, or
    /// has yet to. That takes a note stored in the moment between the last
    /// poll and the worker's read, so it is rare, and it costs one session's
    /// fold, which a rebuild costs anyway.
    fn restored_history(
        &self,
        ndb: &nostrdb::Ndb,
        account: &nostrdb_net::Pubkey,
        state: &session_loader::SessionState,
        loaded: session_loader::LoadedSession,
        first_drop: Option<&NoteKey>,
    ) -> session_loader::LoadedSession {
        let polled_through = self
            .conversation_feed
            .as_ref()
            .and_then(ConversationFeed::polled_through);
        let Some(bound) =
            conversation_feed::restored_history_bound(polled_through, first_drop.copied())
        else {
            return loaded;
        };
        if loaded.max_key.is_none_or(|max_key| max_key <= bound) {
            return loaded;
        }
        let Ok(txn) = Transaction::new(ndb) else {
            return loaded;
        };
        session_loader::load_session_messages_through(
            ndb,
            &txn,
            account,
            &state.claude_session_id,
            bound,
        )
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

            // Load any conversation history that arrived with it, up to where
            // the conversation poll has got: a note past that is still to come
            // through the poll, which processes it.
            let loaded = self.load_session_history(ctx.ndb, &txn, &account, claude_sid);

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

        let loaded = self.load_session_history(ndb, &txn, &account, &state.claude_session_id);

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
    ///
    /// `source` says how much of the store is history: see [`ResumedHistory`].
    fn load_resumed_session_history(
        &mut self,
        ndb: &nostrdb::Ndb,
        account: nostrdb_net::Pubkey,
        dave_sid: SessionId,
        claude_sid: &str,
        source: ResumedHistory,
    ) {
        let txn = Transaction::new(ndb).expect("txn");
        let loaded = match source {
            ResumedHistory::Stored => self.load_session_history(ndb, &txn, &account, claude_sid),
            ResumedHistory::Imported => {
                session_loader::load_session_messages_for_author(ndb, &txn, &account, claude_sid)
            }
        };
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
        if let Some(agentic) = &mut session.agentic {
            agentic.event_id = claude_sid.to_string();
            if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id) {
                agentic.live_threading.seed(root, last);
            }
        }
        crate::conversation::apply_loaded_chat(session, loaded);
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
            self.load_resumed_session_history(
                ctx.ndb,
                account,
                dave_sid,
                &claude_sid,
                ResumedHistory::Stored,
            );
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

        self.load_resumed_session_history(
            ndb,
            account,
            dave_sid,
            &claude_sid,
            ResumedHistory::Imported,
        );
    }
}

/// Where a session-picker resume's history comes from, which decides how much
/// of the store it may claim.
#[derive(Clone, Copy)]
enum ResumedHistory {
    /// Notes already in ndb, which may include one the conversation poll has
    /// yet to deliver (a phone's message): fold only what the poll has passed,
    /// and leave the rest to it.
    Stored,
    /// Notes this host just converted from the session's JSONL archive: all of
    /// them are history, however far the poll has got, and none may be taken
    /// for a live message when the poll reaches it.
    Imported,
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
/// The history goes in with [`apply_loaded_chat`](crate::conversation::apply_loaded_chat),
/// the same install a rebuild does, so a hydrated session also gets its
/// fast-path tail seeded from the fold and its subagent rows indexed (a
/// background subagent outlives the host's restart, and its completion finds
/// its row that way).
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
        // Set remote status and permission mode from the state event.
        agentic.remote_status = AgentStatus::from_status_str(&state.status);
        agentic.remote_status_ts = state.created_at;
        if let Some(ref pm) = state.permission_mode {
            agentic.permission_mode = crate::session::permission_mode_from_str(pm);
        }
        // Live conversation events flow through the shared per-account
        // subscription; no per-session subscription needed here.
    }

    // The history, and the state that points into it (dedup set, permission
    // state, fast-path tail, subagent rows) — the same install a rebuild does.
    crate::conversation::apply_loaded_chat(session, loaded);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiMode;
    use crate::pns_runtime::PnsLocalState;
    use crate::session_events::{build_live_event, LiveEventTags, ThreadingState};
    use crate::tests::{test_config, test_dave, test_secret_key};
    use crate::SessionManager;
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
            max_key: None,
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
            max_key: None,
        };

        let session = manager.get_mut(sid).unwrap();
        hydrate_session_from_state(session, &state, loaded, "my-host");

        let agentic = manager.get_mut(sid).unwrap().agentic.as_ref().unwrap();
        assert_eq!(agentic.event_id, "dead-dtag");
        assert_eq!(agentic.resume_session_id, None);
    }

    /// A restored session gets the bookkeeping a live one has: the fast-path
    /// tail is seeded from the fold, and a background subagent still running
    /// when the host went down is indexed, so its completion (which arrives on
    /// a later wake-up turn) finds its row.
    #[tokio::test]
    async fn hydrator_seeds_tail_and_background_subagent() {
        use crate::messages::{SubagentInfo, SubagentStatus};
        use crate::Message;

        let sk = test_secret_key();
        let account = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        let sid = "restored-subagent";
        let tmp = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp.path().to_str().unwrap(), &test_config()).unwrap();

        let mut threading = ThreadingState::new();
        let live = |content: &str, role: &str, threading: &mut ThreadingState| {
            build_live_event(
                content,
                role,
                sid,
                None,
                LiveEventTags::default(),
                threading,
                &sk,
            )
            .unwrap()
        };
        let user = live("explore the loader", "user", &mut threading);
        let subagent = session_events::build_subagent_event(
            &SubagentInfo {
                task_id: "s1".to_string(),
                description: "Map the loader".to_string(),
                subagent_type: "Explore".to_string(),
                status: SubagentStatus::Running,
                output: String::new(),
                max_output_size: 4000,
                tool_results: Vec::new(),
                background: true,
            },
            sid,
            &mut threading,
            &sk,
        )
        .unwrap();
        let reply = live("it runs in the background", "assistant", &mut threading);

        let filter = nostrdb::Filter::new().build();
        let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
        for ev in [&user, &subagent, &reply] {
            ndb.process_event_with(&ev.to_event_json(), IngestMetadata::new().client(true))
                .unwrap();
        }
        ndb.wait_for_all_notes(sub, 3).await.unwrap();

        let txn = Transaction::new(&ndb).unwrap();
        let loaded = session_loader::load_session_messages_for_author(&ndb, &txn, &account, sid);
        let max_order = loaded.max_order;
        assert!(max_order.is_some());

        let mut manager = SessionManager::new();
        let id = manager.new_resumed_session(
            PathBuf::from("/tmp/proj"),
            String::new(),
            "placeholder".to_string(),
            AiMode::Agentic,
            BackendType::Claude,
        );
        let session = manager.get_mut(id).unwrap();
        hydrate_session_from_state(session, &hydrate_test_state(sid, None), loaded, "my-host");

        let agentic = session.agentic.as_ref().unwrap();
        assert_eq!(agentic.tail_order, max_order, "the tail is seeded");
        assert_eq!(agentic.subagent_indices.get("s1"), Some(&1));
        assert!(matches!(&session.chat[1], Message::Subagent(info) if info.background));

        session.complete_subagent("s1", "mapped it");
        let Message::Subagent(info) = &session.chat[1] else {
            panic!("the subagent row moved");
        };
        assert_eq!(info.status, SubagentStatus::Completed);
    }

    /// A store and a [`Dave`] wired the way `ensure_pns_local_state` wires
    /// them, for driving the restore/poll boundary: the account's key unwraps
    /// PNS envelopes, and the conversation feed is created on demand, so a
    /// test decides which notes predate it.
    struct BoundaryHarness {
        dave: Dave,
        ndb: Ndb,
        sk: [u8; 32],
        account: nostrdb_net::Pubkey,
        threading: ThreadingState,
        _dirs: (TempDir, TempDir),
    }

    impl BoundaryHarness {
        fn new() -> Self {
            let sk = test_secret_key();
            let account = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
                .unwrap()
                .pubkey;
            let base_dir = TempDir::new().unwrap();
            let mut dave = test_dave(&DataPath::new(base_dir.path()));
            dave.pns_local_state = Some(PnsLocalState {
                account,
                has_secret_key: true,
            });
            let ndb_dir = TempDir::new().unwrap();
            let ndb = Ndb::new(ndb_dir.path().to_str().unwrap(), &test_config()).unwrap();
            assert!(ndb.add_key(&sk));
            Self {
                dave,
                ndb,
                sk,
                account,
                threading: ThreadingState::new(),
                _dirs: (base_dir, ndb_dir),
            }
        }

        /// Create the shared conversation subscription: notes stored from
        /// here on come through the poll, earlier ones never do.
        fn subscribe(&mut self) {
            let sub = crate::conversation::subscribe_conversation_events(&self.ndb, self.account)
                .unwrap();
            self.dave.conversation_feed = Some(ConversationFeed::new(sub));
        }

        /// Store a session's kind-31988 state, hosted on `hostname`.
        async fn store_state(&self, sid: &str, hostname: &str) {
            let state = session_events::build_session_state_event(
                sid,
                "Boundary",
                None,
                "/tmp",
                "idle",
                None,
                hostname,
                "/home/dev",
                "claude",
                "default",
                Some("cli-boundary"),
                None,
                None,
                None,
                1_000,
                &self.sk,
            )
            .unwrap();
            let sub = self
                .ndb
                .subscribe(&[nostrdb::Filter::new().build()])
                .unwrap();
            self.ndb
                .process_event_with(&state.to_event_json(), IngestMetadata::new().client(true))
                .unwrap();
            self.ndb.wait_for_notes(sub, 1).await.unwrap();
        }

        /// Store a conversation note PNS-wrapped, as it arrives from another
        /// device, and wait until nostrdb has indexed it (without polling the
        /// conversation feed).
        async fn store_note(&mut self, sid: &str, role: &str, content: &str) {
            let ev = build_live_event(
                content,
                role,
                sid,
                None,
                LiveEventTags::default(),
                &mut self.threading,
                &self.sk,
            )
            .unwrap();
            let sub = self
                .ndb
                .subscribe(&[nostrdb::Filter::new()
                    .kinds([session_events::AI_CONVERSATION_KIND as u64])
                    .build()])
                .unwrap();
            assert!(crate::publish::pns_ingest(
                &self.ndb,
                &ev.note_json,
                &self.sk
            ));
            self.ndb.wait_for_notes(sub, 1).await.unwrap();
        }

        /// One conversation poll; returns the remote user messages and the
        /// envelopes it collected for fan-out.
        fn poll(&mut self) -> (Vec<(SessionId, String)>, Vec<NoteKey>) {
            let mut fan_out = Vec::new();
            let msgs =
                self.dave
                    .poll_remote_conversation_events(&self.ndb, Some(&self.sk), &mut fan_out);
            (msgs, fan_out)
        }

        /// Start the background restore the way `ensure_pns_local_state` does,
        /// and wait until the worker has read its snapshot and sent every
        /// session (without draining any of it).
        async fn run_restore_worker(&mut self) -> notedeck::Waker {
            let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = wakes.clone();
            let waker = notedeck::Waker::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
            if let Some(feed) = &mut self.dave.conversation_feed {
                feed.begin_restore();
            }
            self.dave
                .session_restore_loader
                .start(waker.clone(), self.ndb.clone());
            self.dave
                .session_restore_loader
                .restore_account(self.account);
            // The worker wakes once after `Started` and once after `Finished`.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while wakes.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                assert!(std::time::Instant::now() < deadline, "restore worker hung");
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            waker
        }

        /// Drain everything the worker sent, as `update` does each frame.
        fn drain(&mut self, waker: &notedeck::Waker) -> Vec<(SessionId, String)> {
            self.dave
                .drain_session_restore(&self.ndb, Some(&self.sk), waker)
        }

        fn session_for(&self, sid: &str) -> &ChatSession {
            self.dave
                .session_manager
                .iter()
                .find(|s| {
                    s.agentic
                        .as_ref()
                        .is_some_and(|a| a.event_session_id() == sid)
                })
                .expect("session materialized")
        }
    }

    fn texts_of(chat: &[crate::Message]) -> Vec<&str> {
        chat.iter()
            .filter_map(|m| match m {
                crate::Message::User(msg) => Some(msg.text.as_str()),
                crate::Message::Assistant(msg) => Some(msg.text()),
                _ => None,
            })
            .collect()
    }

    /// A phone's message to a local session, stored after the restore worker
    /// read its snapshot and polled before the drain installed the session,
    /// was in neither: shown by nothing, dispatched by nothing. The drain now
    /// replays it, so it is dispatched once, and the poll fanned it out once.
    #[tokio::test]
    async fn restore_dispatches_a_message_the_poll_dropped() {
        let mut h = BoundaryHarness::new();
        let host = h.dave.hostname.clone();
        let sid = "restore-local-drop";
        h.store_state(sid, &host).await;
        h.store_note(sid, "assistant", "earlier reply").await;
        h.subscribe();
        let waker = h.run_restore_worker().await;

        h.store_note(sid, "user", "from the phone").await;
        let (msgs, fan_out) = h.poll();
        assert!(msgs.is_empty(), "no session yet to hand it to");
        assert_eq!(fan_out.len(), 1, "the poll fans the envelope out");

        let msgs = h.drain(&waker);
        assert_eq!(msgs.len(), 1, "the dropped message is dispatched once");
        assert_eq!(msgs[0].1, "from the phone");
        let session = h.session_for(sid);
        assert!(!session.is_remote());
        assert_eq!(texts_of(&session.chat), ["earlier reply", "from the phone"]);
        assert!(session.should_dispatch_remote_message());

        let (msgs, fan_out) = h.poll();
        assert!(msgs.is_empty() && fan_out.is_empty(), "and never again");
    }

    /// The same drop on a remote session left its chat missing the note until
    /// some later note forced a rebuild; a session that went quiet kept the
    /// gap until restart. The drain's replay shows it straight away.
    #[tokio::test]
    async fn restore_shows_a_remote_note_the_poll_dropped() {
        let mut h = BoundaryHarness::new();
        let sid = "restore-remote-drop";
        h.store_state(sid, "elsewhere").await;
        h.store_note(sid, "assistant", "A").await;
        h.subscribe();
        let waker = h.run_restore_worker().await;

        h.store_note(sid, "assistant", "B").await;
        h.poll();
        h.drain(&waker);

        let session = h.session_for(sid);
        assert!(session.is_remote());
        assert_eq!(texts_of(&session.chat), ["A", "B"], "no later note needed");
    }

    /// A note stored after the subscription but before the worker's read is
    /// in the snapshot *and* was dropped by the poll. It is the poll's, not
    /// history: the snapshot is cut back before it, and it is replayed like
    /// any other drop, so it shows once and is dispatched once.
    #[tokio::test]
    async fn restore_replays_a_dropped_note_inside_its_snapshot() {
        let mut h = BoundaryHarness::new();
        let host = h.dave.hostname.clone();
        let sid = "restore-snapshot-drop";
        h.store_state(sid, &host).await;
        h.store_note(sid, "assistant", "earlier reply").await;
        h.subscribe();
        h.store_note(sid, "user", "from the phone").await;
        h.dave.conversation_feed.as_mut().unwrap().begin_restore();
        h.poll();

        let waker = h.run_restore_worker().await;
        let msgs = h.drain(&waker);

        assert_eq!(msgs.len(), 1, "dispatched once");
        let session = h.session_for(sid);
        assert_eq!(texts_of(&session.chat), ["earlier reply", "from the phone"]);
    }

    /// `agentium resume` of a local session loads its history on the render
    /// thread. A phone's message stored but not yet polled was folded in and
    /// marked seen, so the poll skipped it: shown, never dispatched. The load
    /// now stops where the poll has got, and the poll delivers it.
    #[tokio::test]
    async fn reopen_leaves_an_unpolled_message_to_the_poll() {
        let mut h = BoundaryHarness::new();
        let host = h.dave.hostname.clone();
        let sid = "reopen-unpolled";
        h.store_state(sid, &host).await;
        h.store_note(sid, "assistant", "earlier reply").await;
        h.subscribe();
        // The poll has passed something, so the cursor is set.
        h.store_note("another-session", "assistant", "elsewhere")
            .await;
        h.poll();
        h.store_note(sid, "user", "from the phone").await;

        let reopened = h
            .dave
            .reopen_session(&h.ndb, h.account, sid)
            .expect("reopened");
        let session = h.dave.session_manager.get(reopened).unwrap();
        assert_eq!(texts_of(&session.chat), ["earlier reply"]);

        let (msgs, _) = h.poll();
        assert_eq!(msgs.len(), 1, "the poll delivers it");
        assert_eq!(msgs[0].1, "from the phone");
        let session = h.dave.session_manager.get(reopened).unwrap();
        assert_eq!(texts_of(&session.chat), ["earlier reply", "from the phone"]);
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
        let m1 = build_live_event(
            "hello",
            "user",
            sid,
            None,
            LiveEventTags::default(),
            &mut threading,
            &sk,
        )
        .unwrap();
        let m2 = build_live_event(
            "hi there",
            "assistant",
            sid,
            None,
            LiveEventTags::default(),
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
            dave.drain_session_restore(&ndb, Some(&sk), &waker);
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
        dave.open(nostrdb_net::NoteId::new(remote_tomb.note_id));
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
        dave.open(nostrdb_net::NoteId::new(remote_tomb.note_id));
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
        dave.open(nostrdb_net::NoteId::new(local_tomb.note_id));
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

    // =========================================================================
    // Open with a message (OpenUri `msg`)
    // =========================================================================

    /// A backend that records how many turns it was asked to dispatch and never
    /// streams anything back.
    struct CountingBackend {
        requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl crate::backend::AiBackend for CountingBackend {
        fn stream_request(
            &self,
            _messages: Vec<crate::Message>,
            _tools: std::sync::Arc<std::collections::HashMap<String, crate::tools::Tool>>,
            _model: Option<String>,
            _user_id: String,
            _session_id: String,
            _session_env: std::collections::BTreeMap<String, String>,
            _cwd: Option<PathBuf>,
            _resume_session_id: Option<String>,
            _permission_mode: claude_agent_sdk_rs::PermissionMode,
            _waker: Waker,
        ) -> (
            Option<std::sync::mpsc::Receiver<crate::DaveApiResponse>>,
            Option<tokio::task::JoinHandle<()>>,
        ) {
            self.requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (None, None)
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

    /// Replace `dave`'s backends with one [`CountingBackend`] serving Claude
    /// sessions, returning its dispatch counter.
    fn install_counting_backend(dave: &mut Dave) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        dave.backends.clear();
        dave.backends.insert(
            BackendType::Claude,
            Box::new(CountingBackend {
                requests: requests.clone(),
            }),
        );
        requests
    }

    /// A notedeck host whose selected account signs with `sk`, so the send path
    /// (which needs a full `AppContext`) runs exactly as in the app.
    fn test_host(dir: &std::path::Path, sk: &[u8; 32]) -> notedeck::Notedeck {
        let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
        let mut host = notedeck::Notedeck::init(&egui::Context::default(), dir, &args);
        let full = nostrdb_net::FullKeypair::from_secret_bytes(sk).unwrap();
        {
            let mut app_ctx = host.app_context();
            let _ = app_ctx
                .accounts
                .add_account(nostrdb_net::Keypair::from_secret(full.secret_key.clone()));
            app_ctx.select_account(&full.pubkey);
        }
        host
    }

    /// Ingest `event` into `ndb` and wait until it is queryable.
    async fn ingest(ndb: &Ndb, event: &session_events::BuiltEvent) {
        let filter = nostrdb::Filter::new().build();
        let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
        ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
            .unwrap();
        ndb.wait_for_notes(sub, 1).await.unwrap();
    }

    /// A kind-31988 state for session `sid` on `host`, with `status`.
    fn state_event(
        sid: &str,
        host: &str,
        status: &str,
        sk: &[u8; 32],
    ) -> session_events::BuiltEvent {
        session_events::build_session_state_event(
            sid,
            "Open Me",
            None,
            "/tmp/proj",
            status,
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
            sk,
        )
        .unwrap()
    }

    /// The texts of `dave`'s session `sid`'s user messages.
    fn user_texts(dave: &Dave, sid: SessionId) -> Vec<String> {
        dave.session_manager
            .get(sid)
            .unwrap()
            .chat
            .iter()
            .filter_map(|m| match m {
                crate::Message::User(u) => Some(u.text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Opening a live local session with a message sends it, once, through the
    /// same path as the input box's Enter: one user message in the chat, one
    /// backend dispatch, an empty draft — and a later frame sends nothing more.
    #[tokio::test]
    async fn open_with_message_sends_into_a_live_session_once() {
        let sk = test_secret_key();
        let base_dir = TempDir::new().unwrap();
        let mut dave = test_dave(&DataPath::new(base_dir.path()));
        let requests = install_counting_backend(&mut dave);
        let sid = dave.session_manager.new_session(
            PathBuf::from("/tmp/proj"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        let event_sid = dave
            .session_manager
            .get(sid)
            .unwrap()
            .agentic
            .as_ref()
            .unwrap()
            .event_session_id()
            .to_owned();

        let host_dir = TempDir::new().unwrap();
        let mut host = test_host(host_dir.path(), &sk);
        let app_ctx = host.app_context();
        let state = state_event(&event_sid, &dave.hostname.clone(), "idle", &sk);
        ingest(app_ctx.ndb, &state).await;

        let msg = "launch a /code-review for the work done in this session";
        dave.open_with_message(nostrdb_net::NoteId::new(state.note_id), Some(msg.into()));
        for _frame in 0..2 {
            if let Some(opened) = dave.process_pending_open(app_ctx.ndb) {
                dave.deliver_open_message(opened, &app_ctx);
            }
        }

        assert_eq!(dave.session_manager.active_id(), Some(sid), "focused");
        assert_eq!(
            user_texts(&dave, sid),
            vec![msg.to_owned()],
            "one user message"
        );
        assert_eq!(
            requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "dispatched to the backend exactly once"
        );
        assert!(dave.session_manager.get(sid).unwrap().input.is_empty());
    }

    /// Opening a deleted (tombstoned) session with a message revives it but
    /// can't send yet: the message lands in its input draft, beside what the
    /// user had there, and nothing is dispatched.
    #[tokio::test]
    async fn open_with_message_drafts_into_a_deleted_session() {
        let sk = test_secret_key();
        let account = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        let base_dir = TempDir::new().unwrap();
        let mut dave = test_dave(&DataPath::new(base_dir.path()));
        let requests = install_counting_backend(&mut dave);
        dave.pns_local_state = Some(PnsLocalState {
            account,
            has_secret_key: true,
        });

        let host_dir = TempDir::new().unwrap();
        let mut host = test_host(host_dir.path(), &sk);
        let app_ctx = host.app_context();
        let tomb = state_event("dead-sess", &dave.hostname.clone(), "deleted", &sk);
        ingest(app_ctx.ndb, &tomb).await;

        let msg = "launch a /code-review";
        dave.open_with_message(nostrdb_net::NoteId::new(tomb.note_id), Some(msg.into()));
        let opened = dave
            .process_pending_open(app_ctx.ndb)
            .expect("the tombstone is reopened");
        assert!(!opened.was_live, "a reopened session isn't live yet");
        let sid = opened.session;
        dave.deliver_open_message(opened, &app_ctx);

        let session = dave.session_manager.get(sid).unwrap();
        assert_eq!(session.input, msg, "the message waits in the draft");
        assert!(user_texts(&dave, sid).is_empty(), "nothing was sent");
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// A live local session no backend on this device runs can't take the
    /// message either; it goes to the draft rather than a dispatch that would
    /// silently do nothing.
    #[test]
    fn open_message_drafts_when_no_backend_runs_the_session() {
        let base_dir = TempDir::new().unwrap();
        let mut dave = test_dave(&DataPath::new(base_dir.path()));
        install_counting_backend(&mut dave);
        let claude = dave.session_manager.new_session(
            PathBuf::from("/tmp/proj"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        let codex = dave.session_manager.new_session(
            PathBuf::from("/tmp/proj"),
            AiMode::Agentic,
            BackendType::Codex,
        );

        assert_eq!(
            dave.open_message_delivery(claude, true),
            OpenMessageDelivery::Send
        );
        assert!(matches!(
            dave.open_message_delivery(codex, true),
            OpenMessageDelivery::Draft(_)
        ));
        assert!(matches!(
            dave.open_message_delivery(claude, false),
            OpenMessageDelivery::Draft(_)
        ));
    }
}
