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
mod vec3;

use backend::{AiBackend, BackendType, ClaudeBackend, OpenAiBackend};
use chrono::{Duration, Local};
use claude_agent_sdk_rs::PermissionMode;
use egui_wgpu::RenderState;
use enostr::KeypairUnowned;
use focus_queue::{FocusPriority, FocusQueue};
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
                        Some(Message::Assistant(msg)) => *msg = msg.clone() + &token,
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
        // Add to recent directories
        self.directory_picker.add_recent(cwd.clone());

        let id = self.session_manager.new_session(cwd, self.ai_mode);
        // Request focus on the new session's input
        if let Some(session) = self.session_manager.get_mut(id) {
            session.focus_requested = true;
            // Also update scene selection and camera if in scene view
            if self.show_scene {
                self.scene.select(id);
                if let Some(agentic) = &session.agentic {
                    self.scene.focus_on(agentic.scene_position);
                }
            }
        }
    }

    /// Create a new session that resumes an existing Claude conversation
    fn create_resumed_session_with_cwd(
        &mut self,
        cwd: PathBuf,
        resume_session_id: String,
        title: String,
    ) {
        // Add to recent directories
        self.directory_picker.add_recent(cwd.clone());

        let id =
            self.session_manager
                .new_resumed_session(cwd, resume_session_id, title, self.ai_mode);
        // Request focus on the new session's input
        if let Some(session) = self.session_manager.get_mut(id) {
            session.focus_requested = true;
            // Also update scene selection and camera if in scene view
            if self.show_scene {
                self.scene.select(id);
                if let Some(agentic) = &session.agentic {
                    self.scene.focus_on(agentic.scene_position);
                }
            }
        }
    }

    /// Clone the active agent, creating a new session with the same working directory
    fn clone_active_agent(&mut self) {
        if let Some(cwd) = self
            .session_manager
            .get_active()
            .and_then(|s| s.cwd().cloned())
        {
            self.create_session_with_cwd(cwd);
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
        // Remove from focus queue first
        self.focus_queue.remove_session(id);
        if self.session_manager.delete_session(id) {
            // Clean up backend resources (e.g., close persistent connections)
            let session_id = format!("dave-session-{}", id);
            self.backend.cleanup_session(session_id);

            // If no sessions remain, open the directory picker for a new session
            if self.session_manager.is_empty() {
                self.directory_picker.open();
            }
        }
    }

    /// Timeout for confirming interrupt (in seconds)
    const INTERRUPT_CONFIRM_TIMEOUT_SECS: f32 = 1.5;

    /// Handle an interrupt request - requires double-Escape to confirm
    fn handle_interrupt_request(&mut self, ui: &egui::Ui) {
        // Only allow interrupt if there's an active AI operation
        let has_active_operation = self
            .session_manager
            .get_active()
            .map(|s| s.incoming_tokens.is_some())
            .unwrap_or(false);

        if !has_active_operation {
            // No active operation, just clear any pending state
            self.interrupt_pending_since = None;
            return;
        }

        let now = Instant::now();

        if let Some(pending_since) = self.interrupt_pending_since {
            // Check if we're within the confirmation timeout
            if now.duration_since(pending_since).as_secs_f32()
                < Self::INTERRUPT_CONFIRM_TIMEOUT_SECS
            {
                // Second Escape within timeout - confirm interrupt
                self.handle_interrupt(ui);
                self.interrupt_pending_since = None;
            } else {
                // Timeout expired, treat as new first press
                self.interrupt_pending_since = Some(now);
            }
        } else {
            // First Escape press - start pending state
            self.interrupt_pending_since = Some(now);
        }
    }

    /// Check if interrupt confirmation has timed out and clear it
    fn check_interrupt_timeout(&mut self) {
        if let Some(pending_since) = self.interrupt_pending_since {
            if Instant::now().duration_since(pending_since).as_secs_f32()
                >= Self::INTERRUPT_CONFIRM_TIMEOUT_SECS
            {
                self.interrupt_pending_since = None;
            }
        }
    }

    /// Returns true if an interrupt is pending confirmation
    pub fn is_interrupt_pending(&self) -> bool {
        self.interrupt_pending_since.is_some()
    }

    /// Handle an interrupt action - stop the current AI operation
    fn handle_interrupt(&mut self, ui: &egui::Ui) {
        if let Some(session) = self.session_manager.get_active_mut() {
            let session_id = format!("dave-session-{}", session.id);
            // Send interrupt to backend
            self.backend.interrupt_session(session_id, ui.ctx().clone());
            // Clear the incoming token receiver so we stop processing
            session.incoming_tokens = None;
            // Clear pending permissions since we're interrupting
            if let Some(agentic) = &mut session.agentic {
                agentic.pending_permissions.clear();
            }
            tracing::debug!("Interrupted session {}", session.id);
        }
    }

    /// Toggle plan mode for the active session
    fn toggle_plan_mode(&mut self, ctx: &egui::Context) {
        if let Some(session) = self.session_manager.get_active_mut() {
            if let Some(agentic) = &mut session.agentic {
                // Toggle between Plan and Default modes
                let new_mode = match agentic.permission_mode {
                    PermissionMode::Plan => PermissionMode::Default,
                    _ => PermissionMode::Plan,
                };
                agentic.permission_mode = new_mode;

                // Notify the backend
                let session_id = format!("dave-session-{}", session.id);
                self.backend
                    .set_permission_mode(session_id, new_mode, ctx.clone());

                tracing::debug!(
                    "Toggled plan mode for session {} to {:?}",
                    session.id,
                    new_mode
                );
            }
        }
    }

    /// Exit plan mode for the active session (switch to Default mode)
    fn exit_plan_mode(&mut self, ctx: &egui::Context) {
        if let Some(session) = self.session_manager.get_active_mut() {
            if let Some(agentic) = &mut session.agentic {
                agentic.permission_mode = PermissionMode::Default;
                let session_id = format!("dave-session-{}", session.id);
                self.backend
                    .set_permission_mode(session_id, PermissionMode::Default, ctx.clone());
                tracing::debug!("Exited plan mode for session {}", session.id);
            }
        }
    }

    /// Get the first pending permission request ID for the active session
    fn first_pending_permission(&self) -> Option<uuid::Uuid> {
        self.session_manager
            .get_active()
            .and_then(|session| session.agentic.as_ref())
            .and_then(|agentic| agentic.pending_permissions.keys().next().copied())
    }

    /// Check if the first pending permission is an AskUserQuestion tool call
    fn has_pending_question(&self) -> bool {
        self.pending_permission_tool_name() == Some("AskUserQuestion")
    }

    /// Check if the first pending permission is an ExitPlanMode tool call
    fn has_pending_exit_plan_mode(&self) -> bool {
        self.pending_permission_tool_name() == Some("ExitPlanMode")
    }

    /// Get the tool name of the first pending permission request
    fn pending_permission_tool_name(&self) -> Option<&str> {
        let session = self.session_manager.get_active()?;
        let agentic = session.agentic.as_ref()?;
        let request_id = agentic.pending_permissions.keys().next()?;

        for msg in &session.chat {
            if let Message::PermissionRequest(req) = msg {
                if &req.id == request_id {
                    return Some(&req.tool_name);
                }
            }
        }

        None
    }

    /// Handle a permission response (from UI button or keybinding)
    fn handle_permission_response(&mut self, request_id: uuid::Uuid, response: PermissionResponse) {
        if let Some(session) = self.session_manager.get_active_mut() {
            // Record the response type in the message for UI display
            let response_type = match &response {
                PermissionResponse::Allow { .. } => messages::PermissionResponseType::Allowed,
                PermissionResponse::Deny { .. } => messages::PermissionResponseType::Denied,
            };

            // If Allow has a message, add it as a User message to the chat
            // (SDK doesn't support message field on Allow, so we inject it as context)
            if let PermissionResponse::Allow { message: Some(msg) } = &response {
                if !msg.is_empty() {
                    session.chat.push(Message::User(msg.clone()));
                }
            }

            // Clear permission message state (agentic only)
            if let Some(agentic) = &mut session.agentic {
                agentic.permission_message_state = crate::session::PermissionMessageState::None;
            }

            for msg in &mut session.chat {
                if let Message::PermissionRequest(req) = msg {
                    if req.id == request_id {
                        req.response = Some(response_type);
                        break;
                    }
                }
            }

            if let Some(agentic) = &mut session.agentic {
                if let Some(sender) = agentic.pending_permissions.remove(&request_id) {
                    if sender.send(response).is_err() {
                        tracing::error!(
                            "Failed to send permission response for request {}",
                            request_id
                        );
                    }
                } else {
                    tracing::warn!("No pending permission found for request {}", request_id);
                }
            }
        }
    }

    /// Handle a user's response to an AskUserQuestion tool call
    fn handle_question_response(&mut self, request_id: uuid::Uuid, answers: Vec<QuestionAnswer>) {
        use messages::{AnswerSummary, AnswerSummaryEntry};

        if let Some(session) = self.session_manager.get_active_mut() {
            // Find the original AskUserQuestion request to get the question labels
            let questions_input = session.chat.iter().find_map(|msg| {
                if let Message::PermissionRequest(req) = msg {
                    if req.id == request_id && req.tool_name == "AskUserQuestion" {
                        serde_json::from_value::<AskUserQuestionInput>(req.tool_input.clone()).ok()
                    } else {
                        None
                    }
                } else {
                    None
                }
            });

            // Format answers as JSON for the tool response, and build summary for display
            let (formatted_response, answer_summary) = if let Some(ref questions) = questions_input
            {
                let mut answers_obj = serde_json::Map::new();
                let mut summary_entries = Vec::with_capacity(questions.questions.len());

                for (q_idx, (question, answer)) in
                    questions.questions.iter().zip(answers.iter()).enumerate()
                {
                    let mut answer_obj = serde_json::Map::new();

                    // Map selected indices to option labels
                    let selected_labels: Vec<String> = answer
                        .selected
                        .iter()
                        .filter_map(|&idx| question.options.get(idx).map(|o| o.label.clone()))
                        .collect();

                    answer_obj.insert(
                        "selected".to_string(),
                        serde_json::Value::Array(
                            selected_labels
                                .iter()
                                .cloned()
                                .map(serde_json::Value::String)
                                .collect(),
                        ),
                    );

                    // Build display text for summary
                    let mut display_parts = selected_labels;
                    if let Some(ref other) = answer.other_text {
                        if !other.is_empty() {
                            answer_obj.insert(
                                "other".to_string(),
                                serde_json::Value::String(other.clone()),
                            );
                            display_parts.push(format!("Other: {}", other));
                        }
                    }

                    // Use header as the key, fall back to question index
                    let key = if !question.header.is_empty() {
                        question.header.clone()
                    } else {
                        format!("question_{}", q_idx)
                    };
                    answers_obj.insert(key.clone(), serde_json::Value::Object(answer_obj));

                    summary_entries.push(AnswerSummaryEntry {
                        header: key,
                        answer: display_parts.join(", "),
                    });
                }

                (
                    serde_json::json!({ "answers": answers_obj }).to_string(),
                    Some(AnswerSummary {
                        entries: summary_entries,
                    }),
                )
            } else {
                // Fallback: just serialize the answers directly
                (
                    serde_json::to_string(&answers).unwrap_or_else(|_| "{}".to_string()),
                    None,
                )
            };

            // Mark the request as allowed in the UI and store the summary for display
            for msg in &mut session.chat {
                if let Message::PermissionRequest(req) = msg {
                    if req.id == request_id {
                        req.response = Some(messages::PermissionResponseType::Allowed);
                        req.answer_summary = answer_summary.clone();
                        break;
                    }
                }
            }

            // Clean up transient answer state and send response (agentic only)
            if let Some(agentic) = &mut session.agentic {
                agentic.question_answers.remove(&request_id);
                agentic.question_index.remove(&request_id);

                // Send the response through the permission channel
                // AskUserQuestion responses are sent as Allow with the formatted answers as the message
                if let Some(sender) = agentic.pending_permissions.remove(&request_id) {
                    let response = PermissionResponse::Allow {
                        message: Some(formatted_response),
                    };
                    if sender.send(response).is_err() {
                        tracing::error!(
                            "Failed to send question response for request {}",
                            request_id
                        );
                    }
                } else {
                    tracing::warn!("No pending permission found for request {}", request_id);
                }
            }
        }
    }

    /// Switch to agent by index in the ordered list (0-indexed)
    fn switch_to_agent_by_index(&mut self, index: usize) {
        let ids = self.session_manager.session_ids();
        if let Some(&id) = ids.get(index) {
            self.session_manager.switch_to(id);
            // Also update scene selection if in scene view
            if self.show_scene {
                self.scene.select(id);
            }
            // Focus input if no permission request is pending
            if let Some(session) = self.session_manager.get_mut(id) {
                if !session.has_pending_permissions() {
                    session.focus_requested = true;
                }
            }
        }
    }

    /// Cycle to the next agent
    fn cycle_next_agent(&mut self) {
        let ids = self.session_manager.session_ids();
        if ids.is_empty() {
            return;
        }
        let current_idx = self
            .session_manager
            .active_id()
            .and_then(|active| ids.iter().position(|&id| id == active))
            .unwrap_or(0);
        let next_idx = (current_idx + 1) % ids.len();
        if let Some(&id) = ids.get(next_idx) {
            self.session_manager.switch_to(id);
            if self.show_scene {
                self.scene.select(id);
            }
            // Focus input if no permission request is pending
            if let Some(session) = self.session_manager.get_mut(id) {
                if !session.has_pending_permissions() {
                    session.focus_requested = true;
                }
            }
        }
    }

    /// Cycle to the previous agent
    fn cycle_prev_agent(&mut self) {
        let ids = self.session_manager.session_ids();
        if ids.is_empty() {
            return;
        }
        let current_idx = self
            .session_manager
            .active_id()
            .and_then(|active| ids.iter().position(|&id| id == active))
            .unwrap_or(0);
        let prev_idx = if current_idx == 0 {
            ids.len() - 1
        } else {
            current_idx - 1
        };
        if let Some(&id) = ids.get(prev_idx) {
            self.session_manager.switch_to(id);
            if self.show_scene {
                self.scene.select(id);
            }
            // Focus input if no permission request is pending
            if let Some(session) = self.session_manager.get_mut(id) {
                if !session.has_pending_permissions() {
                    session.focus_requested = true;
                }
            }
        }
    }

    /// Navigate to the next item in the focus queue
    fn focus_queue_next(&mut self) {
        if let Some(session_id) = self.focus_queue.next() {
            self.session_manager.switch_to(session_id);
            if self.show_scene {
                self.scene.select(session_id);
                if let Some(session) = self.session_manager.get(session_id) {
                    if let Some(agentic) = &session.agentic {
                        self.scene.focus_on(agentic.scene_position);
                    }
                }
            }
            // Focus input if no permission request is pending
            if let Some(session) = self.session_manager.get_mut(session_id) {
                if !session.has_pending_permissions() {
                    session.focus_requested = true;
                }
            }
        }
    }

    /// Navigate to the previous item in the focus queue
    fn focus_queue_prev(&mut self) {
        if let Some(session_id) = self.focus_queue.prev() {
            self.session_manager.switch_to(session_id);
            if self.show_scene {
                self.scene.select(session_id);
                if let Some(session) = self.session_manager.get(session_id) {
                    if let Some(agentic) = &session.agentic {
                        self.scene.focus_on(agentic.scene_position);
                    }
                }
            }
            // Focus input if no permission request is pending
            if let Some(session) = self.session_manager.get_mut(session_id) {
                if !session.has_pending_permissions() {
                    session.focus_requested = true;
                }
            }
        }
    }

    /// Toggle Done status for the current focus queue item.
    /// If the item is Done, remove it from the queue.
    fn focus_queue_toggle_done(&mut self) {
        if let Some(entry) = self.focus_queue.current() {
            if entry.priority == FocusPriority::Done {
                self.focus_queue.dequeue(entry.session_id);
            }
        }
    }

    /// Toggle auto-steal focus mode
    fn toggle_auto_steal(&mut self) {
        self.auto_steal_focus = !self.auto_steal_focus;

        if self.auto_steal_focus {
            // Enabling: record current session as home
            self.home_session = self.session_manager.active_id();
            tracing::debug!(
                "Auto-steal focus enabled, home session: {:?}",
                self.home_session
            );
        } else {
            // Disabling: switch back to home session if set
            if let Some(home_id) = self.home_session.take() {
                self.session_manager.switch_to(home_id);
                if self.show_scene {
                    self.scene.select(home_id);
                    if let Some(session) = self.session_manager.get(home_id) {
                        if let Some(agentic) = &session.agentic {
                            self.scene.focus_on(agentic.scene_position);
                        }
                    }
                }
                tracing::debug!("Auto-steal focus disabled, returned to home session");
            }
        }

        // Request focus on input after toggle
        if let Some(session) = self.session_manager.get_active_mut() {
            session.focus_requested = true;
        }
    }

    /// Open an external editor for composing the input text
    fn open_external_editor(&mut self) {
        use std::process::Command;

        let Some(session) = self.session_manager.get_active_mut() else {
            return;
        };

        // Create temp file with current input content
        let temp_path = std::env::temp_dir().join("notedeck_input.txt");
        if let Err(e) = std::fs::write(&temp_path, &session.input) {
            tracing::error!("Failed to write temp file for external editor: {}", e);
            return;
        }

        // Try $VISUAL first (GUI editors), then fall back to terminal + $EDITOR
        let visual = std::env::var("VISUAL").ok();
        let editor = std::env::var("EDITOR").ok();

        let result = if let Some(visual_editor) = visual {
            // $VISUAL is set - use it directly (assumes GUI editor)
            tracing::debug!("Opening external editor via $VISUAL: {}", visual_editor);
            Command::new(&visual_editor).arg(&temp_path).status()
        } else {
            // Fall back to terminal + $EDITOR
            let editor_cmd = editor.unwrap_or_else(|| "vim".to_string());
            let terminal = std::env::var("TERMINAL")
                .ok()
                .or_else(Self::find_terminal)
                .unwrap_or_else(|| "xterm".to_string());

            tracing::debug!(
                "Opening external editor via terminal: {} -e {} {}",
                terminal,
                editor_cmd,
                temp_path.display()
            );
            Command::new(&terminal)
                .arg("-e")
                .arg(&editor_cmd)
                .arg(&temp_path)
                .status()
        };

        match result {
            Ok(status) if status.success() => {
                // Read the edited content back
                match std::fs::read_to_string(&temp_path) {
                    Ok(content) => {
                        // Re-get mutable session reference after potential borrow issues
                        if let Some(session) = self.session_manager.get_active_mut() {
                            session.input = content;
                            session.focus_requested = true;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to read temp file after editing: {}", e);
                    }
                }
            }
            Ok(status) => {
                tracing::warn!("External editor exited with status: {}", status);
            }
            Err(e) => {
                tracing::error!("Failed to spawn external editor: {}", e);
            }
        }

        // Clean up temp file
        let _ = std::fs::remove_file(&temp_path);
    }

    /// Try to find a common terminal emulator
    fn find_terminal() -> Option<String> {
        use std::process::Command;
        let terminals = [
            "alacritty",
            "kitty",
            "gnome-terminal",
            "konsole",
            "urxvtc",
            "urxvt",
            "xterm",
        ];
        for term in terminals {
            if Command::new("which")
                .arg(term)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                return Some(term.to_string());
            }
        }
        None
    }

    /// Process auto-steal focus logic: switch to focus queue items as needed
    fn process_auto_steal_focus(&mut self) {
        if !self.auto_steal_focus {
            return;
        }

        let has_needs_input = self.focus_queue.has_needs_input();

        if has_needs_input {
            // There are NeedsInput items - check if we need to steal focus
            let current_session = self.session_manager.active_id();
            let current_priority =
                current_session.and_then(|id| self.focus_queue.get_session_priority(id));
            let already_on_needs_input = current_priority == Some(FocusPriority::NeedsInput);

            if !already_on_needs_input {
                // Save current session before stealing (only if we haven't saved yet)
                if self.home_session.is_none() {
                    self.home_session = current_session;
                    tracing::debug!("Auto-steal: saved home session {:?}", self.home_session);
                }

                // Jump to first NeedsInput item
                if let Some(idx) = self.focus_queue.first_needs_input_index() {
                    self.focus_queue.set_cursor(idx);
                    if let Some(entry) = self.focus_queue.current() {
                        self.session_manager.switch_to(entry.session_id);
                        if self.show_scene {
                            self.scene.select(entry.session_id);
                            if let Some(session) = self.session_manager.get(entry.session_id) {
                                if let Some(agentic) = &session.agentic {
                                    self.scene.focus_on(agentic.scene_position);
                                }
                            }
                        }
                        tracing::debug!("Auto-steal: switched to session {:?}", entry.session_id);
                    }
                }
            }
        } else if let Some(home_id) = self.home_session.take() {
            // No more NeedsInput items - return to saved session
            self.session_manager.switch_to(home_id);
            if self.show_scene {
                self.scene.select(home_id);
                if let Some(session) = self.session_manager.get(home_id) {
                    if let Some(agentic) = &session.agentic {
                        self.scene.focus_on(agentic.scene_position);
                    }
                }
            }
            tracing::debug!("Auto-steal: returned to home session {:?}", home_id);
        }
        // If no NeedsInput and no home_session saved, do nothing - allow free navigation
    }

    /// Handle a user send action triggered by the ui
    fn handle_user_send(&mut self, app_ctx: &AppContext, ui: &egui::Ui) {
        // Check for /cd command first (agentic only)
        let cd_result = if let Some(session) = self.session_manager.get_active_mut() {
            let input = session.input.trim().to_string();
            if input.starts_with("/cd ") {
                let path_str = input.strip_prefix("/cd ").unwrap().trim();
                let path = PathBuf::from(path_str);
                session.input.clear();
                if path.exists() && path.is_dir() {
                    if let Some(agentic) = &mut session.agentic {
                        agentic.cwd = path.clone();
                    }
                    session.chat.push(Message::System(format!(
                        "Working directory set to: {}",
                        path.display()
                    )));
                    Some(Ok(path))
                } else {
                    session
                        .chat
                        .push(Message::Error(format!("Invalid directory: {}", path_str)));
                    Some(Err(()))
                }
            } else {
                None
            }
        } else {
            None
        };

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
        let mut dave_action: Option<DaveAction> = None;

        // always insert system prompt if we have no context in active session
        if let Some(session) = self.session_manager.get_active_mut() {
            if session.chat.is_empty() {
                //session.chat.push(Dave::system_prompt());
            }
        }

        // Poll for external spawn-agent commands via IPC
        self.poll_ipc_commands();

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
                    self.handle_interrupt_request(ui);
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

        // Check if interrupt confirmation has timed out
        self.check_interrupt_timeout();

        //update_dave(self, ctx, ui.ctx());
        let should_send = self.process_events(ctx);

        // Update all session statuses after processing events
        self.session_manager.update_all_statuses();

        // Update focus queue based on status changes (replaces auto-focus-stealing)
        let status_iter = self.session_manager.iter().map(|s| (s.id, s.status()));
        self.focus_queue.update_from_statuses(status_iter);

        // Process auto-steal focus mode
        self.process_auto_steal_focus();

        if let Some(action) = self.ui(ctx, ui).action {
            match action {
                DaveAction::ToggleChrome => {
                    app_action = Some(AppAction::ToggleChrome);
                }
                DaveAction::Note(n) => {
                    app_action = Some(AppAction::Note(n));
                }
                DaveAction::NewChat => {
                    self.handle_new_chat();
                }
                DaveAction::Send => {
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
                DaveAction::ShowSessionList => {
                    self.show_session_list = !self.show_session_list;
                }
                DaveAction::OpenSettings => {
                    self.active_overlay = DaveOverlay::Settings;
                }
                DaveAction::UpdateSettings(settings) => {
                    dave_action = Some(DaveAction::UpdateSettings(settings));
                }
                DaveAction::PermissionResponse {
                    request_id,
                    response,
                } => {
                    self.handle_permission_response(request_id, response);
                }
                DaveAction::Interrupt => {
                    self.handle_interrupt(ui);
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
        }

        if should_send {
            self.send_user_message(ctx, ui.ctx());
        }

        // If we have a dave action that needs to bubble up, we can't return it
        // through AppResponse directly, but parent apps can check settings()
        let _ = dave_action; // Parent app can poll settings() after update

        AppResponse::action(app_action)
    }
}
