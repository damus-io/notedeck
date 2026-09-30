mod agent_status;
mod auto_accept;
mod avatar;
pub mod backend;
pub(crate) mod collapse_state;
pub mod config;
#[cfg(test)]
mod convergence_tests;
mod conversation;
mod conversation_feed;
mod focus_queue;
pub(crate) mod git_status;
pub mod ipc;
pub(crate) mod mesh;
mod notifications;
mod path_normalize;
pub(crate) mod path_utils;
mod pns_runtime;
mod publish;
mod quaternion;
mod reconcile;
pub mod reference;
pub mod render;
mod restore;
mod run_configs;
pub mod session;
pub mod session_cache;
mod session_commands;
pub mod session_discovery;
mod session_restore_loader;
mod stream_events;
mod view;

// The pure, egui-free engine modules live in the platform-neutral
// `agentium-core` crate. Re-export them under their historical `crate::` paths
// so the rest of dave keeps referring to `crate::messages`, `crate::tools`, etc.
// The async_openai request mapping for these types lives in `backend/openai.rs`.
pub use agentium_core::{
    file_update, messages, session_converter, session_events, session_jsonl, session_loader,
    session_reconstructor, tools,
};
pub mod ui;
pub mod update;
mod vec3;
pub mod worktree;

use backend::{
    AiBackend, BackendType, ClaudeBackend, CodexBackend, Model, OpenAiBackend, RemoteOnlyBackend,
};
use chrono::{Duration, Local};
use egui_wgpu::RenderState;
use focus_queue::FocusQueue;
use nostrdb::{NoteKey, Transaction};
use nostrdb_net::KeypairUnowned;
use notedeck::{
    timed_serializer::TimedSerializer, AppAction, AppContext, AppResponse, DataPath, DataPathType,
    Waker,
};
use pns_runtime::{PnsLocalRuntime, PnsLocalState};
use publish::{record_user_message, session_state_snapshot};
use restore::PendingMessageLoad;
use run_configs::kill_process_tree;
use session_commands::{PendingResumeCommand, PendingSpawnCommand, SpawnIdempotencyRecord};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::string::ToString;
use std::sync::Arc;
use std::time::Instant;
use stream_events::{
    dispatch_compact_for_session, dispatch_turn, DispatchCtx, ProcessEventsResult,
};

pub use agentium_core::messages::{
    AssistantMessage, DaveApiResponse, ExecutedTool, ImageAttachment, Message, PermissionResponse,
    PermissionResponseType, QuestionAnswer, QuestionSetInput, RunningTool, SessionInfo,
    SubagentInfo, SubagentStatus, UserMessage,
};
pub use avatar::DaveAvatar;
pub use config::{AiMode, AiProvider, DaveSettings, ModelConfig, RunConfig};
pub use quaternion::Quaternion;
pub use restore::PendingOpen;
pub use session::{ChatSession, SessionId, SessionManager};
pub use session_discovery::{discover_sessions, format_relative_time, ResumableSession};
pub use tools::{
    PartialToolCall, QueryCall, QueryResponse, Tool, ToolCall, ToolCalls, ToolResponse,
    ToolResponses,
};
pub use ui::{
    check_keybindings, run_config_editor::RunConfigEditor, AgentScene, DaveAction, DaveResponse,
    DaveSettingsPanel, DaveUi, DirectoryPicker, DirectoryPickerAction, KeyActionResult,
    OverlayResult, RunAction, SceneAction, SceneResponse, SceneViewAction, SendActionResult,
    SessionListAction, SessionListUi, SessionPicker, SessionPickerAction, SettingsPanelAction,
    UiActionResult, WorktreeCreator, WorktreeCreatorAction,
};
pub use vec3::Vec3;

/// How long a pending placeholder session waits before being removed.
const PENDING_SESSION_TIMEOUT_SECS: f64 = 15.0;

/// Extract a 32-byte secret key from a keypair.
fn secret_key_bytes(keypair: KeypairUnowned<'_>) -> Option<[u8; 32]> {
    keypair.secret_key.map(|sk| {
        sk.as_secret_bytes()
            .try_into()
            .expect("secret key is 32 bytes")
    })
}

/// Build a loop-less [`agentium_core::Engine`] over dave's shared db, bound to
/// the selected account's secret.
///
/// Dave drives its own relay stack, so it takes the *embedded* engine (no relay
/// loop, no Tokio requirement) and uses the engine's `prepare_*` methods to
/// build + locally-ingest its remote-session write events; the host's
/// private-sync Session fans the freshly-ingested envelopes out. Constructed on
/// demand at each drain from the current account, so it always signs and
/// author-scopes with whichever account is selected. `None` if the secret is
/// rejected (logged).
fn embedded_engine(ndb: &nostrdb::Ndb, secret_key: &[u8; 32]) -> Option<agentium_core::Engine> {
    match agentium_core::Engine::embedded(ndb.clone(), *secret_key) {
        Ok(engine) => Some(engine),
        Err(e) => {
            tracing::error!("failed to build embedded engine: {:?}", e);
            None
        }
    }
}

/// Where a "new session" request should route, given local AI capability and
/// whether any remote agentic hosts are known. Pure so the decision can be
/// unit-tested without constructing a full [`Dave`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewSessionRoute {
    /// Start a local chat session directly.
    Chat,
    /// Ask whether to start a local chat or a remote agentic session.
    ChooseKind,
    /// Pick a remote host to spawn an agentic session on.
    HostPicker,
    /// Pick a local working directory for an agentic session.
    LocalDirectoryPicker,
}

/// Decide how a new-session request routes.
///
/// Remote agentic sessions are only offered once remote hosts are known (i.e.
/// remote sessions already exist). A thin client with no local agentic backend
/// (`AiMode::Chat`, e.g. Android) then asks which kind to start, rather than
/// silently creating a local chat — the bug this addresses. A locally-agentic
/// client goes straight to host selection.
fn route_new_session(ai_mode: AiMode, has_remote_hosts: bool) -> NewSessionRoute {
    match (ai_mode, has_remote_hosts) {
        (AiMode::Chat, true) => NewSessionRoute::ChooseKind,
        (AiMode::Chat, false) => NewSessionRoute::Chat,
        (AiMode::Agentic, true) => NewSessionRoute::HostPicker,
        (AiMode::Agentic, false) => NewSessionRoute::LocalDirectoryPicker,
    }
}

/// Represents which full-screen overlay (if any) is currently active.
/// Data-carrying variants hold the state needed for that step in the
/// session-creation flow, replacing scattered `pending_*` fields.
#[derive(Default)]
pub enum DaveOverlay {
    #[default]
    None,
    Settings,
    /// Choosing between a local chat and a remote agentic session (shown on
    /// thin clients that have no local agentic backend but know of remote
    /// hosts).
    NewSessionKind,
    HostPicker,
    DirectoryPicker,
    /// Backend has been chosen; showing resumable-session list.
    SessionPicker {
        backend: BackendType,
        /// Model chosen in backend picker (threaded to session creation).
        model: Model,
    },
    /// Directory chosen; waiting for user to pick a backend and model.
    BackendPicker {
        cwd: PathBuf,
        /// Optional remote host to spawn on after backend/model selection.
        target_host: Option<String>,
        /// Per-backend selected model index (persists across frames).
        selected_models: HashMap<BackendType, usize>,
    },
    /// User requested a new worktree from an existing session.
    WorktreeCreator(Box<ui::WorktreeCreator>),
    /// User is creating or editing a named run configuration.
    RunConfigEditor(Box<RunConfigEditor>),
}

pub struct Dave {
    /// AI interaction mode (Chat vs Agentic)
    ai_mode: AiMode,
    /// Manages multiple chat sessions
    session_manager: SessionManager,
    /// Realtime fold of the account's kind-31988 session state, shared (behind
    /// `Rc<RefCell<…>>`) with the inline `agentium:` reference parser and session
    /// renderer this app registers, so a chip drawn in a note/Dave-chat reads the
    /// same live state as the open surface. Pumped every frame in [`Self::update`].
    session_cache: std::rc::Rc<std::cell::RefCell<session_cache::AgentiumSessionCache>>,
    /// A 3d representation of dave.
    avatar: Option<DaveAvatar>,
    /// Shared tools available to all sessions
    tools: Arc<HashMap<String, Tool>>,
    /// AI backends keyed by type — multiple may be available simultaneously
    backends: HashMap<BackendType, Box<dyn AiBackend>>,
    /// Which agentic backends are available (detected from PATH at startup)
    available_backends: Vec<BackendType>,
    /// Model configuration
    model_config: ModelConfig,
    /// Whether to show session list on mobile
    show_session_list: bool,
    /// User settings
    settings: DaveSettings,
    /// Settings panel UI state
    settings_panel: DaveSettingsPanel,
    /// RTS-style scene view
    scene: AgentScene,
    /// Whether to show scene view (vs classic chat view)
    show_scene: bool,
    /// Tracks when first Escape was pressed for interrupt confirmation
    interrupt_pending_since: Option<Instant>,
    /// Focus queue for agents needing attention
    focus_queue: FocusQueue,
    /// Tracks which host/cwd folders are collapsed in the session list
    collapse_state: collapse_state::CollapseState,
    collapse_serializer: TimedSerializer<collapse_state::CollapseState>,
    /// Auto-steal focus state: Disabled, Idle (enabled, nothing pending),
    /// or Pending (enabled, waiting to fire / retrying).
    auto_steal: focus_queue::AutoStealState,
    /// The session ID to return to after processing all NeedsInput items
    home_session: Option<SessionId>,
    /// Progress through a leader-key chord, carried across frames.
    chord: ui::keybindings::ChordState,
    /// `settings.leader_key` resolved to an egui key, refreshed whenever the
    /// settings change so the per-frame keybinding pass never parses it.
    leader: ui::keybindings::Leader,
    /// A kind-31988 session-state note to focus, raised when its inline
    /// `agentium:` chip is clicked in another app (a note, a Dave chat) or it is
    /// opened by URI, with any message to send into it. Resolved to a session and
    /// switched to on the next [`update`](Self::update), then cleared. See
    /// [`Self::open_with_message`] / [`Self::process_pending_open`].
    pending_open: Option<restore::PendingOpen>,
    /// Directory picker for selecting working directory when creating sessions
    directory_picker: DirectoryPicker,
    /// Session picker for resuming existing Claude sessions
    session_picker: SessionPicker,
    /// Current overlay taking over the UI (if any)
    active_overlay: DaveOverlay,
    /// IPC listener for external spawn-agent commands
    ipc_listener: Option<ipc::IpcListener>,
    /// Notification state for desktop notifications when unfocused
    notification_state: notifications::NotificationState,
    /// Pending archive conversion: (jsonl_path, dave_session_id, claude_session_id).
    /// Set when resuming a session; processed in update() where AppContext is available.
    pending_archive_convert: Option<(std::path::PathBuf, SessionId, String)>,
    /// Waiting for ndb to finish indexing 1988 events so we can load messages.
    pending_message_load: Option<PendingMessageLoad>,
    /// Events waiting to be published to relays (queued from non-pool contexts).
    /// Local ndb subscription for kind-31988 session state events.
    /// Fires when new session states are unwrapped from PNS events.
    session_state_sub: Option<nostrdb::Subscription>,
    /// Local ndb subscription for kind-31989 session command events.
    session_command_sub: Option<nostrdb::Subscription>,
    /// One shared per-account subscription for kind-1988 live conversation
    /// events across every session, with how far it has delivered: the line
    /// between a session's history and its live notes (see
    /// [`conversation_feed`]).
    conversation_feed: Option<conversation_feed::ConversationFeed>,
    /// Independent shared cursor over the same kind-1988 events, consumed by
    /// `poll_remote_conversation_actions` (permission responses / mode commands)
    /// at a different point in the frame than `conversation_feed`.
    conversation_action_sub: Option<nostrdb::Subscription>,
    /// Command UUIDs already processed (dedup for spawn commands).
    processed_commands: std::collections::HashSet<String>,
    /// Sessions this host has materialized, keyed by their spawn command's
    /// `idempotency_key` — the record that lets a retry be answered with the
    /// session it already produced. Keyed on the *request*, unlike
    /// `processed_commands`, which keys on the command's d-tag: that is a fresh
    /// UUID per transmission, so it dedupes re-delivery of one command but
    /// cannot see that two commands are the same spawn asked for twice.
    ///
    /// In memory only, like `processed_commands`. The window it enforces
    /// ([`SPAWN_DEDUPE_WINDOW_SECS`](session_events::SPAWN_DEDUPE_WINDOW_SECS))
    /// is minutes, so a host restart between a
    /// spawn and its retry is not the case this guards — and the CLI's own
    /// pre-publish guard covers a host that has forgotten.
    spawn_idempotency: HashMap<String, SpawnIdempotencyRecord>,
    /// Spawn commands waiting to be built+published in update() where secret key is available.
    pending_spawn_commands: Vec<PendingSpawnCommand>,
    /// Resume commands (deleted-chip resume for sessions on another host) waiting
    /// to be built+published in update() where the secret key is available.
    pending_resume_commands: Vec<PendingResumeCommand>,
    /// Permission responses queued for relay publishing (from remote sessions).
    /// Built and published in the update loop where AppContext is available.
    pending_perm_responses: Vec<PermissionPublish>,
    /// Permission mode commands queued for relay publishing (observer → host).
    pending_mode_commands: Vec<update::ModeCommandPublish>,
    /// Interrupt commands queued for relay publishing (observer → host).
    pending_interrupt_commands: Vec<update::InterruptPublish>,
    /// Sessions pending deletion state event publication.
    /// Populated in delete_session(), drained in the update loop where AppContext is available.
    pending_deletions: Vec<session_loader::SessionState>,
    pending_worktree_removals: Vec<PendingWorktreeRemoval>,
    /// Thread summaries pending processing. Queued by summarize_thread(),
    /// resolved in update() where AppContext (ndb) is available.
    pending_summaries: Vec<nostrdb_net::NoteId>,
    /// Local machine hostname, included in session state events.
    hostname: String,
    /// Last selected account used to populate Dave's local PNS-backed state.
    pns_local_state: Option<PnsLocalState>,
    /// Hidden selected-account runtime buckets. The active bucket lives in the
    /// regular Dave fields so existing UI/update code keeps operating directly.
    pns_local_runtimes: HashMap<nostrdb_net::Pubkey, PnsLocalRuntime>,
    /// Persists DaveSettings to dave_settings.json
    settings_serializer: TimedSerializer<DaveSettings>,
    /// Running app processes launched via the Run button.
    /// Keyed by (session ID, config UUID string). The config UUID is stable
    /// across renames, reloads, and Nostr sync.
    run_processes: HashMap<SessionId, HashMap<String, std::process::Child>>,
    /// Maps session ID to the set of config UUIDs currently running.
    /// Updated once per frame by `reap_run_processes()`.
    running_session_ids: HashMap<SessionId, HashSet<String>>,
    /// Run configs keyed by CWD — loaded from kind-31991 Nostr events on startup.
    run_configs: HashMap<std::path::PathBuf, Vec<crate::config::RunConfig>>,
    /// ndb subscription for incoming kind-31991 run-config events (live updates).
    run_config_sub: Option<nostrdb::Subscription>,
    /// Killed child processes waiting to be reaped via non-blocking try_wait() each frame.
    pending_reap: Vec<std::process::Child>,
    /// Background worker that reads + renders an account's persisted sessions off
    /// the render thread, streamed into the manager a few per frame by
    /// [`Self::drain_session_restore`]. A shared pool, not per-account UI state,
    /// so it is *not* swapped by [`PnsLocalRuntime`].
    session_restore_loader: session_restore_loader::SessionRestoreLoader,
    /// Accounts already dispatched to the restore loader, so re-selecting an
    /// account (whose sessions are preserved in `pns_local_runtimes`) doesn't
    /// re-restore. Guards one dispatch per account per process.
    restored_accounts: HashSet<nostrdb_net::Pubkey>,
}

use update::PermissionPublish;

/// Async git worktree removal: spawns a background thread and polls the result.
struct PendingWorktreeRemoval {
    session_id: SessionId,
    rx: std::sync::mpsc::Receiver<Result<(), String>>,
}

impl PendingWorktreeRemoval {
    fn spawn(session_id: SessionId, cwd: std::path::PathBuf) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(worktree::remove_git_worktree(&cwd));
        });
        Self { session_id, rx }
    }
}

/// Calculate an anonymous user_id from a keypair
/// Look up a backend by type from the map, falling back to Remote.
fn get_backend(
    backends: &HashMap<BackendType, Box<dyn AiBackend>>,
    bt: BackendType,
) -> &dyn AiBackend {
    backends
        .get(&bt)
        .or_else(|| backends.get(&BackendType::Remote))
        .unwrap()
        .as_ref()
}

fn calculate_user_id(keypair: KeypairUnowned) -> String {
    use sha2::{Digest, Sha256};
    // pubkeys have degraded privacy, don't do that
    let key_input = keypair
        .secret_key
        .map(|sk| sk.as_secret_bytes())
        .unwrap_or(keypair.pubkey.bytes());
    let hex_key = hex::encode(key_input);
    let input = format!("{hex_key}notedeck_dave_user_id");
    hex::encode(Sha256::digest(input))
}

impl Dave {
    pub fn avatar_mut(&mut self) -> Option<&mut DaveAvatar> {
        self.avatar.as_mut()
    }

    fn _system_prompt() -> Message {
        let now = Local::now();
        let yesterday = now - Duration::hours(24);
        let date = now.format("%Y-%m-%d %H:%M:%S");
        let timestamp = now.timestamp();
        let yesterday_timestamp = yesterday.timestamp();

        Message::System(format!(
            r#"
You are an AI agent for the nostr protocol called Dave, created by Damus. nostr is a decentralized social media and internet communications protocol. You are embedded in a nostr browser called 'Damus Notedeck'.

- The current date is {date} ({timestamp} unix timestamp if needed for queries).

- Yesterday (-24hrs) was {yesterday_timestamp}. You can use this in combination with `since` queries for pulling notes for summarizing notes the user might have missed while they were away.

# Response Guidelines

- You *MUST* call the present_notes tool with a list of comma-separated note id references when referring to notes so that the UI can display them. Do *NOT* include note id references in the text response, but you *SHOULD* use ^1, ^2, etc to reference note indices passed to present_notes.
- When a user asks for a digest instead of specific query terms, make sure to include both since and until to pull notes for the correct range.
- When tasked with open-ended queries such as looking for interesting notes or summarizing the day, make sure to add enough notes to the context (limit: 100-200) so that it returns enough data for summarization.
"#
        ))
    }

    pub fn new(
        render_state: Option<&RenderState>,
        ndb: nostrdb::Ndb,
        waker: Waker,
        path: &DataPath,
    ) -> Self {
        let settings_serializer =
            TimedSerializer::new(path, DataPathType::Setting, "dave_settings.json".to_owned());

        let collapse_serializer = TimedSerializer::new(
            path,
            DataPathType::Setting,
            "collapse_state.json".to_owned(),
        );
        let collapse_state = collapse_serializer.get_item().unwrap_or_default();

        // Load saved settings, falling back to env-var-based defaults
        let (model_config, settings) = if let Some(saved_settings) = settings_serializer.get_item()
        {
            let config = ModelConfig::from_settings(&saved_settings);
            (config, saved_settings)
        } else {
            let config = ModelConfig::default();
            let settings = DaveSettings::from_model_config(&config);
            (config, settings)
        };
        let leader = ui::keybindings::Leader::resolve(&settings.leader_key);

        // Determine AI mode from backend type
        let ai_mode = model_config.ai_mode();

        // Detect available agentic backends from PATH
        let available_backends = config::available_agentic_backends();
        tracing::info!(
            "detected {} agentic backends: {:?}",
            available_backends.len(),
            available_backends
        );

        // Create backends for all available agentic CLIs + the configured primary
        let mut backends: HashMap<BackendType, Box<dyn AiBackend>> = HashMap::new();

        for &bt in &available_backends {
            match bt {
                BackendType::Claude => {
                    backends.insert(BackendType::Claude, Box::new(ClaudeBackend::new()));
                }
                BackendType::Codex => {
                    backends.insert(
                        BackendType::Codex,
                        Box::new(CodexBackend::new(
                            std::env::var("CODEX_BINARY").unwrap_or_else(|_| "codex".to_string()),
                        )),
                    );
                }
                _ => {}
            }
        }

        // If the configured backend is OpenAI and not yet created, add it
        if model_config.backend == BackendType::OpenAI {
            use async_openai::Client;
            let client = Client::with_config(model_config.to_api());
            backends.insert(
                BackendType::OpenAI,
                Box::new(OpenAiBackend::new(client, ndb.clone())),
            );
        }

        // Remote backend is always available for discovered sessions
        backends.insert(BackendType::Remote, Box::new(RemoteOnlyBackend));

        let avatar = render_state.map(DaveAvatar::new);
        let mut tools: HashMap<String, Tool> = HashMap::new();
        for tool in tools::dave_tools() {
            tools.insert(tool.name().to_string(), tool);
        }

        let directory_picker = DirectoryPicker::new();

        // Create IPC listener for external spawn-agent commands
        let ipc_listener = ipc::create_listener(waker);

        let hostname = gethostname::gethostname().to_string_lossy().into_owned();

        // In Chat mode, create a default session immediately and skip directory picker
        // In Agentic mode, show directory picker on startup
        let (session_manager, active_overlay) = match ai_mode {
            AiMode::Chat => {
                let mut manager = SessionManager::new();
                // Create a default session with current directory
                let sid = manager.new_session(
                    std::env::current_dir().unwrap_or_default(),
                    ai_mode,
                    model_config.backend,
                );
                if let Some(session) = manager.get_mut(sid) {
                    session.details.hostname = hostname.clone();
                }
                manager.rebuild_groups();
                (manager, DaveOverlay::None)
            }
            AiMode::Agentic => (SessionManager::new(), DaveOverlay::DirectoryPicker),
        };

        Dave {
            ai_mode,
            backends,
            available_backends,
            avatar,
            session_manager,
            session_cache: std::rc::Rc::new(std::cell::RefCell::new(
                session_cache::AgentiumSessionCache::default(),
            )),
            tools: Arc::new(tools),
            model_config,
            show_session_list: false,
            settings,
            settings_panel: DaveSettingsPanel::new(),
            scene: AgentScene::new(),
            show_scene: false, // Default to list view
            interrupt_pending_since: None,
            focus_queue: FocusQueue::new(),
            collapse_state,
            collapse_serializer,
            auto_steal: focus_queue::AutoStealState::Disabled,
            home_session: None,
            chord: ui::keybindings::ChordState::default(),
            leader,
            pending_open: None,
            directory_picker,
            session_picker: SessionPicker::new(),
            active_overlay,
            ipc_listener,
            notification_state: notifications::NotificationState::new(),
            pending_archive_convert: None,
            pending_message_load: None,
            session_state_sub: None,
            session_command_sub: None,
            conversation_feed: None,
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
            hostname,
            pns_local_state: None,
            pns_local_runtimes: HashMap::new(),
            settings_serializer,
            run_processes: HashMap::new(),
            running_session_ids: HashMap::new(),
            run_configs: HashMap::new(),
            pending_reap: Vec::new(),
            run_config_sub: None,
            session_restore_loader: session_restore_loader::SessionRestoreLoader::new(),
            restored_accounts: HashSet::new(),
        }
    }

    /// Get current settings for persistence
    pub fn settings(&self) -> &DaveSettings {
        &self.settings
    }

    /// Apply new settings and persist to disk.
    /// Note: Provider changes require app restart to take effect.
    pub fn apply_settings(&mut self, settings: DaveSettings) {
        self.model_config = ModelConfig::from_settings(&settings);
        self.leader = ui::keybindings::Leader::resolve(&settings.leader_key);
        self.settings_serializer.try_save(settings.clone());
        self.settings = settings;
    }

    /// Toggle a host collapse state, persist it, and re-arm auto-steal if needed.
    fn toggle_host_collapse(&mut self, hostname: &str) {
        self.collapse_state.toggle_host(hostname);
        self.collapse_serializer
            .try_save(self.collapse_state.clone());
        if self.auto_steal.is_enabled() && !self.focus_queue.is_empty() {
            self.auto_steal = focus_queue::AutoStealState::Pending;
        }
    }

    /// Toggle a project collapse state, persist it, and re-arm auto-steal if needed.
    fn toggle_project_collapse(&mut self, hostname: &str, root: &std::path::Path) {
        self.collapse_state.toggle_project(hostname, root);
        self.collapse_serializer
            .try_save(self.collapse_state.clone());
        if self.auto_steal.is_enabled() && !self.focus_queue.is_empty() {
            self.auto_steal = focus_queue::AutoStealState::Pending;
        }
    }

    /// Toggle a cwd collapse state, persist it, and re-arm auto-steal if needed.
    fn toggle_cwd_collapse(&mut self, hostname: &str, cwd: &std::path::Path) {
        self.collapse_state.toggle_cwd(hostname, cwd);
        self.collapse_serializer
            .try_save(self.collapse_state.clone());
        if self.auto_steal.is_enabled() && !self.focus_queue.is_empty() {
            self.auto_steal = focus_queue::AutoStealState::Pending;
        }
    }

    /// Queue a thread summary request. The thread is fetched and formatted
    /// in update() where AppContext (ndb) is available.
    pub fn summarize_thread(&mut self, note_id: nostrdb_net::NoteId) {
        self.pending_summaries.push(note_id);
    }

    /// Focus a session referenced from elsewhere in the app — raised when its
    /// inline `agentium:` chip (drawn by [`render::AgentiumSessionRenderer`]) is
    /// clicked in another app like a note or Dave chat. `note` is the kind-31988
    /// session-state event; the switch happens on the next
    /// [`update`](Self::update) (see [`process_pending_open`](Self::process_pending_open)).
    pub fn open(&mut self, note: nostrdb_net::NoteId) {
        self.open_with_message(note, None);
    }

    /// [`open`](Self::open) a session and, once it is focused, send `msg` into it
    /// as a user message — as if typed into its input box and submitted. This is
    /// how an `OpenUri` like `agentium:<word-id>?msg=…` lands. A session that
    /// can't take input yet (it is being reopened from a tombstone, or no local
    /// backend runs it) gets `msg` in its input draft instead; see
    /// [`deliver_open_message`](Self::deliver_open_message).
    pub fn open_with_message(&mut self, note: nostrdb_net::NoteId, msg: Option<String>) {
        self.pending_open = Some(restore::PendingOpen { note, msg });
    }

    /// The open an [`open_with_message`](Self::open_with_message) left for the
    /// next [`update`](Self::update), if it hasn't run yet: where a host's open
    /// by reference landed, for the host's tests to check.
    pub fn pending_open(&self) -> Option<&PendingOpen> {
        self.pending_open.as_ref()
    }

    /// Fetch the thread from ndb, format it, and create a session with the prompt.
    fn build_summary_session(
        &mut self,
        ndb: &nostrdb::Ndb,
        note_id: &nostrdb_net::NoteId,
    ) -> Option<SessionId> {
        let txn = Transaction::new(ndb).ok()?;

        // Resolve to the root note of the thread
        let clicked_note = ndb.get_note_by_id(&txn, note_id.bytes()).ok()?;
        let root_id = nostrdb::NoteReply::new(clicked_note.tags())
            .root()
            .map(|r| *r.id)
            .unwrap_or(*note_id.bytes());

        let root_note = ndb.get_note_by_id(&txn, &root_id).ok()?;
        let root_simple = tools::note_to_simple(&txn, ndb, &root_note);

        // Fetch all replies referencing the root note
        let filter = nostrdb::Filter::new().kinds([1]).event(&root_id).build();

        let replies = ndb.query(&txn, &[filter], 500).ok().unwrap_or_default();

        let mut simple_notes = vec![root_simple];
        for result in &replies {
            if let Ok(note) = ndb.get_note_by_key(&txn, result.note_key) {
                simple_notes.push(tools::note_to_simple(&txn, ndb, &note));
            }
        }

        let thread_json = tools::format_simple_notes_json(&simple_notes);
        let system = format!(
            "You are summarizing a nostr thread. \
             Here is the thread data:\n\n{}\n\n\
             When referencing specific notes in your summary, call the \
             present_notes tool with their note_ids so the UI can display them inline.",
            thread_json
        );

        let cwd = std::env::current_dir().unwrap_or_default();
        let id = update::create_session_with_cwd(
            &mut self.session_manager,
            &mut self.directory_picker,
            &mut self.scene,
            self.show_scene,
            AiMode::Chat,
            cwd,
            &self.hostname,
            self.model_config.backend,
            Model::Default,
        );

        if let Some(session) = self.session_manager.get_mut(id) {
            session.chat.push(Message::System(system));

            // Show the root note inline so the user can see what's being summarized
            let present = tools::ToolCall::new(
                "summarize-thread".to_string(),
                tools::ToolCalls::PresentNotes(tools::PresentNotesCall {
                    note_ids: vec![nostrdb_net::NoteId::new(root_id)],
                }),
            );
            session.chat.push(Message::ToolCalls(vec![present]));

            session.chat.push(Message::User(
                "Summarize this thread concisely.".to_string().into(),
            ));
            session.update_title_from_last_message();
        }

        Some(id)
    }

    fn handle_new_chat(&mut self) {
        match route_new_session(self.ai_mode, !self.known_remote_hosts().is_empty()) {
            NewSessionRoute::Chat => {
                // In chat mode, create a session directly without any picker.
                let cwd = std::env::current_dir().unwrap_or_default();
                self.create_session_with_cwd(cwd, self.model_config.backend, Model::Default);
            }
            NewSessionRoute::ChooseKind => {
                self.active_overlay = DaveOverlay::NewSessionKind;
            }
            NewSessionRoute::HostPicker => {
                self.active_overlay = DaveOverlay::HostPicker;
            }
            NewSessionRoute::LocalDirectoryPicker => {
                self.directory_picker.target_host = None;
                self.active_overlay = DaveOverlay::DirectoryPicker;
            }
        }
    }

    /// Collect remote hostnames from sessions and directory picker's
    /// event-sourced paths. Excludes the local hostname.
    fn known_remote_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = Vec::new();

        // From active sessions
        for hostname in self.session_manager.remote_hostnames() {
            if hostname != self.hostname && !hosts.contains(&hostname) {
                hosts.push(hostname);
            }
        }

        // From event-sourced paths (may include hosts with no active sessions)
        for hostname in self.directory_picker.host_recent_paths.keys() {
            if hostname != &self.hostname && !hosts.contains(hostname) {
                hosts.push(hostname.clone());
            }
        }

        hosts.sort();
        hosts
    }

    /// Anchor auto-steal focus to a session the user just deliberately opened.
    ///
    /// Creating, duplicating, or navigating to a session (a list-row click, an
    /// `agentium:` chip clicked in another app) makes it the active session. But
    /// with auto-steal enabled the very next focus-queue change re-arms it and it
    /// yanks focus onto some *other* session that needs input — jarring right
    /// after the user deliberately opened this one (e.g. jumping in from a
    /// Headway chip). Record the opened session as the home session so auto-steal
    /// returns here once anything urgent is handled, and cancel any pending steal
    /// so it doesn't fire on top of this navigation. No-op when auto-steal is off
    /// (the default), where `home_session` and the pending state are unused.
    fn anchor_focus(&mut self, id: SessionId) {
        update::anchor_auto_steal(&mut self.auto_steal, &mut self.home_session, id);
    }

    /// Create a new session with the given cwd (called after directory picker selection)
    fn create_session_with_cwd(&mut self, cwd: PathBuf, backend_type: BackendType, model: Model) {
        let id = update::create_session_with_cwd(
            &mut self.session_manager,
            &mut self.directory_picker,
            &mut self.scene,
            self.show_scene,
            self.ai_mode,
            cwd,
            &self.hostname,
            backend_type,
            model,
        );
        self.anchor_focus(id);
    }

    /// Create a new session that resumes an existing Claude conversation
    fn create_resumed_session_with_cwd(
        &mut self,
        cwd: PathBuf,
        resume_session_id: String,
        title: String,
        backend_type: BackendType,
    ) -> SessionId {
        let id = update::create_resumed_session_with_cwd(
            &mut self.session_manager,
            &mut self.directory_picker,
            &mut self.scene,
            self.show_scene,
            self.ai_mode,
            cwd,
            resume_session_id,
            title,
            &self.hostname,
            backend_type,
        );
        self.anchor_focus(id);
        id
    }

    /// Duplicate a session by ID, creating a new session with the same working directory.
    /// For remote sessions, sends a spawn command to the remote host.
    fn duplicate_session(&mut self, id: SessionId) {
        match update::clone_session(
            &mut self.session_manager,
            &mut self.directory_picker,
            &mut self.scene,
            self.show_scene,
            self.ai_mode,
            &self.hostname,
            id,
        ) {
            // Remote: the clone is spawned on its host, no local session yet.
            Some(spawn) => self.queue_spawn_command(&spawn.host, &spawn.cwd, spawn.backend),
            // Local: the new session was created and made active — anchor to it.
            None => {
                if let Some(new_id) = self.session_manager.active_id() {
                    self.anchor_focus(new_id);
                }
            }
        }
    }

    /// Clone the active agent, creating a new session with the same working directory
    fn clone_active_agent(&mut self) {
        if let Some(id) = self.session_manager.active_id() {
            self.duplicate_session(id);
        }
    }

    /// Poll for IPC spawn-agent commands from external tools
    fn poll_ipc_commands(&mut self) {
        let Some(listener) = self.ipc_listener.as_ref() else {
            return;
        };

        // Drain all pending connections (non-blocking). Track the last session
        // created so we can anchor auto-steal focus to it after the loop — we
        // can't call `anchor_focus` (a `&mut self` method) inside, since the
        // `listener` borrow of `self.ipc_listener` is live across the loop.
        let mut last_created: Option<SessionId> = None;
        while let Some(mut pending) = listener.try_recv() {
            // Create the session and get its ID
            let id = self.session_manager.new_session(
                pending.cwd.clone(),
                self.ai_mode,
                self.model_config.backend,
            );
            self.directory_picker.add_recent(pending.cwd);

            // Focus on new session
            if let Some(session) = self.session_manager.get_mut(id) {
                session.details.hostname = self.hostname.clone();
                session.focus_requested = true;
                if self.show_scene {
                    self.scene.select(id);
                    if let Some(agentic) = &session.agentic {
                        self.scene.focus_on(agentic.scene_position.into());
                    }
                }
            }
            self.session_manager.rebuild_groups();
            last_created = Some(id);

            // Close directory picker if open
            if matches!(self.active_overlay, DaveOverlay::DirectoryPicker) {
                self.active_overlay = DaveOverlay::None;
            }

            // Send success response back to the client
            #[cfg(unix)]
            {
                let response = ipc::SpawnResponse::ok(id);
                let _ = ipc::send_response(&mut pending.stream, &response);
            }

            tracing::info!("Spawned agent via IPC (session {})", id);
        }

        // The newest session is the active one; anchor focus to it.
        if let Some(id) = last_created {
            self.anchor_focus(id);
        }
    }

    fn poll_pending_worktree_removal(&mut self) {
        let mut completed = Vec::new();
        self.pending_worktree_removals
            .retain(|p| match p.rx.try_recv() {
                Ok(r) => {
                    completed.push((p.session_id, Ok(r)));
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    completed.push((
                        p.session_id,
                        Err("worktree removal thread disconnected".to_string()),
                    ));
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
            });

        for (session_id, result) in completed {
            match result {
                Ok(Ok(())) => self.delete_session(session_id),
                Ok(Err(msg)) | Err(msg) => tracing::error!("failed to remove worktree: {msg}"),
            }
        }
    }

    fn rename_session(&mut self, id: SessionId, new_title: String) {
        let Some(session) = self.session_manager.get_mut(id) else {
            return;
        };
        session.details.custom_title = Some(new_title);
        session.state_dirty = true;
    }

    /// Clear a session: duplicate it (preserving working directory) then delete the original.
    /// This is the canonical "reset" action used by the Clear menu button, Ctrl+Shift+K, and /clear.
    fn clear_session(&mut self, id: SessionId) {
        self.duplicate_session(id);
        self.delete_session(id);
    }

    fn delete_session(&mut self, id: SessionId) {
        self.kill_session_run_processes(id);

        // Snapshot the full session state before deletion so we can publish a
        // tombstone that carries every persisted field — including the owning
        // `hostname`, so a deleted remote session's chip resumes on its owner
        // (see `process_pending_open`), not here. `created_at` is recomputed at
        // publish time (`publish_pending_deletions`). The publish decision fields
        // are the deleted status and the session's own host; a tombstone carries
        // no attention indicator.
        if let Some(session) = self.session_manager.get(id) {
            if let Some(mut state) = session_state_snapshot(
                session,
                session_loader::DELETED_STATUS.to_string(),
                session.details.hostname.clone(),
                0,
            ) {
                state.indicator = None;
                self.pending_deletions.push(state);
            }
        }

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

    /// If only one agentic backend is available, return it. Otherwise None
    /// (meaning we need to show the backend picker).
    fn single_agentic_backend(&self) -> Option<BackendType> {
        if self.available_backends.len() == 1 {
            Some(self.available_backends[0])
        } else {
            None
        }
    }

    fn create_or_pick_backend(&mut self, cwd: PathBuf, target_host: Option<String>) {
        tracing::info!(
            "create_or_pick_backend: {} available backends: {:?} target_host={:?}",
            self.available_backends.len(),
            self.available_backends,
            target_host
        );
        let remote_target = target_host
            .filter(|host| !host.is_empty())
            .filter(|host| host != &self.hostname);

        if let Some(bt) = self.single_agentic_backend() {
            tracing::info!("single backend detected, skipping picker: {:?}", bt);
            if let Some(host) = remote_target.as_deref() {
                self.queue_spawn_command(host, &cwd, bt);
            } else {
                self.create_or_resume_session(cwd, bt, Model::Default);
            }
        } else if self.available_backends.is_empty() {
            // No agentic backends — fall back to configured backend
            if let Some(host) = remote_target.as_deref() {
                self.queue_spawn_command(host, &cwd, self.model_config.backend);
            } else {
                self.create_or_resume_session(cwd, self.model_config.backend, Model::Default);
            }
        } else {
            tracing::info!(
                "multiple backends available, showing backend picker: {:?}",
                self.available_backends
            );
            self.active_overlay = DaveOverlay::BackendPicker {
                cwd,
                target_host: remote_target,
                selected_models: HashMap::new(),
            };
        }
    }

    /// After a backend is determined, either create a session directly or
    /// show the session picker if there are resumable sessions for this backend.
    fn create_or_resume_session(&mut self, cwd: PathBuf, backend_type: BackendType, model: Model) {
        // Only Claude has discoverable resumable sessions (from ~/.claude/)
        if backend_type == BackendType::Claude {
            let resumable = discover_sessions(&cwd);
            if !resumable.is_empty() {
                tracing::info!(
                    "found {} resumable sessions, showing session picker",
                    resumable.len()
                );
                self.session_picker.open(cwd);
                self.active_overlay = DaveOverlay::SessionPicker {
                    backend: backend_type,
                    model,
                };
                return;
            }
        }
        self.create_session_with_cwd(cwd, backend_type, model);
        self.active_overlay = DaveOverlay::None;
    }

    /// Record a user-authored message in the target session.
    ///
    /// This is the one user-send path — the input box's Enter
    /// ([`handle_user_send`](Self::handle_user_send)), spawn commands' first
    /// prompts and opened-by-URI messages all go through it: create a live user
    /// event when possible, append `Message::User` to chat, and update the
    /// session title.
    ///
    /// Returns `true` when the caller should dispatch this session to the
    /// backend immediately.
    pub fn add_user_message_for_session(
        &mut self,
        sid: SessionId,
        app_ctx: &AppContext,
        user_text: String,
        images: Vec<ImageAttachment>,
    ) -> bool {
        let Some(session) = self.session_manager.get_mut(sid) else {
            return false;
        };

        let sk = secret_key_bytes(app_ctx.accounts.get_selected_account().keypair());
        record_user_message(session, app_ctx.ndb, sk.as_ref(), user_text, images);

        // Remote sessions: the event above publishes it to the host; there's no
        // local backend to send it to.
        if session.is_remote() {
            return false;
        }

        // Already dispatched (waiting for or receiving a response): queue it in
        // chat; needs_redispatch_after_stream_end() dispatches it when the
        // current turn finishes.
        if session.is_dispatched() {
            tracing::info!("message queued, will dispatch after current turn");
            return false;
        }

        true
    }

    /// Handle a user send action triggered by the ui
    fn handle_user_send(&mut self, app_ctx: &AppContext) {
        // Check for /cd command first (agentic only)
        let sk = secret_key_bytes(app_ctx.accounts.get_selected_account().keypair());
        let cd_result = self
            .session_manager
            .get_active_mut()
            .and_then(|session| update::handle_cd_command(session, app_ctx.ndb, &sk));

        // If /cd command was processed, add to recent directories
        if let Some(Ok(path)) = cd_result {
            self.directory_picker.add_recent(path);
            return;
        } else if cd_result.is_some() {
            // Error case - already handled above
            return;
        }

        // Handle /clear command: reset session (same as Clear menu action)
        if let Some(session) = self.session_manager.get_active() {
            if session.input.trim() == "/clear" {
                if let Some(id) = self.session_manager.active_id() {
                    if let Some(s) = self.session_manager.get_mut(id) {
                        s.input.clear();
                    }
                    self.clear_session(id);
                }
                return;
            }
        }

        // Normal message handling: the draft becomes a user message. This is the
        // same path an opened-by-URI message takes (`deliver_open_message`).
        let Some(sid) = self.session_manager.active_id() else {
            return;
        };
        let Some(session) = self.session_manager.get_mut(sid) else {
            return;
        };
        let user_text = std::mem::take(&mut session.input);
        let images = std::mem::take(&mut session.pending_images);
        if self.add_user_message_for_session(sid, app_ctx, user_text, images) {
            self.send_user_message_for(sid, app_ctx, app_ctx.waker);
        }
    }

    /// Send a message for a specific session by ID
    fn send_user_message_for(&mut self, sid: SessionId, app_ctx: &AppContext, waker: &Waker) {
        let Some(session) = self.session_manager.get_mut(sid) else {
            return;
        };

        // Only dispatch if we have the backend this session needs.
        // Without this guard, get_backend falls back to Remote which
        // immediately disconnects, causing an infinite redispatch loop.
        if !self.backends.contains_key(&session.backend_type) {
            return;
        }

        let account = app_ctx.accounts.get_selected_account();
        let sk = secret_key_bytes(account.keypair());
        let ctx = DispatchCtx {
            ndb: app_ctx.ndb,
            secret_key: sk.as_ref(),
            user_id: calculate_user_id(account.keypair()),
            tools: self.tools.clone(),
            session_env: &self.settings.session_env,
            waker,
        };
        dispatch_turn(
            session,
            get_backend(&self.backends, session.backend_type),
            &ctx,
        );
    }
}

impl Drop for Dave {
    fn drop(&mut self) {
        for procs in self.run_processes.values_mut() {
            for child in procs.values_mut() {
                kill_process_tree(child);
            }
        }
        for child in &mut self.pending_reap {
            kill_process_tree(child);
        }
        for runtime in self.pns_local_runtimes.values_mut() {
            runtime.kill_run_processes();
        }
    }
}

/// Whether `kind` is one Dave renders inline and routes clicks for — the
/// kind-31988 session-state event ([`render::AgentiumSessionRenderer`]). The
/// chrome uses this to route a click on an `agentium:` chip to Dave rather than
/// the timeline (mirrors `notedeck_notebook::is_notebook_kind`).
pub fn is_agentium_kind(kind: u32) -> bool {
    kind == session_events::AI_SESSION_STATE_KIND
}

impl notedeck::App for Dave {
    fn update(&mut self, ctx: &mut AppContext<'_>) {
        // Copied out up front: the backend dispatches below hand a clone of this
        // to worker tasks, and each of those calls needs it while `ctx` is
        // mut-borrowed elsewhere.
        let waker = ctx.waker;

        // Ensure the background session-restore worker is running (idempotent).
        self.session_restore_loader
            .start(waker.clone(), ctx.ndb.clone());

        // Focus a session whose inline chip was clicked (or that was opened by
        // URI) in another app, then send it the open's message, if any.
        if let Some(opened) = self.process_pending_open(ctx.ndb) {
            self.deliver_open_message(opened, ctx);
        }
        self.ensure_pns_local_state(ctx);
        // The account's inbound PNS 1080 sync (and its settle signal, read via
        // `ctx.private_sync_settled`) is now owned by the notedeck host, running
        // off-foreground for every app. Dave keeps only its own outbound publish
        // (below) and its local session-state fold.

        // Poll for external spawn-agent commands via IPC
        self.poll_ipc_commands();

        // Process pending thread summary requests
        let pending = std::mem::take(&mut self.pending_summaries);
        for note_id in pending {
            if let Some(sid) = self.build_summary_session(ctx.ndb, &note_id) {
                self.send_user_message_for(sid, ctx, waker);
            }
        }

        // Poll for external editor completion
        update::poll_editor_job(&mut self.session_manager);

        // Reap killed child processes without blocking the frame
        self.poll_pending_reap();

        // Poll for new session states from PNS-unwrapped relay events
        self.poll_session_state_events(ctx);

        // Drain background-restored sessions into the manager (a few per frame).
        // Replaying what the poll dropped before a session existed can yield
        // remote user messages, dispatched below with the poll's.
        let sk_bytes = secret_key_bytes(ctx.accounts.get_selected_account().keypair());
        let restored_user_msgs = self.drain_session_restore(ctx.ndb, sk_bytes.as_ref(), ctx.waker);

        // Advance the shared inline-session cache backing `agentium:` chips.
        self.pump_session_cache(ctx);

        // Poll for spawn commands targeting this host. A spawn carrying a `prompt`
        // tag returns its new session with the first message already in chat;
        // dispatch it to the backend now (we hold the egui context it needs).
        let spawned_with_prompt = self.poll_session_command_events(ctx);
        for sid in spawned_with_prompt {
            self.send_user_message_for(sid, ctx, waker);
        }

        // Poll for live run-config updates from PNS relay
        self.poll_run_config_events(ctx.ndb);

        // Poll for live conversation events on all sessions.
        // Returns user messages from remote clients that need backend dispatch.
        // Only dispatch if the session isn't already streaming a response —
        // the message is already in chat, so it will be included when the
        // current stream finishes and we re-dispatch.
        let mut fan_out_keys: Vec<NoteKey> = Vec::new();
        let remote_user_msgs =
            self.poll_remote_conversation_events(ctx.ndb, sk_bytes.as_ref(), &mut fan_out_keys);

        // Re-broadcast each freshly-arrived conversation envelope to the account's
        // private relays it hasn't reached yet. This is the outbound half of
        // cross-device sync for conversation events that never passed through
        // dave's own publish queue — chiefly a user message injected by the
        // `agentium` CLI, which publishes only to the local embedded relay. Our
        // own events already ride `pending_relay_events`; the relay dedupes the
        // overlap by event id. `fan_out_unseen_notes` skips notes already seen on
        // each target relay (and the plaintext rumor, via its `is_rumor` guard).
        if !fan_out_keys.is_empty() {
            let private_relays = ctx.accounts.selected_account_private_relays();
            if !private_relays.is_empty() {
                if let Ok(txn) = Transaction::new(ctx.ndb) {
                    let mut api = ctx.remote.publisher_explicit();
                    notedeck::fan_out_unseen_notes(
                        &mut api,
                        ctx.ndb,
                        &txn,
                        &fan_out_keys,
                        &private_relays,
                    );
                }
            }
        }

        // Say so when a session at rest is held off its reconcile by a note
        // that never indexed; the reconcile itself waits silently.
        reconcile::warn_stalled_reconciles(
            self.session_manager.iter_mut(),
            std::time::Instant::now(),
        );

        for (sid, _msg) in restored_user_msgs.into_iter().chain(remote_user_msgs) {
            let should_dispatch = self
                .session_manager
                .get(sid)
                .is_some_and(|s| s.should_dispatch_remote_message());
            if should_dispatch {
                self.send_user_message_for(sid, ctx, waker);
            }
        }

        self.process_archive_conversion(ctx);
        self.poll_pending_message_load(ctx.ndb);

        // Check if interrupt confirmation has timed out
        self.check_interrupt_timeout();

        // Process incoming AI responses for all sessions. Every event these
        // handlers build is ingested locally into nostrdb; the host's
        // private-sync Session fans the freshly-ingested PNS envelopes out to the
        // private relays, so dave keeps no outbound publish queue of its own.
        let ProcessEventsResult {
            needs_send: sessions_needing_send,
            needs_compact: sessions_needing_compact,
        } = self.process_events(ctx);

        // Build permission response events from remote sessions
        self.publish_pending_perm_responses(ctx);

        // Build spawn command events through the engine (needs the selected
        // account's secret from AppContext). The engine ingests each event
        // locally; the host fans it out to the private relays.
        if !self.pending_spawn_commands.is_empty() {
            if let Some(engine) = secret_key_bytes(ctx.accounts.get_selected_account().keypair())
                .and_then(|sk| embedded_engine(ctx.ndb, &sk))
            {
                for cmd in std::mem::take(&mut self.pending_spawn_commands) {
                    if let Err(e) = engine.prepare_spawn_command(
                        &cmd.target_host,
                        &cmd.cwd.to_string_lossy(),
                        cmd.backend.as_str(),
                        &cmd.spawn_id,
                    ) {
                        tracing::warn!("failed to build spawn command: {:?}", e);
                    }
                }
            }
        }

        // Build resume command events (deleted-chip resume of a session on
        // another host) the same way; the engine ingests, the host fans out.
        if !self.pending_resume_commands.is_empty() {
            if let Some(engine) = secret_key_bytes(ctx.accounts.get_selected_account().keypair())
                .and_then(|sk| embedded_engine(ctx.ndb, &sk))
            {
                for cmd in std::mem::take(&mut self.pending_resume_commands) {
                    if let Err(e) = engine.prepare_resume_command(
                        &cmd.target_host,
                        &cmd.cwd.to_string_lossy(),
                        cmd.backend.as_str(),
                        &cmd.spawn_id,
                        &cmd.target_session_id,
                        &cmd.cli_session_id,
                    ) {
                        tracing::warn!("failed to build resume command: {:?}", e);
                    }
                }
            }
        }

        // Build permission mode command events for remote sessions
        self.publish_pending_mode_commands(ctx);

        // Build interrupt command events for remote sessions
        self.publish_pending_interrupt_commands(ctx);

        // Poll for remote conversation actions (permission responses, commands).
        let applies = self.poll_remote_conversation_actions(ctx.ndb);
        for apply in applies.mode_changes {
            get_backend(&self.backends, apply.backend_type).set_permission_mode(
                apply.backend_sid,
                apply.mode,
                waker.clone(),
            );
        }
        for apply in applies.interrupts {
            get_backend(&self.backends, apply.backend_type)
                .interrupt_session(apply.backend_sid, waker.clone());
        }

        // Poll git status for local agentic sessions
        for session in self.session_manager.iter_mut() {
            if session.is_remote() {
                continue;
            }
            if let Some(agentic) = &mut session.agentic {
                agentic.git_status.poll();
                agentic.git_status.maybe_auto_refresh();
            }
        }

        // Expire pending placeholder sessions that timed out
        loop {
            let expired = self.session_manager.iter().find_map(|s| {
                s.pending_created_at
                    .filter(|t| t.elapsed().as_secs_f64() > PENDING_SESSION_TIMEOUT_SECS)
                    .map(|_| s.id)
            });
            if let Some(id) = expired {
                tracing::warn!("pending session {} timed out, removing", id);
                update::delete_session(
                    &mut self.session_manager,
                    &mut self.focus_queue,
                    get_backend(&self.backends, BackendType::Remote),
                    &mut self.directory_picker,
                    id,
                );
            } else {
                break;
            }
        }

        // Update all session statuses after processing events, publishing the
        // responses of any permissions the runtime allowlist just resolved
        let auto_resolved = self.session_manager.update_all_statuses();
        self.publish_auto_resolved(ctx, &auto_resolved);

        // Publish kind-31988 state events for sessions whose status changed
        self.publish_dirty_session_states(ctx);

        // Reap finished run processes and compute the set of still-running
        // session IDs in a single pass. The cached set is read by the UI layer
        // so we avoid redundant try_wait() syscalls during rendering.
        self.reap_run_processes();

        // Complete async worktree removal and delete session on success
        self.poll_pending_worktree_removal();

        // Publish "deleted" state events for recently deleted sessions
        self.publish_pending_deletions(ctx);

        // Update focus queue from persisted indicator field
        let indicator_iter = self.session_manager.iter().map(|s| (s.id, s.indicator));
        let queue_update = self.focus_queue.update_from_indicators(indicator_iter);

        // Vibrate on Android whenever a session transitions to NeedsInput
        if queue_update.new_needs_input {
            notedeck::platform::try_vibrate();
        }

        // Transition to Pending on queue changes so auto-steal retries
        // across frames if temporarily suppressed (e.g. user is typing).
        if queue_update.changed && self.auto_steal.is_enabled() {
            self.auto_steal = focus_queue::AutoStealState::Pending;
        }

        // Run auto-steal when pending.  Transitions back to Idle once
        // the steal logic executes (even if no switch was needed).
        // Stays Pending while the user is typing or holding modifier keys
        // so it retries next frame.
        //
        // The whole thing is about which session the *window* shows, so it needs
        // one: it reads held modifiers and raises the app. With no window
        // (`--headless`) there is nothing to focus and no keyboard to suppress
        // it, so the request stays Pending — a display that appears later can
        // still honour it — and nothing else here depends on it clearing.
        if let (focus_queue::AutoStealState::Pending, Some(egui_ctx)) = (self.auto_steal, ctx.egui)
        {
            let user_is_typing = self
                .session_manager
                .get_active()
                .is_some_and(|s| !s.input.is_empty());

            // Suppress while modifier keys are held so a chord like
            // ctrl-shift-k (reset session) can't be hijacked by a
            // last-second auto-switch onto the wrong session.
            let holding_modifiers = egui_ctx.input(|i| i.modifiers.any());

            if !user_is_typing && !holding_modifiers {
                let stole_focus = update::process_auto_steal_focus(
                    &mut self.session_manager,
                    &mut self.focus_queue,
                    &self.collapse_state,
                    &mut self.scene,
                    self.show_scene,
                    true,
                    &mut self.home_session,
                );

                if stole_focus {
                    activate_app(egui_ctx);
                }

                self.auto_steal = focus_queue::AutoStealState::Idle;
            }
        }

        // Send continuation messages for all sessions that have queued messages
        for session_id in sessions_needing_send {
            tracing::info!(
                "Session {}: dispatching queued message via send_user_message_for",
                session_id
            );
            self.send_user_message_for(session_id, ctx, waker);
        }

        // Dispatch compact queries for sessions in compact-and-proceed flow
        for session_id in sessions_needing_compact {
            dispatch_compact_for_session(
                &mut self.session_manager,
                &self.backends,
                session_id,
                waker,
            );
        }
    }

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        ui::own_item_spacing(ui);
        self.process_keybindings(ui.ctx());

        let mut app_action: Option<AppAction> = None;

        // Check if we should send a desktop notification (when unfocused and NeedsInput)
        self.notification_state
            .maybe_notify(ui.ctx(), &self.focus_queue, &self.session_manager);

        if let Some(action) = self.ui(ctx, ui).action {
            if let Some(returned_action) = self.handle_ui_action(action, ctx, ui) {
                app_action = Some(returned_action);
            }
        }

        AppResponse::action(app_action)
    }

    fn tab_notifications(&self, _ctx: &AppContext<'_>) -> notedeck::TabNotifications {
        notedeck::TabNotifications::count(self.focus_queue.needs_input_count() as u32)
    }

    /// Contribute the `agentium:<word-id>` reference parser so a session reference
    /// written inline in any note/comment/Dave-chat resolves to the session's
    /// current kind-31988 state event (drawn by the session renderer). Shares the
    /// app's one [`AgentiumSessionCache`](session_cache::AgentiumSessionCache) — cloning
    /// the `Rc` in — so a session referenced by word id resolves off the same
    /// realtime-pumped session fold the foreground reads, and a live update is
    /// reflected in the resolution.
    fn reference_parsers(&self) -> Vec<Box<dyn notedeck::ReferenceParser>> {
        vec![Box::new(reference::AgentiumRefParser::new(
            self.session_cache.clone(),
        ))]
    }

    /// Contribute the session (kind 31988) renderer, so an `agentium:<word-id>` (or
    /// `nostr:`) reference to a session draws a live chip/card of its current
    /// title/status. Shares the app's one
    /// [`AgentiumSessionCache`](session_cache::AgentiumSessionCache) (cloned in, like
    /// headway's issue renderer), so a session referenced by word id folds the same
    /// realtime session state the foreground UI and the reference parser read — a
    /// live status update shows on the chip, not just the open session.
    fn kind_renderers(&self) -> Vec<Box<dyn notedeck::KindRenderer>> {
        vec![Box::new(render::AgentiumSessionRenderer::new(
            self.session_cache.clone(),
        ))]
    }
}

/// Bring the application to the front.
///
/// On macOS, egui's ViewportCommand::Focus focuses the window but doesn't
/// always activate the app (bring it in front of other apps). Stage Manager
/// single-window mode is particularly aggressive, so we use both
/// NSRunningApplication::activateWithOptions and orderFrontRegardless
/// on the key window.
fn activate_app(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);

    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSApplicationActivationOptions, NSRunningApplication};

        // Safety: UI update runs on the main thread
        if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);

            // Activate via NSRunningApplication for per-process activation
            let current = NSRunningApplication::currentApplication();
            current.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);

            // Also force the key window to front regardless of Stage Manager
            if let Some(window) = app.keyWindow() {
                window.orderFrontRegardless();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiMode;

    use nostrdb::{Config, Ndb};
    use notedeck::timed_serializer::TimedSerializer;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn new_session_routes_by_capability_and_hosts() {
        // Thin client (no local agentic backend, AiMode::Chat): start a local
        // chat directly until remote hosts exist, then ask which kind — this is
        // the Android bug, which previously always created a local chat.
        assert_eq!(
            route_new_session(AiMode::Chat, false),
            NewSessionRoute::Chat
        );
        assert_eq!(
            route_new_session(AiMode::Chat, true),
            NewSessionRoute::ChooseKind
        );
        // Locally agentic (desktop): local directory picker until remote hosts
        // exist, then the host picker. Unchanged by this fix.
        assert_eq!(
            route_new_session(AiMode::Agentic, false),
            NewSessionRoute::LocalDirectoryPicker
        );
        assert_eq!(
            route_new_session(AiMode::Agentic, true),
            NewSessionRoute::HostPicker
        );
    }

    pub(crate) fn test_config() -> Config {
        if cfg!(target_os = "windows") {
            Config::new().set_mapsize(32 * 1024 * 1024)
        } else {
            Config::new()
        }
    }

    pub(crate) fn test_secret_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        key[0] = 1;
        key
    }

    pub(crate) fn test_dave(data_path: &DataPath) -> Dave {
        let ndb_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(ndb_dir.path().to_str().unwrap(), &test_config()).unwrap();
        seed_agentic_settings(data_path);
        Dave::new(None, ndb, Waker::noop(), data_path)
    }

    /// Pin the constructed [`Dave`] to [`AiMode::Agentic`], which every test using
    /// [`test_dave`] assumes — the session manager starts empty there.
    ///
    /// Without this the mode is whatever the machine running the test happens to
    /// have installed. `Dave::new` falls back to `ModelConfig::default()` when no
    /// settings are saved, and that is
    /// `from_env(env, has_binary_on_path("claude"), has_binary_on_path("codex"))`:
    /// a dev box with the claude CLI resolves to `BackendType::Claude` and so
    /// Agentic, while CI — where neither CLI is on PATH — falls through to OpenAI
    /// and so `AiMode::Chat`, which *creates a default session immediately*. That
    /// phantom session shifts every session count by one, which is exactly how
    /// these tests passed locally and failed in CI.
    ///
    /// Writing settings first takes `Dave::new`'s saved-settings branch instead,
    /// and `AiProvider::Anthropic` maps to `BackendType::Claude` (see
    /// `ModelConfig::from_settings`), so the mode is the same everywhere.
    fn seed_agentic_settings(data_path: &DataPath) {
        use crate::config::{AiProvider, DaveSettings};
        let mut settings = TimedSerializer::<DaveSettings>::new(
            data_path,
            DataPathType::Setting,
            "dave_settings.json".to_owned(),
        )
        .with_delay(Duration::ZERO);
        assert!(
            settings.try_save(DaveSettings::with_provider(AiProvider::Anthropic)),
            "seed dave settings so the test runs in a deterministic AiMode"
        );
    }

    /// Deleting a session must carry its `cli_session_id`, `custom_title`,
    /// `spawn_id`, and owning `hostname` into the tombstone. The fat kind-31988
    /// note republishes wholesale on every status change, so the winning
    /// (deleted) revision built from a lossy snapshot would strand the backend
    /// `--resume` id, silently lose a user rename, and — by restamping the
    /// tombstone with *this* machine's hostname — make a deleted remote
    /// session's chip resume locally instead of on its owning host. Regression
    /// guard for the fat-note carry-forward hazard.
    #[test]
    fn delete_carries_resume_id_title_and_spawn_into_tombstone() {
        let base_dir = TempDir::new().unwrap();
        let data_path = DataPath::new(base_dir.path());
        let mut dave = test_dave(&data_path);

        let sid = dave.session_manager.new_resumed_session(
            PathBuf::from("/tmp/proj"),
            "cli-xyz".to_string(), // the real CLI --resume id
            "A Session".to_string(),
            AiMode::Agentic,
            BackendType::Remote,
        );
        // The fields the tombstone used to drop.
        let session = dave.session_manager.get_mut(sid).unwrap();
        session.details.custom_title = Some("Renamed".to_string());
        session.spawn_id = Some("spawn-1".to_string());
        // A session owned by another host: its tombstone must keep that host, or
        // clicking the deleted chip would revive it here instead of resuming it
        // on its owner (see `process_pending_open`).
        session.details.hostname = "other-host".to_string();
        // Set an indicator so clearing it is observable: the fixture never had
        // one, so the snapshot already yielded `None` and the delete path's
        // `state.indicator = None` could be removed unnoticed.
        session.indicator = Some(focus_queue::FocusPriority::NeedsInput);

        dave.delete_session(sid);

        assert_eq!(dave.pending_deletions.len(), 1);
        let tomb = &dave.pending_deletions[0];
        assert_eq!(
            tomb.status,
            session_loader::DELETED_STATUS,
            "the snapshot is stamped as a tombstone"
        );
        assert_eq!(
            tomb.indicator, None,
            "a tombstone carries no attention indicator"
        );
        assert_eq!(
            tomb.cli_session_id.as_deref(),
            Some("cli-xyz"),
            "the backend --resume id must survive the tombstone"
        );
        assert_eq!(
            tomb.hostname, "other-host",
            "the owning host must survive the tombstone so a remote chip resumes remotely"
        );
        assert_eq!(
            tomb.custom_title.as_deref(),
            Some("Renamed"),
            "a user rename must survive the tombstone"
        );
        assert_eq!(
            tomb.spawn_id.as_deref(),
            Some("spawn-1"),
            "the spawn linkage must survive the tombstone"
        );
    }

    #[test]
    fn collapse_state_persists_across_restart() {
        let base_dir = TempDir::new().unwrap();
        let data_path = DataPath::new(base_dir.path());

        let mut dave = test_dave(&data_path);
        dave.collapse_serializer = TimedSerializer::new(
            &data_path,
            DataPathType::Setting,
            "collapse_state.json".to_owned(),
        )
        .with_delay(Duration::ZERO);

        dave.toggle_host_collapse("remote-a");
        dave.toggle_cwd_collapse("remote-b", std::path::Path::new("/srv/api"));

        let persisted = dave
            .collapse_serializer
            .get_item()
            .expect("collapse state should be persisted");
        assert!(persisted.is_host_collapsed("remote-a"));
        assert!(persisted.is_cwd_collapsed("remote-b", std::path::Path::new("/srv/api")));

        drop(dave);

        let restored = test_dave(&data_path);
        assert!(restored.collapse_state.is_host_collapsed("remote-a"));
        assert!(restored
            .collapse_state
            .is_cwd_collapsed("remote-b", std::path::Path::new("/srv/api")));
    }

    #[test]
    fn invalid_collapse_state_file_falls_back_to_default() {
        let base_dir = TempDir::new().unwrap();
        let data_path = DataPath::new(base_dir.path());
        let settings_dir = data_path.path(DataPathType::Setting);
        std::fs::create_dir_all(&settings_dir).expect("settings dir should be created");
        std::fs::write(settings_dir.join("collapse_state.json"), "{not valid json")
            .expect("invalid collapse state should be written");

        let mut restored = test_dave(&data_path);

        assert!(
            !restored.collapse_state.is_host_collapsed("remote-a"),
            "invalid saved state should fall back to a clean default"
        );

        // A clean default is also what you get if persistence is never read at
        // all, so that assertion alone can't tell the two apart. What makes the
        // fallback meaningful is that the corrupt file doesn't wedge later
        // saves: toggle, restart, and the toggle must come back.
        restored.collapse_serializer = TimedSerializer::new(
            &data_path,
            DataPathType::Setting,
            "collapse_state.json".to_owned(),
        )
        .with_delay(Duration::ZERO);
        restored.toggle_host_collapse("remote-a");
        drop(restored);

        let reloaded = test_dave(&data_path);
        assert!(
            reloaded.collapse_state.is_host_collapsed("remote-a"),
            "a corrupt file must not wedge saves made after it"
        );
    }

    #[test]
    fn collapse_toggle_rearms_auto_steal_and_persists_current_state() {
        let base_dir = TempDir::new().unwrap();
        let data_path = DataPath::new(base_dir.path());

        let mut dave = test_dave(&data_path);
        dave.collapse_serializer = TimedSerializer::new(
            &data_path,
            DataPathType::Setting,
            "collapse_state.json".to_owned(),
        )
        .with_delay(Duration::ZERO);
        dave.auto_steal = focus_queue::AutoStealState::Idle;
        dave.focus_queue
            .enqueue(42, focus_queue::FocusPriority::NeedsInput);

        dave.toggle_host_collapse("remote-a");

        assert_eq!(dave.auto_steal, focus_queue::AutoStealState::Pending);
        let persisted = dave
            .collapse_serializer
            .get_item()
            .expect("collapse state should be saved");
        assert!(persisted.is_host_collapsed("remote-a"));

        dave.toggle_cwd_collapse("remote-a", std::path::Path::new("/srv/api"));

        let persisted = dave
            .collapse_serializer
            .get_item()
            .expect("collapse state should stay saved");
        assert!(persisted.is_host_collapsed("remote-a"));
        assert!(persisted.is_cwd_collapsed("remote-a", std::path::Path::new("/srv/api")));
    }
}
