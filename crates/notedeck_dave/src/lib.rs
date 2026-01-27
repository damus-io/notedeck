mod agent_status;
mod avatar;
mod backend;
mod config;
pub mod file_update;
pub(crate) mod mesh;
mod messages;
mod quaternion;
pub mod session;
mod tools;
mod ui;
mod vec3;

use backend::{AiBackend, BackendType, ClaudeBackend, OpenAiBackend};
use chrono::{Duration, Local};
use egui_wgpu::RenderState;
use enostr::KeypairUnowned;
use nostrdb::Transaction;
use notedeck::{ui::is_narrow, AppAction, AppContext, AppResponse};
use std::collections::HashMap;
use std::string::ToString;
use std::sync::Arc;
use std::time::Instant;

pub use avatar::DaveAvatar;
pub use config::{AiProvider, DaveSettings, ModelConfig};
pub use messages::{
    DaveApiResponse, Message, PermissionResponse, PermissionResponseType, ToolResult,
};
pub use quaternion::Quaternion;
pub use session::{ChatSession, SessionId, SessionManager};
pub use tools::{
    PartialToolCall, QueryCall, QueryResponse, Tool, ToolCall, ToolCalls, ToolResponse,
    ToolResponses,
};
pub use ui::{
    check_keybindings, AgentScene, DaveAction, DaveResponse, DaveSettingsPanel, DaveUi, KeyAction,
    SceneAction, SceneResponse, SessionListAction, SessionListUi, SettingsPanelAction,
};
pub use vec3::Vec3;

pub struct Dave {
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

    pub fn new(render_state: Option<&RenderState>, ndb: nostrdb::Ndb) -> Self {
        let model_config = ModelConfig::default();
        //let model_config = ModelConfig::ollama();

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

        Dave {
            backend,
            avatar,
            session_manager: SessionManager::new(),
            tools: Arc::new(tools),
            model_config,
            show_session_list: false,
            settings,
            settings_panel: DaveSettingsPanel::new(),
            scene: AgentScene::new(),
            show_scene: true, // Default to scene view
            interrupt_pending_since: None,
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

                        // Store the response sender for later
                        session
                            .pending_permissions
                            .insert(pending.request.id, pending.response_tx);

                        // Add the request to chat for UI display
                        session
                            .chat
                            .push(Message::PermissionRequest(pending.request));
                    }

                    DaveApiResponse::ToolResult(result) => {
                        tracing::debug!("Tool result: {} - {}", result.tool_name, result.summary);
                        session.chat.push(Message::ToolResult(result));
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
        use egui_extras::{Size, StripBuilder};

        let mut dave_response = DaveResponse::default();
        let mut scene_response: Option<SceneResponse> = None;
        // Update all session statuses
        self.session_manager.update_all_statuses();

        // Check for agents needing attention and auto-jump to them
        if let Some(attention_id) = self.scene.check_attention(&self.session_manager) {
            // Also sync with session manager's active session
            self.session_manager.switch_to(attention_id);
        }

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
                            .button("+ New Agent [N]")
                            .on_hover_text("Press N to spawn new agent")
                            .clicked()
                        {
                            dave_response = DaveResponse::new(DaveAction::NewChat);
                        }
                        ui.separator();
                        if ui
                            .button("Classic View")
                            .on_hover_text("Tab/Shift+Tab to cycle agents")
                            .clicked()
                        {
                            self.show_scene = false;
                        }
                    });
                    ui.separator();

                    // Render the scene
                    scene_response = Some(self.scene.ui(&self.session_manager, ui));
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
                                    let has_pending_permission =
                                        !session.pending_permissions.is_empty();
                                    let response = DaveUi::new(
                                        self.model_config.trial,
                                        &session.chat,
                                        &mut session.input,
                                        &mut session.focus_requested,
                                    )
                                    .compact(true)
                                    .is_working(is_working)
                                    .interrupt_pending(interrupt_pending)
                                    .has_pending_permission(has_pending_permission)
                                    .ui(app_ctx, ui);

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
                            session.scene_position = position;
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
                        // Add scene view toggle button
                        if ui.button("Scene View").clicked() {
                            self.show_scene = true;
                        }
                        ui.separator();
                        SessionListUi::new(&self.session_manager).ui(ui)
                    })
                    .inner
            })
            .inner;

        // Now we can mutably borrow for chat
        let interrupt_pending = self.is_interrupt_pending();
        let chat_response = ui
            .allocate_new_ui(egui::UiBuilder::new().max_rect(chat_rect), |ui| {
                if let Some(session) = self.session_manager.get_active_mut() {
                    let is_working = session.status() == crate::agent_status::AgentStatus::Working;
                    let has_pending_permission = !session.pending_permissions.is_empty();
                    DaveUi::new(
                        self.model_config.trial,
                        &session.chat,
                        &mut session.input,
                        &mut session.focus_requested,
                    )
                    .is_working(is_working)
                    .interrupt_pending(interrupt_pending)
                    .has_pending_permission(has_pending_permission)
                    .ui(app_ctx, ui)
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
            let session_action = egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(egui::Margin::symmetric(8, 12))
                .show(ui, |ui| SessionListUi::new(&self.session_manager).ui(ui))
                .inner;
            if let Some(action) = session_action {
                match action {
                    SessionListAction::NewSession => {
                        self.session_manager.new_session();
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
            if let Some(session) = self.session_manager.get_active_mut() {
                let is_working = session.status() == crate::agent_status::AgentStatus::Working;
                let has_pending_permission = !session.pending_permissions.is_empty();
                DaveUi::new(
                    self.model_config.trial,
                    &session.chat,
                    &mut session.input,
                    &mut session.focus_requested,
                )
                .is_working(is_working)
                .interrupt_pending(interrupt_pending)
                .has_pending_permission(has_pending_permission)
                .ui(app_ctx, ui)
            } else {
                DaveResponse::default()
            }
        }
    }

    fn handle_new_chat(&mut self) {
        let id = self.session_manager.new_session();
        // Request focus on the new session's input
        if let Some(session) = self.session_manager.get_mut(id) {
            session.focus_requested = true;
            // Also update scene selection and camera if in scene view
            if self.show_scene {
                self.scene.select(id);
                self.scene.focus_on(session.scene_position);
            }
        }
    }

    /// Delete a session and clean up backend resources
    fn delete_session(&mut self, id: SessionId) {
        if self.session_manager.delete_session(id) {
            // Clean up backend resources (e.g., close persistent connections)
            let session_id = format!("dave-session-{}", id);
            self.backend.cleanup_session(session_id);
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
            if now.duration_since(pending_since).as_secs_f32() < Self::INTERRUPT_CONFIRM_TIMEOUT_SECS
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
            session.pending_permissions.clear();
            tracing::debug!("Interrupted session {}", session.id);
        }
    }

    /// Get the first pending permission request ID for the active session
    fn first_pending_permission(&self) -> Option<uuid::Uuid> {
        self.session_manager
            .get_active()
            .and_then(|session| session.pending_permissions.keys().next().copied())
    }

    /// Handle a permission response (from UI button or keybinding)
    fn handle_permission_response(&mut self, request_id: uuid::Uuid, response: PermissionResponse) {
        if let Some(session) = self.session_manager.get_active_mut() {
            // Record the response type in the message for UI display
            let response_type = match &response {
                PermissionResponse::Allow => messages::PermissionResponseType::Allowed,
                PermissionResponse::Deny { .. } => messages::PermissionResponseType::Denied,
            };

            for msg in &mut session.chat {
                if let Message::PermissionRequest(req) = msg {
                    if req.id == request_id {
                        req.response = Some(response_type);
                        break;
                    }
                }
            }

            if let Some(sender) = session.pending_permissions.remove(&request_id) {
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
                if session.pending_permissions.is_empty() {
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
                if session.pending_permissions.is_empty() {
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
                if session.pending_permissions.is_empty() {
                    session.focus_requested = true;
                }
            }
        }
    }

    /// Handle a user send action triggered by the ui
    fn handle_user_send(&mut self, app_ctx: &AppContext, ui: &egui::Ui) {
        if let Some(session) = self.session_manager.get_active_mut() {
            session.chat.push(Message::User(session.input.clone()));
            session.input.clear();
            session.update_title_from_first_message();
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
        let tools = self.tools.clone();
        let model_name = self.model_config.model().to_owned();
        let ctx = ctx.clone();

        // Use backend to stream request
        let (rx, task_handle) = self
            .backend
            .stream_request(messages, tools, model_name, user_id, session_id, ctx);
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

        // Render settings panel and handle its actions
        if let Some(settings_action) = self.settings_panel.ui(ui.ctx()) {
            match settings_action {
                SettingsPanelAction::Save(new_settings) => {
                    self.apply_settings(new_settings.clone());
                    dave_action = Some(DaveAction::UpdateSettings(new_settings));
                }
                SettingsPanelAction::Cancel => {
                    // Panel closed, nothing to do
                }
            }
        }

        // Handle global keybindings (when no text input has focus)
        let has_pending_permission = self.first_pending_permission().is_some();
        if let Some(key_action) = check_keybindings(ui.ctx(), has_pending_permission) {
            match key_action {
                KeyAction::AcceptPermission => {
                    if let Some(request_id) = self.first_pending_permission() {
                        self.handle_permission_response(request_id, PermissionResponse::Allow);
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
                                reason: "User denied via keyboard".into(),
                            },
                        );
                        // Restore input focus after permission response
                        if let Some(session) = self.session_manager.get_active_mut() {
                            session.focus_requested = true;
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
                KeyAction::Interrupt => {
                    self.handle_interrupt_request(ui);
                }
            }
        }

        // Check if interrupt confirmation has timed out
        self.check_interrupt_timeout();

        //update_dave(self, ctx, ui.ctx());
        let should_send = self.process_events(ctx);
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
                    self.handle_user_send(ctx, ui);
                }
                DaveAction::ShowSessionList => {
                    self.show_session_list = !self.show_session_list;
                }
                DaveAction::OpenSettings => {
                    self.settings_panel.open(&self.settings);
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
