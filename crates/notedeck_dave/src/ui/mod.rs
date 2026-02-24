mod ask_question;
pub mod badge;
mod dave;
pub mod diff;
pub mod directory_picker;
mod git_status_ui;
pub mod keybind_hint;
pub mod keybindings;
pub mod markdown_ui;
mod pill;
mod query_ui;
pub mod scene;
pub mod session_list;
pub mod session_picker;
mod settings;
mod top_buttons;

pub use ask_question::{ask_user_question_summary_ui, ask_user_question_ui};
pub use dave::{DaveAction, DaveResponse, DaveUi};
pub use directory_picker::{DirectoryPicker, DirectoryPickerAction};
pub use keybind_hint::{keybind_hint, paint_keybind_hint};
pub use keybindings::{check_keybindings, KeyAction};
pub use scene::{AgentScene, SceneAction, SceneResponse};
pub use session_list::{SessionListAction, SessionListUi};
pub use session_picker::{SessionPicker, SessionPickerAction};
pub use settings::{DaveSettingsPanel, SettingsPanelAction};

// =============================================================================
// Standalone UI Functions
// =============================================================================

use crate::agent_status::AgentStatus;
use crate::backend::BackendType;
use crate::config::{AiMode, DaveSettings, ModelConfig};
use crate::focus_queue::FocusQueue;
use crate::messages::PermissionResponse;
use crate::session::{ChatSession, PermissionMessageState, SessionId, SessionManager};
use crate::update;
use crate::DaveOverlay;
use egui::include_image;

/// Build a DaveUi from a session, wiring up all the common builder fields.
fn build_dave_ui<'a>(
    session: &'a mut ChatSession,
    model_config: &ModelConfig,
    is_interrupt_pending: bool,
    auto_steal_focus: bool,
) -> DaveUi<'a> {
    let is_working = session.status() == AgentStatus::Working;
    let has_pending_permission = session.has_pending_permissions();
    let plan_mode_active = session.is_plan_mode();
    let is_remote = session.is_remote();

    let mut ui_builder = DaveUi::new(
        model_config.trial,
        session.id,
        &session.chat,
        &mut session.input,
        &mut session.focus_requested,
        session.ai_mode,
    )
    .is_working(is_working)
    .interrupt_pending(is_interrupt_pending)
    .has_pending_permission(has_pending_permission)
    .plan_mode_active(plan_mode_active)
    .auto_steal_focus(auto_steal_focus)
    .is_remote(is_remote)
    .dispatched_user_count(session.dispatched_user_count)
    .details(&session.details)
    .backend_type(session.backend_type);

    if let Some(agentic) = &mut session.agentic {
        let model = agentic
            .session_info
            .as_ref()
            .and_then(|si| si.model.as_deref());
        ui_builder = ui_builder
            .permission_message_state(agentic.permission_message_state)
            .question_answers(&mut agentic.question_answers)
            .question_index(&mut agentic.question_index)
            .is_compacting(agentic.is_compacting)
            .usage(&agentic.usage, model);

        // Only show git status for local sessions
        if !is_remote {
            ui_builder = ui_builder.git_status(&mut agentic.git_status);
        }
    }

    ui_builder
}

/// Set tentative permission state on the active session's agentic data.
fn set_tentative_state(session_manager: &mut SessionManager, state: PermissionMessageState) {
    if let Some(session) = session_manager.get_active_mut() {
        if let Some(agentic) = &mut session.agentic {
            agentic.permission_message_state = state;
        }
        session.focus_requested = true;
    }
}

/// UI result from overlay rendering
pub enum OverlayResult {
    /// No action taken
    None,
    /// Close the overlay
    Close,
    /// Directory was selected (no resumable sessions)
    DirectorySelected(std::path::PathBuf),
    /// Resume a session
    ResumeSession {
        cwd: std::path::PathBuf,
        session_id: String,
        title: String,
        /// Path to the JSONL file for archive conversion
        file_path: std::path::PathBuf,
    },
    /// Create a new session in the given directory
    NewSession { cwd: std::path::PathBuf },
    /// Go back to directory picker
    BackToDirectoryPicker,
    /// Apply new settings
    ApplySettings(DaveSettings),
}

/// Render the settings overlay UI.
pub fn settings_overlay_ui(
    settings_panel: &mut DaveSettingsPanel,
    settings: &DaveSettings,
    ui: &mut egui::Ui,
) -> OverlayResult {
    if let Some(action) = settings_panel.overlay_ui(ui, settings) {
        match action {
            SettingsPanelAction::Save(new_settings) => {
                return OverlayResult::ApplySettings(new_settings);
            }
            SettingsPanelAction::Cancel => {
                return OverlayResult::Close;
            }
        }
    }
    OverlayResult::None
}

/// Render the directory picker overlay UI.
pub fn directory_picker_overlay_ui(
    directory_picker: &mut DirectoryPicker,
    has_sessions: bool,
    ui: &mut egui::Ui,
) -> OverlayResult {
    if let Some(action) = directory_picker.overlay_ui(ui, has_sessions) {
        match action {
            DirectoryPickerAction::DirectorySelected(path) => {
                return OverlayResult::DirectorySelected(path);
            }
            DirectoryPickerAction::Cancelled => {
                if has_sessions {
                    return OverlayResult::Close;
                }
            }
            DirectoryPickerAction::BrowseRequested => {}
        }
    }
    OverlayResult::None
}

/// Render the session picker overlay UI.
pub fn session_picker_overlay_ui(
    session_picker: &mut SessionPicker,
    ui: &mut egui::Ui,
) -> OverlayResult {
    if let Some(action) = session_picker.overlay_ui(ui) {
        match action {
            SessionPickerAction::ResumeSession {
                cwd,
                session_id,
                title,
                file_path,
            } => {
                return OverlayResult::ResumeSession {
                    cwd,
                    session_id,
                    title,
                    file_path,
                };
            }
            SessionPickerAction::NewSession { cwd } => {
                return OverlayResult::NewSession { cwd };
            }
            SessionPickerAction::BackToDirectoryPicker => {
                return OverlayResult::BackToDirectoryPicker;
            }
        }
    }
    OverlayResult::None
}

/// Brand color for a backend type.
pub fn backend_color(bt: BackendType) -> egui::Color32 {
    match bt {
        BackendType::Claude => egui::Color32::from_rgb(0xD9, 0x77, 0x57), // Anthropic terracotta
        BackendType::Codex => egui::Color32::from_rgb(0x10, 0xA3, 0x7F),  // OpenAI green
        _ => egui::Color32::WHITE,
    }
}

/// Get an icon image for a backend type, tinted with its brand color.
pub fn backend_icon(bt: BackendType) -> egui::Image<'static> {
    let img = match bt {
        BackendType::Claude => {
            egui::Image::new(include_image!("../../../../assets/icons/claude-code.svg"))
        }
        BackendType::Codex => {
            egui::Image::new(include_image!("../../../../assets/icons/codex.svg"))
        }
        _ => egui::Image::new(include_image!("../../../../assets/icons/sparkle.svg")),
    };
    img.tint(backend_color(bt))
}

/// Render the backend picker overlay UI.
/// Returns Some(BackendType) when the user has selected a backend.
pub fn backend_picker_overlay_ui(
    available_backends: &[BackendType],
    ui: &mut egui::Ui,
) -> Option<BackendType> {
    let mut selected = None;

    // Handle keyboard shortcuts: 1-9 for quick selection
    for (idx, &bt) in available_backends.iter().enumerate().take(9) {
        let key = match idx {
            0 => egui::Key::Num1,
            1 => egui::Key::Num2,
            2 => egui::Key::Num3,
            3 => egui::Key::Num4,
            4 => egui::Key::Num5,
            _ => continue,
        };
        if ui.input(|i| i.key_pressed(key)) {
            return Some(bt);
        }
    }

    let is_narrow = notedeck::ui::is_narrow(ui.ctx());

    egui::Frame::new()
        .fill(ui.visuals().panel_fill)
        .inner_margin(egui::Margin::symmetric(if is_narrow { 16 } else { 40 }, 20))
        .show(ui, |ui| {
            ui.heading("Select Backend");
            ui.add_space(8.0);
            ui.label("Choose which AI backend to use for this session:");
            ui.add_space(16.0);

            let max_width = if is_narrow {
                ui.available_width()
            } else {
                400.0
            };

            ui.allocate_ui_with_layout(
                egui::vec2(max_width, ui.available_height()),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    for (idx, &bt) in available_backends.iter().enumerate() {
                        let desired = egui::vec2(max_width, 44.0);
                        let (rect, response) =
                            ui.allocate_exact_size(desired, egui::Sense::click());
                        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

                        // Background
                        let fill = if response.hovered() {
                            ui.visuals().widgets.hovered.weak_bg_fill
                        } else {
                            ui.visuals().widgets.inactive.weak_bg_fill
                        };
                        ui.painter().rect_filled(rect, 8.0, fill);

                        // Icon
                        let icon_size = 20.0;
                        let icon_x = rect.left() + 12.0;
                        let icon_rect = egui::Rect::from_center_size(
                            egui::pos2(icon_x + icon_size / 2.0, rect.center().y),
                            egui::vec2(icon_size, icon_size),
                        );
                        backend_icon(bt).paint_at(ui, icon_rect);

                        // Label
                        let label = format!("[{}] {}", idx + 1, bt.display_name());
                        let text_pos = egui::pos2(icon_x + icon_size + 10.0, rect.center().y);
                        ui.painter().text(
                            text_pos,
                            egui::Align2::LEFT_CENTER,
                            &label,
                            egui::FontId::proportional(16.0),
                            ui.visuals().text_color(),
                        );

                        if response.clicked() {
                            selected = Some(bt);
                        }
                        ui.add_space(4.0);
                    }
                },
            );
        });

    selected
}

/// Scene view action returned after rendering
pub enum SceneViewAction {
    None,
    ToggleToListView,
    SpawnAgent,
    DeleteSelected(Vec<SessionId>),
}

/// Render the scene view with RTS-style agent visualization and chat side panel.
#[allow(clippy::too_many_arguments)]
pub fn scene_ui(
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    focus_queue: &mut FocusQueue,
    model_config: &ModelConfig,
    is_interrupt_pending: bool,
    auto_steal_focus: bool,
    app_ctx: &mut notedeck::AppContext,
    ui: &mut egui::Ui,
) -> (DaveResponse, SceneViewAction) {
    use egui_extras::{Size, StripBuilder};

    let mut dave_response = DaveResponse::default();
    let mut scene_response_opt: Option<SceneResponse> = None;
    let mut view_action = SceneViewAction::None;

    let ctrl_held = ui.input(|i| i.modifiers.ctrl);

    StripBuilder::new(ui)
        .size(Size::relative(0.25))
        .size(Size::remainder())
        .clip(true)
        .horizontal(|mut strip| {
            strip.cell(|ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button("+ New Agent")
                        .on_hover_text("Hold Ctrl to see keybindings")
                        .clicked()
                    {
                        view_action = SceneViewAction::SpawnAgent;
                    }
                    if ctrl_held {
                        keybind_hint(ui, "N");
                    }
                    ui.separator();
                    if ui
                        .button("List View")
                        .on_hover_text("Ctrl+L to toggle views")
                        .clicked()
                    {
                        view_action = SceneViewAction::ToggleToListView;
                    }
                    if ctrl_held {
                        keybind_hint(ui, "L");
                    }
                });
                ui.separator();
                scene_response_opt = Some(scene.ui(session_manager, focus_queue, ui, ctrl_held));
            });

            strip.cell(|ui| {
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .inner_margin(egui::Margin::symmetric(8, 12))
                    .show(ui, |ui| {
                        if let Some(selected_id) = scene.primary_selection() {
                            if let Some(session) = session_manager.get_mut(selected_id) {
                                ui.heading(session.details.display_title());
                                ui.separator();

                                let response = build_dave_ui(
                                    session,
                                    model_config,
                                    is_interrupt_pending,
                                    auto_steal_focus,
                                )
                                .compact(true)
                                .ui(app_ctx, ui);
                                if response.action.is_some() {
                                    dave_response = response;
                                }
                            }
                        } else {
                            ui.centered_and_justified(|ui| {
                                ui.label("Select an agent to view chat");
                            });
                        }
                    });
            });
        });

    // Handle scene actions
    if let Some(response) = scene_response_opt {
        if let Some(action) = response.action {
            match action {
                SceneAction::SelectionChanged(ids) => {
                    if let Some(id) = ids.first() {
                        session_manager.switch_to(*id);
                        focus_queue.dequeue(*id);
                    }
                }
                SceneAction::SpawnAgent => {
                    view_action = SceneViewAction::SpawnAgent;
                }
                SceneAction::DeleteSelected => {
                    view_action = SceneViewAction::DeleteSelected(scene.selected.clone());
                }
                SceneAction::AgentMoved { id, position } => {
                    if let Some(session) = session_manager.get_mut(id) {
                        if let Some(agentic) = &mut session.agentic {
                            agentic.scene_position = position;
                        }
                    }
                }
            }
        }
    }

    (dave_response, view_action)
}

/// Desktop layout with sidebar for session list.
#[allow(clippy::too_many_arguments)]
pub fn desktop_ui(
    session_manager: &mut SessionManager,
    focus_queue: &FocusQueue,
    model_config: &ModelConfig,
    is_interrupt_pending: bool,
    auto_steal_focus: bool,
    app_ctx: &mut notedeck::AppContext,
    ui: &mut egui::Ui,
) -> (DaveResponse, Option<SessionListAction>, bool) {
    let available = ui.available_rect_before_wrap();
    let sidebar_width = if available.width() < 830.0 {
        200.0
    } else {
        280.0
    };
    let ctrl_held = ui.input(|i| i.modifiers.ctrl);
    let mut toggle_scene = false;

    let sidebar_rect =
        egui::Rect::from_min_size(available.min, egui::vec2(sidebar_width, available.height()));
    let chat_rect = egui::Rect::from_min_size(
        egui::pos2(available.min.x + sidebar_width, available.min.y),
        egui::vec2(available.width() - sidebar_width, available.height()),
    );

    let session_action = ui
        .allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_rect), |ui| {
            egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(egui::Margin::symmetric(8, 12))
                .show(ui, |ui| {
                    let has_agentic = session_manager
                        .sessions_ordered()
                        .iter()
                        .any(|s| s.ai_mode == AiMode::Agentic);
                    if has_agentic {
                        ui.horizontal(|ui| {
                            if ui
                                .button("Scene View")
                                .on_hover_text("Ctrl+L to toggle views")
                                .clicked()
                            {
                                toggle_scene = true;
                            }
                            if ctrl_held {
                                keybind_hint(ui, "L");
                            }
                        });
                        ui.separator();
                    }
                    SessionListUi::new(session_manager, focus_queue, ctrl_held).ui(ui)
                })
                .inner
        })
        .inner;

    let chat_response = ui
        .allocate_new_ui(egui::UiBuilder::new().max_rect(chat_rect), |ui| {
            if let Some(session) = session_manager.get_active_mut() {
                build_dave_ui(
                    session,
                    model_config,
                    is_interrupt_pending,
                    auto_steal_focus,
                )
                .ui(app_ctx, ui)
            } else {
                DaveResponse::default()
            }
        })
        .inner;

    (chat_response, session_action, toggle_scene)
}

/// Narrow/mobile layout - shows either session list or chat.
#[allow(clippy::too_many_arguments)]
pub fn narrow_ui(
    session_manager: &mut SessionManager,
    focus_queue: &FocusQueue,
    model_config: &ModelConfig,
    is_interrupt_pending: bool,
    auto_steal_focus: bool,
    show_session_list: bool,
    app_ctx: &mut notedeck::AppContext,
    ui: &mut egui::Ui,
) -> (DaveResponse, Option<SessionListAction>) {
    if show_session_list {
        let ctrl_held = ui.input(|i| i.modifiers.ctrl);
        let session_action = egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::symmetric(8, 12))
            .show(ui, |ui| {
                SessionListUi::new(session_manager, focus_queue, ctrl_held).ui(ui)
            })
            .inner;
        (DaveResponse::default(), session_action)
    } else if let Some(session) = session_manager.get_active_mut() {
        let dot_color = focus_queue.current().map(|e| e.priority.color());
        let response = build_dave_ui(
            session,
            model_config,
            is_interrupt_pending,
            auto_steal_focus,
        )
        .status_dot_color(dot_color)
        .ui(app_ctx, ui);
        (response, None)
    } else {
        (DaveResponse::default(), None)
    }
}

/// Result from handling a key action
pub enum KeyActionResult {
    None,
    ToggleView,
    HandleInterrupt,
    CloneAgent,
    DeleteSession(SessionId),
    SetAutoSteal(bool),
    /// Permission response needs relay publishing.
    PublishPermissionResponse(update::PermissionPublish),
}

/// Handle a keybinding action.
#[allow(clippy::too_many_arguments)]
pub fn handle_key_action(
    key_action: KeyAction,
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    focus_queue: &mut FocusQueue,
    backend: &dyn crate::backend::AiBackend,
    show_scene: bool,
    auto_steal_focus: bool,
    home_session: &mut Option<SessionId>,
    active_overlay: &mut DaveOverlay,
    ctx: &egui::Context,
) -> KeyActionResult {
    match key_action {
        KeyAction::AcceptPermission => {
            if let Some(request_id) = update::first_pending_permission(session_manager) {
                let result = update::handle_permission_response(
                    session_manager,
                    request_id,
                    PermissionResponse::Allow { message: None },
                );
                if let Some(session) = session_manager.get_active_mut() {
                    session.focus_requested = true;
                }
                if let Some(publish) = result {
                    return KeyActionResult::PublishPermissionResponse(publish);
                }
            }
            KeyActionResult::None
        }
        KeyAction::DenyPermission => {
            if let Some(request_id) = update::first_pending_permission(session_manager) {
                let result = update::handle_permission_response(
                    session_manager,
                    request_id,
                    PermissionResponse::Deny {
                        reason: "User denied".into(),
                    },
                );
                if let Some(session) = session_manager.get_active_mut() {
                    session.focus_requested = true;
                }
                if let Some(publish) = result {
                    return KeyActionResult::PublishPermissionResponse(publish);
                }
            }
            KeyActionResult::None
        }
        KeyAction::TentativeAccept => {
            set_tentative_state(session_manager, PermissionMessageState::TentativeAccept);
            KeyActionResult::None
        }
        KeyAction::TentativeDeny => {
            set_tentative_state(session_manager, PermissionMessageState::TentativeDeny);
            KeyActionResult::None
        }
        KeyAction::CancelTentative => {
            if let Some(session) = session_manager.get_active_mut() {
                if let Some(agentic) = &mut session.agentic {
                    agentic.permission_message_state = PermissionMessageState::None;
                }
            }
            KeyActionResult::None
        }
        KeyAction::SwitchToAgent(index) => {
            update::switch_to_agent_by_index(session_manager, scene, show_scene, index);
            KeyActionResult::None
        }
        KeyAction::NextAgent => {
            update::cycle_next_agent(session_manager, scene, show_scene);
            KeyActionResult::None
        }
        KeyAction::PreviousAgent => {
            update::cycle_prev_agent(session_manager, scene, show_scene);
            KeyActionResult::None
        }
        KeyAction::NewAgent => {
            *active_overlay = DaveOverlay::DirectoryPicker;
            KeyActionResult::None
        }
        KeyAction::CloneAgent => KeyActionResult::CloneAgent,
        KeyAction::Interrupt => KeyActionResult::HandleInterrupt,
        KeyAction::ToggleView => KeyActionResult::ToggleView,
        KeyAction::TogglePlanMode => {
            update::toggle_plan_mode(session_manager, backend, ctx);
            if let Some(session) = session_manager.get_active_mut() {
                session.focus_requested = true;
            }
            KeyActionResult::None
        }
        KeyAction::DeleteActiveSession => {
            if let Some(id) = session_manager.active_id() {
                KeyActionResult::DeleteSession(id)
            } else {
                KeyActionResult::None
            }
        }
        KeyAction::FocusQueueNext => {
            update::focus_queue_next(session_manager, focus_queue, scene, show_scene);
            KeyActionResult::None
        }
        KeyAction::FocusQueuePrev => {
            update::focus_queue_prev(session_manager, focus_queue, scene, show_scene);
            KeyActionResult::None
        }
        KeyAction::FocusQueueToggleDone => {
            update::focus_queue_toggle_done(focus_queue);
            KeyActionResult::None
        }
        KeyAction::ToggleAutoSteal => {
            let new_state = update::toggle_auto_steal(
                session_manager,
                scene,
                show_scene,
                auto_steal_focus,
                home_session,
            );
            KeyActionResult::SetAutoSteal(new_state)
        }
        KeyAction::OpenExternalEditor => {
            update::open_external_editor(session_manager);
            KeyActionResult::None
        }
    }
}

/// Result from handling a send action
pub enum SendActionResult {
    /// Permission response was sent, no further action needed
    Handled,
    /// Normal send - caller should send the user message
    SendMessage,
    /// Permission response needs relay publishing.
    NeedsRelayPublish(update::PermissionPublish),
}

/// Handle the Send action, including tentative permission states.
pub fn handle_send_action(
    session_manager: &mut SessionManager,
    backend: &dyn crate::backend::AiBackend,
    ctx: &egui::Context,
) -> SendActionResult {
    let tentative_state = session_manager
        .get_active()
        .and_then(|s| s.agentic.as_ref())
        .map(|a| a.permission_message_state)
        .unwrap_or(PermissionMessageState::None);

    match tentative_state {
        PermissionMessageState::TentativeAccept => {
            let is_exit_plan_mode = update::has_pending_exit_plan_mode(session_manager);
            if let Some(request_id) = update::first_pending_permission(session_manager) {
                let message = session_manager
                    .get_active()
                    .map(|s| s.input.clone())
                    .filter(|m| !m.is_empty());
                if let Some(session) = session_manager.get_active_mut() {
                    session.input.clear();
                }
                if is_exit_plan_mode {
                    update::exit_plan_mode(session_manager, backend, ctx);
                }
                let result = update::handle_permission_response(
                    session_manager,
                    request_id,
                    PermissionResponse::Allow { message },
                );
                if let Some(publish) = result {
                    return SendActionResult::NeedsRelayPublish(publish);
                }
            }
            SendActionResult::Handled
        }
        PermissionMessageState::TentativeDeny => {
            if let Some(request_id) = update::first_pending_permission(session_manager) {
                let reason = session_manager
                    .get_active()
                    .map(|s| s.input.clone())
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "User denied".into());
                if let Some(session) = session_manager.get_active_mut() {
                    session.input.clear();
                }
                let result = update::handle_permission_response(
                    session_manager,
                    request_id,
                    PermissionResponse::Deny { reason },
                );
                if let Some(publish) = result {
                    return SendActionResult::NeedsRelayPublish(publish);
                }
            }
            SendActionResult::Handled
        }
        PermissionMessageState::None => SendActionResult::SendMessage,
    }
}

/// Result from handling a UI action
pub enum UiActionResult {
    /// Action was fully handled
    Handled,
    /// Send action - caller should handle send
    SendAction,
    /// Return an AppAction
    AppAction(notedeck::AppAction),
    /// Permission response needs relay publishing.
    PublishPermissionResponse(update::PermissionPublish),
    /// Toggle auto-steal focus mode (needs state from DaveApp)
    ToggleAutoSteal,
}

/// Handle a UI action from DaveUi.
#[allow(clippy::too_many_arguments)]
pub fn handle_ui_action(
    action: DaveAction,
    session_manager: &mut SessionManager,
    backend: &dyn crate::backend::AiBackend,
    active_overlay: &mut DaveOverlay,
    show_session_list: &mut bool,
    ctx: &egui::Context,
) -> UiActionResult {
    match action {
        DaveAction::ToggleChrome => UiActionResult::AppAction(notedeck::AppAction::ToggleChrome),
        DaveAction::Note(n) => UiActionResult::AppAction(notedeck::AppAction::Note(n)),
        DaveAction::NewChat => {
            *active_overlay = DaveOverlay::DirectoryPicker;
            UiActionResult::Handled
        }
        DaveAction::Send => UiActionResult::SendAction,
        DaveAction::ShowSessionList => {
            *show_session_list = !*show_session_list;
            UiActionResult::Handled
        }
        DaveAction::OpenSettings => {
            *active_overlay = DaveOverlay::Settings;
            UiActionResult::Handled
        }
        DaveAction::UpdateSettings(_settings) => UiActionResult::Handled,
        DaveAction::PermissionResponse {
            request_id,
            response,
        } => update::handle_permission_response(session_manager, request_id, response).map_or(
            UiActionResult::Handled,
            UiActionResult::PublishPermissionResponse,
        ),
        DaveAction::Interrupt => {
            update::execute_interrupt(session_manager, backend, ctx);
            UiActionResult::Handled
        }
        DaveAction::TentativeAccept => {
            set_tentative_state(session_manager, PermissionMessageState::TentativeAccept);
            UiActionResult::Handled
        }
        DaveAction::TentativeDeny => {
            set_tentative_state(session_manager, PermissionMessageState::TentativeDeny);
            UiActionResult::Handled
        }
        DaveAction::QuestionResponse {
            request_id,
            answers,
        } => update::handle_question_response(session_manager, request_id, answers).map_or(
            UiActionResult::Handled,
            UiActionResult::PublishPermissionResponse,
        ),
        DaveAction::TogglePlanMode => {
            update::toggle_plan_mode(session_manager, backend, ctx);
            if let Some(session) = session_manager.get_active_mut() {
                session.focus_requested = true;
            }
            UiActionResult::Handled
        }
        DaveAction::ToggleAutoSteal => UiActionResult::ToggleAutoSteal,
        DaveAction::ExitPlanMode {
            request_id,
            approved,
        } => {
            let result = if approved {
                update::exit_plan_mode(session_manager, backend, ctx);
                update::handle_permission_response(
                    session_manager,
                    request_id,
                    PermissionResponse::Allow { message: None },
                )
            } else {
                update::handle_permission_response(
                    session_manager,
                    request_id,
                    PermissionResponse::Deny {
                        reason: "User rejected plan".into(),
                    },
                )
            };
            result.map_or(
                UiActionResult::Handled,
                UiActionResult::PublishPermissionResponse,
            )
        }
        DaveAction::CompactAndApprove { request_id } => {
            update::exit_plan_mode(session_manager, backend, ctx);
            let result = update::handle_permission_response(
                session_manager,
                request_id,
                PermissionResponse::Allow {
                    message: Some("/compact".into()),
                },
            );
            if let Some(session) = session_manager.get_active_mut() {
                if let Some(agentic) = &mut session.agentic {
                    agentic.compact_and_proceed =
                        crate::session::CompactAndProceedState::WaitingForCompaction;
                }
            }
            result.map_or(
                UiActionResult::Handled,
                UiActionResult::PublishPermissionResponse,
            )
        }
    }
}
