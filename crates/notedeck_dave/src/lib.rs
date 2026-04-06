mod agent_status;
mod auto_accept;
mod avatar;
pub mod backend;
pub(crate) mod collapse_state;
pub mod config;
pub mod events;
pub mod file_update;
mod focus_queue;
pub(crate) mod git_status;
pub mod ipc;
pub(crate) mod mesh;
mod messages;
mod path_normalize;
pub(crate) mod path_utils;
mod quaternion;
pub mod session;
pub mod session_converter;
pub mod session_discovery;
pub mod session_events;
pub mod session_jsonl;
pub mod session_loader;
pub mod session_reconstructor;
mod tools;
pub mod ui;
pub mod update;
mod vec3;
pub mod worktree;

use agent_status::AgentStatus;
use backend::{
    AiBackend, BackendType, ClaudeBackend, CodexBackend, Model, OpenAiBackend, RemoteOnlyBackend,
};
use chrono::{Duration, Local};
use egui_wgpu::RenderState;
use enostr::{KeypairUnowned, RelayPool};
use focus_queue::FocusQueue;
use nostrdb::{Subscription, Transaction};
use notedeck::{
    timed_serializer::TimedSerializer, ui::is_narrow, AppAction, AppContext, AppResponse, DataPath,
    DataPathType,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::string::ToString;
use std::sync::Arc;
use std::time::Instant;

pub use avatar::DaveAvatar;
pub use config::{AiMode, AiProvider, DaveSettings, ModelConfig, RunConfig};
pub use messages::{
    AssistantMessage, DaveApiResponse, ExecutedTool, ImageAttachment, Message, PermissionResponse,
    PermissionResponseType, QuestionAnswer, QuestionSetInput, SessionInfo, SubagentInfo,
    SubagentStatus, UserMessage,
};
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

/// Default relay URL used for PNS event publishing and subscription.
const DEFAULT_PNS_RELAY: &str = "ws://relay.jb55.com/";

/// Maximum consecutive negentropy sync rounds before stopping.
/// Each round pulls up to the relay's limit (typically 500 events),
/// so 20 rounds fetches up to ~10000 recent events.
const MAX_NEG_SYNC_ROUNDS: u8 = 20;

/// Normalize a relay URL to always have a trailing slash.
fn normalize_relay_url(url: String) -> String {
    if url.ends_with('/') {
        url
    } else {
        url + "/"
    }
}

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

/// A pending spawn command waiting to be built and published.
struct PendingSpawnCommand {
    target_host: String,
    cwd: PathBuf,
    backend: BackendType,
    /// UUID that links this command to the placeholder session and the
    /// kind-31988 response from the remote host.
    spawn_id: String,
}

/// Represents which full-screen overlay (if any) is currently active.
/// Data-carrying variants hold the state needed for that step in the
/// session-creation flow, replacing scattered `pending_*` fields.
#[derive(Default)]
pub enum DaveOverlay {
    #[default]
    None,
    Settings,
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
    pool: RelayPool,
    /// AI interaction mode (Chat vs Agentic)
    ai_mode: AiMode,
    /// Manages multiple chat sessions
    session_manager: SessionManager,
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
    /// Directory picker for selecting working directory when creating sessions
    directory_picker: DirectoryPicker,
    /// Session picker for resuming existing Claude sessions
    session_picker: SessionPicker,
    /// Current overlay taking over the UI (if any)
    active_overlay: DaveOverlay,
    /// IPC listener for external spawn-agent commands
    ipc_listener: Option<ipc::IpcListener>,
    /// Pending archive conversion: (jsonl_path, dave_session_id, claude_session_id).
    /// Set when resuming a session; processed in update() where AppContext is available.
    pending_archive_convert: Option<(std::path::PathBuf, SessionId, String)>,
    /// Waiting for ndb to finish indexing 1988 events so we can load messages.
    pending_message_load: Option<PendingMessageLoad>,
    /// Events waiting to be published to relays (queued from non-pool contexts).
    pending_relay_events: Vec<session_events::BuiltEvent>,
    /// Whether sessions have been restored from ndb on startup.
    sessions_restored: bool,
    /// Remote relay subscription ID for PNS events (kind-1080).
    /// Used to discover session state events from other devices.
    pns_relay_sub: Option<String>,
    /// Local ndb subscription for kind-31988 session state events.
    /// Fires when new session states are unwrapped from PNS events.
    session_state_sub: Option<nostrdb::Subscription>,
    /// Local ndb subscription for kind-31989 session command events.
    session_command_sub: Option<nostrdb::Subscription>,
    /// Command UUIDs already processed (dedup for spawn commands).
    processed_commands: std::collections::HashSet<String>,
    /// Spawn commands waiting to be built+published in update() where secret key is available.
    pending_spawn_commands: Vec<PendingSpawnCommand>,
    /// Permission responses queued for relay publishing (from remote sessions).
    /// Built and published in the update loop where AppContext is available.
    pending_perm_responses: Vec<PermissionPublish>,
    /// Permission mode commands queued for relay publishing (observer → host).
    pending_mode_commands: Vec<update::ModeCommandPublish>,
    /// Sessions pending deletion state event publication.
    /// Populated in delete_session(), drained in the update loop where AppContext is available.
    pending_deletions: Vec<DeletedSessionInfo>,
    pending_worktree_removals: Vec<PendingWorktreeRemoval>,
    /// Thread summaries pending processing. Queued by summarize_thread(),
    /// resolved in update() where AppContext (ndb) is available.
    pending_summaries: Vec<enostr::NoteId>,
    /// Local machine hostname, included in session state events.
    hostname: String,
    /// PNS relay URL (configurable via DAVE_RELAY env or settings UI).
    pns_relay_url: String,
    /// Negentropy sync state for PNS event reconciliation.
    neg_sync: enostr::negentropy::NegentropySync,
    /// How many consecutive negentropy sync rounds have completed.
    /// Reset on startup/reconnect, incremented each time events are found.
    /// Caps at [`MAX_NEG_SYNC_ROUNDS`] to avoid pulling the entire history.
    neg_sync_round: u8,
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
}

use update::PermissionPublish;

use crate::events::try_process_events_core;
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

/// Result from processing incoming AI backend tokens for all sessions.
struct ProcessEventsResult {
    /// Sessions that need to dispatch queued user messages.
    needs_send: HashSet<SessionId>,
    /// Nostr events to publish to relays.
    events_to_publish: Vec<session_events::BuiltEvent>,
    /// Sessions that need a compact query dispatched (compact-and-proceed).
    needs_compact: HashSet<SessionId>,
}

/// Info captured from a session before deletion, for publishing a "deleted" state event.
struct DeletedSessionInfo {
    claude_session_id: String,
    title: String,
    cwd: String,
    home_dir: String,
    backend: BackendType,
}

/// Subscription waiting for ndb to index 1988 conversation events.
struct PendingMessageLoad {
    /// ndb subscription for kind-1988 events matching the session
    sub: Subscription,
    /// Dave's internal session ID
    dave_session_id: SessionId,
    /// Claude session ID (the `d` tag value)
    claude_session_id: String,
}

/// PNS-wrap an event and ingest the 1080 wrapper into ndb.
///
/// ndb's `process_pns` will unwrap it internally, making the inner
/// event queryable. This ensures 1080 events exist in ndb for relay sync.
fn pns_ingest(ndb: &nostrdb::Ndb, event_json: &str, secret_key: &[u8; 32]) {
    let pns_keys = enostr::pns::derive_pns_keys(secret_key);
    match session_events::wrap_pns(event_json, &pns_keys) {
        Ok(pns_json) => {
            // wrap_pns returns bare {…} JSON; use relay format
            // ["EVENT", "subid", {…}] so ndb triggers PNS unwrapping
            let wrapped = format!("[\"EVENT\", \"_pns\", {}]", pns_json);
            if let Err(e) = ndb.process_event(&wrapped) {
                tracing::warn!("failed to ingest PNS event: {:?}", e);
            }
        }
        Err(e) => {
            tracing::warn!("failed to PNS-wrap for local ingest: {}", e);
        }
    }
}

/// Ingest a freshly-built event: PNS-wrap into local ndb and push to the
/// relay publish queue. Logs on success with `event_desc` and on failure.
/// Returns `true` if the event was queued successfully.
fn queue_built_event(
    result: Result<session_events::BuiltEvent, session_events::EventBuildError>,
    event_desc: &str,
    ndb: &nostrdb::Ndb,
    sk: &[u8; 32],
    queue: &mut Vec<session_events::BuiltEvent>,
) -> bool {
    match result {
        Ok(evt) => {
            tracing::info!("{}", event_desc);
            pns_ingest(ndb, &evt.note_json, sk);
            queue.push(evt);
            true
        }
        Err(e) => {
            tracing::error!("failed to build event ({}): {}", event_desc, e);
            false
        }
    }
}

/// Build and ingest a live kind-1988 event into ndb (via PNS wrapping).
///
/// Extracts cwd and session ID from the session's agentic data,
/// builds the event, PNS-wraps and ingests it, and returns the event
/// for relay publishing.
fn ingest_live_event(
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
        ctx: egui::Context,
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

        let pns_relay_url = normalize_relay_url(
            model_config
                .pns_relay
                .clone()
                .unwrap_or_else(|| DEFAULT_PNS_RELAY.to_string()),
        );

        let directory_picker = DirectoryPicker::new();

        // Create IPC listener for external spawn-agent commands
        let ipc_listener = ipc::create_listener(ctx);

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
                manager.rebuild_cwd_groups();
                (manager, DaveOverlay::None)
            }
            AiMode::Agentic => (SessionManager::new(), DaveOverlay::DirectoryPicker),
        };

        let pool = RelayPool::new();

        Dave {
            pool,
            ai_mode,
            backends,
            available_backends,
            avatar,
            session_manager,
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
            directory_picker,
            session_picker: SessionPicker::new(),
            active_overlay,
            ipc_listener,
            pending_archive_convert: None,
            pending_message_load: None,
            pending_relay_events: Vec::new(),
            sessions_restored: false,
            pns_relay_sub: None,
            session_state_sub: None,
            session_command_sub: None,
            processed_commands: std::collections::HashSet::new(),
            pending_spawn_commands: Vec::new(),
            pending_perm_responses: Vec::new(),
            pending_mode_commands: Vec::new(),
            pending_deletions: Vec::new(),
            pending_worktree_removals: Vec::new(),
            pending_summaries: Vec::new(),
            hostname,
            pns_relay_url,
            neg_sync: enostr::negentropy::NegentropySync::new(),
            neg_sync_round: 0,
            settings_serializer,
            run_processes: HashMap::new(),
            running_session_ids: HashMap::new(),
            run_configs: HashMap::new(),
            pending_reap: Vec::new(),
            run_config_sub: None,
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
        self.pns_relay_url = normalize_relay_url(
            settings
                .pns_relay
                .clone()
                .unwrap_or_else(|| DEFAULT_PNS_RELAY.to_string()),
        );
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
    pub fn summarize_thread(&mut self, note_id: enostr::NoteId) {
        self.pending_summaries.push(note_id);
    }

    /// Fetch the thread from ndb, format it, and create a session with the prompt.
    fn build_summary_session(
        &mut self,
        ndb: &nostrdb::Ndb,
        note_id: &enostr::NoteId,
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
            None,
            Model::Default,
        );

        if let Some(session) = self.session_manager.get_mut(id) {
            session.chat.push(Message::System(system));

            // Show the root note inline so the user can see what's being summarized
            let present = tools::ToolCall::new(
                "summarize-thread".to_string(),
                tools::ToolCalls::PresentNotes(tools::PresentNotesCall {
                    note_ids: vec![enostr::NoteId::new(root_id)],
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

    /// Process incoming tokens from the ai backend for ALL sessions.
    fn process_events(&mut self, app_ctx: &AppContext) -> ProcessEventsResult {
        let mut needs_send: HashSet<SessionId> = HashSet::new();
        let mut events_to_publish: Vec<session_events::BuiltEvent> = Vec::new();
        let mut needs_compact: HashSet<SessionId> = HashSet::new();
        let active_id = self.session_manager.active_id();

        // Extract secret key once for live event generation
        let secret_key = secret_key_bytes(app_ctx.accounts.get_selected_account().keypair());

        // Get all session IDs to process
        let session_ids = self.session_manager.session_ids();

        for session_id in session_ids {
            // Take the receiver out to avoid borrow conflicts
            let recvr = {
                let Some(session) = self.session_manager.get_mut(session_id) else {
                    continue;
                };
                session.incoming_tokens.take()
            };

            let Some(recvr) = recvr else {
                continue;
            };

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

                // Track when we last received any backend message (for stall detection)
                session.last_backend_msg = Some(std::time::Instant::now());

                // Determine the live event to publish for this response.
                // Centralised here so every response type that needs relay
                // propagation is handled in one place.
                let live_event: Option<(String, &str, Option<&str>)> = match &res {
                    DaveApiResponse::Failed(err) => Some((err.clone(), "error", None)),
                    DaveApiResponse::ToolResult(result) => Some((
                        format!("{}: {}", result.tool_name, result.summary),
                        "tool_result",
                        Some(result.tool_name.as_str()),
                    )),
                    DaveApiResponse::CompactionStarted => {
                        Some((String::new(), "compaction_started", None))
                    }
                    DaveApiResponse::CompactionComplete(info) => {
                        Some((info.pre_tokens.to_string(), "compaction_complete", None))
                    }
                    // PermissionRequest has custom event building (below).
                    // Token, ToolCalls, SessionInfo, Subagent* don't publish.
                    _ => None,
                };

                if let Some((content, role, tool_name)) = live_event {
                    if let Some(sk) = &secret_key {
                        if let Some(evt) = ingest_live_event(
                            session,
                            app_ctx.ndb,
                            sk,
                            &content,
                            role,
                            None,
                            tool_name,
                        ) {
                            events_to_publish.push(evt);
                        }
                    }
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
                        session.chat.push(Message::Error(err.to_string()));
                    }
                    DaveApiResponse::Token(token) => {
                        session.append_token(&token);
                    }
                    DaveApiResponse::ToolCalls(toolcalls) => {
                        if handle_tool_calls(session, &toolcalls, app_ctx.ndb) {
                            needs_send.insert(session_id);
                        }
                    }
                    DaveApiResponse::PermissionRequest(pending) => {
                        handle_permission_request(
                            session,
                            pending,
                            &secret_key,
                            app_ctx.ndb,
                            &mut events_to_publish,
                        );
                    }
                    DaveApiResponse::ToolResult(result) => {
                        handle_tool_result(session, result);
                    }
                    DaveApiResponse::SessionInfo(info) => {
                        handle_session_info(session, info, app_ctx.ndb);
                    }
                    DaveApiResponse::SubagentSpawned(subagent) => {
                        handle_subagent_spawned(session, subagent);
                    }
                    DaveApiResponse::SubagentOutput { task_id, output } => {
                        session.update_subagent_output(&task_id, &output);
                    }
                    DaveApiResponse::SubagentCompleted { task_id, result } => {
                        session.complete_subagent(&task_id, &result);
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
                    }
                }
            }

            // Check if channel is disconnected (stream ended)
            match recvr.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if let Some(session) = self.session_manager.get_mut(session_id) {
                        handle_stream_end(
                            session,
                            session_id,
                            &secret_key,
                            app_ctx.ndb,
                            &mut events_to_publish,
                            &mut needs_send,
                            &mut needs_compact,
                        );
                    }
                }
                _ => {
                    // Channel still open — defense-in-depth stall detection.
                    // The backends themselves have proper timeouts on their
                    // async operations, so this should rarely fire. It exists
                    // as a safety net in case a backend hangs in an unforeseen
                    // way (e.g. a new code path without a timeout).
                    if let Some(session) = self.session_manager.get_mut(session_id) {
                        const STALL_TIMEOUT: std::time::Duration =
                            std::time::Duration::from_secs(300);

                        // Skip stall detection during compaction — the backend
                        // can be legitimately silent for a long time while the
                        // LLM provider compacts the context window.
                        let is_compacting =
                            session.agentic.as_ref().is_some_and(|a| a.is_compacting());

                        // Skip stall detection when a permission request is
                        // pending — the backend is legitimately blocked waiting
                        // for the user to accept/deny. The user may be composing
                        // a message in an external editor (Ctrl+G), which can
                        // easily exceed the stall timeout.
                        let has_pending_perm = session.has_pending_permissions();

                        let stalled = !is_compacting
                            && !has_pending_perm
                            && session
                                .last_backend_msg
                                .is_some_and(|t| t.elapsed() > STALL_TIMEOUT);

                        if stalled {
                            let elapsed = session
                                .last_backend_msg
                                .map(|t| t.elapsed().as_secs())
                                .unwrap_or(0);
                            tracing::error!(
                                "Session {}: backend stalled for {}s (safety net), aborting",
                                session_id,
                                elapsed
                            );
                            if let Some(handle) = session.task_handle.take() {
                                handle.abort();
                            }
                            // Clean up the backend's session actor so the next
                            // send_user_message_for() creates a fresh connection
                            // instead of sending commands to a dead actor.
                            let backend_type = session.backend_type;
                            let backend_session_id = format!("dave-session-{}", session_id);
                            get_backend(&self.backends, backend_type)
                                .cleanup_session(backend_session_id);
                            drop(recvr);
                            handle_stream_end(
                                session,
                                session_id,
                                &secret_key,
                                app_ctx.ndb,
                                &mut events_to_publish,
                                &mut needs_send,
                                &mut needs_compact,
                            );
                            if !matches!(session.chat.last(), Some(Message::Error(_))) {
                                session.chat.push(Message::Error(
                                    "Backend timed out (no response for 5 minutes)".into(),
                                ));
                            }
                        } else {
                            session.incoming_tokens = Some(recvr);
                        }
                    }
                }
            }
        }

        ProcessEventsResult {
            needs_send,
            events_to_publish,
            needs_compact,
        }
    }

    fn ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        // Check overlays first — take ownership so we can call &mut self
        // methods freely. Put the variant back if the overlay stays open.
        let overlay = std::mem::take(&mut self.active_overlay);
        match overlay {
            DaveOverlay::Settings => {
                match ui::settings_overlay_ui(&mut self.settings_panel, &self.settings, ui) {
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
        match self.ai_mode {
            AiMode::Chat => {
                // In chat mode, create a session directly without the directory picker
                let cwd = std::env::current_dir().unwrap_or_default();
                self.create_session_with_cwd(cwd, self.model_config.backend, Model::Default);
            }
            AiMode::Agentic => {
                // If remote hosts are known, show host picker first
                if !self.known_remote_hosts().is_empty() {
                    self.active_overlay = DaveOverlay::HostPicker;
                } else {
                    self.directory_picker.target_host = None;
                    self.active_overlay = DaveOverlay::DirectoryPicker;
                }
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

    /// Create a new session with the given cwd (called after directory picker selection)
    fn create_session_with_cwd(&mut self, cwd: PathBuf, backend_type: BackendType, model: Model) {
        update::create_session_with_cwd(
            &mut self.session_manager,
            &mut self.directory_picker,
            &mut self.scene,
            self.show_scene,
            self.ai_mode,
            cwd,
            &self.hostname,
            backend_type,
            None,
            model,
        );
    }

    /// Create a new session that resumes an existing Claude conversation
    fn create_resumed_session_with_cwd(
        &mut self,
        cwd: PathBuf,
        resume_session_id: String,
        title: String,
        backend_type: BackendType,
    ) -> SessionId {
        update::create_resumed_session_with_cwd(
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
        )
    }

    /// Duplicate a session by ID, creating a new session with the same working directory.
    /// For remote sessions, sends a spawn command to the remote host.
    fn duplicate_session(&mut self, id: SessionId) {
        if let Some(spawn) = update::clone_session(
            &mut self.session_manager,
            &mut self.directory_picker,
            &mut self.scene,
            self.show_scene,
            self.ai_mode,
            &self.hostname,
            id,
        ) {
            self.queue_spawn_command(&spawn.host, &spawn.cwd, spawn.backend);
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

        // Drain all pending connections (non-blocking)
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
                        self.scene.focus_on(agentic.scene_position);
                    }
                }
            }
            self.session_manager.rebuild_cwd_groups();

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
    }

    /// Poll for remote conversation actions arriving via nostr relays.
    ///
    /// Dispatches kind-1988 events by `role` tag:
    /// - `permission_response`: route through oneshot channel (first-response-wins)
    /// - `set_permission_mode`: apply mode change locally
    ///
    /// Returns (backend_session_id, backend_type, mode) tuples for mode changes
    /// that need to be applied to the local CLI backend.
    fn poll_remote_conversation_actions(
        &mut self,
        ndb: &nostrdb::Ndb,
    ) -> Vec<(String, BackendType, claude_agent_sdk_rs::PermissionMode)> {
        let mut mode_applies = Vec::new();
        let session_ids = self.session_manager.session_ids();
        for session_id in session_ids {
            let Some(session) = self.session_manager.get_mut(session_id) else {
                continue;
            };
            // Only local sessions poll for remote actions
            if session.is_remote() {
                continue;
            }
            let Some(agentic) = &mut session.agentic else {
                continue;
            };
            let Some(sub) = agentic.conversation_action_sub else {
                continue;
            };

            let note_keys = ndb.poll_for_notes(sub, 64);
            if note_keys.is_empty() {
                continue;
            }

            let txn = match Transaction::new(ndb) {
                Ok(txn) => txn,
                Err(_) => continue,
            };

            for key in note_keys {
                let Ok(note) = ndb.get_note_by_key(&txn, key) else {
                    continue;
                };

                match session_events::get_tag_value(&note, "role") {
                    Some("permission_response") => {
                        handle_remote_permission_response(&note, agentic, &mut session.chat);
                    }
                    Some("set_permission_mode") => {
                        let content = note.content();
                        let mode_str = match serde_json::from_str::<serde_json::Value>(content) {
                            Ok(v) => v
                                .get("mode")
                                .and_then(|m| m.as_str())
                                .unwrap_or("default")
                                .to_string(),
                            Err(_) => continue,
                        };

                        let new_mode = crate::session::permission_mode_from_str(&mode_str);
                        agentic.permission_mode = new_mode;
                        session.state_dirty = true;

                        mode_applies.push((
                            format!("dave-session-{}", session_id),
                            session.backend_type,
                            new_mode,
                        ));

                        tracing::info!(
                            "remote command: set permission mode to {:?} for session {}",
                            new_mode,
                            session_id,
                        );
                    }
                    _ => {}
                }
            }
        }
        mode_applies
    }

    /// Publish kind-31988 state events for sessions whose status changed.
    fn publish_dirty_session_states(&mut self, ctx: &mut AppContext<'_>) {
        let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) else {
            return;
        };

        for session in self.session_manager.iter_mut() {
            if !session.state_dirty {
                continue;
            }

            // Remote sessions are owned by another machine — only the
            // session owner should publish state events.
            if session.is_remote() {
                session.state_dirty = false;
                continue;
            }

            let Some(agentic) = &session.agentic else {
                continue;
            };

            let event_sid = agentic.event_session_id().to_string();
            let cwd = agentic.cwd.to_string_lossy();
            let status = session.status().as_str();
            let indicator = session.indicator.as_ref().map(|i| i.as_str());
            let perm_mode = crate::session::permission_mode_to_str(agentic.permission_mode);
            let cli_sid = agentic.cli_resume_id().map(|s| s.to_string());

            queue_built_event(
                session_events::build_session_state_event(
                    &event_sid,
                    &session.details.title,
                    session.details.custom_title.as_deref(),
                    &cwd,
                    status,
                    indicator,
                    &self.hostname,
                    &session.details.home_dir,
                    session.backend_type.as_str(),
                    perm_mode,
                    cli_sid.as_deref(),
                    session.spawn_id.as_deref(),
                    &sk,
                ),
                &format!("publishing session state: {} -> {}", event_sid, status),
                ctx.ndb,
                &sk,
                &mut self.pending_relay_events,
            );

            session.state_dirty = false;
        }
    }

    /// Publish "deleted" state events for sessions that were deleted.
    /// Called in the update loop where AppContext is available.
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

    fn publish_pending_deletions(&mut self, ctx: &mut AppContext<'_>) {
        if self.pending_deletions.is_empty() {
            return;
        }

        let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) else {
            return;
        };

        for info in std::mem::take(&mut self.pending_deletions) {
            queue_built_event(
                session_events::build_session_state_event(
                    &info.claude_session_id,
                    &info.title,
                    None,
                    &info.cwd,
                    "deleted",
                    None, // no indicator for deleted sessions
                    &self.hostname,
                    &info.home_dir,
                    info.backend.as_str(),
                    "default",
                    None,
                    None, // no spawn_id for deletions
                    &sk,
                ),
                &format!(
                    "publishing deleted session state: {}",
                    info.claude_session_id
                ),
                ctx.ndb,
                &sk,
                &mut self.pending_relay_events,
            );
        }
    }

    /// Build and queue permission response events.
    /// Called in the update loop where AppContext is available.
    fn publish_pending_perm_responses(&mut self, ctx: &AppContext<'_>) {
        if self.pending_perm_responses.is_empty() {
            return;
        }

        let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) else {
            tracing::warn!("no secret key for publishing permission responses");
            self.pending_perm_responses.clear();
            return;
        };

        let pending = std::mem::take(&mut self.pending_perm_responses);

        for resp in pending {
            queue_built_event(
                session_events::build_permission_response_event(
                    &resp.perm_id,
                    &resp.request_note_id,
                    resp.allowed,
                    resp.message.as_deref(),
                    resp.cancel_turn,
                    &resp.event_session_id,
                    &sk,
                ),
                &format!(
                    "queued permission response for {} ({})",
                    resp.perm_id,
                    if resp.allowed { "allow" } else { "deny" }
                ),
                ctx.ndb,
                &sk,
                &mut self.pending_relay_events,
            );
        }
    }

    /// Publish permission mode command events for remote sessions.
    /// Called in the update loop where AppContext is available.
    fn publish_pending_mode_commands(&mut self, ctx: &AppContext<'_>) {
        if self.pending_mode_commands.is_empty() {
            return;
        }

        let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) else {
            tracing::warn!("no secret key for publishing mode commands");
            self.pending_mode_commands.clear();
            return;
        };

        for cmd in std::mem::take(&mut self.pending_mode_commands) {
            queue_built_event(
                session_events::build_set_permission_mode_event(cmd.mode, &cmd.session_id, &sk),
                &format!(
                    "publishing permission mode command: {} -> {}",
                    cmd.session_id, cmd.mode
                ),
                ctx.ndb,
                &sk,
                &mut self.pending_relay_events,
            );
        }
    }

    /// Restore sessions from kind-31988 state events in ndb.
    /// Called once on first `update()`.
    fn restore_sessions_from_ndb(&mut self, ctx: &mut AppContext<'_>) {
        let txn = match Transaction::new(ctx.ndb) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("failed to open txn for session restore: {:?}", e);
                return;
            }
        };

        let states = session_loader::load_session_states(ctx.ndb, &txn);
        if states.is_empty() {
            return;
        }

        tracing::info!("restoring {} sessions from ndb", states.len());

        for state in &states {
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
                Some(_) => {
                    // Empty cli_session — backend never started, nothing to resume
                    String::new()
                }
                None => {
                    // Legacy: d-tag IS the CLI session ID
                    state.claude_session_id.clone()
                }
            };

            let dave_sid = self.session_manager.new_resumed_session(
                cwd,
                resume_id,
                state.title.clone(),
                AiMode::Agentic,
                backend,
            );

            // Load conversation history from kind-1988 events
            let loaded =
                session_loader::load_session_messages(ctx.ndb, &txn, &state.claude_session_id);

            if let Some(session) = self.session_manager.get_mut(dave_sid) {
                tracing::info!(
                    "restored session '{}': {} messages",
                    state.title,
                    loaded.messages.len(),
                );
                session.chat = loaded.messages;

                if is_session_remote(&state.hostname, &state.cwd, &self.hostname) {
                    session.source = session::SessionSource::Remote;
                }

                // Local sessions use the current machine's hostname;
                // remote sessions use what was stored in the event.
                session.details.hostname = if session.is_remote() {
                    state.hostname.clone()
                } else {
                    self.hostname.clone()
                };

                session.details.custom_title = state.custom_title.clone();
                session.spawn_id = state.spawn_id.clone();

                // Restore focus indicator from state event
                session.indicator = state
                    .indicator
                    .as_deref()
                    .and_then(focus_queue::FocusPriority::from_indicator_str);

                // Use home_dir from the event for remote abbreviation
                if !state.home_dir.is_empty() {
                    session.details.home_dir = state.home_dir.clone();
                }

                if let Some(agentic) = &mut session.agentic {
                    // Restore the event_id from the d-tag so published
                    // state events keep using the same Nostr identity.
                    agentic.event_id = state.claude_session_id.clone();

                    // If cli_session was empty the backend never ran —
                    // clear resume_session_id so we don't try --resume
                    // with the event UUID.
                    if state.cli_session_id.as_ref().is_some_and(|s| s.is_empty()) {
                        agentic.resume_session_id = None;
                    }

                    if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id) {
                        agentic.live_threading.seed(root, last, loaded.event_count);
                    }
                    // Load permission state and dedup set from events
                    agentic.permissions.merge_loaded(
                        loaded.permissions.responded,
                        loaded.permissions.request_note_ids,
                    );
                    agentic.seen_note_ids = loaded.note_ids;
                    // Set remote status and permission mode from state event
                    agentic.remote_status = AgentStatus::from_status_str(&state.status);
                    agentic.remote_status_ts = state.created_at;
                    if let Some(ref pm) = state.permission_mode {
                        agentic.permission_mode = crate::session::permission_mode_from_str(pm);
                    }

                    setup_conversation_subscription(agentic, &state.claude_session_id, ctx.ndb);
                }
            }
        }

        self.session_manager.rebuild_cwd_groups();

        // Seed per-host recent paths from session state events
        let host_paths = session_loader::load_recent_paths_by_host(ctx.ndb, &txn);
        self.directory_picker
            .seed_host_paths(host_paths, &self.hostname);

        // Skip the directory picker since we restored sessions
        self.active_overlay = DaveOverlay::None;
    }

    /// Poll for new kind-31988 session state events from the ndb subscription.
    ///
    /// When PNS events arrive from relays and get unwrapped, new session state
    /// events may appear. This detects them and creates sessions we don't already have.
    fn poll_session_state_events(&mut self, ctx: &mut AppContext<'_>) {
        let Some(sub) = self.session_state_sub else {
            return;
        };

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
                self.session_manager.rebuild_cwd_groups();
                continue;
            }

            // Look up the latest revision of this session. PNS wrapping
            // causes old revisions (including pre-deletion) to arrive from
            // the relay. Only create a session if the latest revision is valid.
            let Some(state) = session_loader::latest_valid_session(ctx.ndb, &txn, claude_sid)
            else {
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

            // Check for a pending placeholder matching this session's spawn_id.
            // If found, upgrade it in-place instead of creating a new session.
            let pending_sid = state.spawn_id.as_ref().and_then(|incoming_id| {
                self.session_manager
                    .iter()
                    .find(|s| {
                        s.pending_created_at.is_some() && s.spawn_id.as_ref() == Some(incoming_id)
                    })
                    .map(|s| s.id)
            });

            let dave_sid = if let Some(sid) = pending_sid {
                tracing::info!("upgrading pending placeholder to real session");
                sid
            } else {
                self.session_manager.new_resumed_session(
                    cwd,
                    resume_id.clone(),
                    state.title.clone(),
                    AiMode::Agentic,
                    backend,
                )
            };

            // Load any conversation history that arrived with it
            let loaded = session_loader::load_session_messages(ctx.ndb, &txn, claude_sid);

            if let Some(session) = self.session_manager.get_mut(dave_sid) {
                // Clear pending state (upgrades placeholder to real session)
                session.pending_created_at = None;
                session.details.title = state.title.clone();
                session.details.hostname = state.hostname.clone();
                session.details.custom_title = state.custom_title.clone();
                session.indicator = state
                    .indicator
                    .as_deref()
                    .and_then(focus_queue::FocusPriority::from_indicator_str);
                if !state.home_dir.is_empty() {
                    session.details.home_dir = state.home_dir.clone();
                }
                if !loaded.messages.is_empty() {
                    tracing::info!(
                        "loaded {} messages for discovered session",
                        loaded.messages.len()
                    );
                    session.chat = loaded.messages;
                }

                if is_session_remote(&state.hostname, &state.cwd, &self.hostname) {
                    session.source = session::SessionSource::Remote;
                }

                // Initialize agentic data if absent (e.g. upgraded placeholder)
                if session.agentic.is_none() {
                    session.agentic = Some(session::AgenticSessionData::new(
                        dave_sid,
                        PathBuf::from(&state.cwd),
                    ));
                }

                if let Some(agentic) = &mut session.agentic {
                    // Restore the event_id from the d-tag
                    agentic.event_id = claude_sid.to_string();

                    if !resume_id.is_empty() {
                        agentic.resume_session_id = Some(resume_id.clone());
                    }

                    // If cli_session was empty the backend never ran —
                    // clear resume_session_id so we don't try --resume
                    // with the event UUID.
                    if state.cli_session_id.as_ref().is_some_and(|s| s.is_empty()) {
                        agentic.resume_session_id = None;
                    }

                    if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id) {
                        agentic.live_threading.seed(root, last, loaded.event_count);
                    }
                    // Load permission state and dedup set
                    agentic.permissions.merge_loaded(
                        loaded.permissions.responded,
                        loaded.permissions.request_note_ids,
                    );
                    agentic.seen_note_ids = loaded.note_ids;
                    // Set remote status and permission mode
                    agentic.remote_status = AgentStatus::from_status_str(&state.status);
                    agentic.remote_status_ts = state.created_at;
                    if let Some(ref pm) = state.permission_mode {
                        agentic.permission_mode = crate::session::permission_mode_from_str(pm);
                    }

                    setup_conversation_subscription(agentic, claude_sid, ctx.ndb);
                }
            }

            self.session_manager.rebuild_cwd_groups();

            // If we were showing the directory picker, switch to showing sessions
            if matches!(self.active_overlay, DaveOverlay::DirectoryPicker) {
                self.active_overlay = DaveOverlay::None;
            }
        }
    }

    /// Poll for kind-31989 spawn command events.
    ///
    /// When a remote device wants to create a session on this host, it publishes
    /// a kind-31989 event with `target_host` matching our hostname. We pick it up
    /// here and create the session locally.
    fn poll_session_command_events(&mut self, ctx: &mut AppContext<'_>) {
        let Some(sub) = self.session_command_sub else {
            return;
        };

        let note_keys = ctx.ndb.poll_for_notes(sub, 16);
        if note_keys.is_empty() {
            return;
        }

        let txn = match Transaction::new(ctx.ndb) {
            Ok(t) => t,
            Err(_) => return,
        };

        for key in note_keys {
            let Ok(note) = ctx.ndb.get_note_by_key(&txn, key) else {
                continue;
            };

            let Some(command_id) = session_events::get_tag_value(&note, "d") else {
                continue;
            };

            // Dedup: skip already-processed commands
            if self.processed_commands.contains(command_id) {
                continue;
            }

            let command = session_events::get_tag_value(&note, "command").unwrap_or("");
            if command != "spawn_session" {
                continue;
            }

            let target = session_events::get_tag_value(&note, "target_host").unwrap_or("");
            if target != self.hostname {
                continue;
            }

            let cwd = session_events::get_tag_value(&note, "cwd").unwrap_or("");
            let backend_str = session_events::get_tag_value(&note, "backend").unwrap_or("");
            let backend =
                BackendType::from_tag_str(backend_str).unwrap_or(self.model_config.backend);
            let spawn_id = session_events::get_tag_value(&note, "spawn_id").map(|s| s.to_string());

            tracing::info!(
                "received spawn command {}: cwd={}, backend={:?}, spawn_id={:?}",
                command_id,
                cwd,
                backend,
                spawn_id,
            );

            self.processed_commands.insert(command_id.to_string());
            let sid = update::create_session_with_cwd(
                &mut self.session_manager,
                &mut self.directory_picker,
                &mut self.scene,
                self.show_scene,
                self.ai_mode,
                PathBuf::from(cwd),
                &self.hostname,
                backend,
                Some(ctx.ndb),
                Model::Default,
            );

            // Store spawn_id so it's echoed in kind-31988 state events,
            // letting the sender match this session to its placeholder.
            if let Some(spawn_id) = spawn_id {
                if let Some(session) = self.session_manager.get_mut(sid) {
                    session.spawn_id = Some(spawn_id);
                }
            }
        }
    }

    /// Poll for new kind-1988 conversation events.
    ///
    /// For remote sessions: process all roles (user, assistant, tool_call, etc.)
    /// to keep the phone UI in sync with the desktop's conversation.
    ///
    /// For local sessions: only process `role=user` messages arriving from
    /// remote clients (phone), collecting them for backend dispatch.
    fn poll_remote_conversation_events(
        &mut self,
        ndb: &nostrdb::Ndb,
        secret_key: Option<&[u8; 32]>,
    ) -> (Vec<(SessionId, String)>, Vec<session_events::BuiltEvent>) {
        let mut remote_user_messages: Vec<(SessionId, String)> = Vec::new();
        let mut events_to_publish: Vec<session_events::BuiltEvent> = Vec::new();
        let session_ids = self.session_manager.session_ids();
        for session_id in session_ids {
            let Some(session) = self.session_manager.get_mut(session_id) else {
                continue;
            };
            let is_remote = session.is_remote();

            // Get sub without holding agentic borrow
            let sub = match session
                .agentic
                .as_ref()
                .and_then(|a| a.live_conversation_sub)
            {
                Some(s) => s,
                None => continue,
            };

            let note_keys = ndb.poll_for_notes(sub, 128);
            if note_keys.is_empty() {
                continue;
            }

            let txn = match Transaction::new(ndb) {
                Ok(txn) => txn,
                Err(_) => continue,
            };

            let notes: Vec<_> = note_keys
                .iter()
                .filter_map(|key| ndb.get_note_by_key(&txn, *key).ok())
                .collect();

            let result =
                process_conversation_notes(notes, session, session_id, is_remote, secret_key, ndb);
            remote_user_messages.extend(result.remote_user_messages);
            events_to_publish.extend(result.events_to_publish);
        }
        (remote_user_messages, events_to_publish)
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
        // Kill any running app processes for this session to avoid orphans
        if let Some(mut procs) = self.run_processes.remove(&id) {
            for (_, mut child) in procs.drain() {
                kill_process_tree(&mut child);
                self.pending_reap.push(child);
            }
        }

        // Capture session info before deletion so we can publish a "deleted" state event
        if let Some(session) = self.session_manager.get(id) {
            if let Some(agentic) = &session.agentic {
                self.pending_deletions.push(DeletedSessionInfo {
                    claude_session_id: agentic.event_session_id().to_string(),
                    title: session.details.title.clone(),
                    cwd: agentic.cwd.to_string_lossy().to_string(),
                    home_dir: session.details.home_dir.clone(),
                    backend: session.backend_type,
                });
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

    /// Handle an interrupt request - requires double-Escape to confirm
    fn handle_interrupt_request(&mut self, ctx: &egui::Context) {
        let bt = self
            .session_manager
            .get_active()
            .map(|s| s.backend_type)
            .unwrap_or(BackendType::Remote);
        self.interrupt_pending_since = update::handle_interrupt_request(
            &self.session_manager,
            get_backend(&self.backends, bt),
            self.interrupt_pending_since,
            ctx,
        );
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
        queue_built_event(
            session_events::build_run_config_event(
                config,
                &cwd.to_string_lossy(),
                &self.hostname,
                sk,
            ),
            "run-config",
            ndb,
            sk,
            &mut self.pending_relay_events,
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
        queue_built_event(
            session_events::build_run_config_delete_event(
                config_id,
                &cwd.to_string_lossy(),
                &self.hostname,
                sk,
            ),
            "run-config-delete",
            ndb,
            sk,
            &mut self.pending_relay_events,
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

    /// Queue a spawn command request. The event is built and published in
    /// update() where AppContext (and thus the secret key) is available.
    /// Also creates a pending placeholder session so the user sees immediate feedback.
    fn queue_spawn_command(&mut self, target_host: &str, cwd: &Path, backend: BackendType) {
        let spawn_id = uuid::Uuid::new_v4().to_string();
        tracing::info!(
            "queuing spawn command {} for {} at {:?}",
            spawn_id,
            target_host,
            cwd
        );
        self.pending_spawn_commands.push(PendingSpawnCommand {
            target_host: target_host.to_string(),
            cwd: cwd.to_path_buf(),
            backend,
            spawn_id: spawn_id.clone(),
        });

        // Create a lightweight pending placeholder for immediate UI feedback
        self.session_manager.new_pending_placeholder(
            cwd.to_path_buf(),
            target_host.to_string(),
            backend,
            spawn_id,
        );
        self.active_overlay = DaveOverlay::None;
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
        if let Some(key_action) = check_keybindings(
            egui_ctx,
            has_pending_permission,
            has_pending_question,
            in_tentative_state,
            active_ai_mode,
        ) {
            self.handle_key_action(key_action, egui_ctx);
        }
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
                self.handle_user_send(ctx, ui);
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
            if let Some(evt) =
                ingest_live_event(session, app_ctx.ndb, &sk, &user_text, "user", None, None)
            {
                self.pending_relay_events.push(evt);
            }
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

    /// Dispatch a compact request to the backend for the active session.
    fn dispatch_compact(&mut self, bt: BackendType, ui: &egui::Ui) {
        dispatch_compact_for_active(&mut self.session_manager, &self.backends, bt, ui.ctx());
    }

    /// Handle a user send action triggered by the ui
    fn handle_user_send(&mut self, app_ctx: &AppContext, ui: &egui::Ui) {
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

            // Generate live event for user message
            if let Some(sk) = secret_key_bytes(app_ctx.accounts.get_selected_account().keypair()) {
                if let Some(evt) =
                    ingest_live_event(session, app_ctx.ndb, &sk, &user_text, "user", None, None)
                {
                    self.pending_relay_events.push(evt);
                }
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
        self.send_user_message(app_ctx, ui.ctx());
    }

    fn send_user_message(&mut self, app_ctx: &AppContext, ctx: &egui::Context) {
        let Some(active_id) = self.session_manager.active_id() else {
            return;
        };
        self.send_user_message_for(active_id, app_ctx, ctx);
    }

    /// Send a message for a specific session by ID
    fn send_user_message_for(&mut self, sid: SessionId, app_ctx: &AppContext, ctx: &egui::Context) {
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
        let messages = session.chat.clone();
        let cwd = session.agentic.as_ref().map(|a| a.cwd.clone());
        let resume_session_id = session
            .agentic
            .as_ref()
            .and_then(|a| a.cli_resume_id().map(|s| s.to_string()));
        let backend_type = session.backend_type;
        let tools = self.tools.clone();
        let model_name = session.details.resolve_model();
        let ctx = ctx.clone();

        // Use backend to stream request
        let (rx, task_handle) = get_backend(&self.backends, backend_type).stream_request(
            messages,
            tools,
            model_name,
            user_id,
            session_id,
            cwd,
            resume_session_id,
            ctx,
        );
        session.incoming_tokens = Some(rx);
        session.last_backend_msg = Some(std::time::Instant::now());
        session.task_handle = task_handle;
    }

    /// Process pending archive conversion (JSONL to nostr events).
    ///
    /// When resuming a session, the JSONL archive needs to be converted to
    /// nostr events. If events already exist in ndb, load them directly.
    fn process_archive_conversion(&mut self, ctx: &mut AppContext<'_>) {
        let Some((file_path, dave_sid, claude_sid)) = self.pending_archive_convert.take() else {
            return;
        };

        let txn = Transaction::new(ctx.ndb).expect("txn");
        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
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
            let loaded_txn = Transaction::new(ctx.ndb).expect("txn");
            let loaded = session_loader::load_session_messages(ctx.ndb, &loaded_txn, &claude_sid);
            if let Some(session) = self.session_manager.get_mut(dave_sid) {
                tracing::info!("loaded {} messages into chat UI", loaded.messages.len());
                session.chat = loaded.messages;

                if let Some(agentic) = &mut session.agentic {
                    if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id) {
                        agentic.live_threading.seed(root, last, loaded.event_count);
                    }
                    agentic
                        .permissions
                        .request_note_ids
                        .extend(loaded.permissions.request_note_ids);
                }
            }
        } else if let Some(secret_bytes) =
            secret_key_bytes(ctx.accounts.get_selected_account().keypair())
        {
            let sub_filter = nostrdb::Filter::new()
                .kinds([session_events::AI_CONVERSATION_KIND as u64])
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

        let txn = Transaction::new(ndb).expect("txn");
        let loaded = session_loader::load_session_messages(ndb, &txn, &pending.claude_session_id);
        if let Some(session) = self.session_manager.get_mut(pending.dave_session_id) {
            tracing::info!("loaded {} messages into chat UI", loaded.messages.len());
            session.chat = loaded.messages;

            if let Some(agentic) = &mut session.agentic {
                if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id) {
                    agentic.live_threading.seed(root, last, loaded.event_count);
                }
                agentic
                    .permissions
                    .request_note_ids
                    .extend(loaded.permissions.request_note_ids);
            }
        }
        self.pending_message_load = None;
    }

    /// Process relay events and run negentropy reconciliation against PNS relay.
    ///
    /// Collects negentropy protocol events from the relay, re-subscribes on
    /// reconnect, and drives multi-round sync to fetch missing PNS events.
    fn process_negentropy_sync(&mut self, ctx: &mut AppContext<'_>, egui_ctx: &egui::Context) {
        let pns_sub_id = self.pns_relay_sub.clone();
        let pns_relay = self.pns_relay_url.clone();
        let mut neg_events: Vec<enostr::negentropy::NegEvent> = Vec::new();
        try_process_events_core(ctx, &mut self.pool, egui_ctx, |app_ctx, pool, ev| {
            if ev.relay == pns_relay {
                if let enostr::RelayEvent::Opened = (&ev.event).into() {
                    neg_events.push(enostr::negentropy::NegEvent::RelayOpened);
                    if let Some(sub_id) = &pns_sub_id {
                        if let Some(sk) =
                            app_ctx.accounts.get_selected_account().keypair().secret_key
                        {
                            let pns_keys = enostr::pns::derive_pns_keys(&sk.secret_bytes());
                            let pns_filter = nostrdb::Filter::new()
                                .kinds([enostr::pns::PNS_KIND as u64])
                                .authors([pns_keys.keypair.pubkey.bytes()])
                                .limit(500)
                                .build();
                            let req = enostr::ClientMessage::req(sub_id.clone(), vec![pns_filter]);
                            pool.send_to(&req, &pns_relay);
                            tracing::info!("re-subscribed for PNS events after relay reconnect");
                        }
                    }
                }

                neg_events.extend(enostr::negentropy::NegEvent::from_relay(&ev.event));
            }
        });

        // Reset round counter on relay reconnect so we do a fresh burst
        if neg_events
            .iter()
            .any(|e| matches!(e, enostr::negentropy::NegEvent::RelayOpened))
        {
            self.neg_sync_round = 0;
        }

        // Reconcile local events against PNS relay,
        // fetch any missing kind-1080 events via standard REQ.
        if self.neg_sync.needs_process(&neg_events) {
            if let Some(sk) = ctx.accounts.get_selected_account().keypair().secret_key {
                let pns_keys = enostr::pns::derive_pns_keys(&sk.secret_bytes());
                let since = notedeck::unix_time_secs() - (7 * 86400);
                let filter = nostrdb::Filter::new()
                    .kinds([enostr::pns::PNS_KIND as u64])
                    .authors([pns_keys.keypair.pubkey.bytes()])
                    .since(since)
                    .build();
                let result = self.neg_sync.process(
                    neg_events,
                    ctx.ndb,
                    &mut self.pool,
                    &filter,
                    &self.pns_relay_url,
                );

                // If events were found and we haven't hit the round limit,
                // trigger another sync to pull more recent data.
                if result.new_events > 0 {
                    self.neg_sync_round += 1;
                    if self.neg_sync_round < MAX_NEG_SYNC_ROUNDS {
                        tracing::info!(
                            "negentropy: scheduling round {}/{} (got {} new, {} skipped)",
                            self.neg_sync_round + 1,
                            MAX_NEG_SYNC_ROUNDS,
                            result.new_events,
                            result.skipped
                        );
                        self.neg_sync.trigger_now();
                    } else {
                        tracing::info!(
                            "negentropy: reached max rounds ({}), stopping",
                            MAX_NEG_SYNC_ROUNDS
                        );
                    }
                } else if result.skipped > 0 {
                    tracing::info!(
                        "negentropy: relay has {} events we can't reconcile, stopping",
                        result.skipped
                    );
                }
            }
        }
    }

    /// One-time initialization on first update.
    ///
    /// Restores sessions from ndb, triggers initial negentropy sync,
    /// and sets up relay subscriptions.
    fn initialize_once(&mut self, ctx: &mut AppContext<'_>, egui_ctx: &egui::Context) {
        self.sessions_restored = true;

        self.restore_sessions_from_ndb(ctx);

        // Trigger initial negentropy sync after startup
        self.neg_sync.trigger_now();
        self.neg_sync_round = 0;

        // Subscribe to PNS events on relays for session discovery from other devices.
        // Also subscribe locally in ndb for kind-31988 session state events
        // so we detect new sessions appearing after PNS unwrapping.
        if let Some(sk) = ctx.accounts.get_selected_account().keypair().secret_key {
            let pns_keys = enostr::pns::derive_pns_keys(&sk.secret_bytes());

            // Ensure the PNS relay is in the pool
            let egui_ctx = egui_ctx.clone();
            let wakeup = move || egui_ctx.request_repaint();
            if let Err(e) = self.pool.add_url(self.pns_relay_url.clone(), wakeup) {
                tracing::warn!("failed to add PNS relay {}: {:?}", self.pns_relay_url, e);
            }

            // Remote: subscribe on PNS relay for kind-1080 authored by our PNS pubkey
            let pns_filter = nostrdb::Filter::new()
                .kinds([enostr::pns::PNS_KIND as u64])
                .authors([pns_keys.keypair.pubkey.bytes()])
                .limit(500)
                .build();
            let sub_id = uuid::Uuid::new_v4().to_string();
            let req = enostr::ClientMessage::req(sub_id.clone(), vec![pns_filter]);
            self.pool.send_to(&req, &self.pns_relay_url);
            self.pns_relay_sub = Some(sub_id);
            tracing::info!("subscribed for PNS events on {}", self.pns_relay_url);

            // Local: subscribe in ndb for kind-31988 session state events
            let state_filter = nostrdb::Filter::new()
                .kinds([session_events::AI_SESSION_STATE_KIND as u64])
                .build();
            match ctx.ndb.subscribe(&[state_filter]) {
                Ok(sub) => {
                    self.session_state_sub = Some(sub);
                    tracing::info!("subscribed for session state events in ndb");
                }
                Err(e) => {
                    tracing::warn!("failed to subscribe for session state events: {:?}", e);
                }
            }

            // Local: subscribe in ndb for kind-31989 session command events
            let cmd_filter = nostrdb::Filter::new()
                .kinds([session_events::AI_SESSION_COMMAND_KIND as u64])
                .build();
            match ctx.ndb.subscribe(&[cmd_filter]) {
                Ok(sub) => {
                    self.session_command_sub = Some(sub);
                    tracing::info!("subscribed for session command events in ndb");
                }
                Err(e) => {
                    tracing::warn!("failed to subscribe for session command events: {:?}", e);
                }
            }

            // Load existing run configs from ndb (kind-31991) and subscribe for updates.
            // Each config carries its own stable UUID (the d-tag); no runtime ID generation needed.
            let txn = nostrdb::Transaction::new(ctx.ndb).expect("txn");
            self.run_configs =
                session_loader::load_run_configs_from_ndb(ctx.ndb, &txn, &self.hostname);
            tracing::info!("loaded {} run config CWDs from ndb", self.run_configs.len());

            let rc_filter = nostrdb::Filter::new()
                .kinds([crate::config::AI_RUN_CONFIG_KIND as u64])
                .build();
            match ctx.ndb.subscribe(&[rc_filter]) {
                Ok(sub) => {
                    self.run_config_sub = Some(sub);
                    tracing::info!("subscribed for run config events in ndb");
                }
                Err(e) => {
                    tracing::warn!("failed to subscribe for run config events: {:?}", e);
                }
            }
        }
    }
}

impl Drop for Dave {
    fn drop(&mut self) {
        for procs in self.run_processes.values_mut() {
            for child in procs.values_mut() {
                kill_process_tree(child);
            }
        }
    }
}

impl notedeck::App for Dave {
    fn update(&mut self, ctx: &mut AppContext<'_>, egui_ctx: &egui::Context) {
        self.process_negentropy_sync(ctx, egui_ctx);

        // Poll for external spawn-agent commands via IPC
        self.poll_ipc_commands();

        // Process pending thread summary requests
        let pending = std::mem::take(&mut self.pending_summaries);
        for note_id in pending {
            if let Some(sid) = self.build_summary_session(ctx.ndb, &note_id) {
                self.send_user_message_for(sid, ctx, egui_ctx);
            }
        }

        // One-time initialization on first update
        if !self.sessions_restored {
            self.initialize_once(ctx, egui_ctx);
        }

        // Poll for external editor completion
        update::poll_editor_job(&mut self.session_manager);

        // Reap killed child processes without blocking the frame
        self.poll_pending_reap();

        // Poll for new session states from PNS-unwrapped relay events
        self.poll_session_state_events(ctx);

        // Poll for spawn commands targeting this host
        self.poll_session_command_events(ctx);

        // Poll for live run-config updates from PNS relay
        self.poll_run_config_events(ctx.ndb);

        // Poll for live conversation events on all sessions.
        // Returns user messages from remote clients that need backend dispatch.
        // Only dispatch if the session isn't already streaming a response —
        // the message is already in chat, so it will be included when the
        // current stream finishes and we re-dispatch.
        let sk_bytes = secret_key_bytes(ctx.accounts.get_selected_account().keypair());
        let (remote_user_msgs, conv_events) =
            self.poll_remote_conversation_events(ctx.ndb, sk_bytes.as_ref());
        self.pending_relay_events.extend(conv_events);
        for (sid, _msg) in remote_user_msgs {
            let should_dispatch = self
                .session_manager
                .get(sid)
                .is_some_and(|s| s.should_dispatch_remote_message());
            if should_dispatch {
                self.send_user_message_for(sid, ctx, egui_ctx);
            }
        }

        self.process_archive_conversion(ctx);
        self.poll_pending_message_load(ctx.ndb);

        // Check if interrupt confirmation has timed out
        self.check_interrupt_timeout();

        // Process incoming AI responses for all sessions
        let ProcessEventsResult {
            needs_send: sessions_needing_send,
            events_to_publish,
            needs_compact: sessions_needing_compact,
        } = self.process_events(ctx);

        // Build permission response events from remote sessions
        self.publish_pending_perm_responses(ctx);

        // Build spawn command events (need secret key from AppContext)
        if !self.pending_spawn_commands.is_empty() {
            if let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) {
                for cmd in std::mem::take(&mut self.pending_spawn_commands) {
                    match session_events::build_spawn_command_event(
                        &cmd.target_host,
                        &cmd.cwd.to_string_lossy(),
                        cmd.backend.as_str(),
                        &cmd.spawn_id,
                        &sk,
                    ) {
                        Ok(evt) => self.pending_relay_events.push(evt),
                        Err(e) => tracing::warn!("failed to build spawn command: {:?}", e),
                    }
                }
            }
        }

        // Build permission mode command events for remote sessions
        self.publish_pending_mode_commands(ctx);

        // PNS-wrap and publish events to relays
        let pending = std::mem::take(&mut self.pending_relay_events);
        let all_events = events_to_publish.iter().chain(pending.iter());
        if let Some(sk) = ctx.accounts.get_selected_account().keypair().secret_key {
            let pns_keys = enostr::pns::derive_pns_keys(&sk.secret_bytes());
            for event in all_events {
                match session_events::wrap_pns(&event.note_json, &pns_keys) {
                    Ok(pns_json) => match enostr::ClientMessage::event_json(pns_json) {
                        Ok(msg) => self.pool.send_to(&msg, &self.pns_relay_url),
                        Err(e) => tracing::warn!("failed to build relay message: {:?}", e),
                    },
                    Err(e) => tracing::warn!("failed to PNS-wrap event: {}", e),
                }
            }
        }

        // Poll for remote conversation actions (permission responses, commands).
        let mode_applies = self.poll_remote_conversation_actions(ctx.ndb);
        for (backend_sid, bt, mode) in mode_applies {
            get_backend(&self.backends, bt).set_permission_mode(
                backend_sid,
                mode,
                egui_ctx.clone(),
            );
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
        // Stays Pending while the user is typing so it retries next frame.
        if self.auto_steal == focus_queue::AutoStealState::Pending {
            let user_is_typing = self
                .session_manager
                .get_active()
                .is_some_and(|s| !s.input.is_empty());

            if !user_is_typing {
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
            self.send_user_message_for(session_id, ctx, egui_ctx);
        }

        // Dispatch compact queries for sessions in compact-and-proceed flow
        for session_id in sessions_needing_compact {
            dispatch_compact_for_session(
                &mut self.session_manager,
                &self.backends,
                session_id,
                egui_ctx,
            );
        }
    }

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.process_keybindings(ui.ctx());

        let mut app_action: Option<AppAction> = None;

        if let Some(action) = self.ui(ctx, ui).action {
            if let Some(returned_action) = self.handle_ui_action(action, ctx, ui) {
                app_action = Some(returned_action);
            }
        }

        AppResponse::action(app_action)
    }
}

/// Bring the application to the front.
///
/// On macOS, egui's ViewportCommand::Focus focuses the window but doesn't
/// always activate the app (bring it in front of other apps). Stage Manager
/// single-window mode is particularly aggressive, so we use both
/// NSRunningApplication::activateWithOptions and orderFrontRegardless
/// on the key window.
/// Set up a live conversation subscription for a session if not already subscribed.
///
/// Subscribes to kind-1988 events tagged with the session's claude ID so we
/// receive messages from remote clients (phone) even before the local backend starts.
pub(crate) fn setup_conversation_subscription(
    agentic: &mut session::AgenticSessionData,
    claude_session_id: &str,
    ndb: &nostrdb::Ndb,
) {
    if agentic.live_conversation_sub.is_some() {
        return;
    }
    let filter = nostrdb::Filter::new()
        .kinds([session_events::AI_CONVERSATION_KIND as u64])
        .tags([claude_session_id], 'd')
        .build();
    match ndb.subscribe(&[filter]) {
        Ok(sub) => {
            agentic.live_conversation_sub = Some(sub);
            tracing::info!(
                "subscribed for live conversation events (session {})",
                claude_session_id,
            );
        }
        Err(e) => {
            tracing::warn!("failed to subscribe for conversation events: {:?}", e,);
        }
    }
}

/// Subscribe for kind-1988 conversation action events (permission responses,
/// mode commands) for the given session d-tag.
pub(crate) fn setup_conversation_action_subscription(
    agentic: &mut session::AgenticSessionData,
    event_id: &str,
    ndb: &nostrdb::Ndb,
) {
    if agentic.conversation_action_sub.is_some() {
        return;
    }
    let filter = nostrdb::Filter::new()
        .kinds([session_events::AI_CONVERSATION_KIND as u64])
        .tags([event_id], 'd')
        .build();
    match ndb.subscribe(&[filter]) {
        Ok(sub) => {
            agentic.conversation_action_sub = Some(sub);
            tracing::info!("subscribed for conversation actions (session {})", event_id,);
        }
        Err(e) => {
            tracing::warn!("failed to subscribe for conversation actions: {:?}", e);
        }
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
    session.chat.push(Message::ToolCalls(toolcalls.to_vec()));

    let txn = Transaction::new(ndb).unwrap();
    let mut needs_send = false;

    for call in toolcalls {
        match call.calls() {
            ToolCalls::PresentNotes(present) => {
                session.chat.push(Message::ToolResponse(ToolResponse::new(
                    call.id().to_owned(),
                    ToolResponses::PresentNotes(present.note_ids.len() as i32),
                )));
                needs_send = true;
            }
            ToolCalls::Invalid(invalid) => {
                session.chat.push(Message::tool_error(
                    call.id().to_string(),
                    invalid.error.clone(),
                ));
                needs_send = true;
            }
            ToolCalls::Query(search_call) => {
                let resp = search_call.execute(&txn, ndb);
                session.chat.push(Message::ToolResponse(ToolResponse::new(
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
/// Builds and publishes a permission request event for remote clients,
/// stores the response sender for later, and adds the request to chat.
fn handle_permission_request(
    session: &mut session::ChatSession,
    pending: messages::PendingPermission,
    secret_key: &Option<[u8; 32]>,
    ndb: &nostrdb::Ndb,
    events_to_publish: &mut Vec<session_events::BuiltEvent>,
) {
    tracing::info!(
        "Permission request for tool '{}': {:?}",
        pending.request.tool_name,
        pending.request.tool_input
    );

    // Check runtime allowlist — auto-accept and show as already-allowed in chat
    if let Some(agentic) = &session.agentic {
        if agentic.should_runtime_allow(&pending.request.tool_name, &pending.request.tool_input) {
            tracing::info!(
                "runtime allow: auto-accepting '{}' for this session",
                pending.request.tool_name,
            );
            let _ = pending
                .response_tx
                .send(PermissionResponse::Allow { message: None });
            let mut request = pending.request;
            request.response = Some(crate::messages::PermissionResponseType::Allowed);
            session.chat.push(Message::PermissionRequest(request));
            return;
        }
    }

    // Build and publish a proper permission request event
    // with perm-id, tool-name tags for remote clients
    if let Some(sk) = secret_key {
        if let Some(agentic) = &mut session.agentic {
            let sid = agentic.event_session_id().to_string();
            match session_events::build_permission_request_event(
                &pending.request.id,
                &pending.request.tool_name,
                &pending.request.tool_input,
                &sid,
                &mut agentic.live_threading,
                sk,
            ) {
                Ok(evt) => {
                    pns_ingest(ndb, &evt.note_json, sk);
                    agentic
                        .permissions
                        .request_note_ids
                        .insert(pending.request.id, evt.note_id);
                    events_to_publish.push(evt);
                }
                Err(e) => {
                    tracing::warn!("failed to build permission request event: {}", e);
                }
            }
        }
    }

    // Store the response sender for later (agentic only)
    if let Some(agentic) = &mut session.agentic {
        agentic
            .permissions
            .pending
            .insert(pending.request.id, pending.response_tx);
    }

    // Add the request to chat for UI display
    session
        .chat
        .push(Message::PermissionRequest(pending.request));
}

/// Result of processing a batch of conversation notes.
pub(crate) struct ProcessedNotes {
    /// User messages received from remote clients (for local sessions).
    pub remote_user_messages: Vec<(SessionId, String)>,
    /// Events that should be published to relays.
    pub events_to_publish: Vec<session_events::BuiltEvent>,
}

/// Process a batch of kind-1988 notes for a single session.
///
/// Sorts by `(created_at, seq)`, deduplicates via `seen_note_ids`, and
/// appends messages to `session.chat`. Returns any remote user messages
/// (for local sessions) and events to publish.
pub(crate) fn process_conversation_notes<'a>(
    mut notes: Vec<nostrdb::Note<'a>>,
    session: &mut session::ChatSession,
    session_id: SessionId,
    is_remote: bool,
    secret_key: Option<&[u8; 32]>,
    ndb: &nostrdb::Ndb,
) -> ProcessedNotes {
    let mut remote_user_messages: Vec<(SessionId, String)> = Vec::new();
    let mut events_to_publish: Vec<session_events::BuiltEvent> = Vec::new();

    // Sort by (created_at, seq) to process in order.
    // The seq tag is a per-session tiebreaker for events within the
    // same second (e.g. tool_call before permission_request).
    notes.sort_by_key(|n| {
        let seq = session_events::get_tag_value(n, "seq")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        (n.created_at(), seq)
    });

    for note in &notes {
        // Skip events we've already processed (dedup)
        let note_id = *note.id();
        let dominated = session
            .agentic
            .as_mut()
            .map(|a| !a.seen_note_ids.insert(note_id))
            .unwrap_or(true);
        if dominated {
            continue;
        }

        let content = note.content();
        let role = session_events::get_tag_value(note, "role");

        // Local sessions: only process incoming user messages from remote clients
        if !is_remote {
            if role == Some("user") {
                tracing::info!("received remote user message for local session");
                session.chat.push(Message::User(content.to_string().into()));
                session.update_title_from_last_message();
                remote_user_messages.push((session_id, content.to_string()));
            }
            continue;
        }

        let Some(agentic) = &mut session.agentic else {
            continue;
        };

        match role {
            Some("user") => {
                session.chat.push(Message::User(content.to_string().into()));
            }
            Some("assistant") => {
                session.chat.push(Message::Assistant(
                    crate::messages::AssistantMessage::from_text(content.to_string()),
                ));
            }
            Some("tool_call") => {
                session.chat.push(Message::Assistant(
                    crate::messages::AssistantMessage::from_text(content.to_string()),
                ));
            }
            Some("tool_result") => {
                let summary = if content.chars().count() > 100 {
                    let truncated: String = content.chars().take(100).collect();
                    format!("{}...", truncated)
                } else {
                    content.to_string()
                };
                let tool_name = session_events::get_tag_value(note, "tool-name")
                    .unwrap_or("tool")
                    .to_string();
                session
                    .chat
                    .push(Message::ToolResponse(ToolResponse::executed_tool(
                        crate::messages::ExecutedTool {
                            tool_name,
                            summary,
                            parent_task_id: None,
                            file_update: None,
                        },
                    )));
            }
            Some("permission_request") => {
                handle_remote_permission_request(
                    note,
                    content,
                    agentic,
                    &mut session.chat,
                    secret_key,
                    &mut events_to_publish,
                );
            }
            Some("permission_response") => {
                // Track that this permission was responded to
                if let Some(perm_id_str) = session_events::get_tag_value(note, "perm-id") {
                    if let Ok(perm_id) = uuid::Uuid::parse_str(perm_id_str) {
                        let (response_type, _, _) =
                            session_events::decode_permission_response(content);
                        agentic.permissions.responded.insert(perm_id, response_type);
                        // Update the matching PermissionRequest in chat
                        for msg in session.chat.iter_mut() {
                            if let Message::PermissionRequest(req) = msg {
                                if req.id == perm_id && req.response.is_none() {
                                    req.response = Some(response_type);
                                }
                            }
                        }
                    }
                }
            }
            Some("compaction_started") => {
                if agentic.compact_intent.is_none() {
                    agentic.compact_intent = Some(session::CompactIntent::Manual);
                }
            }
            Some("compaction_complete") => {
                let pre_tokens = content.parse::<u64>().unwrap_or(0);
                let info = crate::messages::CompactionInfo { pre_tokens };
                agentic.last_compaction = Some(info.clone());
                session.chat.push(Message::CompactionComplete(info));

                // Advance compact-and-proceed: for remote sessions,
                // there's no stream-end to wait for, so go straight
                // to ReadyToProceed and consume immediately.
                match agentic.compact_intent {
                    Some(session::CompactIntent::ProceedAfterCompaction) => {
                        agentic.compact_intent = Some(session::CompactIntent::ReadyToProceed);
                    }
                    _ => {
                        agentic.compact_intent = None;
                    }
                }
            }
            _ => {
                // Skip progress, queue-operation, etc.
            }
        }

        // Handle proceed after compaction for remote sessions.
        // Published as a relay event so the desktop backend picks it up.
        if session.take_compact_and_proceed() {
            if let Some(sk) = secret_key {
                if let Some(evt) = ingest_live_event(
                    session,
                    ndb,
                    sk,
                    "Proceed with implementing the plan.",
                    "user",
                    None,
                    None,
                ) {
                    events_to_publish.push(evt);
                }
            }
        }
    }

    ProcessedNotes {
        remote_user_messages,
        events_to_publish,
    }
}

/// Handle a remote permission request from a kind-1988 conversation event.
/// Checks runtime allowlist for auto-accept, otherwise adds to chat for UI display.
fn handle_remote_permission_request(
    note: &nostrdb::Note,
    content: &str,
    agentic: &mut session::AgenticSessionData,
    chat: &mut Vec<Message>,
    secret_key: Option<&[u8; 32]>,
    events_to_publish: &mut Vec<session_events::BuiltEvent>,
) {
    let Ok(content_json) = serde_json::from_str::<serde_json::Value>(content) else {
        return;
    };
    let tool_name = content_json["tool_name"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let tool_input = content_json
        .get("tool_input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let perm_id = session_events::get_tag_value(note, "perm-id")
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .unwrap_or_else(uuid::Uuid::new_v4);

    // Store the note ID for linking responses
    agentic
        .permissions
        .request_note_ids
        .insert(perm_id, *note.id());

    // Runtime allowlist auto-accept
    if agentic.should_runtime_allow(&tool_name, &tool_input) {
        tracing::info!(
            "runtime allow: auto-accepting remote '{}' for this session",
            tool_name,
        );
        agentic
            .permissions
            .responded
            .insert(perm_id, crate::messages::PermissionResponseType::Allowed);
        if let Some(sk) = secret_key {
            let sid = agentic.event_session_id();
            if let Ok(evt) = session_events::build_permission_response_event(
                &perm_id,
                note.id(),
                true,
                None,
                false,
                sid,
                sk,
            ) {
                events_to_publish.push(evt);
            }
        }
        chat.push(Message::PermissionRequest(
            crate::messages::PermissionRequest::new(
                perm_id,
                tool_name,
                tool_input,
                None,
                Some(crate::messages::PermissionResponseType::Allowed),
                None,
            ),
        ));
        return;
    }

    // Check if we already responded
    let response = agentic.permissions.responded.get(&perm_id).copied();

    chat.push(Message::PermissionRequest(
        crate::messages::PermissionRequest::new(
            perm_id, tool_name, tool_input, None, response, None,
        ),
    ));
}

/// Handle a remote permission response from a kind-1988 event.
fn handle_remote_permission_response(
    note: &nostrdb::Note,
    agentic: &mut session::AgenticSessionData,
    chat: &mut [Message],
) {
    let Some(perm_id_str) = session_events::get_tag_value(note, "perm-id") else {
        tracing::warn!("permission_response event missing perm-id tag");
        return;
    };
    let Ok(perm_id) = uuid::Uuid::parse_str(perm_id_str) else {
        tracing::warn!("invalid perm-id UUID: {}", perm_id_str);
        return;
    };

    let (response_type, message, cancel_turn) =
        session_events::decode_permission_response(note.content());
    let allowed = response_type == crate::messages::PermissionResponseType::Allowed;

    if let Some(sender) = agentic.permissions.pending.remove(&perm_id) {
        let response = if allowed {
            PermissionResponse::Allow { message }
        } else if cancel_turn {
            PermissionResponse::Cancel {
                reason: message.unwrap_or_else(|| "Tool call exited by remote".to_string()),
            }
        } else {
            PermissionResponse::Deny {
                reason: message.unwrap_or_else(|| "Denied by remote".to_string()),
            }
        };
        for msg in chat.iter_mut() {
            if let Message::PermissionRequest(req) = msg {
                if req.id == perm_id {
                    req.response = Some(response_type);
                    break;
                }
            }
        }

        if sender.send(response).is_err() {
            tracing::warn!("failed to send remote permission response for {}", perm_id);
        } else {
            tracing::info!(
                "remote permission response for {}: {}",
                perm_id,
                if allowed { "allowed" } else { "denied" }
            );
        }
    }
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
    if let Some(result) = session.fold_tool_result(result) {
        session
            .chat
            .push(Message::ToolResponse(ToolResponse::executed_tool(result)));
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
    let idx = session.chat.len();
    session.chat.push(Message::Subagent(subagent));
    if let Some(agentic) = &mut session.agentic {
        agentic.subagent_indices.insert(task_id, idx);
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
    session.chat.push(Message::CompactionComplete(info));
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
///
/// Sets up ndb subscriptions for permission responses and conversation events
/// when we first learn the claude session ID.
fn handle_session_info(session: &mut session::ChatSession, info: SessionInfo, ndb: &nostrdb::Ndb) {
    // Propagate the runtime model for header display only.
    // Keep the original requested override intact so duplicate/clear
    // can reuse the user's intent instead of the backend's resolved model.
    if info.model.is_some() {
        session.details.model.clone_from(&info.model);
    }

    if let Some(agentic) = &mut session.agentic {
        // Use the stable event_id (not the CLI session ID) for subscriptions,
        // since all live events are tagged with event_id as the d-tag.
        let event_id = agentic.event_session_id().to_string();
        setup_conversation_action_subscription(agentic, &event_id, ndb);
        setup_conversation_subscription(agentic, &event_id, ndb);

        agentic.session_info = Some(info);
    }
    // Persist initial session state now that we know the claude_session_id
    session.state_dirty = true;
}

/// Handle stream-end for a session after the AI backend disconnects.
///
/// Finalizes the assistant message, publishes the live event,
/// and checks whether queued messages need redispatch.
fn handle_stream_end(
    session: &mut session::ChatSession,
    session_id: SessionId,
    secret_key: &Option<[u8; 32]>,
    ndb: &nostrdb::Ndb,
    events_to_publish: &mut Vec<session_events::BuiltEvent>,
    needs_send: &mut HashSet<SessionId>,
    needs_compact: &mut HashSet<SessionId>,
) {
    session.finalize_last_assistant();

    // Generate live event for the finalized assistant message
    if let Some(sk) = secret_key {
        if let Some(text) = session.last_assistant_text() {
            if let Some(evt) = ingest_live_event(session, ndb, sk, &text, "assistant", None, None) {
                events_to_publish.push(evt);
            }
        }
    }

    session.task_handle = None;
    session.last_backend_msg = None;

    // If the backend returned nothing (dispatch_state never left
    // AwaitingResponse), show an error so the user isn't left staring
    // at silence.
    if matches!(
        session.dispatch_state,
        session::DispatchState::AwaitingResponse { .. }
    ) && session.last_assistant_text().is_none()
    {
        tracing::warn!("Session {}: backend returned empty response", session_id);
        session
            .chat
            .push(Message::Error("No response from backend".into()));
    }

    // Check redispatch BEFORE resetting dispatch_state — the check
    // reads the state to distinguish empty responses from new messages.
    if session.needs_redispatch_after_stream_end() {
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
    // (ReadyToProceed) before we send "Proceed".
    if session.take_compact_and_proceed() {
        needs_send.insert(session_id);
    }
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
    if let Some(rx) = get_backend(backends, bt).compact_session(session_id.clone(), ctx.clone()) {
        tracing::info!("Compact dispatched for session {}", session_id);
        if let Some(session) = session_manager.get_active_mut() {
            session.incoming_tokens = Some(rx);
            session.last_backend_msg = Some(std::time::Instant::now());
        }
    } else {
        tracing::warn!("Compact failed: no backend session for {}", session_id);
    }
}

/// Dispatch a compact query for a specific session (compact-and-proceed flow).
fn dispatch_compact_for_session(
    session_manager: &mut session::SessionManager,
    backends: &HashMap<BackendType, Box<dyn AiBackend>>,
    session_id: SessionId,
    ctx: &egui::Context,
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
    if let Some(rx) = get_backend(backends, bt).compact_session(backend_session_id, ctx.clone()) {
        if let Some(session) = session_manager.get_mut(session_id) {
            session.incoming_tokens = Some(rx);
            session.last_backend_msg = Some(std::time::Instant::now());
            if let Some(agentic) = &mut session.agentic {
                agentic.compact_intent = Some(session::CompactIntent::ProceedAfterCompaction);
            }
        }
    }
}

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
    use crate::session::SessionSource;
    use crate::session_events::{build_live_event, build_permission_request_event, ThreadingState};
    use nostrdb::{Config, IngestMetadata, Ndb, Transaction};
    use notedeck::timed_serializer::TimedSerializer;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;

    fn test_config() -> Config {
        if cfg!(target_os = "windows") {
            Config::new().set_mapsize(32 * 1024 * 1024)
        } else {
            Config::new()
        }
    }

    fn test_secret_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        key[0] = 1;
        key
    }

    fn test_dave(data_path: &DataPath) -> Dave {
        let ndb_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(ndb_dir.path().to_str().unwrap(), &test_config()).unwrap();
        Dave::new(None, ndb, egui::Context::default(), data_path)
    }

    /// Integration test: events ingested out of order into ndb are sorted
    /// by `(created_at, seq)` and produce correctly ordered chat messages.
    /// This exercises the actual `process_conversation_notes` code path
    /// used by `poll_remote_conversation_events`.
    #[tokio::test]
    async fn test_process_conversation_notes_ordering() {
        let sk = test_secret_key();
        let mut threading = ThreadingState::new();
        let session_id_str = "poll-ordering-test";

        // Build events: tool_call (seq=0), permission_request (seq=1), tool_result (seq=2)
        let tool_call_evt = build_live_event(
            r#"{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}"#,
            "tool_call",
            session_id_str,
            None,
            Some("toolu_1"),
            Some("Bash"),
            &mut threading,
            &sk,
        )
        .unwrap();

        let perm_id = uuid::Uuid::new_v4();
        let perm_evt = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({"command": "rm -rf /tmp/test"}),
            session_id_str,
            &mut threading,
            &sk,
        )
        .unwrap();

        let tool_result_evt = build_live_event(
            "file1.txt\nfile2.txt",
            "tool_result",
            session_id_str,
            None,
            Some("toolu_1"),
            Some("Bash"),
            &mut threading,
            &sk,
        )
        .unwrap();

        // Set up ndb
        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();

        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
            .build();

        // Ingest in REVERSED order to simulate out-of-order relay delivery
        for event in [&tool_result_evt, &perm_evt, &tool_call_evt] {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest failed");
            let _keys = ndb.wait_for_notes(sub, 1).await.unwrap();
        }

        // Create a remote agentic session
        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        session.source = SessionSource::Remote;

        // First pass: query, process, and verify ordering
        {
            let txn = Transaction::new(&ndb).unwrap();
            let results = ndb.query(&txn, std::slice::from_ref(&filter), 128).unwrap();
            let notes: Vec<_> = results
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .collect();
            assert_eq!(notes.len(), 3, "should have 3 events in ndb");

            let result = process_conversation_notes(
                notes,
                &mut session,
                1,
                true, // is_remote
                Some(&sk),
                &ndb,
            );

            assert!(result.remote_user_messages.is_empty());
        }

        // Assert correct ordering in chat
        assert_eq!(
            session.chat.len(),
            3,
            "should have 3 chat messages, got {}",
            session.chat.len()
        );
        assert!(
            matches!(&session.chat[0], Message::Assistant(_)),
            "chat[0] should be Assistant (tool_call)",
        );
        assert!(
            matches!(&session.chat[1], Message::PermissionRequest(_)),
            "chat[1] should be PermissionRequest",
        );
        assert!(
            matches!(&session.chat[2], Message::ToolResponse(_)),
            "chat[2] should be ToolResponse (tool_result)",
        );

        // Verify permission request has correct tool name
        if let Message::PermissionRequest(req) = &session.chat[1] {
            assert_eq!(req.tool_name, "Bash");
            assert_eq!(req.id, perm_id);
        }

        // Second pass: verify dedup prevents duplicate messages
        {
            let txn = Transaction::new(&ndb).unwrap();
            let results = ndb.query(&txn, &[filter], 128).unwrap();
            let notes: Vec<_> = results
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .collect();

            let _result = process_conversation_notes(notes, &mut session, 1, true, Some(&sk), &ndb);
        }
        assert_eq!(
            session.chat.len(),
            3,
            "dedup should prevent duplicate messages"
        );
    }

    /// A denied permission_response event must set PermissionResponseType::Denied
    /// on the matching chat PermissionRequest, not hardcode Allowed.
    ///
    /// This test processes events in two passes (simulating real polling):
    /// first the permission_request, then the permission_response. This
    /// ensures the response branch sees an existing pending request in chat.
    #[tokio::test]
    async fn test_permission_response_denied_is_decoded() {
        let sk = test_secret_key();
        let mut threading = ThreadingState::new();
        let session_id_str = "perm-deny-test";
        let perm_id = uuid::Uuid::new_v4();

        // 1) Build a permission_request event.
        let perm_req_evt = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({"command": "rm -rf /"}),
            session_id_str,
            &mut threading,
            &sk,
        )
        .unwrap();

        // 2) Build a permission_response event with allowed=false (deny).
        let perm_resp_evt = session_events::build_permission_response_event(
            &perm_id,
            &[0u8; 32], // dummy request note id
            false,      // DENIED
            Some("too dangerous"),
            false,
            session_id_str,
            &sk,
        )
        .unwrap();

        // Set up ndb
        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();

        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
            .build();

        // Ingest both events
        for event in [&perm_req_evt, &perm_resp_evt] {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest failed");
            let _keys = ndb.wait_for_notes(sub, 1).await.unwrap();
        }

        // Create a remote agentic session
        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Remote,
        );
        session.source = SessionSource::Remote;

        // Pass 1: process only the permission_request event so the chat
        // gets a pending PermissionRequest with response=None.
        {
            let txn = Transaction::new(&ndb).unwrap();
            let results = ndb.query(&txn, std::slice::from_ref(&filter), 128).unwrap();
            let notes: Vec<_> = results
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .filter(|n| session_events::get_tag_value(n, "role") == Some("permission_request"))
                .collect();
            assert_eq!(notes.len(), 1, "should have 1 permission_request");

            let _result = process_conversation_notes(notes, &mut session, 1, true, Some(&sk), &ndb);
        }

        // Verify the request is pending (response=None)
        let pending = session.chat.iter().find_map(|m| {
            if let Message::PermissionRequest(req) = m {
                Some(req.response)
            } else {
                None
            }
        });
        assert_eq!(
            pending,
            Some(None),
            "request should be pending before response"
        );

        // Pass 2: process the permission_response event.
        // Reset the seen set for the response event only (request was already seen).
        {
            let txn = Transaction::new(&ndb).unwrap();
            let results = ndb.query(&txn, &[filter], 128).unwrap();
            let notes: Vec<_> = results
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .collect();

            let _result = process_conversation_notes(notes, &mut session, 1, true, Some(&sk), &ndb);
        }

        // Find the PermissionRequest in chat and verify it was marked Denied
        let perm_msg = session
            .chat
            .iter()
            .find_map(|m| {
                if let Message::PermissionRequest(req) = m {
                    if req.id == perm_id {
                        Some(req)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .expect("should have a PermissionRequest in chat");

        assert_eq!(
            perm_msg.response,
            Some(crate::messages::PermissionResponseType::Denied),
            "denied permission_response should set Denied, not Allowed"
        );
    }

    /// When both permission_request and permission_response arrive in the
    /// same batch, the response may sort before the request. The request
    /// handler checks `responded` — it must use the stored decision, not
    /// hardcode Allowed.
    #[tokio::test]
    async fn test_permission_denied_single_batch() {
        let sk = test_secret_key();
        let mut threading = ThreadingState::new();
        let session_id_str = "perm-single-batch";
        let perm_id = uuid::Uuid::new_v4();

        let perm_req_evt = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({"command": "rm -rf /"}),
            session_id_str,
            &mut threading,
            &sk,
        )
        .unwrap();

        let perm_resp_evt = session_events::build_permission_response_event(
            &perm_id,
            &[0u8; 32],
            false, // DENIED
            Some("too dangerous"),
            false,
            session_id_str,
            &sk,
        )
        .unwrap();

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();

        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
            .build();

        for event in [&perm_req_evt, &perm_resp_evt] {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest failed");
            let _keys = ndb.wait_for_notes(sub, 1).await.unwrap();
        }

        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Remote,
        );
        session.source = SessionSource::Remote;

        // Process all events in one batch
        {
            let txn = Transaction::new(&ndb).unwrap();
            let results = ndb.query(&txn, &[filter], 128).unwrap();
            let notes: Vec<_> = results
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .collect();
            assert_eq!(notes.len(), 2);

            let _result = process_conversation_notes(notes, &mut session, 1, true, Some(&sk), &ndb);
        }

        // Find the PermissionRequest — regardless of processing order,
        // the denied response must be reflected.
        let perm_msg = session
            .chat
            .iter()
            .find_map(|m| {
                if let Message::PermissionRequest(req) = m {
                    if req.id == perm_id {
                        Some(req)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .expect("should have a PermissionRequest in chat");

        assert_eq!(
            perm_msg.response,
            Some(crate::messages::PermissionResponseType::Denied),
            "single-batch denied response should not be marked Allowed"
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

        let restored = test_dave(&data_path);

        assert!(
            !restored.collapse_state.is_host_collapsed("remote-a"),
            "invalid saved state should fall back to a clean default"
        );
        assert!(
            !restored
                .collapse_state
                .is_cwd_collapsed("remote-a", std::path::Path::new("/srv/api")),
            "invalid saved state should not restore any collapsed cwd entries"
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
