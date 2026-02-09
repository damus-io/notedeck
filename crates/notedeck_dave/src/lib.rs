mod agent_status;
mod auto_accept;
mod avatar;
mod backend;
mod config;
pub mod file_update;
mod focus_queue;
pub mod ipc;
pub(crate) mod mesh;
mod messages;
mod quaternion;
pub mod session;
pub mod session_discovery;
mod tools;
mod ui;
mod update;
mod vec3;

use backend::{AiBackend, BackendType, ClaudeBackend, OpenAiBackend};
use chrono::{Duration, Local};
use egui_wgpu::RenderState;
use enostr::KeypairUnowned;
use focus_queue::FocusQueue;
use nostrdb::Transaction;
use notedeck::{ui::is_narrow, AppAction, AppContext, AppResponse};
use std::collections::HashMap;
use std::path::PathBuf;
use std::string::ToString;
use std::sync::Arc;
use std::time::Instant;

pub use avatar::DaveAvatar;
pub use config::{AiMode, AiProvider, DaveSettings, ModelConfig};
pub use messages::{
    AskUserQuestionInput, DaveApiResponse, Message, PermissionResponse, PermissionResponseType,
    QuestionAnswer, SessionInfo, SubagentInfo, SubagentStatus, ToolResult,
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
    DirectoryPicker, DirectoryPickerAction, KeyAction, SceneAction, SceneResponse,
    SessionListAction, SessionListUi, SessionPicker, SessionPickerAction, SettingsPanelAction,
};
pub use vec3::Vec3;

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

        // In Chat mode, create a default session immediately and skip directory picker
        // In Agentic mode, show directory picker on startup
        let (session_manager, active_overlay) = match ai_mode {
            AiMode::Chat => {
                let mut manager = SessionManager::new();
                // Create a default session with current directory
                manager.new_session(std::env::current_dir().unwrap_or_default(), ai_mode);
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

    /// Process incoming tokens from the ai backend for ALL sessions
    fn process_events(&mut self, app_ctx: &AppContext) -> bool {
        // Should we continue sending requests? Set this to true if
        // we have tool responses to send back to the ai (only for active session)
        let mut should_send = false;
        let active_id = self.session_manager.active_id();

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
                    DaveApiResponse::Failed(err) => session.chat.push(Message::Error(err)),

                    DaveApiResponse::Token(token) => match session.chat.last_mut() {
                        Some(Message::Assistant(msg)) => msg.push_str(&token),
                        Some(_) => session.chat.push(Message::Assistant(token)),
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

                                    // Only send for active session
                                    if active_id == Some(session_id) {
                                        should_send = true;
                                    }
                                }

                                ToolCalls::Invalid(invalid) => {
                                    if active_id == Some(session_id) {
                                        should_send = true;
                                    }

                                    session.chat.push(Message::tool_error(
                                        call.id().to_string(),
                                        invalid.error.clone(),
                                    ));
                                }

                                ToolCalls::Query(search_call) => {
                                    if active_id == Some(session_id) {
                                        should_send = true;
                                    }

                                    let resp = search_call.execute(&txn, app_ctx.ndb);
                                    session.chat.push(Message::ToolResponse(ToolResponse::new(
                                        call.id().to_owned(),
                                        ToolResponses::Query(resp),
                                    )))
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
                        session.chat.push(Message::ToolResult(result));
                    }

                    DaveApiResponse::SessionInfo(info) => {
                        tracing::debug!(
                            "Session info: model={:?}, tools={}, agents={}",
                            info.model,
                            info.tools.len(),
                            info.agents.len()
                        );
                        if let Some(agentic) = &mut session.agentic {
                            agentic.session_info = Some(info);
                        }
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
                        session.task_handle = None;
                        // Don't restore incoming_tokens - leave it None
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

        should_send
    }

    fn ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        // Check overlays first - they take over the entire UI
        match self.active_overlay {
            DaveOverlay::Settings => return self.settings_overlay_ui(app_ctx, ui),
            DaveOverlay::DirectoryPicker => return self.directory_picker_overlay_ui(app_ctx, ui),
            DaveOverlay::SessionPicker => return self.session_picker_overlay_ui(app_ctx, ui),
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

    /// Full-screen settings overlay
    fn settings_overlay_ui(
        &mut self,
        _app_ctx: &mut AppContext,
        ui: &mut egui::Ui,
    ) -> DaveResponse {
        if let Some(action) = self.settings_panel.overlay_ui(ui, &self.settings) {
            match action {
                SettingsPanelAction::Save(new_settings) => {
                    self.apply_settings(new_settings.clone());
                    self.active_overlay = DaveOverlay::None;
                    return DaveResponse::new(DaveAction::UpdateSettings(new_settings));
                }
                SettingsPanelAction::Cancel => {
                    self.active_overlay = DaveOverlay::None;
                }
            }
        }
        DaveResponse::default()
    }

    /// Full-screen directory picker overlay
    fn directory_picker_overlay_ui(
        &mut self,
        _app_ctx: &mut AppContext,
        ui: &mut egui::Ui,
    ) -> DaveResponse {
        let has_sessions = !self.session_manager.is_empty();
        if let Some(action) = self.directory_picker.overlay_ui(ui, has_sessions) {
            match action {
                DirectoryPickerAction::DirectorySelected(path) => {
                    // Check if there are resumable sessions for this directory
                    let resumable_sessions = discover_sessions(&path);
                    if resumable_sessions.is_empty() {
                        // No previous sessions, create new directly
                        self.create_session_with_cwd(path);
                        self.active_overlay = DaveOverlay::None;
                    } else {
                        // Show session picker to let user choose
                        self.session_picker.open(path);
                        self.active_overlay = DaveOverlay::SessionPicker;
                    }
                }
                DirectoryPickerAction::Cancelled => {
                    // Only close if there are existing sessions to fall back to
                    if has_sessions {
                        self.active_overlay = DaveOverlay::None;
                    }
                }
                DirectoryPickerAction::BrowseRequested => {
                    // Handled internally by the picker
                }
            }
        }
        DaveResponse::default()
    }

    /// Full-screen session picker overlay (for resuming Claude sessions)
    fn session_picker_overlay_ui(
        &mut self,
        _app_ctx: &mut AppContext,
        ui: &mut egui::Ui,
    ) -> DaveResponse {
        if let Some(action) = self.session_picker.overlay_ui(ui) {
            match action {
                SessionPickerAction::ResumeSession {
                    cwd,
                    session_id,
                    title,
                } => {
                    // Create a session that resumes the existing Claude conversation
                    self.create_resumed_session_with_cwd(cwd, session_id, title);
                    self.session_picker.close();
                    self.active_overlay = DaveOverlay::None;
                }
                SessionPickerAction::NewSession { cwd } => {
                    // User chose to start fresh
                    self.create_session_with_cwd(cwd);
                    self.session_picker.close();
                    self.active_overlay = DaveOverlay::None;
                }
                SessionPickerAction::BackToDirectoryPicker => {
                    // Go back to directory picker
                    self.session_picker.close();
                    self.active_overlay = DaveOverlay::DirectoryPicker;
                }
            }
        }
        DaveResponse::default()
    }

    /// Scene view with RTS-style agent visualization and chat side panel
    fn scene_ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        use egui_extras::{Size, StripBuilder};

        let mut dave_response = DaveResponse::default();
        let mut scene_response: Option<SceneResponse> = None;

        // Check if Ctrl is held for showing keybinding hints
        let ctrl_held = ui.input(|i| i.modifiers.ctrl);
        let auto_steal_focus = self.auto_steal_focus;

        StripBuilder::new(ui)
            .size(Size::relative(0.25)) // Scene area: 25%
            .size(Size::remainder()) // Chat panel: 75%
            .clip(true) // Clip content to cell bounds
            .horizontal(|mut strip| {
                // Scene area (main)
                strip.cell(|ui| {
                    // Scene toolbar at top
                    ui.horizontal(|ui| {
                        if ui
                            .button("+ New Agent")
                            .on_hover_text("Hold Ctrl to see keybindings")
                            .clicked()
                        {
                            dave_response = DaveResponse::new(DaveAction::NewChat);
                        }
                        // Show keybinding hint only when Ctrl is held
                        if ctrl_held {
                            ui::keybind_hint(ui, "N");
                        }
                        ui.separator();
                        if ui
                            .button("List View")
                            .on_hover_text("Ctrl+L to toggle views")
                            .clicked()
                        {
                            self.show_scene = false;
                        }
                        if ctrl_held {
                            ui::keybind_hint(ui, "L");
                        }
                    });
                    ui.separator();

                    // Render the scene, passing ctrl_held for keybinding hints
                    scene_response = Some(self.scene.ui(
                        &self.session_manager,
                        &self.focus_queue,
                        ui,
                        ctrl_held,
                    ));
                });

                // Chat side panel
                strip.cell(|ui| {
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .inner_margin(egui::Margin::symmetric(8, 12))
                        .show(ui, |ui| {
                            if let Some(selected_id) = self.scene.primary_selection() {
                                let interrupt_pending = self.is_interrupt_pending();
                                if let Some(session) = self.session_manager.get_mut(selected_id) {
                                    // Show title
                                    ui.heading(&session.title);
                                    ui.separator();

                                    let is_working = session.status()
                                        == crate::agent_status::AgentStatus::Working;

                                    // Render chat UI for selected session
                                    let has_pending_permission = session.has_pending_permissions();
                                    let plan_mode_active = session.is_plan_mode();
                                    let mut ui_builder = DaveUi::new(
                                        self.model_config.trial,
                                        &session.chat,
                                        &mut session.input,
                                        &mut session.focus_requested,
                                        session.ai_mode,
                                    )
                                    .compact(true)
                                    .is_working(is_working)
                                    .interrupt_pending(interrupt_pending)
                                    .has_pending_permission(has_pending_permission)
                                    .plan_mode_active(plan_mode_active)
                                    .auto_steal_focus(auto_steal_focus);

                                    // Add agentic-specific UI state if available
                                    if let Some(agentic) = &mut session.agentic {
                                        ui_builder = ui_builder
                                            .permission_message_state(
                                                agentic.permission_message_state,
                                            )
                                            .question_answers(&mut agentic.question_answers)
                                            .question_index(&mut agentic.question_index)
                                            .is_compacting(agentic.is_compacting);
                                    }

                                    let response = ui_builder.ui(app_ctx, ui);

                                    if response.action.is_some() {
                                        dave_response = response;
                                    }
                                }
                            } else {
                                // No selection
                                ui.centered_and_justified(|ui| {
                                    ui.label("Select an agent to view chat");
                                });
                            }
                        });
                });
            });

        // Handle scene actions after strip rendering
        if let Some(response) = scene_response {
            if let Some(action) = response.action {
                match action {
                    SceneAction::SelectionChanged(ids) => {
                        // Selection updated, sync with session manager's active
                        if let Some(id) = ids.first() {
                            self.session_manager.switch_to(*id);
                        }
                    }
                    SceneAction::SpawnAgent => {
                        dave_response = DaveResponse::new(DaveAction::NewChat);
                    }
                    SceneAction::DeleteSelected => {
                        for id in self.scene.selected.clone() {
                            self.delete_session(id);
                        }
                        // Focus another node after deletion
                        if let Some(session) = self.session_manager.sessions_ordered().first() {
                            self.scene.select(session.id);
                        } else {
                            self.scene.clear_selection();
                        }
                    }
                    SceneAction::AgentMoved { id, position } => {
                        if let Some(session) = self.session_manager.get_mut(id) {
                            if let Some(agentic) = &mut session.agentic {
                                agentic.scene_position = position;
                            }
                        }
                    }
                }
            }
        }

        dave_response
    }

    /// Desktop layout with sidebar for session list
    fn desktop_ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let available = ui.available_rect_before_wrap();
        let sidebar_width = 280.0;
        let ctrl_held = ui.input(|i| i.modifiers.ctrl);

        let sidebar_rect =
            egui::Rect::from_min_size(available.min, egui::vec2(sidebar_width, available.height()));
        let chat_rect = egui::Rect::from_min_size(
            egui::pos2(available.min.x + sidebar_width, available.min.y),
            egui::vec2(available.width() - sidebar_width, available.height()),
        );

        // Render sidebar first - borrow released after this
        let session_action = ui
            .allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_rect), |ui| {
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .inner_margin(egui::Margin::symmetric(8, 12))
                    .show(ui, |ui| {
                        // Add scene view toggle button - only in Agentic mode
                        if self.ai_mode == AiMode::Agentic {
                            ui.horizontal(|ui| {
                                if ui
                                    .button("Scene View")
                                    .on_hover_text("Ctrl+L to toggle views")
                                    .clicked()
                                {
                                    self.show_scene = true;
                                }
                                if ctrl_held {
                                    ui::keybind_hint(ui, "L");
                                }
                            });
                            ui.separator();
                        }
                        SessionListUi::new(
                            &self.session_manager,
                            &self.focus_queue,
                            ctrl_held,
                            self.ai_mode,
                        )
                        .ui(ui)
                    })
                    .inner
            })
            .inner;

        // Now we can mutably borrow for chat
        let interrupt_pending = self.is_interrupt_pending();
        let auto_steal_focus = self.auto_steal_focus;
        let chat_response = ui
            .allocate_new_ui(egui::UiBuilder::new().max_rect(chat_rect), |ui| {
                if let Some(session) = self.session_manager.get_active_mut() {
                    let is_working = session.status() == crate::agent_status::AgentStatus::Working;
                    let has_pending_permission = session.has_pending_permissions();
                    let plan_mode_active = session.is_plan_mode();
                    let mut ui_builder = DaveUi::new(
                        self.model_config.trial,
                        &session.chat,
                        &mut session.input,
                        &mut session.focus_requested,
                        session.ai_mode,
                    )
                    .is_working(is_working)
                    .interrupt_pending(interrupt_pending)
                    .has_pending_permission(has_pending_permission)
                    .plan_mode_active(plan_mode_active)
                    .auto_steal_focus(auto_steal_focus);

                    if let Some(agentic) = &mut session.agentic {
                        ui_builder = ui_builder
                            .permission_message_state(agentic.permission_message_state)
                            .question_answers(&mut agentic.question_answers)
                            .question_index(&mut agentic.question_index)
                            .is_compacting(agentic.is_compacting);
                    }

                    ui_builder.ui(app_ctx, ui)
                } else {
                    DaveResponse::default()
                }
            })
            .inner;

        // Handle actions after rendering
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
        if self.show_session_list {
            // Show session list
            let ctrl_held = ui.input(|i| i.modifiers.ctrl);
            let session_action = egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(egui::Margin::symmetric(8, 12))
                .show(ui, |ui| {
                    SessionListUi::new(
                        &self.session_manager,
                        &self.focus_queue,
                        ctrl_held,
                        self.ai_mode,
                    )
                    .ui(ui)
                })
                .inner;
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
            DaveResponse::default()
        } else {
            // Show chat
            let interrupt_pending = self.is_interrupt_pending();
            let auto_steal_focus = self.auto_steal_focus;
            if let Some(session) = self.session_manager.get_active_mut() {
                let is_working = session.status() == crate::agent_status::AgentStatus::Working;
                let has_pending_permission = session.has_pending_permissions();
                let plan_mode_active = session.is_plan_mode();
                let mut ui_builder = DaveUi::new(
                    self.model_config.trial,
                    &session.chat,
                    &mut session.input,
                    &mut session.focus_requested,
                    session.ai_mode,
                )
                .is_working(is_working)
                .interrupt_pending(interrupt_pending)
                .has_pending_permission(has_pending_permission)
                .plan_mode_active(plan_mode_active)
                .auto_steal_focus(auto_steal_focus);

                if let Some(agentic) = &mut session.agentic {
                    ui_builder = ui_builder
                        .permission_message_state(agentic.permission_message_state)
                        .question_answers(&mut agentic.question_answers)
                        .question_index(&mut agentic.question_index)
                        .is_compacting(agentic.is_compacting);
                }

                ui_builder.ui(app_ctx, ui)
            } else {
                DaveResponse::default()
            }
        }
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
        );
    }

    /// Create a new session that resumes an existing Claude conversation
    fn create_resumed_session_with_cwd(
        &mut self,
        cwd: PathBuf,
        resume_session_id: String,
        title: String,
    ) {
        update::create_resumed_session_with_cwd(
            &mut self.session_manager,
            &mut self.directory_picker,
            &mut self.scene,
            self.show_scene,
            self.ai_mode,
            cwd,
            resume_session_id,
            title,
        );
    }

    /// Clone the active agent, creating a new session with the same working directory
    fn clone_active_agent(&mut self) {
        update::clone_active_agent(
            &mut self.session_manager,
            &mut self.directory_picker,
            &mut self.scene,
            self.show_scene,
            self.ai_mode,
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

    /// Delete a session and clean up backend resources
    fn delete_session(&mut self, id: SessionId) {
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
        self.interrupt_pending_since = update::check_interrupt_timeout(self.interrupt_pending_since);
    }

    /// Returns true if an interrupt is pending confirmation
    pub fn is_interrupt_pending(&self) -> bool {
        self.interrupt_pending_since.is_some()
    }

    /// Handle an interrupt action - stop the current AI operation
    fn handle_interrupt(&mut self, ctx: &egui::Context) {
        update::execute_interrupt(&mut self.session_manager, self.backend.as_ref(), ctx);
    }

    /// Toggle plan mode for the active session
    fn toggle_plan_mode(&mut self, ctx: &egui::Context) {
        update::toggle_plan_mode(&mut self.session_manager, self.backend.as_ref(), ctx);
    }

    /// Exit plan mode for the active session (switch to Default mode)
    fn exit_plan_mode(&mut self, ctx: &egui::Context) {
        update::exit_plan_mode(&mut self.session_manager, self.backend.as_ref(), ctx);
    }

    /// Get the first pending permission request ID for the active session
    fn first_pending_permission(&self) -> Option<uuid::Uuid> {
        update::first_pending_permission(&self.session_manager)
    }

    /// Check if the first pending permission is an AskUserQuestion tool call
    fn has_pending_question(&self) -> bool {
        update::has_pending_question(&self.session_manager)
    }

    /// Check if the first pending permission is an ExitPlanMode tool call
    fn has_pending_exit_plan_mode(&self) -> bool {
        update::has_pending_exit_plan_mode(&self.session_manager)
    }

    /// Handle a permission response (from UI button or keybinding)
    fn handle_permission_response(&mut self, request_id: uuid::Uuid, response: PermissionResponse) {
        update::handle_permission_response(&mut self.session_manager, request_id, response);
    }

    /// Handle a user's response to an AskUserQuestion tool call
    fn handle_question_response(&mut self, request_id: uuid::Uuid, answers: Vec<QuestionAnswer>) {
        update::handle_question_response(&mut self.session_manager, request_id, answers);
    }

    /// Switch to agent by index in the ordered list (0-indexed)
    fn switch_to_agent_by_index(&mut self, index: usize) {
        update::switch_to_agent_by_index(
            &mut self.session_manager,
            &mut self.scene,
            self.show_scene,
            index,
        );
    }

    /// Cycle to the next agent
    fn cycle_next_agent(&mut self) {
        update::cycle_next_agent(&mut self.session_manager, &mut self.scene, self.show_scene);
    }

    /// Cycle to the previous agent
    fn cycle_prev_agent(&mut self) {
        update::cycle_prev_agent(&mut self.session_manager, &mut self.scene, self.show_scene);
    }

    /// Navigate to the next item in the focus queue
    fn focus_queue_next(&mut self) {
        update::focus_queue_next(
            &mut self.session_manager,
            &mut self.focus_queue,
            &mut self.scene,
            self.show_scene,
        );
    }

    /// Navigate to the previous item in the focus queue
    fn focus_queue_prev(&mut self) {
        update::focus_queue_prev(
            &mut self.session_manager,
            &mut self.focus_queue,
            &mut self.scene,
            self.show_scene,
        );
    }

    /// Toggle Done status for the current focus queue item.
    fn focus_queue_toggle_done(&mut self) {
        update::focus_queue_toggle_done(&mut self.focus_queue);
    }

    /// Toggle auto-steal focus mode
    fn toggle_auto_steal(&mut self) {
        self.auto_steal_focus = update::toggle_auto_steal(
            &mut self.session_manager,
            &mut self.scene,
            self.show_scene,
            self.auto_steal_focus,
            &mut self.home_session,
        );
    }

    /// Open an external editor for composing the input text (non-blocking)
    fn open_external_editor(&mut self) {
        update::open_external_editor(&mut self.session_manager);
    }

    /// Poll for external editor completion (called each frame)
    fn poll_editor_job(&mut self) {
        update::poll_editor_job(&mut self.session_manager);
    }

    /// Process auto-steal focus logic: switch to focus queue items as needed
    fn process_auto_steal_focus(&mut self) {
        update::process_auto_steal_focus(
            &mut self.session_manager,
            &mut self.focus_queue,
            &mut self.scene,
            self.show_scene,
            self.auto_steal_focus,
            &mut self.home_session,
        );
    }

    /// Handle a keybinding action
    fn handle_key_action(&mut self, key_action: KeyAction, ui: &egui::Ui) {
        match key_action {
            KeyAction::AcceptPermission => {
                if let Some(request_id) = self.first_pending_permission() {
                    self.handle_permission_response(
                        request_id,
                        PermissionResponse::Allow { message: None },
                    );
                    // Restore input focus after permission response
                    if let Some(session) = self.session_manager.get_active_mut() {
                        session.focus_requested = true;
                    }
                }
            }
            KeyAction::DenyPermission => {
                if let Some(request_id) = self.first_pending_permission() {
                    self.handle_permission_response(
                        request_id,
                        PermissionResponse::Deny {
                            reason: "User denied".into(),
                        },
                    );
                    // Restore input focus after permission response
                    if let Some(session) = self.session_manager.get_active_mut() {
                        session.focus_requested = true;
                    }
                }
            }
            KeyAction::TentativeAccept => {
                // Enter tentative accept mode - user will type message, then Enter to send
                if let Some(session) = self.session_manager.get_active_mut() {
                    if let Some(agentic) = &mut session.agentic {
                        agentic.permission_message_state =
                            crate::session::PermissionMessageState::TentativeAccept;
                    }
                    session.focus_requested = true;
                }
            }
            KeyAction::TentativeDeny => {
                // Enter tentative deny mode - user will type message, then Enter to send
                if let Some(session) = self.session_manager.get_active_mut() {
                    if let Some(agentic) = &mut session.agentic {
                        agentic.permission_message_state =
                            crate::session::PermissionMessageState::TentativeDeny;
                    }
                    session.focus_requested = true;
                }
            }
            KeyAction::CancelTentative => {
                // Cancel tentative mode
                if let Some(session) = self.session_manager.get_active_mut() {
                    if let Some(agentic) = &mut session.agentic {
                        agentic.permission_message_state =
                            crate::session::PermissionMessageState::None;
                    }
                }
            }
            KeyAction::SwitchToAgent(index) => {
                self.switch_to_agent_by_index(index);
            }
            KeyAction::NextAgent => {
                self.cycle_next_agent();
            }
            KeyAction::PreviousAgent => {
                self.cycle_prev_agent();
            }
            KeyAction::NewAgent => {
                self.handle_new_chat();
            }
            KeyAction::CloneAgent => {
                self.clone_active_agent();
            }
            KeyAction::Interrupt => {
                self.handle_interrupt_request(ui.ctx());
            }
            KeyAction::ToggleView => {
                self.show_scene = !self.show_scene;
            }
            KeyAction::TogglePlanMode => {
                self.toggle_plan_mode(ui.ctx());
                // Restore input focus after toggling plan mode
                if let Some(session) = self.session_manager.get_active_mut() {
                    session.focus_requested = true;
                }
            }
            KeyAction::DeleteActiveSession => {
                if let Some(id) = self.session_manager.active_id() {
                    self.delete_session(id);
                }
            }
            KeyAction::FocusQueueNext => {
                self.focus_queue_next();
            }
            KeyAction::FocusQueuePrev => {
                self.focus_queue_prev();
            }
            KeyAction::FocusQueueToggleDone => {
                self.focus_queue_toggle_done();
            }
            KeyAction::ToggleAutoSteal => {
                self.toggle_auto_steal();
            }
            KeyAction::OpenExternalEditor => {
                self.open_external_editor();
            }
        }
    }

    /// Handle the Send action, including tentative permission states
    fn handle_send_action(&mut self, ctx: &AppContext, ui: &egui::Ui) {
        // Check if we're in tentative state - if so, send permission response with message
        let tentative_state = self
            .session_manager
            .get_active()
            .and_then(|s| s.agentic.as_ref())
            .map(|a| a.permission_message_state)
            .unwrap_or(crate::session::PermissionMessageState::None);

        match tentative_state {
            crate::session::PermissionMessageState::TentativeAccept => {
                // Send permission Allow with the message from input
                // If this is ExitPlanMode, also exit plan mode
                let is_exit_plan_mode = self.has_pending_exit_plan_mode();
                if let Some(request_id) = self.first_pending_permission() {
                    let message = self
                        .session_manager
                        .get_active()
                        .map(|s| s.input.clone())
                        .filter(|m| !m.is_empty());
                    // Clear input
                    if let Some(session) = self.session_manager.get_active_mut() {
                        session.input.clear();
                    }
                    if is_exit_plan_mode {
                        self.exit_plan_mode(ui.ctx());
                    }
                    self.handle_permission_response(
                        request_id,
                        PermissionResponse::Allow { message },
                    );
                }
            }
            crate::session::PermissionMessageState::TentativeDeny => {
                // Send permission Deny with the message from input
                if let Some(request_id) = self.first_pending_permission() {
                    let reason = self
                        .session_manager
                        .get_active()
                        .map(|s| s.input.clone())
                        .filter(|m| !m.is_empty())
                        .unwrap_or_else(|| "User denied".into());
                    // Clear input
                    if let Some(session) = self.session_manager.get_active_mut() {
                        session.input.clear();
                    }
                    self.handle_permission_response(
                        request_id,
                        PermissionResponse::Deny { reason },
                    );
                }
            }
            crate::session::PermissionMessageState::None => {
                // Normal send behavior
                self.handle_user_send(ctx, ui);
            }
        }
    }

    /// Handle a UI action from DaveUi
    fn handle_ui_action(&mut self, action: DaveAction, ctx: &AppContext, ui: &egui::Ui) -> Option<AppAction> {
        match action {
            DaveAction::ToggleChrome => {
                return Some(AppAction::ToggleChrome);
            }
            DaveAction::Note(n) => {
                return Some(AppAction::Note(n));
            }
            DaveAction::NewChat => {
                self.handle_new_chat();
            }
            DaveAction::Send => {
                self.handle_send_action(ctx, ui);
            }
            DaveAction::ShowSessionList => {
                self.show_session_list = !self.show_session_list;
            }
            DaveAction::OpenSettings => {
                self.active_overlay = DaveOverlay::Settings;
            }
            DaveAction::UpdateSettings(_settings) => {
                // Parent app can poll settings() after update
            }
            DaveAction::PermissionResponse {
                request_id,
                response,
            } => {
                self.handle_permission_response(request_id, response);
            }
            DaveAction::Interrupt => {
                self.handle_interrupt(ui.ctx());
            }
            DaveAction::TentativeAccept => {
                // Enter tentative accept mode (from Shift+click)
                if let Some(session) = self.session_manager.get_active_mut() {
                    if let Some(agentic) = &mut session.agentic {
                        agentic.permission_message_state =
                            crate::session::PermissionMessageState::TentativeAccept;
                    }
                    session.focus_requested = true;
                }
            }
            DaveAction::TentativeDeny => {
                // Enter tentative deny mode (from Shift+click)
                if let Some(session) = self.session_manager.get_active_mut() {
                    if let Some(agentic) = &mut session.agentic {
                        agentic.permission_message_state =
                            crate::session::PermissionMessageState::TentativeDeny;
                    }
                    session.focus_requested = true;
                }
            }
            DaveAction::QuestionResponse {
                request_id,
                answers,
            } => {
                self.handle_question_response(request_id, answers);
            }
            DaveAction::ExitPlanMode {
                request_id,
                approved,
            } => {
                if approved {
                    // Exit plan mode and allow the tool call
                    self.exit_plan_mode(ui.ctx());
                    self.handle_permission_response(
                        request_id,
                        PermissionResponse::Allow { message: None },
                    );
                } else {
                    // Deny the tool call
                    self.handle_permission_response(
                        request_id,
                        PermissionResponse::Deny {
                            reason: "User rejected plan".into(),
                        },
                    );
                }
            }
        }
        None
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
            session.chat.push(Message::User(session.input.clone()));
            session.input.clear();
            session.update_title_from_last_message();
        }
        self.send_user_message(app_ctx, ui.ctx());
    }

    fn send_user_message(&mut self, app_ctx: &AppContext, ctx: &egui::Context) {
        let Some(session) = self.session_manager.get_active_mut() else {
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

        // Poll for external spawn-agent commands via IPC
        self.poll_ipc_commands();

        // Poll for external editor completion
        self.poll_editor_job();

        // Handle global keybindings (when no text input has focus)
        let has_pending_permission = self.first_pending_permission().is_some();
        let has_pending_question = self.has_pending_question();
        let in_tentative_state = self
            .session_manager
            .get_active()
            .and_then(|s| s.agentic.as_ref())
            .map(|a| a.permission_message_state != crate::session::PermissionMessageState::None)
            .unwrap_or(false);
        if let Some(key_action) = check_keybindings(
            ui.ctx(),
            has_pending_permission,
            has_pending_question,
            in_tentative_state,
            self.ai_mode,
        ) {
            self.handle_key_action(key_action, ui);
        }

        // Check if interrupt confirmation has timed out
        self.check_interrupt_timeout();

        // Process incoming AI responses for all sessions
        let should_send = self.process_events(ctx);

        // Update all session statuses after processing events
        self.session_manager.update_all_statuses();

        // Update focus queue based on status changes
        let status_iter = self.session_manager.iter().map(|s| (s.id, s.status()));
        self.focus_queue.update_from_statuses(status_iter);

        // Process auto-steal focus mode
        self.process_auto_steal_focus();

        // Render UI and handle actions
        if let Some(action) = self.ui(ctx, ui).action {
            if let Some(returned_action) = self.handle_ui_action(action, ctx, ui) {
                app_action = Some(returned_action);
            }
        }

        // Send continuation message if we have tool responses
        if should_send {
            self.send_user_message(ctx, ui.ctx());
        }

        AppResponse::action(app_action)
    }
}
