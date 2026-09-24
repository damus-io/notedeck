mod agent_status;
mod auto_accept;
mod avatar;
pub mod backend;
pub(crate) mod collapse_state;
pub mod config;
mod conversation;
mod focus_queue;
pub(crate) mod git_status;
pub mod ipc;
pub(crate) mod mesh;
mod notifications;
mod path_normalize;
pub(crate) mod path_utils;
mod publish;
mod quaternion;
pub mod reference;
pub mod render;
pub mod session;
pub mod session_cache;
mod session_commands;
pub mod session_discovery;
mod session_restore_loader;
mod stream_events;

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

use agent_status::AgentStatus;
use backend::{
    AiBackend, BackendType, ClaudeBackend, CodexBackend, Model, OpenAiBackend, RemoteOnlyBackend,
};
use chrono::{Duration, Local};
use conversation::subscribe_conversation_events;
use egui_wgpu::RenderState;
use focus_queue::FocusQueue;
use nostrdb::{NoteKey, Subscription, Transaction};
use nostrdb_net::KeypairUnowned;
use notedeck::{
    timed_serializer::TimedSerializer, ui::is_narrow, AppAction, AppContext, AppResponse, DataPath,
    DataPathType, Waker,
};
use publish::{build_user_send_event, ingest_built_event, session_state_snapshot};
use session_commands::{PendingResumeCommand, PendingSpawnCommand, SpawnIdempotencyRecord};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::string::ToString;
use std::sync::Arc;
use std::time::Instant;
use stream_events::{dispatch_compact_for_session, ProcessEventsResult};

pub use agentium_core::messages::{
    AssistantMessage, DaveApiResponse, ExecutedTool, ImageAttachment, Message, PermissionResponse,
    PermissionResponseType, QuestionAnswer, QuestionSetInput, RunningTool, SessionInfo,
    SubagentInfo, SubagentStatus, UserMessage,
};
pub use avatar::DaveAvatar;
pub use config::{AiMode, AiProvider, DaveSettings, ModelConfig, RunConfig};
pub use quaternion::Quaternion;
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

/// Per-frame time budget for draining background-restored sessions into the
/// manager, so a large restore fills in over several frames without stalling any
/// single one (mirrors `notedeck_columns`' `TIMELINE_LOADER_APPLY_BUDGET`).
const SESSION_RESTORE_APPLY_BUDGET: std::time::Duration = std::time::Duration::from_millis(2);

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

#[derive(Clone, Debug, Eq, PartialEq)]
struct PnsLocalState {
    account: nostrdb_net::Pubkey,
    has_secret_key: bool,
}

struct PnsLocalRuntime {
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

    fn kill_run_processes(&mut self) {
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
    /// `agentium:` chip is clicked in another app (a note, a Dave chat). Resolved
    /// to a session and switched to on the next [`update`](Self::update), then
    /// cleared. See [`Self::open`] / [`Self::process_pending_open`].
    pending_open: Option<nostrdb_net::NoteId>,
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
    /// events across every session. Notes are demuxed by their `d`-tag
    /// (`event_session_id`) to the owning session in
    /// `poll_remote_conversation_events`, so the number of live sessions is no
    /// longer bounded by nostrdb's per-db subscription cap.
    conversation_sub: Option<nostrdb::Subscription>,
    /// Independent shared cursor over the same kind-1988 events, consumed by
    /// `poll_remote_conversation_actions` (permission responses / mode commands)
    /// at a different point in the frame than `conversation_sub`.
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

use crate::ui::keybindings::KeyAction;

/// Kill a spawned process and all of its descendants.
///
/// On Unix, we use the process group created at spawn time (via `process_group(0)`),
/// sending SIGKILL to the entire group so that grandchildren like `cargo`, `rustc`,
/// or a compiled binary are all terminated.
///
/// On non-Unix platforms we fall back to killing only the immediate child.
fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child's PID is also its PGID because we called process_group(0) at spawn.
        // A negative PID in kill(2) targets the entire process group.
        let pgid = child.id() as libc::pid_t;
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

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

/// Subscription waiting for ndb to index 1988 conversation events.
struct PendingMessageLoad {
    /// ndb subscription for kind-1988 events matching the session
    sub: Subscription,
    /// Account that signed the archived conversation events.
    account: nostrdb_net::Pubkey,
    /// Dave's internal session ID
    dave_session_id: SessionId,
    /// Claude session ID (the `d` tag value)
    claude_session_id: String,
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
        self.pending_open = Some(note);
    }

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
    fn process_pending_open(&mut self, ndb: &nostrdb::Ndb) {
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

    fn ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        // Check overlays first — take ownership so we can call &mut self
        // methods freely. Put the variant back if the overlay stays open.
        let overlay = std::mem::take(&mut self.active_overlay);
        match overlay {
            DaveOverlay::Settings => {
                match ui::settings_overlay_ui(
                    &mut self.settings_panel,
                    &self.settings,
                    app_ctx.i18n,
                    ui,
                ) {
                    OverlayResult::ApplySettings(new_settings) => {
                        self.apply_settings(new_settings.clone());
                        return DaveResponse::new(DaveAction::UpdateSettings(new_settings));
                    }
                    OverlayResult::Close => {}
                    _ => {
                        self.active_overlay = DaveOverlay::Settings;
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::NewSessionKind => {
                let has_sessions = !self.session_manager.is_empty();
                match ui::session_kind_picker_overlay_ui(ui, has_sessions) {
                    OverlayResult::NewSessionChat => {
                        let cwd = std::env::current_dir().unwrap_or_default();
                        self.create_session_with_cwd(
                            cwd,
                            self.model_config.backend,
                            Model::Default,
                        );
                        self.active_overlay = DaveOverlay::None;
                    }
                    OverlayResult::NewSessionAgentic => {
                        self.active_overlay = DaveOverlay::HostPicker;
                    }
                    OverlayResult::Close => {}
                    _ => {
                        self.active_overlay = DaveOverlay::NewSessionKind;
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::HostPicker => {
                let has_sessions = !self.session_manager.is_empty();
                let known_hosts = self.known_remote_hosts();
                match ui::host_picker_overlay_ui(&self.hostname, &known_hosts, has_sessions, ui) {
                    OverlayResult::HostSelected(host) => {
                        self.directory_picker.target_host = host;
                        self.active_overlay = DaveOverlay::DirectoryPicker;
                    }
                    OverlayResult::Close => {}
                    _ => {
                        self.active_overlay = DaveOverlay::HostPicker;
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::DirectoryPicker => {
                let has_sessions = !self.session_manager.is_empty();
                match ui::directory_picker_overlay_ui(&mut self.directory_picker, has_sessions, ui)
                {
                    OverlayResult::DirectorySelected(path) => {
                        if let Some(target_host) = self.directory_picker.target_host.take() {
                            tracing::info!(
                                "remote directory selected: {:?} on {}",
                                path,
                                target_host
                            );
                            self.queue_spawn_command(
                                &target_host,
                                &path,
                                self.model_config.backend,
                            );
                        } else {
                            tracing::info!("directory selected: {:?}", path);
                            self.create_or_pick_backend(path, None);
                        }
                    }
                    OverlayResult::Close => {
                        self.directory_picker.target_host = None;
                    }
                    _ => {
                        self.active_overlay = DaveOverlay::DirectoryPicker;
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::SessionPicker { backend, model } => {
                match ui::session_picker_overlay_ui(&mut self.session_picker, ui) {
                    OverlayResult::ResumeSession {
                        cwd,
                        session_id,
                        title,
                        file_path,
                    } => {
                        // Resumed sessions are always Claude (discovered from JSONL)
                        let claude_session_id = session_id.clone();
                        let sid = self.create_resumed_session_with_cwd(
                            cwd,
                            session_id,
                            title,
                            BackendType::Claude,
                        );
                        self.pending_archive_convert = Some((file_path, sid, claude_session_id));
                        self.session_picker.close();
                    }
                    OverlayResult::NewSession { cwd } => {
                        tracing::info!(
                            "new session from session picker: {:?} (backend: {:?})",
                            cwd,
                            backend
                        );
                        self.session_picker.close();
                        self.create_session_with_cwd(cwd, backend, model.clone());
                    }
                    OverlayResult::BackToDirectoryPicker => {
                        self.session_picker.close();
                        self.active_overlay = DaveOverlay::DirectoryPicker;
                    }
                    _ => {
                        self.active_overlay = DaveOverlay::SessionPicker { backend, model };
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::BackendPicker {
                cwd,
                target_host,
                mut selected_models,
            } => {
                if let Some((bt, model)) = ui::backend_picker_overlay_ui(
                    &self.available_backends,
                    &mut selected_models,
                    ui,
                ) {
                    tracing::info!("backend selected: {:?}, model: {:?}", bt, model);
                    if let Some(host) = target_host {
                        self.queue_spawn_command(&host, &cwd, bt);
                    } else {
                        self.create_or_resume_session(cwd, bt, model);
                    }
                } else {
                    self.active_overlay = DaveOverlay::BackendPicker {
                        cwd,
                        target_host,
                        selected_models,
                    };
                }
                return DaveResponse::default();
            }
            DaveOverlay::WorktreeCreator(mut creator) => {
                match ui::worktree_creator_overlay_ui(&mut creator, ui, &self.available_backends) {
                    Some(ui::WorktreeCreatorAction::Created {
                        worktree_path,
                        branch,
                        is_new_branch,
                        backend_type,
                    }) => {
                        match worktree::create_git_worktree(
                            &creator.from_cwd,
                            &worktree_path,
                            &branch,
                            is_new_branch,
                        ) {
                            Ok(()) => {
                                self.create_session_with_cwd(
                                    worktree_path,
                                    backend_type,
                                    Model::Default,
                                );
                            }
                            Err(msg) => {
                                creator.error = Some(msg);
                                self.active_overlay = DaveOverlay::WorktreeCreator(creator);
                            }
                        }
                    }
                    Some(ui::WorktreeCreatorAction::Cancelled) => { /* overlay closes */ }
                    None => {
                        self.active_overlay = DaveOverlay::WorktreeCreator(creator);
                    }
                }

                return DaveResponse::default();
            }
            DaveOverlay::RunConfigEditor(mut editor) => {
                match ui::run_config_editor_overlay_ui(&mut editor, ui) {
                    Some(editor_action) => {
                        let change = editor_action.process(&mut self.run_configs);
                        if let ui::RunConfigChange::Deleted { ref config_id, .. } = change {
                            self.kill_run_config_processes(config_id);
                        }
                        if let Some(sk) =
                            secret_key_bytes(app_ctx.accounts.get_selected_account().keypair())
                        {
                            match change {
                                ui::RunConfigChange::Saved { cwd, config } => {
                                    self.publish_run_config(&config, &cwd, app_ctx.ndb, &sk);
                                }
                                ui::RunConfigChange::Deleted { cwd, config_id } => {
                                    self.publish_run_config_delete(
                                        &config_id,
                                        &cwd,
                                        app_ctx.ndb,
                                        &sk,
                                    );
                                }
                                ui::RunConfigChange::None => {}
                            }
                        }
                    }
                    None => {
                        self.active_overlay = DaveOverlay::RunConfigEditor(editor);
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::None => {}
        }

        // Normal routing
        if is_narrow(ui.ctx()) {
            self.narrow_ui(app_ctx, ui)
        } else if self.show_scene {
            self.scene_ui(app_ctx, ui)
        } else {
            self.desktop_ui(app_ctx, ui)
        }
    }

    /// Scene view with RTS-style agent visualization and chat side panel
    fn scene_ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let is_interrupt_pending = self.is_interrupt_pending();
        let (dave_response, view_action) = ui::scene_ui(
            &mut self.session_manager,
            &mut self.scene,
            &mut self.focus_queue,
            &self.model_config,
            is_interrupt_pending,
            self.auto_steal.is_enabled(),
            self.chord.view(),
            &self.run_configs,
            &self.running_session_ids,
            app_ctx,
            ui,
        );

        // Handle view actions
        match view_action {
            SceneViewAction::ToggleToListView => {
                self.show_scene = false;
            }
            SceneViewAction::SpawnAgent => {
                return DaveResponse::new(DaveAction::NewChat);
            }
            SceneViewAction::DeleteSelected(ids) => {
                for id in ids {
                    self.delete_session(id);
                }
                if let Some(session) = self.session_manager.sessions_ordered().first() {
                    self.scene.select(session.id);
                } else {
                    self.scene.clear_selection();
                }
            }
            SceneViewAction::SelectedSession(id) => {
                self.anchor_focus(id);
            }
            SceneViewAction::None => {}
        }

        dave_response
    }

    /// Desktop layout with sidebar for session list
    fn desktop_ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let is_interrupt_pending = self.is_interrupt_pending();
        let (chat_response, session_action, toggle_scene) = ui::desktop_ui(
            &mut self.session_manager,
            &self.focus_queue,
            &self.collapse_state,
            &self.model_config,
            is_interrupt_pending,
            self.auto_steal.is_enabled(),
            self.chord.view(),
            &self.run_configs,
            &self.running_session_ids,
            app_ctx,
            ui,
        );

        if toggle_scene {
            self.show_scene = true;
        }

        if let Some(action) = session_action {
            match action {
                SessionListAction::NewSession => return DaveResponse::new(DaveAction::NewChat),
                SessionListAction::SwitchTo(id) => {
                    self.session_manager.switch_to(id);
                    self.focus_queue.dequeue(id);
                    self.anchor_focus(id);
                }
                SessionListAction::Delete(id) => {
                    self.delete_session(id);
                }
                SessionListAction::Rename(id, new_title) => {
                    self.rename_session(id, new_title);
                }
                SessionListAction::DismissDone(id) => {
                    self.focus_queue.dequeue_done(id);
                    if let Some(session) = self.session_manager.get_mut(id) {
                        if session.indicator == Some(focus_queue::FocusPriority::Done) {
                            session.indicator = None;
                            session.state_dirty = true;
                        }
                    }
                }
                SessionListAction::Duplicate(id) => {
                    self.duplicate_session(id);
                }
                SessionListAction::Reset(id) => {
                    self.clear_session(id);
                }
                SessionListAction::NewWorktree(session_id) => {
                    if let Some((cwd, backend_type)) = self
                        .session_manager
                        .get(session_id)
                        .and_then(|s| s.cwd().cloned().map(|c| (c, s.backend_type)))
                    {
                        self.active_overlay = DaveOverlay::WorktreeCreator(Box::new(
                            ui::WorktreeCreator::new(session_id, cwd, backend_type),
                        ));
                    }
                }
                SessionListAction::DeleteWorktree(session_id) => {
                    if let Some(cwd) = self
                        .session_manager
                        .get(session_id)
                        .and_then(|s| s.cwd().cloned())
                    {
                        self.pending_worktree_removals
                            .push(PendingWorktreeRemoval::spawn(session_id, cwd));
                    }
                }
                SessionListAction::ToggleHostCollapse(hostname) => {
                    self.toggle_host_collapse(&hostname);
                }
                SessionListAction::ToggleProjectCollapse(hostname, root) => {
                    self.toggle_project_collapse(&hostname, &root);
                }
                SessionListAction::ToggleCwdCollapse(hostname, cwd) => {
                    self.toggle_cwd_collapse(&hostname, &cwd);
                }
                SessionListAction::NewSessionInCwd(hostname, cwd) => {
                    let target_host = if hostname.is_empty() {
                        None
                    } else {
                        Some(hostname)
                    };
                    self.create_or_pick_backend(cwd, target_host);
                }
            }
        }

        chat_response
    }

    /// Narrow/mobile layout - shows either session list or chat
    fn narrow_ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let is_interrupt_pending = self.is_interrupt_pending();
        let (dave_response, session_action) = ui::narrow_ui(
            &mut self.session_manager,
            &self.focus_queue,
            &self.collapse_state,
            &self.model_config,
            is_interrupt_pending,
            self.auto_steal.is_enabled(),
            self.chord.view(),
            &self.run_configs,
            &self.running_session_ids,
            self.show_session_list,
            app_ctx,
            ui,
        );

        if let Some(action) = session_action {
            match action {
                SessionListAction::NewSession => {
                    self.handle_new_chat();
                    self.show_session_list = false;
                }
                SessionListAction::SwitchTo(id) => {
                    self.session_manager.switch_to(id);
                    self.focus_queue.dequeue(id);
                    self.anchor_focus(id);
                    self.show_session_list = false;
                }
                SessionListAction::Delete(id) => {
                    self.delete_session(id);
                }
                SessionListAction::Rename(id, new_title) => {
                    self.rename_session(id, new_title);
                }
                SessionListAction::DismissDone(id) => {
                    self.focus_queue.dequeue_done(id);
                    if let Some(session) = self.session_manager.get_mut(id) {
                        if session.indicator == Some(focus_queue::FocusPriority::Done) {
                            session.indicator = None;
                            session.state_dirty = true;
                        }
                    }
                }
                SessionListAction::Duplicate(id) => {
                    self.duplicate_session(id);
                    self.show_session_list = false;
                }
                SessionListAction::Reset(id) => {
                    self.clear_session(id);
                    self.show_session_list = false;
                }
                SessionListAction::NewWorktree(session_id) => {
                    if let Some((cwd, backend_type)) = self
                        .session_manager
                        .get(session_id)
                        .and_then(|s| s.cwd().cloned().map(|c| (c, s.backend_type)))
                    {
                        self.active_overlay = DaveOverlay::WorktreeCreator(Box::new(
                            ui::WorktreeCreator::new(session_id, cwd, backend_type),
                        ));
                        self.show_session_list = false;
                    }
                }
                SessionListAction::DeleteWorktree(session_id) => {
                    if let Some(cwd) = self
                        .session_manager
                        .get(session_id)
                        .and_then(|s| s.cwd().cloned())
                    {
                        self.pending_worktree_removals
                            .push(PendingWorktreeRemoval::spawn(session_id, cwd));
                    }
                }
                SessionListAction::ToggleHostCollapse(hostname) => {
                    self.toggle_host_collapse(&hostname);
                }
                SessionListAction::ToggleProjectCollapse(hostname, root) => {
                    self.toggle_project_collapse(&hostname, &root);
                }
                SessionListAction::ToggleCwdCollapse(hostname, cwd) => {
                    self.toggle_cwd_collapse(&hostname, &cwd);
                }
                SessionListAction::NewSessionInCwd(hostname, cwd) => {
                    let target_host = if hostname.is_empty() {
                        None
                    } else {
                        Some(hostname)
                    };
                    self.create_or_pick_backend(cwd, target_host);
                    self.show_session_list = false;
                }
            }
        }

        dave_response
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
    fn drain_session_restore(&mut self, waker: &Waker) {
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
    fn pump_session_cache(&mut self, ctx: &mut AppContext<'_>) {
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
    fn poll_session_state_events(&mut self, ctx: &mut AppContext<'_>) {
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
    fn reopen_session(
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

    fn kill_session_run_processes(&mut self, id: SessionId) {
        if let Some(mut procs) = self.run_processes.remove(&id) {
            for (_, mut child) in procs.drain() {
                kill_process_tree(&mut child);
                self.pending_reap.push(child);
            }
        }
        self.running_session_ids.remove(&id);
    }

    /// Handle an interrupt request - requires double-Escape to confirm
    fn handle_interrupt_request(&mut self, ctx: &egui::Context) {
        let bt = self
            .session_manager
            .get_active()
            .map(|s| s.backend_type)
            .unwrap_or(BackendType::Remote);
        let outcome = update::handle_interrupt_request(
            &self.session_manager,
            get_backend(&self.backends, bt),
            self.interrupt_pending_since,
            ctx,
        );
        self.interrupt_pending_since = outcome.pending_since;
        if let Some(publish) = outcome.publish {
            self.pending_interrupt_commands.push(publish);
        }
    }

    /// Check if interrupt confirmation has timed out and clear it
    fn check_interrupt_timeout(&mut self) {
        self.interrupt_pending_since =
            update::check_interrupt_timeout(self.interrupt_pending_since);
    }

    /// Returns true if an interrupt is pending confirmation
    pub fn is_interrupt_pending(&self) -> bool {
        self.interrupt_pending_since.is_some()
    }

    /// Reap finished run processes and update `self.running_session_ids` in one pass.
    /// Called once per frame from `update()`.
    fn reap_run_processes(&mut self) {
        let mut still_running: HashMap<SessionId, HashSet<String>> = HashMap::new();
        for (session_id, procs) in self.run_processes.iter_mut() {
            procs.retain(|cfg_id, child| match child.try_wait() {
                Ok(None) => {
                    still_running
                        .entry(*session_id)
                        .or_default()
                        .insert(cfg_id.clone());
                    true
                }
                Ok(Some(status)) => {
                    tracing::trace!(
                        "run process [{cfg_id}] for session {session_id} exited: {status}"
                    );
                    false
                }
                Err(e) => {
                    tracing::warn!(
                        "run process [{cfg_id}] for session {session_id} try_wait error: {e}"
                    );
                    false
                }
            });
        }
        self.run_processes.retain(|_, procs| !procs.is_empty());
        self.running_session_ids = still_running;
    }

    /// Reap killed child processes without blocking; removes entries that have exited.
    fn poll_pending_reap(&mut self) {
        self.pending_reap
            .retain_mut(|child| child.try_wait().ok().flatten().is_none());
    }

    /// Poll ndb for new kind-31991 run-config events and upsert into `self.run_configs`.
    ///
    /// Each event is one config (d-tag = config UUID). Live events may be
    /// upserts (name/command changed) or tombstones (deleted tag present).
    fn poll_run_config_events(&mut self, ndb: &nostrdb::Ndb) {
        let Some(sub) = self.run_config_sub else {
            return;
        };
        let Some(account) = self.pns_local_state.as_ref().map(|state| state.account) else {
            return;
        };
        let note_keys = ndb.poll_for_notes(sub, 1);
        if note_keys.is_empty() {
            return;
        }
        let Ok(txn) = nostrdb::Transaction::new(ndb) else {
            return;
        };
        for key in note_keys {
            let Ok(note) = ndb.get_note_by_key(&txn, key) else {
                continue;
            };
            if note.kind() != crate::config::AI_RUN_CONFIG_KIND {
                continue;
            }
            if *note.pubkey() != *account.bytes() {
                continue;
            }
            if session_events::get_tag_value(&note, "hostname") != Some(self.hostname.as_str()) {
                continue;
            }
            if session_events::is_run_config_deleted(&note) {
                // Tombstone: remove config by d-tag ID, only if newer
                let ts = note.created_at();
                if let Some(config_id) = session_events::run_config_event_id(&note) {
                    let mut removed = false;
                    for configs in self.run_configs.values_mut() {
                        let before = configs.len();
                        configs.retain(|c| c.id != config_id || c.updated_at > ts);
                        if configs.len() < before {
                            removed = true;
                        }
                    }
                    if removed {
                        self.kill_run_config_processes(&config_id);
                    }
                    self.run_configs.retain(|_, v| !v.is_empty());
                }
            } else if let Some((cwd, config)) = session_events::parse_run_config_event(&note) {
                // Upsert: update existing or insert new, only if newer
                let configs = self.run_configs.entry(cwd).or_default();
                if let Some(existing) = configs.iter_mut().find(|c| c.id == config.id) {
                    if config.updated_at >= existing.updated_at {
                        existing.name = config.name;
                        existing.command = config.command;
                        existing.updated_at = config.updated_at;
                    }
                } else {
                    configs.push(config);
                }
                RunConfig::sort_by_name(configs);
            }
        }
    }

    /// Kill a running process for the given session and config ID.
    fn kill_run_process(&mut self, session_id: &SessionId, config_id: &str) {
        if let Some(procs) = self.run_processes.get_mut(session_id) {
            if let Some(mut child) = procs.remove(config_id) {
                kill_process_tree(&mut child);
                self.pending_reap.push(child);
            }
            if procs.is_empty() {
                self.run_processes.remove(session_id);
            }
        }
        if let Some(ids) = self.running_session_ids.get_mut(session_id) {
            ids.remove(config_id);
            if ids.is_empty() {
                self.running_session_ids.remove(session_id);
            }
        }
    }

    /// Kill all running processes for a given config ID across all sessions.
    fn kill_run_config_processes(&mut self, config_id: &str) {
        let session_ids: Vec<_> = self.run_processes.keys().copied().collect();
        for sid in session_ids {
            self.kill_run_process(&sid, config_id);
        }
    }

    /// Collect all existing run configs as editor suggestions.
    fn collect_run_config_suggestions(&self, exclude_id: Option<&str>) -> Vec<RunConfig> {
        ui::run_config_editor::collect_run_config_suggestions(&self.run_configs, exclude_id)
    }

    /// Build and queue a kind-31991 event for a single run config.
    fn publish_run_config(
        &mut self,
        config: &RunConfig,
        cwd: &std::path::Path,
        ndb: &nostrdb::Ndb,
        sk: &[u8; 32],
    ) {
        ingest_built_event(
            session_events::build_run_config_event(
                config,
                &cwd.to_string_lossy(),
                &self.hostname,
                sk,
            ),
            "run-config",
            ndb,
            sk,
        );
    }

    /// Build and queue a tombstone kind-31991 event to delete a config.
    fn publish_run_config_delete(
        &mut self,
        config_id: &str,
        cwd: &std::path::Path,
        ndb: &nostrdb::Ndb,
        sk: &[u8; 32],
    ) {
        ingest_built_event(
            session_events::build_run_config_delete_event(
                config_id,
                &cwd.to_string_lossy(),
                &self.hostname,
                sk,
            ),
            "run-config-delete",
            ndb,
            sk,
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

    /// Get the first pending permission request ID for the active session
    fn first_pending_permission(&self) -> Option<uuid::Uuid> {
        update::first_pending_permission(&self.session_manager)
    }

    /// Check if the first pending permission is a shared question-set prompt
    fn has_pending_question(&self) -> bool {
        update::has_pending_question(&self.session_manager)
    }

    /// Check and dispatch keybindings. Called from render() so that
    /// key consumption only happens when Dave is the active app.
    fn process_keybindings(&mut self, egui_ctx: &egui::Context) {
        // While the settings panel records a new leader, it owns the keyboard.
        if self.settings_panel.is_capturing_leader() {
            return;
        }

        let has_pending_permission = self.first_pending_permission().is_some();
        let has_pending_question = self.has_pending_question();
        let in_tentative_state = self
            .session_manager
            .get_active()
            .and_then(|s| s.agentic.as_ref())
            .map(|a| a.permission_message_state != crate::session::PermissionMessageState::None)
            .unwrap_or(false);
        let active_ai_mode = self
            .session_manager
            .get_active()
            .map(|s| s.ai_mode)
            .unwrap_or(self.ai_mode);
        // The chord's `h` needs the session list on screen: the desktop layout,
        // with no overlay covering it.
        let sessions_shown = !is_narrow(egui_ctx)
            && !self.show_scene
            && matches!(self.active_overlay, DaveOverlay::None);
        if let Some(key_action) = check_keybindings(
            egui_ctx,
            &mut self.chord,
            self.leader,
            sessions_shown,
            has_pending_permission,
            has_pending_question,
            in_tentative_state,
            active_ai_mode,
        ) {
            self.handle_key_action(key_action, egui_ctx);
        }
        ui::settle_chord_focus(&mut self.chord, &mut self.session_manager);
    }

    /// Handle a keybinding action
    fn handle_key_action(&mut self, key_action: KeyAction, egui_ctx: &egui::Context) {
        let bt = self
            .session_manager
            .get_active()
            .map(|s| s.backend_type)
            .unwrap_or(BackendType::Remote);
        match ui::handle_key_action(
            key_action,
            &mut self.session_manager,
            &mut self.scene,
            &mut self.focus_queue,
            &self.collapse_state,
            get_backend(&self.backends, bt),
            self.show_scene,
            self.auto_steal.is_enabled(),
            &mut self.home_session,
            egui_ctx,
        ) {
            KeyActionResult::ToggleView => {
                self.show_scene = !self.show_scene;
            }
            KeyActionResult::HandleInterrupt => {
                self.handle_interrupt_request(egui_ctx);
            }
            KeyActionResult::CloneAgent => {
                self.clone_active_agent();
            }
            KeyActionResult::NewAgent => {
                self.handle_new_chat();
            }
            KeyActionResult::DeleteSession(id) => {
                self.delete_session(id);
            }
            KeyActionResult::ClearAgent => {
                if let Some(id) = self.session_manager.active_id() {
                    self.clear_session(id);
                }
            }
            KeyActionResult::SetAutoSteal(new_state) => {
                self.auto_steal = if new_state {
                    focus_queue::AutoStealState::Pending
                } else {
                    focus_queue::AutoStealState::Disabled
                };
            }
            KeyActionResult::PublishPermissionResponse(publish) => {
                self.pending_perm_responses.push(publish);
            }
            KeyActionResult::PublishModeCommand(cmd) => {
                self.pending_mode_commands.push(cmd);
            }
            KeyActionResult::None => {}
        }
    }

    /// Handle the Send action, including tentative permission states
    fn handle_send_action(&mut self, ctx: &AppContext, ui: &egui::Ui) {
        let bt = self
            .session_manager
            .get_active()
            .map(|s| s.backend_type)
            .unwrap_or(BackendType::Remote);
        match ui::handle_send_action(
            &mut self.session_manager,
            get_backend(&self.backends, bt),
            ui.ctx(),
        ) {
            SendActionResult::SendMessage => {
                self.handle_user_send(ctx);
            }
            SendActionResult::NeedsRelayPublish(publish) => {
                self.pending_perm_responses.push(publish);
            }
            SendActionResult::Handled => {}
        }
    }

    /// Handle a UI action from DaveUi
    fn handle_ui_action(
        &mut self,
        action: DaveAction,
        ctx: &AppContext,
        ui: &egui::Ui,
    ) -> Option<AppAction> {
        // Intercept NewChat to handle chat vs agentic mode
        if matches!(action, DaveAction::NewChat) {
            self.handle_new_chat();
            return None;
        }

        // Intercept run-app actions — handled here, not in ui::handle_ui_action
        if let DaveAction::Run(run_action) = action {
            use ui::RunAction;
            match run_action {
                RunAction::Launch { config_id } => {
                    if let Some(session) = self.session_manager.get_active() {
                        let session_id = session.id;
                        let cwd = session.cwd().cloned();
                        let cmd = cwd
                            .as_deref()
                            .and_then(|p| self.run_configs.get(p))
                            .and_then(|cfgs| cfgs.iter().find(|rc| rc.id == config_id))
                            .map(|rc| rc.command.clone());
                        match (cwd, cmd) {
                            (Some(cwd), Some(cmd)) => {
                                tracing::trace!(
                                    "RunAction::Launch: spawning `{cmd}` in {}",
                                    cwd.display()
                                );
                                #[cfg(unix)]
                                let mut command = std::process::Command::new("sh");
                                #[cfg(windows)]
                                let mut command = std::process::Command::new("cmd");
                                #[cfg(unix)]
                                command.arg("-c").arg(&cmd);
                                #[cfg(windows)]
                                command.arg("/C").arg(&cmd);
                                command
                                    .current_dir(&cwd)
                                    .stdin(std::process::Stdio::null())
                                    .stdout(std::process::Stdio::inherit())
                                    .stderr(std::process::Stdio::inherit());
                                #[cfg(unix)]
                                {
                                    use std::os::unix::process::CommandExt;
                                    command.process_group(0);
                                }
                                match command.spawn() {
                                    Ok(child) => {
                                        tracing::info!(
                                            "RunAction::Launch: spawned pid {}",
                                            child.id()
                                        );
                                        self.run_processes
                                            .entry(session_id)
                                            .or_default()
                                            .insert(config_id, child);
                                    }
                                    Err(e) => {
                                        tracing::error!("failed to spawn run command `{cmd}`: {e}");
                                    }
                                }
                            }
                            (cwd, cmd) => {
                                tracing::warn!(
                                    "RunAction::Launch: missing cwd or command (cwd={:?}, has_cmd={})",
                                    cwd,
                                    cmd.is_some()
                                );
                            }
                        }
                    }
                }
                RunAction::Stop { config_id } => {
                    if let Some(session_id) = self.session_manager.active_id() {
                        self.kill_run_process(&session_id, &config_id);
                    }
                }
                RunAction::OpenNew { cwd } => {
                    let suggestions = self.collect_run_config_suggestions(None);
                    self.active_overlay = DaveOverlay::RunConfigEditor(Box::new(
                        RunConfigEditor::new_config(cwd, suggestions),
                    ));
                }
                RunAction::OpenEdit { cwd, config_id } => {
                    let existing = self
                        .run_configs
                        .get(&cwd)
                        .and_then(|cfgs| cfgs.iter().find(|c| c.id == config_id))
                        .cloned();
                    if let Some(config) = existing {
                        let suggestions = self.collect_run_config_suggestions(Some(&config_id));
                        self.active_overlay = DaveOverlay::RunConfigEditor(Box::new(
                            RunConfigEditor::edit_config(cwd, config, suggestions),
                        ));
                    }
                }
            }
            return None;
        }

        let bt = self
            .session_manager
            .get_active()
            .map(|s| s.backend_type)
            .unwrap_or(BackendType::Remote);
        match ui::handle_ui_action(
            action,
            &mut self.session_manager,
            get_backend(&self.backends, bt),
            &mut self.active_overlay,
            &mut self.show_session_list,
            ui.ctx(),
        ) {
            UiActionResult::AppAction(app_action) => Some(app_action),
            UiActionResult::SendAction => {
                self.handle_send_action(ctx, ui);
                None
            }
            UiActionResult::PublishPermissionResponse(publish) => {
                self.pending_perm_responses.push(publish);
                None
            }
            UiActionResult::PublishModeCommand(cmd) => {
                self.pending_mode_commands.push(cmd);
                None
            }
            UiActionResult::PublishInterruptCommand(cmd) => {
                self.pending_interrupt_commands.push(cmd);
                None
            }
            UiActionResult::ToggleAutoSteal => {
                let new_state = crate::update::toggle_auto_steal(
                    &mut self.session_manager,
                    &mut self.scene,
                    self.show_scene,
                    self.auto_steal.is_enabled(),
                    &mut self.home_session,
                );
                self.auto_steal = if new_state {
                    focus_queue::AutoStealState::Pending
                } else {
                    focus_queue::AutoStealState::Disabled
                };
                None
            }
            UiActionResult::NewChat => {
                self.handle_new_chat();
                None
            }
            UiActionResult::FocusQueueNext => {
                crate::update::focus_queue_next(
                    &mut self.session_manager,
                    &mut self.focus_queue,
                    &self.collapse_state,
                    &mut self.scene,
                    self.show_scene,
                );
                None
            }
            UiActionResult::Compact => {
                self.dispatch_compact(bt, ui);
                None
            }
            UiActionResult::Handled => None,
        }
    }

    /// Record a user-authored message in the target session.
    ///
    /// This uses the same message construction path as the live UI send flow:
    /// create a live user event when possible, append `Message::User` to chat,
    /// and update the session title.
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

        if let Some(sk) = secret_key_bytes(app_ctx.accounts.get_selected_account().keypair()) {
            build_user_send_event(session, app_ctx.ndb, &sk, &user_text);
        }

        session
            .chat
            .push(Message::User(UserMessage::new(user_text, images)));
        session.update_title_from_last_message();

        if session.is_remote() {
            return false;
        }

        if session.is_dispatched() {
            tracing::info!("message queued, will dispatch after current turn");
            return false;
        }

        true
    }

    /// Handle a user send action triggered by the ui
    fn handle_user_send(&mut self, app_ctx: &AppContext) {
        // Check for /cd command first (agentic only)
        let cd_result = self
            .session_manager
            .get_active_mut()
            .and_then(update::handle_cd_command);

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

        // Normal message handling
        if let Some(session) = self.session_manager.get_active_mut() {
            let user_text = session.input.clone();
            session.input.clear();

            // Generate the kind-1988 `user` event (remote sends route through
            // the engine, local sends archive the host turn in-place).
            if let Some(sk) = secret_key_bytes(app_ctx.accounts.get_selected_account().keypair()) {
                build_user_send_event(session, app_ctx.ndb, &sk, &user_text);
            }

            let images = std::mem::take(&mut session.pending_images);
            session
                .chat
                .push(Message::User(UserMessage::new(user_text, images)));
            session.update_title_from_last_message();

            // Remote sessions: publish user message to relay but don't send to local backend
            if session.is_remote() {
                return;
            }

            // If already dispatched (waiting for or receiving response), queue
            // the message in chat without dispatching.
            // needs_redispatch_after_stream_end() will dispatch it when the
            // current turn finishes.
            if session.is_dispatched() {
                tracing::info!("message queued, will dispatch after current turn");
                return;
            }
        }
        self.send_user_message(app_ctx, app_ctx.waker);
    }

    fn send_user_message(&mut self, app_ctx: &AppContext, waker: &Waker) {
        let Some(active_id) = self.session_manager.active_id() else {
            return;
        };
        self.send_user_message_for(active_id, app_ctx, waker);
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

        // Record how many trailing user messages we're dispatching.
        // DispatchState tracks this for append_token insert position,
        // UI queued indicator, and redispatch-after-stream-end logic.
        session.mark_dispatched();

        let user_id = calculate_user_id(app_ctx.accounts.get_selected_account().keypair());
        let session_id = format!("dave-session-{}", session.id);
        // The stable kind-31988 d-tag (UUID), distinct from the ephemeral
        // `dave-session-{n}` routing key above. Subprocess backends export it as
        // the agentium identity so an in-session agent reads its OWN ref. Only
        // agentic sessions have one.
        let agentium_session_id = session
            .agentic
            .as_ref()
            .map(|a| a.event_session_id().to_string());
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
        let backend_type = session.backend_type;
        let tools = self.tools.clone();
        let model_name = session.details.resolve_model();
        // Use backend to stream request. `rx` is `None` for persistent-stream
        // backends on subsequent turns — the session already owns a long-lived
        // channel we must keep, so only replace `incoming_tokens` when a new
        // receiver was minted.
        let (rx, task_handle) = get_backend(&self.backends, backend_type).stream_request(
            messages,
            tools,
            model_name,
            user_id,
            session_id,
            agentium_session_id,
            cwd,
            resume_session_id,
            permission_mode,
            waker.clone(),
        );
        if let Some(rx) = rx {
            session.incoming_tokens = Some(rx);
        }
        session.task_handle = task_handle;
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

    fn process_archive_conversion(&mut self, ctx: &mut AppContext<'_>) {
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
    fn poll_pending_message_load(&mut self, ndb: &nostrdb::Ndb) {
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
    fn ensure_pns_local_state(&mut self, ctx: &mut AppContext<'_>) {
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

    fn subscribe_pns_run_configs(&mut self, ndb: &nostrdb::Ndb, account: nostrdb_net::Pubkey) {
        let rc_filter = nostrdb::Filter::new()
            .kinds([crate::config::AI_RUN_CONFIG_KIND as u64])
            .authors([account.bytes()])
            .build();
        match ndb.subscribe(&[rc_filter]) {
            Ok(sub) => {
                self.run_config_sub = Some(sub);
                tracing::info!("subscribed for run config events in ndb");
            }
            Err(e) => {
                tracing::warn!("failed to subscribe for run config events: {:?}", e);
            }
        }
    }

    fn load_run_configs(&mut self, ndb: &nostrdb::Ndb, account: nostrdb_net::Pubkey) {
        let txn = match nostrdb::Transaction::new(ndb) {
            Ok(txn) => txn,
            Err(err) => {
                tracing::warn!("failed to open txn for run config restore: {err:?}");
                return;
            }
        };
        self.run_configs =
            session_loader::load_run_configs_from_ndb(ndb, &txn, &account, &self.hostname);
        tracing::info!("loaded {} run config CWDs from ndb", self.run_configs.len());
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

        // Focus a session whose inline chip was clicked in another app.
        self.process_pending_open(ctx.ndb);
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
        self.drain_session_restore(ctx.waker);

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
        let sk_bytes = secret_key_bytes(ctx.accounts.get_selected_account().keypair());
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

        for (sid, _msg) in remote_user_msgs {
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

        // Update all session statuses after processing events
        self.session_manager.update_all_statuses();

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

    use crate::session_events::{build_live_event, ThreadingState};
    use nostrdb::{Config, IngestMetadata, Ndb};
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
