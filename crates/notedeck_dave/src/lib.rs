mod agent_status;
mod auto_accept;
mod avatar;
mod backend;
mod config;
pub mod file_update;
mod focus_queue;
pub(crate) mod git_status;
pub mod ipc;
pub(crate) mod mesh;
mod messages;
mod path_normalize;
mod quaternion;
pub mod session;
pub mod session_converter;
pub mod session_discovery;
pub mod session_events;
pub mod session_jsonl;
pub mod session_loader;
pub mod session_reconstructor;
mod tools;
mod ui;
mod update;
mod vec3;

use agent_status::AgentStatus;
use backend::{AiBackend, BackendType, ClaudeBackend, OpenAiBackend, RemoteOnlyBackend};
use chrono::{Duration, Local};
use egui_wgpu::RenderState;
use enostr::KeypairUnowned;
use focus_queue::FocusQueue;
use nostrdb::{Subscription, Transaction};
use notedeck::{try_process_events_core, ui::is_narrow, AppAction, AppContext, AppResponse};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::string::ToString;
use std::sync::Arc;
use std::time::Instant;

pub use avatar::DaveAvatar;
pub use config::{AiMode, AiProvider, DaveSettings, ModelConfig};
pub use messages::{
    AskUserQuestionInput, AssistantMessage, DaveApiResponse, Message, PermissionResponse,
    PermissionResponseType, QuestionAnswer, SessionInfo, SubagentInfo, SubagentStatus, ToolResult,
};
pub use quaternion::Quaternion;
pub use session::{ChatSession, SessionId, SessionManager};
pub use session_discovery::{discover_sessions, format_relative_time, ResumableSession};
pub use tools::{
    PartialToolCall, QueryCall, QueryResponse, Tool, ToolCall, ToolCalls, ToolResponse,
    ToolResponses,
};
pub use ui::{
    check_keybindings, AgentScene, DaveAction, DaveResponse, DaveSettingsPanel, DaveUi,
    DirectoryPicker, DirectoryPickerAction, KeyAction, KeyActionResult, OverlayResult, SceneAction,
    SceneResponse, SceneViewAction, SendActionResult, SessionListAction, SessionListUi,
    SessionPicker, SessionPickerAction, SettingsPanelAction, UiActionResult,
};
pub use vec3::Vec3;

/// Relay URL used for PNS event publishing and subscription.
/// TODO: make this configurable in the UI
const PNS_RELAY_URL: &str = "ws://relay.jb55.com/";

/// Extract a 32-byte secret key from a keypair.
fn secret_key_bytes(keypair: KeypairUnowned<'_>) -> Option<[u8; 32]> {
    keypair.secret_key.map(|sk| {
        sk.as_secret_bytes()
            .try_into()
            .expect("secret key is 32 bytes")
    })
}

/// Represents which full-screen overlay (if any) is currently active
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DaveOverlay {
    #[default]
    None,
    Settings,
    DirectoryPicker,
    SessionPicker,
}

pub struct Dave {
    /// AI interaction mode (Chat vs Agentic)
    ai_mode: AiMode,
    /// Manages multiple chat sessions
    session_manager: SessionManager,
    /// A 3d representation of dave.
    avatar: Option<DaveAvatar>,
    /// Shared tools available to all sessions
    tools: Arc<HashMap<String, Tool>>,
    /// AI backend (OpenAI, Claude, etc.)
    backend: Box<dyn AiBackend>,
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
    /// Auto-steal focus mode: automatically cycle through focus queue items
    auto_steal_focus: bool,
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
    /// Permission responses queued for relay publishing (from remote sessions).
    /// Built and published in the update loop where AppContext is available.
    pending_perm_responses: Vec<PermissionPublish>,
    /// Sessions pending deletion state event publication.
    /// Populated in delete_session(), drained in the update loop where AppContext is available.
    pending_deletions: Vec<DeletedSessionInfo>,
    /// Local machine hostname, included in session state events.
    hostname: String,
}

use update::PermissionPublish;

/// Info captured from a session before deletion, for publishing a "deleted" state event.
struct DeletedSessionInfo {
    claude_session_id: String,
    title: String,
    cwd: String,
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
    let session_id = agentic.event_session_id().map(|s| s.to_string())?;
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

    pub fn new(render_state: Option<&RenderState>, ndb: nostrdb::Ndb, ctx: egui::Context) -> Self {
        let model_config = ModelConfig::default();
        //let model_config = ModelConfig::ollama();

        // Determine AI mode from backend type
        let ai_mode = model_config.ai_mode();

        // Create backend based on configuration
        let backend: Box<dyn AiBackend> = match model_config.backend {
            BackendType::OpenAI => {
                use async_openai::Client;
                let client = Client::with_config(model_config.to_api());
                Box::new(OpenAiBackend::new(client, ndb.clone()))
            }
            BackendType::Claude => {
                let api_key = model_config
                    .anthropic_api_key
                    .as_ref()
                    .expect("Claude backend requires ANTHROPIC_API_KEY or CLAUDE_API_KEY");
                Box::new(ClaudeBackend::new(api_key.clone()))
            }
            BackendType::Remote => Box::new(RemoteOnlyBackend),
        };

        let avatar = render_state.map(DaveAvatar::new);
        let mut tools: HashMap<String, Tool> = HashMap::new();
        for tool in tools::dave_tools() {
            tools.insert(tool.name().to_string(), tool);
        }

        let settings = DaveSettings::from_model_config(&model_config);

        let directory_picker = DirectoryPicker::new();

        // Create IPC listener for external spawn-agent commands
        let ipc_listener = ipc::create_listener(ctx);

        let hostname = gethostname::gethostname()
            .to_string_lossy()
            .into_owned();

        // In Chat mode, create a default session immediately and skip directory picker
        // In Agentic mode, show directory picker on startup
        let (session_manager, active_overlay) = match ai_mode {
            AiMode::Chat => {
                let mut manager = SessionManager::new();
                // Create a default session with current directory
                let sid = manager.new_session(std::env::current_dir().unwrap_or_default(), ai_mode);
                if let Some(session) = manager.get_mut(sid) {
                    session.hostname = hostname.clone();
                }
                (manager, DaveOverlay::None)
            }
            AiMode::Agentic => (SessionManager::new(), DaveOverlay::DirectoryPicker),
        };

        Dave {
            ai_mode,
            backend,
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
            auto_steal_focus: false,
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
            pending_perm_responses: Vec::new(),
            pending_deletions: Vec::new(),
            hostname,
        }
    }

    /// Get current settings for persistence
    pub fn settings(&self) -> &DaveSettings {
        &self.settings
    }

    /// Apply new settings. Note: Provider changes require app restart to take effect.
    pub fn apply_settings(&mut self, settings: DaveSettings) {
        self.model_config = ModelConfig::from_settings(&settings);
        self.settings = settings;
    }

    /// Process incoming tokens from the ai backend for ALL sessions.
    /// Returns (sessions needing tool responses, events to publish to relays).
    fn process_events(
        &mut self,
        app_ctx: &AppContext,
    ) -> (HashSet<SessionId>, Vec<session_events::BuiltEvent>) {
        // Track which sessions need to send tool responses
        let mut needs_send: HashSet<SessionId> = HashSet::new();
        let mut events_to_publish: Vec<session_events::BuiltEvent> = Vec::new();
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

                match res {
                    DaveApiResponse::Failed(ref err) => {
                        if let Some(sk) = &secret_key {
                            if let Some(evt) =
                                ingest_live_event(session, app_ctx.ndb, sk, err, "error", None, None)
                            {
                                events_to_publish.push(evt);
                            }
                        }
                        session.chat.push(Message::Error(err.to_string()));
                    }

                    DaveApiResponse::Token(token) => match session.chat.last_mut() {
                        Some(Message::Assistant(msg)) => msg.push_token(&token),
                        Some(_) => {
                            let mut msg = messages::AssistantMessage::new();
                            msg.push_token(&token);
                            session.chat.push(Message::Assistant(msg));
                        }
                        None => {}
                    },

                    DaveApiResponse::ToolCalls(toolcalls) => {
                        tracing::info!("got tool calls: {:?}", toolcalls);
                        session.chat.push(Message::ToolCalls(toolcalls.clone()));

                        let txn = Transaction::new(app_ctx.ndb).unwrap();
                        for call in &toolcalls {
                            // execute toolcall
                            match call.calls() {
                                ToolCalls::PresentNotes(present) => {
                                    session.chat.push(Message::ToolResponse(ToolResponse::new(
                                        call.id().to_owned(),
                                        ToolResponses::PresentNotes(present.note_ids.len() as i32),
                                    )));

                                    needs_send.insert(session_id);
                                }

                                ToolCalls::Invalid(invalid) => {
                                    session.chat.push(Message::tool_error(
                                        call.id().to_string(),
                                        invalid.error.clone(),
                                    ));

                                    needs_send.insert(session_id);
                                }

                                ToolCalls::Query(search_call) => {
                                    let resp = search_call.execute(&txn, app_ctx.ndb);
                                    session.chat.push(Message::ToolResponse(ToolResponse::new(
                                        call.id().to_owned(),
                                        ToolResponses::Query(resp),
                                    )));

                                    needs_send.insert(session_id);
                                }
                            }
                        }
                    }

                    DaveApiResponse::PermissionRequest(pending) => {
                        tracing::info!(
                            "Permission request for tool '{}': {:?}",
                            pending.request.tool_name,
                            pending.request.tool_input
                        );

                        // Build and publish a proper permission request event
                        // with perm-id, tool-name tags for remote clients
                        if let Some(sk) = &secret_key {
                            let event_session_id = session
                                .agentic
                                .as_ref()
                                .and_then(|a| a.event_session_id().map(|s| s.to_string()));

                            if let Some(sid) = event_session_id {
                                match session_events::build_permission_request_event(
                                    &pending.request.id,
                                    &pending.request.tool_name,
                                    &pending.request.tool_input,
                                    &sid,
                                    sk,
                                ) {
                                    Ok(evt) => {
                                        // PNS-wrap and ingest into local ndb
                                        pns_ingest(app_ctx.ndb, &evt.note_json, sk);
                                        // Store note_id for linking responses
                                        if let Some(agentic) = &mut session.agentic {
                                            agentic
                                                .perm_request_note_ids
                                                .insert(pending.request.id, evt.note_id);
                                        }
                                        events_to_publish.push(evt);
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "failed to build permission request event: {}",
                                            e
                                        );
                                    }
                                }
                            }
                        }

                        // Store the response sender for later (agentic only)
                        if let Some(agentic) = &mut session.agentic {
                            agentic
                                .pending_permissions
                                .insert(pending.request.id, pending.response_tx);
                        }

                        // Add the request to chat for UI display
                        session
                            .chat
                            .push(Message::PermissionRequest(pending.request));
                    }

                    DaveApiResponse::ToolResult(result) => {
                        tracing::debug!("Tool result: {} - {}", result.tool_name, result.summary);

                        // Generate live event for tool result
                        if let Some(sk) = &secret_key {
                            let content = format!("{}: {}", result.tool_name, result.summary);
                            if let Some(evt) = ingest_live_event(
                                session,
                                app_ctx.ndb,
                                sk,
                                &content,
                                "tool_result",
                                None,
                                Some(result.tool_name.as_str()),
                            ) {
                                events_to_publish.push(evt);
                            }
                        }

                        // Invalidate git status after file-modifying tools.
                        // tool_name is a String from the Claude SDK, no enum available.
                        if matches!(result.tool_name.as_str(), "Bash" | "Write" | "Edit") {
                            if let Some(agentic) = &mut session.agentic {
                                agentic.git_status.invalidate();
                            }
                        }
                        session.chat.push(Message::ToolResult(result));
                    }

                    DaveApiResponse::SessionInfo(info) => {
                        tracing::debug!(
                            "Session info: model={:?}, tools={}, agents={}",
                            info.model,
                            info.tools.len(),
                            info.agents.len()
                        );
                        // Set up subscriptions when we learn the claude session ID
                        if let Some(agentic) = &mut session.agentic {
                            if let Some(ref csid) = info.claude_session_id {
                                // Permission response subscription (filtered to ai-permission tag)
                                if agentic.perm_response_sub.is_none() {
                                    let filter = nostrdb::Filter::new()
                                        .kinds([session_events::AI_CONVERSATION_KIND as u64])
                                        .tags([csid.as_str()], 'd')
                                        .tags(["ai-permission"], 't')
                                        .build();
                                    match app_ctx.ndb.subscribe(&[filter]) {
                                        Ok(sub) => {
                                            tracing::info!(
                                                "subscribed for remote permission responses (session {})",
                                                csid
                                            );
                                            agentic.perm_response_sub = Some(sub);
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "failed to subscribe for permission responses: {:?}",
                                                e
                                            );
                                        }
                                    }
                                }
                                // Conversation subscription for incoming remote user messages
                                if agentic.live_conversation_sub.is_none() {
                                    let filter = nostrdb::Filter::new()
                                        .kinds([session_events::AI_CONVERSATION_KIND as u64])
                                        .tags([csid.as_str()], 'd')
                                        .build();
                                    match app_ctx.ndb.subscribe(&[filter]) {
                                        Ok(sub) => {
                                            tracing::info!(
                                                "subscribed for conversation events (session {})",
                                                csid
                                            );
                                            agentic.live_conversation_sub = Some(sub);
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "failed to subscribe for conversation events: {:?}",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                            agentic.session_info = Some(info);
                        }
                        // Persist initial session state now that we know the claude_session_id
                        session.state_dirty = true;
                    }

                    DaveApiResponse::SubagentSpawned(subagent) => {
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

                    DaveApiResponse::SubagentOutput { task_id, output } => {
                        session.update_subagent_output(&task_id, &output);
                    }

                    DaveApiResponse::SubagentCompleted { task_id, result } => {
                        tracing::debug!("Subagent completed: {}", task_id);
                        session.complete_subagent(&task_id, &result);
                    }

                    DaveApiResponse::CompactionStarted => {
                        tracing::debug!("Compaction started for session {}", session_id);
                        if let Some(agentic) = &mut session.agentic {
                            agentic.is_compacting = true;
                        }
                    }

                    DaveApiResponse::CompactionComplete(info) => {
                        tracing::debug!(
                            "Compaction completed for session {}: pre_tokens={}",
                            session_id,
                            info.pre_tokens
                        );
                        if let Some(agentic) = &mut session.agentic {
                            agentic.is_compacting = false;
                            agentic.last_compaction = Some(info.clone());
                        }
                        session.chat.push(Message::CompactionComplete(info));
                    }
                }
            }

            // Check if channel is disconnected (stream ended)
            match recvr.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Stream ended, clear task state
                    if let Some(session) = self.session_manager.get_mut(session_id) {
                        // Finalize any active assistant message to cache parsed elements
                        if let Some(Message::Assistant(msg)) = session.chat.last_mut() {
                            msg.finalize();
                        }

                        // Generate live event for the finalized assistant message
                        if let Some(sk) = &secret_key {
                            if let Some(Message::Assistant(msg)) = session.chat.last() {
                                let text = msg.text().to_string();
                                if !text.is_empty() {
                                    if let Some(evt) = ingest_live_event(
                                        session,
                                        app_ctx.ndb,
                                        sk,
                                        &text,
                                        "assistant",
                                        None,
                                        None,
                                    ) {
                                        events_to_publish.push(evt);
                                    }
                                }
                            }
                        }

                        session.task_handle = None;
                        // Don't restore incoming_tokens - leave it None

                        // If chat ends with a user message, there's an
                        // unanswered remote message that arrived while we
                        // were streaming. Queue it for dispatch.
                        if session.needs_redispatch_after_stream_end() {
                            needs_send.insert(session_id);
                        }
                    }
                }
                _ => {
                    // Channel still open, put receiver back
                    if let Some(session) = self.session_manager.get_mut(session_id) {
                        session.incoming_tokens = Some(recvr);
                    }
                }
            }
        }

        (needs_send, events_to_publish)
    }

    fn ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        // Check overlays first - they take over the entire UI
        match self.active_overlay {
            DaveOverlay::Settings => {
                match ui::settings_overlay_ui(&mut self.settings_panel, &self.settings, ui) {
                    OverlayResult::ApplySettings(new_settings) => {
                        self.apply_settings(new_settings.clone());
                        self.active_overlay = DaveOverlay::None;
                        return DaveResponse::new(DaveAction::UpdateSettings(new_settings));
                    }
                    OverlayResult::Close => {
                        self.active_overlay = DaveOverlay::None;
                    }
                    _ => {}
                }
                return DaveResponse::default();
            }
            DaveOverlay::DirectoryPicker => {
                let has_sessions = !self.session_manager.is_empty();
                match ui::directory_picker_overlay_ui(&mut self.directory_picker, has_sessions, ui)
                {
                    OverlayResult::DirectorySelected(path) => {
                        self.create_session_with_cwd(path);
                        self.active_overlay = DaveOverlay::None;
                    }
                    OverlayResult::ShowSessionPicker(path) => {
                        self.session_picker.open(path);
                        self.active_overlay = DaveOverlay::SessionPicker;
                    }
                    OverlayResult::Close => {
                        self.active_overlay = DaveOverlay::None;
                    }
                    _ => {}
                }
                return DaveResponse::default();
            }
            DaveOverlay::SessionPicker => {
                match ui::session_picker_overlay_ui(&mut self.session_picker, ui) {
                    OverlayResult::ResumeSession {
                        cwd,
                        session_id,
                        title,
                        file_path,
                    } => {
                        let claude_session_id = session_id.clone();
                        let sid = self.create_resumed_session_with_cwd(cwd, session_id, title);
                        self.pending_archive_convert = Some((file_path, sid, claude_session_id));
                        self.session_picker.close();
                        self.active_overlay = DaveOverlay::None;
                    }
                    OverlayResult::NewSession { cwd } => {
                        self.create_session_with_cwd(cwd);
                        self.session_picker.close();
                        self.active_overlay = DaveOverlay::None;
                    }
                    OverlayResult::BackToDirectoryPicker => {
                        self.session_picker.close();
                        self.active_overlay = DaveOverlay::DirectoryPicker;
                    }
                    _ => {}
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
            &self.focus_queue,
            &self.model_config,
            is_interrupt_pending,
            self.auto_steal_focus,
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
            &self.model_config,
            is_interrupt_pending,
            self.auto_steal_focus,
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
                }
                SessionListAction::Delete(id) => {
                    self.delete_session(id);
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
            &self.model_config,
            is_interrupt_pending,
            self.auto_steal_focus,
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
                    self.show_session_list = false;
                }
                SessionListAction::Delete(id) => {
                    self.delete_session(id);
                }
            }
        }

        dave_response
    }

    fn handle_new_chat(&mut self) {
        // Show the directory picker overlay
        self.active_overlay = DaveOverlay::DirectoryPicker;
    }

    /// Create a new session with the given cwd (called after directory picker selection)
    fn create_session_with_cwd(&mut self, cwd: PathBuf) {
        update::create_session_with_cwd(
            &mut self.session_manager,
            &mut self.directory_picker,
            &mut self.scene,
            self.show_scene,
            self.ai_mode,
            cwd,
            &self.hostname,
        );
    }

    /// Create a new session that resumes an existing Claude conversation
    fn create_resumed_session_with_cwd(
        &mut self,
        cwd: PathBuf,
        resume_session_id: String,
        title: String,
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
        )
    }

    /// Clone the active agent, creating a new session with the same working directory
    fn clone_active_agent(&mut self) {
        update::clone_active_agent(
            &mut self.session_manager,
            &mut self.directory_picker,
            &mut self.scene,
            self.show_scene,
            self.ai_mode,
            &self.hostname,
        );
    }

    /// Poll for IPC spawn-agent commands from external tools
    fn poll_ipc_commands(&mut self) {
        let Some(listener) = self.ipc_listener.as_ref() else {
            return;
        };

        // Drain all pending connections (non-blocking)
        while let Some(mut pending) = listener.try_recv() {
            // Create the session and get its ID
            let id = self
                .session_manager
                .new_session(pending.cwd.clone(), self.ai_mode);
            self.directory_picker.add_recent(pending.cwd);

            // Focus on new session
            if let Some(session) = self.session_manager.get_mut(id) {
                session.hostname = self.hostname.clone();
                session.focus_requested = true;
                if self.show_scene {
                    self.scene.select(id);
                    if let Some(agentic) = &session.agentic {
                        self.scene.focus_on(agentic.scene_position);
                    }
                }
            }

            // Close directory picker if open
            if self.active_overlay == DaveOverlay::DirectoryPicker {
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

    /// Poll for remote permission responses arriving via nostr relays.
    ///
    /// Remote clients (phone) publish kind-1988 events with
    /// `role=permission_response` and a `perm-id` tag. We poll each
    /// session's subscription and route matching responses through the
    /// existing oneshot channel, racing with the local UI.
    fn poll_remote_permission_responses(&mut self, ndb: &nostrdb::Ndb) {
        let session_ids = self.session_manager.session_ids();
        for session_id in session_ids {
            let Some(session) = self.session_manager.get_mut(session_id) else {
                continue;
            };
            // Only local sessions poll for remote responses
            if session.is_remote() {
                continue;
            }
            let Some(agentic) = &mut session.agentic else {
                continue;
            };
            let Some(sub) = agentic.perm_response_sub else {
                continue;
            };

            // Poll for new notes (non-blocking)
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

                // Only process permission_response events
                let role = session_events::get_tag_value(&note, "role");
                if role != Some("permission_response") {
                    continue;
                }

                // Extract perm-id
                let Some(perm_id_str) = session_events::get_tag_value(&note, "perm-id") else {
                    tracing::warn!("permission_response event missing perm-id tag");
                    continue;
                };
                let Ok(perm_id) = uuid::Uuid::parse_str(perm_id_str) else {
                    tracing::warn!("invalid perm-id UUID: {}", perm_id_str);
                    continue;
                };

                // Parse the content to determine allow/deny
                let content = note.content();
                let (allowed, message) = match serde_json::from_str::<serde_json::Value>(content) {
                    Ok(v) => {
                        let decision = v.get("decision").and_then(|d| d.as_str()).unwrap_or("deny");
                        let msg = v
                            .get("message")
                            .and_then(|m| m.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string());
                        (decision == "allow", msg)
                    }
                    Err(_) => (false, None),
                };

                // Route through the existing oneshot channel (first-response-wins)
                if let Some(sender) = agentic.pending_permissions.remove(&perm_id) {
                    let response = if allowed {
                        PermissionResponse::Allow { message }
                    } else {
                        PermissionResponse::Deny {
                            reason: message.unwrap_or_else(|| "Denied by remote".to_string()),
                        }
                    };

                    // Mark in UI
                    let response_type = if allowed {
                        crate::messages::PermissionResponseType::Allowed
                    } else {
                        crate::messages::PermissionResponseType::Denied
                    };
                    for msg in &mut session.chat {
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
                // If sender not found, either local UI already responded or
                // this is a stale event — just ignore it silently.
            }
        }
    }

    /// Publish kind-31988 state events for sessions whose status changed.
    fn publish_dirty_session_states(&mut self, ctx: &mut AppContext<'_>) {
        let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) else {
            return;
        };

        for session in self.session_manager.iter_mut() {
            if !session.state_dirty || session.is_remote() {
                continue;
            }

            let Some(agentic) = &session.agentic else {
                continue;
            };

            let Some(claude_sid) = agentic.event_session_id() else {
                continue;
            };
            let claude_sid = claude_sid.to_string();

            let cwd = agentic.cwd.to_string_lossy();
            let status = session.status().as_str();

            match session_events::build_session_state_event(
                &claude_sid,
                &session.title,
                &cwd,
                status,
                &self.hostname,
                &sk,
            ) {
                Ok(evt) => {
                    tracing::info!("publishing session state: {} -> {}", claude_sid, status,);
                    pns_ingest(ctx.ndb, &evt.note_json, &sk);
                    self.pending_relay_events.push(evt);
                }
                Err(e) => {
                    tracing::error!("failed to build session state event: {}", e);
                }
            }

            session.state_dirty = false;
        }
    }

    /// Publish "deleted" state events for sessions that were deleted.
    /// Called in the update loop where AppContext is available.
    fn publish_pending_deletions(&mut self, ctx: &mut AppContext<'_>) {
        if self.pending_deletions.is_empty() {
            return;
        }

        let Some(sk) = secret_key_bytes(ctx.accounts.get_selected_account().keypair()) else {
            return;
        };

        for info in std::mem::take(&mut self.pending_deletions) {
            match session_events::build_session_state_event(
                &info.claude_session_id,
                &info.title,
                &info.cwd,
                "deleted",
                &self.hostname,
                &sk,
            ) {
                Ok(evt) => {
                    tracing::info!(
                        "publishing deleted session state: {}",
                        info.claude_session_id,
                    );
                    pns_ingest(ctx.ndb, &evt.note_json, &sk);
                    self.pending_relay_events.push(evt);
                }
                Err(e) => {
                    tracing::error!("failed to build deleted session state event: {}", e);
                }
            }
        }
    }

    /// Build and queue permission response events from remote sessions.
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

        // Get session info from the active session
        let session = match self.session_manager.get_active() {
            Some(s) => s,
            None => return,
        };
        let agentic = match &session.agentic {
            Some(a) => a,
            None => return,
        };
        let session_id = match agentic.event_session_id() {
            Some(id) => id.to_string(),
            None => return,
        };

        for resp in pending {
            let request_note_id = match agentic.perm_request_note_ids.get(&resp.perm_id) {
                Some(id) => id,
                None => {
                    tracing::warn!("no request note_id for perm_id {}", resp.perm_id);
                    continue;
                }
            };

            match session_events::build_permission_response_event(
                &resp.perm_id,
                request_note_id,
                resp.allowed,
                resp.message.as_deref(),
                &session_id,
                &sk,
            ) {
                Ok(evt) => {
                    tracing::info!(
                        "queued remote permission response for {} ({})",
                        resp.perm_id,
                        if resp.allowed { "allow" } else { "deny" }
                    );
                    pns_ingest(ctx.ndb, &evt.note_json, &sk);
                    self.pending_relay_events.push(evt);
                }
                Err(e) => {
                    tracing::error!("failed to build permission response event: {}", e);
                }
            }
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
            let cwd = std::path::PathBuf::from(&state.cwd);
            let dave_sid = self.session_manager.new_resumed_session(
                cwd,
                state.claude_session_id.clone(),
                state.title.clone(),
                AiMode::Agentic,
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

                // Determine if this is a remote session (cwd doesn't exist locally)
                let cwd = std::path::PathBuf::from(&state.cwd);
                if !cwd.exists() {
                    session.source = session::SessionSource::Remote;
                }
                let is_remote = session.is_remote();

                // Local sessions use the current machine's hostname;
                // remote sessions use what was stored in the event.
                session.hostname = if is_remote {
                    state.hostname.clone()
                } else {
                    self.hostname.clone()
                };

                if let Some(agentic) = &mut session.agentic {
                    if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id) {
                        agentic.live_threading.seed(root, last, loaded.event_count);
                    }
                    // Load permission state and dedup set from events
                    agentic.responded_perm_ids = loaded.responded_perm_ids;
                    agentic
                        .perm_request_note_ids
                        .extend(loaded.perm_request_note_ids);
                    agentic.seen_note_ids = loaded.note_ids;
                    // Set remote status from state event
                    agentic.remote_status = AgentStatus::from_status_str(&state.status);

                    // Set up live conversation subscription for remote sessions
                    if is_remote && agentic.live_conversation_sub.is_none() {
                        let conv_filter = nostrdb::Filter::new()
                            .kinds([session_events::AI_CONVERSATION_KIND as u64])
                            .tags([state.claude_session_id.as_str()], 'd')
                            .build();
                        match ctx.ndb.subscribe(&[conv_filter]) {
                            Ok(sub) => {
                                agentic.live_conversation_sub = Some(sub);
                                tracing::info!(
                                    "subscribed for live conversation events for remote session '{}'",
                                    state.title,
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "failed to subscribe for conversation events: {:?}",
                                    e,
                                );
                            }
                        }
                    }
                }
            }
        }

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
            .filter_map(|s| {
                s.agentic
                    .as_ref()
                    .and_then(|a| a.event_session_id().map(|id| id.to_string()))
            })
            .collect();

        for key in note_keys {
            let Ok(note) = ctx.ndb.get_note_by_key(&txn, key) else {
                continue;
            };

            let Some(claude_sid) = session_events::get_tag_value(&note, "d") else {
                continue;
            };

            let status_str = session_events::get_tag_value(&note, "status").unwrap_or("idle");

            // Skip deleted sessions entirely — don't create or keep them
            if status_str == "deleted" {
                // If we have this session locally, remove it
                if existing_ids.contains(claude_sid) {
                    let to_delete: Vec<SessionId> = self
                        .session_manager
                        .iter()
                        .filter(|s| {
                            s.agentic.as_ref().and_then(|a| a.event_session_id())
                                == Some(claude_sid)
                        })
                        .map(|s| s.id)
                        .collect();
                    for id in to_delete {
                        update::delete_session(
                            &mut self.session_manager,
                            &mut self.focus_queue,
                            self.backend.as_ref(),
                            &mut self.directory_picker,
                            id,
                        );
                    }
                }
                continue;
            }

            // Update remote_status for existing remote sessions
            if existing_ids.contains(claude_sid) {
                let new_status = AgentStatus::from_status_str(status_str);
                for session in self.session_manager.iter_mut() {
                    if session.is_remote() {
                        if let Some(agentic) = &mut session.agentic {
                            if agentic.event_session_id() == Some(claude_sid) {
                                agentic.remote_status = new_status;
                            }
                        }
                    }
                }
                continue;
            }

            let title = session_events::get_tag_value(&note, "title")
                .unwrap_or("Untitled")
                .to_string();
            let cwd_str = session_events::get_tag_value(&note, "cwd").unwrap_or("");
            let cwd = std::path::PathBuf::from(cwd_str);
            let hostname = session_events::get_tag_value(&note, "hostname")
                .unwrap_or("")
                .to_string();

            tracing::info!(
                "discovered new session from relay: '{}' ({}) on {}",
                title,
                claude_sid,
                hostname,
            );

            existing_ids.insert(claude_sid.to_string());

            let dave_sid = self.session_manager.new_resumed_session(
                cwd,
                claude_sid.to_string(),
                title.clone(),
                AiMode::Agentic,
            );

            // Load any conversation history that arrived with it
            let loaded = session_loader::load_session_messages(ctx.ndb, &txn, claude_sid);

            if let Some(session) = self.session_manager.get_mut(dave_sid) {
                session.hostname = hostname;
                if !loaded.messages.is_empty() {
                    tracing::info!(
                        "loaded {} messages for discovered session",
                        loaded.messages.len()
                    );
                    session.chat = loaded.messages;
                }

                // Determine if this is a remote session
                let cwd_path = std::path::PathBuf::from(cwd_str);
                if !cwd_path.exists() {
                    session.source = session::SessionSource::Remote;
                }
                let is_remote = session.is_remote();

                if let Some(agentic) = &mut session.agentic {
                    if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id) {
                        agentic.live_threading.seed(root, last, loaded.event_count);
                    }
                    // Load permission state and dedup set
                    agentic.responded_perm_ids = loaded.responded_perm_ids;
                    agentic
                        .perm_request_note_ids
                        .extend(loaded.perm_request_note_ids);
                    agentic.seen_note_ids = loaded.note_ids;
                    // Set remote status
                    agentic.remote_status = AgentStatus::from_status_str(status_str);

                    // Set up live conversation subscription for remote sessions
                    if is_remote && agentic.live_conversation_sub.is_none() {
                        let conv_filter = nostrdb::Filter::new()
                            .kinds([session_events::AI_CONVERSATION_KIND as u64])
                            .tags([claude_sid], 'd')
                            .build();
                        match ctx.ndb.subscribe(&[conv_filter]) {
                            Ok(sub) => {
                                agentic.live_conversation_sub = Some(sub);
                                tracing::info!(
                                    "subscribed for live conversation events for remote session '{}'",
                                    &title,
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "failed to subscribe for conversation events: {:?}",
                                    e,
                                );
                            }
                        }
                    }
                }
            }

            // If we were showing the directory picker, switch to showing sessions
            if matches!(self.active_overlay, DaveOverlay::DirectoryPicker) {
                self.active_overlay = DaveOverlay::None;
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
    ) -> Vec<(SessionId, String)> {
        let mut remote_user_messages: Vec<(SessionId, String)> = Vec::new();
        let session_ids = self.session_manager.session_ids();
        for session_id in session_ids {
            let Some(session) = self.session_manager.get_mut(session_id) else {
                continue;
            };
            let is_remote = session.is_remote();

            // Get sub without holding agentic borrow
            let sub = match session.agentic.as_ref().and_then(|a| a.live_conversation_sub) {
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

            // Collect and sort by created_at to process in order
            let mut notes: Vec<_> = note_keys
                .iter()
                .filter_map(|key| ndb.get_note_by_key(&txn, *key).ok())
                .collect();
            notes.sort_by_key(|n| n.created_at());

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
                        session.chat.push(Message::User(content.to_string()));
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
                        session.chat.push(Message::User(content.to_string()));
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
                            .push(Message::ToolResult(crate::messages::ToolResult {
                                tool_name,
                                summary,
                            }));
                    }
                    Some("permission_request") => {
                        if let Ok(content_json) = serde_json::from_str::<serde_json::Value>(content)
                        {
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

                            // Check if we already responded
                            let response = if agentic.responded_perm_ids.contains(&perm_id) {
                                Some(crate::messages::PermissionResponseType::Allowed)
                            } else {
                                None
                            };

                            // Store the note ID for linking responses
                            agentic.perm_request_note_ids.insert(perm_id, *note.id());

                            session.chat.push(Message::PermissionRequest(
                                crate::messages::PermissionRequest {
                                    id: perm_id,
                                    tool_name,
                                    tool_input,
                                    response,
                                    answer_summary: None,
                                    cached_plan: None,
                                },
                            ));
                        }
                    }
                    Some("permission_response") => {
                        // Track that this permission was responded to
                        if let Some(perm_id_str) = session_events::get_tag_value(note, "perm-id") {
                            if let Ok(perm_id) = uuid::Uuid::parse_str(perm_id_str) {
                                agentic.responded_perm_ids.insert(perm_id);
                                // Update the matching PermissionRequest in chat
                                for msg in session.chat.iter_mut() {
                                    if let Message::PermissionRequest(req) = msg {
                                        if req.id == perm_id && req.response.is_none() {
                                            req.response = Some(
                                                crate::messages::PermissionResponseType::Allowed,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        // Skip progress, queue-operation, etc.
                    }
                }
            }
        }
        remote_user_messages
    }

    /// Delete a session and clean up backend resources
    fn delete_session(&mut self, id: SessionId) {
        // Capture session info before deletion so we can publish a "deleted" state event
        if let Some(session) = self.session_manager.get(id) {
            if let Some(agentic) = &session.agentic {
                if let Some(claude_sid) = agentic.event_session_id() {
                    self.pending_deletions.push(DeletedSessionInfo {
                        claude_session_id: claude_sid.to_string(),
                        title: session.title.clone(),
                        cwd: agentic.cwd.to_string_lossy().to_string(),
                    });
                }
            }
        }

        update::delete_session(
            &mut self.session_manager,
            &mut self.focus_queue,
            self.backend.as_ref(),
            &mut self.directory_picker,
            id,
        );
    }

    /// Handle an interrupt request - requires double-Escape to confirm
    fn handle_interrupt_request(&mut self, ctx: &egui::Context) {
        self.interrupt_pending_since = update::handle_interrupt_request(
            &self.session_manager,
            self.backend.as_ref(),
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

    /// Get the first pending permission request ID for the active session
    fn first_pending_permission(&self) -> Option<uuid::Uuid> {
        update::first_pending_permission(&self.session_manager)
    }

    /// Check if the first pending permission is an AskUserQuestion tool call
    fn has_pending_question(&self) -> bool {
        update::has_pending_question(&self.session_manager)
    }

    /// Handle a keybinding action
    fn handle_key_action(&mut self, key_action: KeyAction, ui: &egui::Ui) {
        match ui::handle_key_action(
            key_action,
            &mut self.session_manager,
            &mut self.scene,
            &mut self.focus_queue,
            self.backend.as_ref(),
            self.show_scene,
            self.auto_steal_focus,
            &mut self.home_session,
            &mut self.active_overlay,
            ui.ctx(),
        ) {
            KeyActionResult::ToggleView => {
                self.show_scene = !self.show_scene;
            }
            KeyActionResult::HandleInterrupt => {
                self.handle_interrupt_request(ui.ctx());
            }
            KeyActionResult::CloneAgent => {
                self.clone_active_agent();
            }
            KeyActionResult::DeleteSession(id) => {
                self.delete_session(id);
            }
            KeyActionResult::SetAutoSteal(new_state) => {
                self.auto_steal_focus = new_state;
            }
            KeyActionResult::PublishPermissionResponse(publish) => {
                self.pending_perm_responses.push(publish);
            }
            KeyActionResult::None => {}
        }
    }

    /// Handle the Send action, including tentative permission states
    fn handle_send_action(&mut self, ctx: &AppContext, ui: &egui::Ui) {
        match ui::handle_send_action(&mut self.session_manager, self.backend.as_ref(), ui.ctx()) {
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
        match ui::handle_ui_action(
            action,
            &mut self.session_manager,
            self.backend.as_ref(),
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
            UiActionResult::Handled => None,
        }
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

            session.chat.push(Message::User(user_text));
            session.update_title_from_last_message();

            // Remote sessions: publish user message to relay but don't send to local backend
            if session.is_remote() {
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

        let user_id = calculate_user_id(app_ctx.accounts.get_selected_account().keypair());
        let session_id = format!("dave-session-{}", session.id);
        let messages = session.chat.clone();
        let cwd = session.agentic.as_ref().map(|a| a.cwd.clone());
        let resume_session_id = session
            .agentic
            .as_ref()
            .and_then(|a| a.resume_session_id.clone());
        let tools = self.tools.clone();
        let model_name = self.model_config.model().to_owned();
        let ctx = ctx.clone();

        // Use backend to stream request
        let (rx, task_handle) = self.backend.stream_request(
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
        session.task_handle = task_handle;
    }
}

impl notedeck::App for Dave {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let mut app_action: Option<AppAction> = None;

        // Process relay events into ndb (needed when dave is the active app).
        // Re-send PNS subscription when the relay (re)connects.
        let pns_sub_id = self.pns_relay_sub.clone();
        try_process_events_core(ctx, ui.ctx(), |app_ctx, ev| {
            if let enostr::RelayEvent::Opened = (&ev.event).into() {
                if ev.relay == PNS_RELAY_URL {
                    if let Some(sub_id) = &pns_sub_id {
                        if let Some(sk) = app_ctx.accounts.get_selected_account().keypair().secret_key {
                            let pns_keys = enostr::pns::derive_pns_keys(&sk.secret_bytes());
                            let pns_filter = nostrdb::Filter::new()
                                .kinds([enostr::pns::PNS_KIND as u64])
                                .authors([pns_keys.keypair.pubkey.bytes()])
                                .build();
                            let req = enostr::ClientMessage::req(sub_id.clone(), vec![pns_filter]);
                            app_ctx.pool.send_to(&req, PNS_RELAY_URL);
                            tracing::info!("re-subscribed for PNS events after relay reconnect");
                        }
                    }
                }
            }
        });

        // Poll for external spawn-agent commands via IPC
        self.poll_ipc_commands();

        // One-time initialization on first update
        if !self.sessions_restored {
            self.sessions_restored = true;

            // Process any PNS-wrapped events already in ndb
            let pns_ndb = ctx.ndb.clone();
            if let Err(e) = std::thread::Builder::new()
                .name("process_pns".into())
                .spawn(move || {
                    let txn = Transaction::new(&pns_ndb).expect("txn");
                    pns_ndb.process_pns(&txn);
                })
            {
                tracing::error!("failed to spawn process_pns thread: {e}");
            }

            self.restore_sessions_from_ndb(ctx);

            // Subscribe to PNS events on relays for session discovery from other devices.
            // Also subscribe locally in ndb for kind-31988 session state events
            // so we detect new sessions appearing after PNS unwrapping.
            if let Some(sk) = ctx.accounts.get_selected_account().keypair().secret_key {
                let pns_keys = enostr::pns::derive_pns_keys(&sk.secret_bytes());

                // Ensure the PNS relay is in the pool
                let egui_ctx = ui.ctx().clone();
                let wakeup = move || egui_ctx.request_repaint();
                if let Err(e) = ctx.pool.add_url(PNS_RELAY_URL.to_string(), wakeup) {
                    tracing::warn!("failed to add PNS relay {}: {:?}", PNS_RELAY_URL, e);
                }

                // Remote: subscribe on PNS relay for kind-1080 authored by our PNS pubkey
                let pns_filter = nostrdb::Filter::new()
                    .kinds([enostr::pns::PNS_KIND as u64])
                    .authors([pns_keys.keypair.pubkey.bytes()])
                    .build();
                let sub_id = uuid::Uuid::new_v4().to_string();
                let req = enostr::ClientMessage::req(sub_id.clone(), vec![pns_filter]);
                ctx.pool.send_to(&req, PNS_RELAY_URL);
                self.pns_relay_sub = Some(sub_id);
                tracing::info!("subscribed for PNS events on {}", PNS_RELAY_URL);

                // Local: subscribe in ndb for kind-31988 session state events
                let state_filter = nostrdb::Filter::new()
                    .kinds([session_events::AI_SESSION_STATE_KIND as u64])
                    .tags(["ai-session-state"], 't')
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
            }
        }

        // Poll for external editor completion
        update::poll_editor_job(&mut self.session_manager);

        // Poll for new session states from PNS-unwrapped relay events
        self.poll_session_state_events(ctx);

        // Poll for live conversation events on all sessions.
        // Returns user messages from remote clients that need backend dispatch.
        // Only dispatch if the session isn't already streaming a response —
        // the message is already in chat, so it will be included when the
        // current stream finishes and we re-dispatch.
        let remote_user_msgs = self.poll_remote_conversation_events(ctx.ndb);
        for (sid, _msg) in remote_user_msgs {
            let should_dispatch = self
                .session_manager
                .get(sid)
                .is_some_and(|s| s.should_dispatch_remote_message());
            if should_dispatch {
                self.send_user_message_for(sid, ctx, ui.ctx());
            }
        }

        // Process pending archive conversion (JSONL → nostr events)
        if let Some((file_path, dave_sid, claude_sid)) = self.pending_archive_convert.take() {
            // Check if events already exist for this session in ndb
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
                // Events already in ndb (from previous conversion or live events).
                // Skip archive conversion and load directly.
                tracing::info!(
                    "session {} already has events in ndb, skipping archive conversion",
                    claude_sid
                );
                let loaded_txn = Transaction::new(ctx.ndb).expect("txn");
                let loaded =
                    session_loader::load_session_messages(ctx.ndb, &loaded_txn, &claude_sid);
                if let Some(session) = self.session_manager.get_mut(dave_sid) {
                    tracing::info!("loaded {} messages into chat UI", loaded.messages.len());
                    session.chat = loaded.messages;

                    if let Some(agentic) = &mut session.agentic {
                        if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id)
                        {
                            agentic.live_threading.seed(root, last, loaded.event_count);
                        }
                        agentic
                            .perm_request_note_ids
                            .extend(loaded.perm_request_note_ids);
                    }
                }
            } else if let Some(secret_bytes) =
                secret_key_bytes(ctx.accounts.get_selected_account().keypair())
            {
                // Subscribe for 1988 events BEFORE ingesting so we catch them
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

        // Poll pending message load — wait for ndb to index 1988 events
        if let Some(pending) = &self.pending_message_load {
            let notes = ctx.ndb.poll_for_notes(pending.sub, 4096);
            if !notes.is_empty() {
                let txn = Transaction::new(ctx.ndb).expect("txn");
                let loaded = session_loader::load_session_messages(
                    ctx.ndb,
                    &txn,
                    &pending.claude_session_id,
                );
                if let Some(session) = self.session_manager.get_mut(pending.dave_session_id) {
                    tracing::info!("loaded {} messages into chat UI", loaded.messages.len());
                    session.chat = loaded.messages;

                    // Seed live threading from archive events so new events
                    // thread as replies to the existing conversation.
                    if let Some(agentic) = &mut session.agentic {
                        if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id)
                        {
                            agentic.live_threading.seed(root, last, loaded.event_count);
                        }
                        agentic
                            .perm_request_note_ids
                            .extend(loaded.perm_request_note_ids);
                    }
                }
                self.pending_message_load = None;
            }
        }

        // Handle global keybindings (when no text input has focus)
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
            ui.ctx(),
            has_pending_permission,
            has_pending_question,
            in_tentative_state,
            active_ai_mode,
        ) {
            self.handle_key_action(key_action, ui);
        }

        // Check if interrupt confirmation has timed out
        self.check_interrupt_timeout();

        // Process incoming AI responses for all sessions
        let (sessions_needing_send, events_to_publish) = self.process_events(ctx);

        // Build permission response events from remote sessions
        self.publish_pending_perm_responses(ctx);

        // PNS-wrap and publish events to relays
        let pending = std::mem::take(&mut self.pending_relay_events);
        let all_events = events_to_publish.iter().chain(pending.iter());
        if let Some(sk) = ctx.accounts.get_selected_account().keypair().secret_key {
            let pns_keys = enostr::pns::derive_pns_keys(&sk.secret_bytes());
            for event in all_events {
                match session_events::wrap_pns(&event.note_json, &pns_keys) {
                    Ok(pns_json) => match enostr::ClientMessage::event_json(pns_json) {
                        Ok(msg) => ctx.pool.send_to(&msg, PNS_RELAY_URL),
                        Err(e) => tracing::warn!("failed to build relay message: {:?}", e),
                    },
                    Err(e) => tracing::warn!("failed to PNS-wrap event: {}", e),
                }
            }
        }

        // Poll for remote permission responses from relay events.
        // These arrive as kind-1988 events with role=permission_response,
        // published by phone/remote clients. First-response-wins with local UI.
        self.poll_remote_permission_responses(ctx.ndb);

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

        // Update all session statuses after processing events
        self.session_manager.update_all_statuses();

        // Publish kind-31988 state events for sessions whose status changed
        self.publish_dirty_session_states(ctx);

        // Publish "deleted" state events for recently deleted sessions
        self.publish_pending_deletions(ctx);

        // Update focus queue based on status changes
        let status_iter = self.session_manager.iter().map(|s| (s.id, s.status()));
        self.focus_queue.update_from_statuses(status_iter);

        // Suppress auto-steal while the user is typing (non-empty input)
        let user_is_typing = self
            .session_manager
            .get_active()
            .is_some_and(|s| !s.input.is_empty());

        // Process auto-steal focus mode
        let stole_focus = update::process_auto_steal_focus(
            &mut self.session_manager,
            &mut self.focus_queue,
            &mut self.scene,
            self.show_scene,
            self.auto_steal_focus && !user_is_typing,
            &mut self.home_session,
        );

        // Raise the OS window when auto-steal switches to a NeedsInput session
        if stole_focus {
            activate_app(ui.ctx());
        }

        // Render UI and handle actions
        if let Some(action) = self.ui(ctx, ui).action {
            if let Some(returned_action) = self.handle_ui_action(action, ctx, ui) {
                app_action = Some(returned_action);
            }
        }

        // Send continuation messages for all sessions that have tool responses
        for session_id in sessions_needing_send {
            self.send_user_message_for(session_id, ctx, ui.ctx());
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
            let current = unsafe { NSRunningApplication::currentApplication() };
            unsafe {
                current.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
            };

            // Also force the key window to front regardless of Stage Manager
            if let Some(window) = app.keyWindow() {
                unsafe { window.orderFrontRegardless() };
            }
        }
    }
}
