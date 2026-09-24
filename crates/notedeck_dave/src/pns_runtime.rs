//! The selected account's PNS-backed workspace: the per-account runtime
//! buckets swapped in and out as the account picker changes, and the ndb
//! subscriptions each account's sessions are discovered through.

use crate::conversation::subscribe_conversation_events;
use crate::focus_queue::FocusQueue;
use crate::restore::PendingMessageLoad;
use crate::run_configs::kill_process_tree;
use crate::session_commands::{PendingResumeCommand, PendingSpawnCommand, SpawnIdempotencyRecord};
use crate::update::PermissionPublish;
use crate::{
    focus_queue, session_events, session_loader, update, AgentScene, AiMode, Dave, DaveOverlay,
    DirectoryPicker, PendingWorktreeRemoval, SessionId, SessionManager, SessionPicker,
};
use notedeck::AppContext;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PnsLocalState {
    pub(crate) account: nostrdb_net::Pubkey,
    pub(crate) has_secret_key: bool,
}

pub(crate) struct PnsLocalRuntime {
    session_manager: SessionManager,
    show_session_list: bool,
    scene: AgentScene,
    show_scene: bool,
    interrupt_pending_since: Option<std::time::Instant>,
    focus_queue: FocusQueue,
    auto_steal: focus_queue::AutoStealState,
    home_session: Option<SessionId>,
    directory_picker: DirectoryPicker,
    session_picker: SessionPicker,
    active_overlay: DaveOverlay,
    pending_archive_convert: Option<(std::path::PathBuf, SessionId, String)>,
    pending_message_load: Option<PendingMessageLoad>,
    session_state_sub: Option<nostrdb::Subscription>,
    session_command_sub: Option<nostrdb::Subscription>,
    /// One shared per-account subscription for live conversation events across
    /// every session (demuxed by `d`-tag in `poll_remote_conversation_events`),
    /// so the session count is not bounded by nostrdb's per-db subscription cap.
    conversation_sub: Option<nostrdb::Subscription>,
    /// Independent shared cursor over the same conversation events, consumed by
    /// `poll_remote_conversation_actions` at a different point in the frame.
    conversation_action_sub: Option<nostrdb::Subscription>,
    processed_commands: std::collections::HashSet<String>,
    spawn_idempotency: HashMap<String, SpawnIdempotencyRecord>,
    pending_spawn_commands: Vec<PendingSpawnCommand>,
    pending_resume_commands: Vec<PendingResumeCommand>,
    pending_perm_responses: Vec<PermissionPublish>,
    pending_mode_commands: Vec<update::ModeCommandPublish>,
    pending_interrupt_commands: Vec<update::InterruptPublish>,
    pending_deletions: Vec<session_loader::SessionState>,
    pending_worktree_removals: Vec<PendingWorktreeRemoval>,
    pending_summaries: Vec<nostrdb_net::NoteId>,
    run_processes: HashMap<SessionId, HashMap<String, std::process::Child>>,
    running_session_ids: HashMap<SessionId, HashSet<String>>,
    run_configs: HashMap<std::path::PathBuf, Vec<crate::config::RunConfig>>,
    run_config_sub: Option<nostrdb::Subscription>,
    pending_reap: Vec<std::process::Child>,
}

impl PnsLocalRuntime {
    fn empty_agentic() -> Self {
        Self {
            session_manager: SessionManager::new(),
            show_session_list: false,
            scene: AgentScene::new(),
            show_scene: false,
            interrupt_pending_since: None,
            focus_queue: FocusQueue::new(),
            auto_steal: focus_queue::AutoStealState::Disabled,
            home_session: None,
            directory_picker: DirectoryPicker::new(),
            session_picker: SessionPicker::new(),
            active_overlay: DaveOverlay::DirectoryPicker,
            pending_archive_convert: None,
            pending_message_load: None,
            session_state_sub: None,
            session_command_sub: None,
            conversation_sub: None,
            conversation_action_sub: None,
            processed_commands: std::collections::HashSet::new(),
            spawn_idempotency: HashMap::new(),
            pending_spawn_commands: Vec::new(),
            pending_resume_commands: Vec::new(),
            pending_perm_responses: Vec::new(),
            pending_mode_commands: Vec::new(),
            pending_interrupt_commands: Vec::new(),
            pending_deletions: Vec::new(),
            pending_worktree_removals: Vec::new(),
            pending_summaries: Vec::new(),
            run_processes: HashMap::new(),
            running_session_ids: HashMap::new(),
            run_configs: HashMap::new(),
            run_config_sub: None,
            pending_reap: Vec::new(),
        }
    }

    pub(crate) fn kill_run_processes(&mut self) {
        for procs in self.run_processes.values_mut() {
            for child in procs.values_mut() {
                kill_process_tree(child);
            }
        }
        for child in &mut self.pending_reap {
            kill_process_tree(child);
        }
    }
}

impl Dave {
    /// Keep the selected account's PNS session state (workspace + ndb
    /// subscriptions + restored sessions) in sync with the account picker.
    ///
    /// This drives discovery of remote *agentic* sessions, so it must run
    /// regardless of the app's own `ai_mode`. A remote-only device (e.g.
    /// Android, which has no local agentic backend and so boots in
    /// `AiMode::Chat`) still needs to view and control agentic sessions synced
    /// from relays — that is the entire purpose of `RemoteOnlyBackend`.
    ///
    /// The per-account workspace *swap* is Agentic-only: Chat mode keeps a
    /// single default session that must survive account changes, so we never
    /// swap it out from under the user. In Chat mode the subscription and
    /// restore instead run against the existing session manager, adding any
    /// discovered agentic sessions alongside the chat session.
    ///
    /// Known limitation: switching accounts while in Chat mode does not
    /// re-scope the live subscription or evict the previous account's restored
    /// sessions (that bookkeeping is what the Agentic workspace swap handles).
    /// Single-account remote viewing — the common remote-only case — is
    /// unaffected.
    pub(crate) fn ensure_pns_local_state(&mut self, ctx: &mut AppContext<'_>) {
        let account = *ctx.accounts.selected_account_pubkey();
        let has_secret_key = ctx
            .accounts
            .get_selected_account()
            .keypair()
            .secret_key
            .is_some();
        let next_state = PnsLocalState {
            account,
            has_secret_key,
        };

        if self.pns_local_state.as_ref() == Some(&next_state) {
            return;
        }

        if self.ai_mode == AiMode::Agentic {
            self.save_active_pns_local_runtime();

            if has_secret_key {
                let runtime = self
                    .pns_local_runtimes
                    .remove(&account)
                    .unwrap_or_else(PnsLocalRuntime::empty_agentic);
                self.install_pns_local_runtime(runtime);
            } else {
                self.install_pns_local_runtime(PnsLocalRuntime::empty_agentic());
            }
        }

        self.pns_local_state = Some(next_state);

        if !has_secret_key {
            return;
        }

        if self.session_state_sub.is_none() && self.session_command_sub.is_none() {
            self.subscribe_pns_local_events(ctx.ndb, account);
        }
        if self.run_config_sub.is_none() {
            self.subscribe_pns_run_configs(ctx.ndb, account);
        }
        // Restore this account's sessions off the render thread (see
        // `session_restore_loader`). Dispatch once per account — a re-selected
        // account's sessions are preserved in `pns_local_runtimes`, so restoring
        // again would only re-do work the dedup in `drain_session_restore` throws
        // away. The results are drained a few per frame in `update`.
        if self.restored_accounts.insert(account) {
            self.session_restore_loader.restore_account(account);
        }
        self.load_run_configs(ctx.ndb, account);
    }

    fn take_pns_local_runtime(&mut self) -> PnsLocalRuntime {
        PnsLocalRuntime {
            session_manager: std::mem::take(&mut self.session_manager),
            show_session_list: self.show_session_list,
            scene: std::mem::take(&mut self.scene),
            show_scene: self.show_scene,
            interrupt_pending_since: self.interrupt_pending_since.take(),
            focus_queue: std::mem::take(&mut self.focus_queue),
            auto_steal: self.auto_steal,
            home_session: self.home_session.take(),
            directory_picker: std::mem::take(&mut self.directory_picker),
            session_picker: std::mem::take(&mut self.session_picker),
            active_overlay: std::mem::take(&mut self.active_overlay),
            pending_archive_convert: self.pending_archive_convert.take(),
            pending_message_load: self.pending_message_load.take(),
            session_state_sub: self.session_state_sub.take(),
            session_command_sub: self.session_command_sub.take(),
            conversation_sub: self.conversation_sub.take(),
            conversation_action_sub: self.conversation_action_sub.take(),
            processed_commands: std::mem::take(&mut self.processed_commands),
            spawn_idempotency: std::mem::take(&mut self.spawn_idempotency),
            pending_spawn_commands: std::mem::take(&mut self.pending_spawn_commands),
            pending_resume_commands: std::mem::take(&mut self.pending_resume_commands),
            pending_perm_responses: std::mem::take(&mut self.pending_perm_responses),
            pending_mode_commands: std::mem::take(&mut self.pending_mode_commands),
            pending_interrupt_commands: std::mem::take(&mut self.pending_interrupt_commands),
            pending_deletions: std::mem::take(&mut self.pending_deletions),
            pending_worktree_removals: std::mem::take(&mut self.pending_worktree_removals),
            pending_summaries: std::mem::take(&mut self.pending_summaries),
            run_processes: std::mem::take(&mut self.run_processes),
            running_session_ids: std::mem::take(&mut self.running_session_ids),
            run_configs: std::mem::take(&mut self.run_configs),
            run_config_sub: self.run_config_sub.take(),
            pending_reap: std::mem::take(&mut self.pending_reap),
        }
    }

    fn save_active_pns_local_runtime(&mut self) {
        let Some(state) = self.pns_local_state.clone() else {
            return;
        };
        if state.has_secret_key {
            let runtime = self.take_pns_local_runtime();
            self.pns_local_runtimes.insert(state.account, runtime);
        }
    }

    fn install_pns_local_runtime(&mut self, runtime: PnsLocalRuntime) {
        self.session_manager = runtime.session_manager;
        self.show_session_list = runtime.show_session_list;
        self.scene = runtime.scene;
        self.show_scene = runtime.show_scene;
        self.interrupt_pending_since = runtime.interrupt_pending_since;
        self.focus_queue = runtime.focus_queue;
        self.auto_steal = runtime.auto_steal;
        self.home_session = runtime.home_session;
        self.directory_picker = runtime.directory_picker;
        self.session_picker = runtime.session_picker;
        self.active_overlay = runtime.active_overlay;
        self.pending_archive_convert = runtime.pending_archive_convert;
        self.pending_message_load = runtime.pending_message_load;
        self.session_state_sub = runtime.session_state_sub;
        self.session_command_sub = runtime.session_command_sub;
        self.conversation_sub = runtime.conversation_sub;
        self.conversation_action_sub = runtime.conversation_action_sub;
        self.processed_commands = runtime.processed_commands;
        self.spawn_idempotency = runtime.spawn_idempotency;
        self.pending_spawn_commands = runtime.pending_spawn_commands;
        self.pending_resume_commands = runtime.pending_resume_commands;
        self.pending_perm_responses = runtime.pending_perm_responses;
        self.pending_mode_commands = runtime.pending_mode_commands;
        self.pending_interrupt_commands = runtime.pending_interrupt_commands;
        self.pending_deletions = runtime.pending_deletions;
        self.pending_worktree_removals = runtime.pending_worktree_removals;
        self.pending_summaries = runtime.pending_summaries;
        self.run_processes = runtime.run_processes;
        self.running_session_ids = runtime.running_session_ids;
        self.run_configs = runtime.run_configs;
        self.run_config_sub = runtime.run_config_sub;
        self.pending_reap = runtime.pending_reap;
    }

    fn subscribe_pns_local_events(&mut self, ndb: &nostrdb::Ndb, account: nostrdb_net::Pubkey) {
        let state_filter = nostrdb::Filter::new()
            .kinds([session_events::AI_SESSION_STATE_KIND as u64])
            .authors([account.bytes()])
            .build();
        match ndb.subscribe(&[state_filter]) {
            Ok(sub) => {
                self.session_state_sub = Some(sub);
                tracing::info!("subscribed for session state events in ndb");
            }
            Err(e) => {
                tracing::warn!("failed to subscribe for session state events: {:?}", e);
            }
        }

        let cmd_filter = nostrdb::Filter::new()
            .kinds([session_events::AI_SESSION_COMMAND_KIND as u64])
            .authors([account.bytes()])
            .build();
        match ndb.subscribe(&[cmd_filter]) {
            Ok(sub) => {
                self.session_command_sub = Some(sub);
                tracing::info!("subscribed for session command events in ndb");
            }
            Err(e) => {
                tracing::warn!("failed to subscribe for session command events: {:?}", e);
            }
        }

        // Two shared cursors over all kind-1988 conversation events for this
        // account. One drives `poll_remote_conversation_events` (chat sync), the
        // other `poll_remote_conversation_actions` (permission responses / mode
        // commands); they poll at different points in the frame, so each needs
        // its own cursor. Notes are demuxed by `d`-tag to the owning session, so
        // one pair of subscriptions serves any number of sessions.
        self.conversation_sub = subscribe_conversation_events(ndb, account);
        self.conversation_action_sub = subscribe_conversation_events(ndb, account);
    }
}
