mod ask_question;
pub mod badge;
pub mod block_nav;
mod dave;
pub mod diff;
pub mod directory_picker;
mod git_status_ui;
pub mod host_picker;
pub mod inspector;
pub mod keybind_hint;
pub mod keybindings;
pub mod markdown_ui;
mod pill;
mod query_ui;
pub(crate) mod run_config_editor;
pub(crate) mod run_ui;
pub mod scene;
pub mod session_kind_picker;
pub mod session_list;
pub mod session_picker;
mod settings;
mod square_loading_spinner;
pub mod task_list;
mod top_buttons;
pub mod worktree_creator;

pub use ask_question::{ask_user_question_summary_ui, ask_user_question_ui};
pub use block_nav::BlockNav;
pub use dave::{
    DaveAction, DaveResponse, DaveUi, InputMode, InputboxLayout, InputboxResult, Inspected,
    RunAction,
};
pub use directory_picker::{DirectoryPicker, DirectoryPickerAction};
pub use host_picker::HostPickerAction;
pub use keybind_hint::{keybind_hint, paint_keybind_hint};
pub use keybindings::check_keybindings;
pub(crate) use run_config_editor::{run_config_editor_overlay_ui, RunConfigChange};
pub use scene::{AgentScene, SceneAction, SceneResponse};
pub use session_kind_picker::SessionKindAction;
pub use session_list::{SessionListAction, SessionListUi};
pub use session_picker::{SessionPicker, SessionPickerAction};
pub use settings::{DaveSettingsPanel, SettingsPanelAction};
pub use square_loading_spinner::SquareLoadingSpinner;
pub use worktree_creator::{WorktreeCreator, WorktreeCreatorAction};

// =============================================================================
// Standalone UI Functions
// =============================================================================

use crate::agent_status::AgentStatus;
use crate::backend::{BackendType, Model};
use crate::config::{AiMode, DaveSettings, ModelConfig};
use crate::focus_queue::FocusQueue;
use crate::messages::PermissionResponse;
use crate::session::{ChatSession, PermissionMessageState, SessionId, SessionManager};
use crate::ui::keybindings::{KeyAction, NormalMode, NormalView, Pane};
use crate::update;
use crate::update::InputFocus;
use crate::DaveOverlay;
use egui::include_image;

/// Give Dave its own horizontal item gap at its render root.
///
/// The chrome hands every app a zero horizontal item gap
/// (`notedeck_chrome/src/chrome/frame.rs`, `Chrome::show`), which glues
/// adjacent labels together: "Skill" runs into its summary, a subagent's
/// description into "· in background", "computing..." into its Esc hint.
/// Dave's layout assumes a real gap, so it owns one rather than inheriting the
/// chrome's, as Headway does in its `render_board`.
pub fn own_item_spacing(ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing.x = notedeck::tokens::SPACING_SM;
}

/// Run `add_contents` in the `Ui` Dave draws in inside the app: the chrome's
/// setup from [`notedeck::test_harness::app_frame`], then Dave's own root
/// spacing, as `Dave::render` adds it. `None` on the setup frame.
///
/// Integration tests can't reach this (`cfg(test)` only, since
/// `notedeck/snapshot-testing` is a dev-dependency feature); they compose the
/// same two calls.
#[cfg(test)]
pub(crate) fn test_frame<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    notedeck::test_harness::app_frame(ui, |ui| {
        own_item_spacing(ui);
        add_contents(ui)
    })
}

/// Build a DaveUi from a session, wiring up all the common builder fields.
fn build_dave_ui<'a>(
    session: &'a mut ChatSession,
    model_config: &ModelConfig,
    auto_steal_focus: bool,
    normal_mode: Option<NormalView>,
    run_configs: &'a std::collections::HashMap<std::path::PathBuf, Vec<crate::config::RunConfig>>,
    running_sessions: &'a std::collections::HashMap<SessionId, std::collections::HashSet<String>>,
    account_rate_limit: Option<crate::messages::RateLimitInfo>,
) -> DaveUi<'a> {
    let status = session.status();
    let is_working = status == AgentStatus::Working;
    // A placeholder (remote spawn/resume in flight) reports Pending — render it as
    // "connecting…" rather than an empty chat.
    let is_connecting = session.status() == AgentStatus::Pending;
    let has_pending_permission = session.has_pending_permissions();
    let permission_mode = session.permission_mode();
    let is_remote = session.is_remote();
    let turn_has_content = session.turn_has_content();
    // Look up the run configs for this session's CWD
    let session_run_configs: &'a [crate::config::RunConfig] = session
        .cwd()
        .and_then(|cwd| run_configs.get(cwd))
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    // The IDs of configs currently running for this session
    let running_config_ids = running_sessions.get(&session.id);
    // Rate limits are the account's. A local session shows the newest report
    // from any local session; a remote one, what reached it from its host.
    let rate_limit = if is_remote {
        session.inspector.rate_limit
    } else {
        account_rate_limit.or(session.inspector.rate_limit)
    };

    if let Some(agentic) = &session.agentic {
        session.inspector.set_session_id(agentic.event_session_id());
    }

    let mut ui_builder = DaveUi::new(
        model_config.trial,
        session.id,
        &session.chat,
        &mut session.input,
        &mut session.focus_requested,
        &mut session.block_nav,
        session.ai_mode,
    )
    .is_working(is_working)
    .is_connecting(is_connecting)
    .has_pending_permission(has_pending_permission)
    .permission_mode(permission_mode)
    .auto_steal_focus(auto_steal_focus)
    .normal_mode(normal_mode)
    .is_remote(is_remote)
    .dispatch_state(session.dispatch_state)
    .turn_has_content(turn_has_content)
    .details(&session.details)
    .backend_type(session.backend_type)
    .last_activity(session.last_activity)
    .run_configs(session_run_configs)
    .running_config_ids(running_config_ids)
    .pending_images(&mut session.pending_images)
    .rate_limit(rate_limit)
    .inspector(Inspected {
        inspector: &mut session.inspector,
        view: &mut session.inspector_view,
        status,
        waiting_on: session.waiting_on,
        cli_session_id: None,
    });

    if let Some(agentic) = &mut session.agentic {
        let model = agentic
            .session_info
            .as_ref()
            .and_then(|si| si.model.as_deref());
        let is_compacting = agentic.is_compacting();
        ui_builder = ui_builder
            .permission_message_state(agentic.permission_message_state)
            .question_answers(&mut agentic.question_answers)
            .question_index(&mut agentic.question_index)
            .is_compacting(is_compacting)
            .usage(&agentic.usage, model)
            // `cli_resume_id()`, spelled out by field so it can borrow beside
            // the `&mut` fields above.
            .cli_session_id(
                agentic
                    .session_info
                    .as_ref()
                    .and_then(|info| info.claude_session_id.as_deref())
                    .or(agentic.resume_session_id.as_deref()),
            );

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
    /// Host was selected. `None` = local, `Some(hostname)` = remote.
    HostSelected(Option<String>),
    /// New-session chooser: start a local chat.
    NewSessionChat,
    /// New-session chooser: start an agentic (remote) session.
    NewSessionAgentic,
}

/// Render the settings overlay UI.
pub fn settings_overlay_ui(
    settings_panel: &mut DaveSettingsPanel,
    settings: &DaveSettings,
    i18n: &mut notedeck::Localization,
    ui: &mut egui::Ui,
) -> OverlayResult {
    if let Some(action) = settings_panel.overlay_ui(ui, settings, i18n) {
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

/// Render the host picker overlay UI.
pub fn host_picker_overlay_ui(
    local_hostname: &str,
    known_hosts: &[String],
    has_sessions: bool,
    ui: &mut egui::Ui,
) -> OverlayResult {
    if let Some(action) =
        host_picker::host_picker_overlay_ui(ui, local_hostname, known_hosts, has_sessions)
    {
        match action {
            HostPickerAction::HostSelected(host) => {
                return OverlayResult::HostSelected(host);
            }
            HostPickerAction::Cancelled => {
                return OverlayResult::Close;
            }
        }
    }
    OverlayResult::None
}

/// Render the new-session-kind chooser overlay UI.
pub fn session_kind_picker_overlay_ui(ui: &mut egui::Ui, has_sessions: bool) -> OverlayResult {
    match session_kind_picker::session_kind_picker_overlay_ui(ui, has_sessions) {
        Some(SessionKindAction::Chat) => OverlayResult::NewSessionChat,
        Some(SessionKindAction::Agentic) => OverlayResult::NewSessionAgentic,
        Some(SessionKindAction::Cancelled) => OverlayResult::Close,
        None => OverlayResult::None,
    }
}

/// Render the worktree creator overlay UI.
pub fn worktree_creator_overlay_ui(
    creator: &mut WorktreeCreator,
    ui: &mut egui::Ui,
    available_backends: &[BackendType],
) -> Option<WorktreeCreatorAction> {
    creator.overlay_ui(ui, available_backends)
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

/// Resolve the model index to a Model variant.
/// Index 0 = Default, 1+ = specific model from available_models.
fn resolve_picker_model(bt: BackendType, model_idx: usize) -> Model {
    if model_idx == 0 {
        Model::Default
    } else {
        bt.available_models()
            .into_iter()
            .nth(model_idx - 1)
            .unwrap_or(Model::Default)
    }
}

/// Render a single backend row in the picker: icon, name, model dropdown, start button.
/// Returns `Some(model)` if the user clicked Start for this backend.
fn backend_picker_row(
    bt: BackendType,
    idx: usize,
    max_width: f32,
    selected_models: &mut std::collections::HashMap<BackendType, usize>,
    ui: &mut egui::Ui,
) -> Option<Model> {
    let models = bt.available_models();
    // Index 0 = "Default", 1..=N = specific overrides
    let model_idx = selected_models.get(&bt).copied().unwrap_or(0);
    let selected_model = resolve_picker_model(bt, model_idx);
    let display_name = selected_model.display_name();

    // Backend row: icon + name + model dropdown + GO button
    let desired = egui::vec2(max_width, 52.0);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());

    // Background
    let fill = ui.visuals().widgets.inactive.weak_bg_fill;
    ui.painter().rect_filled(rect, 8.0, fill);

    // Icon
    let icon_size = 20.0;
    let icon_x = rect.left() + 12.0;
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(icon_x + icon_size / 2.0, rect.center().y - 6.0),
        egui::vec2(icon_size, icon_size),
    );
    backend_icon(bt).paint_at(ui, icon_rect);

    // Backend name + keyboard shortcut
    let label = format!("[{}] {}", idx + 1, bt.display_name());
    ui.painter().text(
        egui::pos2(icon_x + icon_size + 10.0, rect.center().y - 6.0),
        egui::Align2::LEFT_CENTER,
        &label,
        egui::FontId::proportional(15.0),
        ui.visuals().text_color(),
    );

    // Model selector + Start button on the second line
    let controls_rect = egui::Rect::from_min_size(
        egui::pos2(icon_x + icon_size + 10.0, rect.center().y + 4.0),
        egui::vec2(max_width - icon_size - 34.0, 20.0),
    );

    let mut result = None;

    ui.scope_builder(egui::UiBuilder::new().max_rect(controls_rect), |ui| {
        ui.horizontal(|ui| {
            // Model dropdown: index 0 = "Default", then each override model
            if !models.is_empty() {
                let combo_id = ui.scope_id().with("model").with(idx);
                egui::ComboBox::from_id_salt(combo_id)
                    .selected_text(egui::RichText::new(display_name).size(11.0))
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        // "Default" entry
                        if ui.selectable_label(model_idx == 0, "Default").clicked() {
                            selected_models.insert(bt, 0);
                        }
                        // Specific model overrides
                        for (mi, m) in models.iter().enumerate() {
                            if ui
                                .selectable_label(model_idx == mi + 1, m.display_name())
                                .clicked()
                            {
                                selected_models.insert(bt, mi + 1);
                            }
                        }
                    });
            }

            // Start button
            if ui
                .add(egui::Button::new(egui::RichText::new("Start").size(12.0)).corner_radius(4.0))
                .clicked()
            {
                result = Some(resolve_picker_model(bt, model_idx));
            }
        });
    });

    ui.add_space(6.0);

    result
}

/// A backend and model picked in the backend picker.
#[derive(Clone, Debug)]
pub struct BackendChoice {
    /// The picked backend.
    pub backend: BackendType,
    /// The model picked for it.
    pub model: Model,
}

/// Render the backend picker overlay UI.
/// Returns `Some` when the user has selected a backend.
pub fn backend_picker_overlay_ui(
    available_backends: &[BackendType],
    selected_models: &mut std::collections::HashMap<BackendType, usize>,
    ui: &mut egui::Ui,
) -> Option<BackendChoice> {
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
            let model_idx = selected_models.get(&bt).copied().unwrap_or(0);
            return Some(BackendChoice {
                backend: bt,
                model: resolve_picker_model(bt, model_idx),
            });
        }
    }

    let is_narrow = notedeck::ui::is_narrow(ui.ctx());
    let mut selected = None;

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
                        if let Some(model) =
                            backend_picker_row(bt, idx, max_width, selected_models, ui)
                        {
                            selected = Some(BackendChoice { backend: bt, model });
                        }
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
    /// The user selected a session node in the scene. Already switched to
    /// inline; surfaced so the app can anchor auto-steal focus to it.
    SelectedSession(SessionId),
}

/// What [`scene_ui`] drew: the chat panel's response, and what the scene
/// asks of the app.
pub struct SceneUiResponse {
    /// The chat side panel's response.
    pub response: DaveResponse,
    /// What the scene asks of the app.
    pub action: SceneViewAction,
}

/// What a session-list layout ([`desktop_ui`], [`narrow_ui`]) drew.
pub struct ListUiResponse {
    /// The chat's response.
    pub response: DaveResponse,
    /// What the session list asks of the app, if anything.
    pub session_action: Option<SessionListAction>,
    /// Whether the user asked for the scene view. Only the desktop layout
    /// offers it.
    pub toggle_scene: bool,
}

/// Render the scene view with RTS-style agent visualization and chat side panel.
#[allow(clippy::too_many_arguments)]
pub fn scene_ui(
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    focus_queue: &mut FocusQueue,
    model_config: &ModelConfig,
    auto_steal_focus: bool,
    normal_mode: Option<NormalView>,
    run_configs: &std::collections::HashMap<std::path::PathBuf, Vec<crate::config::RunConfig>>,
    running_sessions: &std::collections::HashMap<SessionId, std::collections::HashSet<String>>,
    account_rate_limit: Option<crate::messages::RateLimitInfo>,
    app_ctx: &mut notedeck::AppContext,
    ui: &mut egui::Ui,
) -> SceneUiResponse {
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
                                    auto_steal_focus,
                                    normal_mode,
                                    run_configs,
                                    running_sessions,
                                    account_rate_limit,
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
                        view_action = SceneViewAction::SelectedSession(*id);
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
                            agentic.scene_position = (position.x, position.y);
                        }
                    }
                }
            }
        }
    }

    SceneUiResponse {
        response: dave_response,
        action: view_action,
    }
}

/// Desktop layout with sidebar for session list.
#[allow(clippy::too_many_arguments)]
pub fn desktop_ui(
    session_manager: &mut SessionManager,
    focus_queue: &FocusQueue,
    collapse_state: &crate::collapse_state::CollapseState,
    model_config: &ModelConfig,
    auto_steal_focus: bool,
    normal_mode: Option<NormalView>,
    run_configs: &std::collections::HashMap<std::path::PathBuf, Vec<crate::config::RunConfig>>,
    running_sessions: &std::collections::HashMap<SessionId, std::collections::HashSet<String>>,
    account_rate_limit: Option<crate::messages::RateLimitInfo>,
    app_ctx: &mut notedeck::AppContext,
    ui: &mut egui::Ui,
) -> ListUiResponse {
    use crate::ui::task_list::TaskListPanel;

    let available = ui.available_rect_before_wrap();
    let sidebar_width = if available.width() < 830.0 {
        200.0
    } else {
        280.0
    };
    let task_panel_width = 200.0;
    let ctrl_held = ui.input(|i| i.modifiers.ctrl);
    let mut toggle_scene = false;

    // Show the task panel when the active session has any tasks.
    let show_task_panel = session_manager
        .get_active()
        .map(|s| TaskListPanel::has_tasks(&s.chat))
        .unwrap_or(false);
    let right_panel_width = if show_task_panel {
        task_panel_width
    } else {
        0.0
    };

    let sidebar_rect =
        egui::Rect::from_min_size(available.min, egui::vec2(sidebar_width, available.height()));
    let chat_rect = egui::Rect::from_min_size(
        egui::pos2(available.min.x + sidebar_width, available.min.y),
        egui::vec2(
            available.width() - sidebar_width - right_panel_width,
            available.height(),
        ),
    );
    let task_panel_rect = egui::Rect::from_min_size(
        egui::pos2(available.max.x - task_panel_width, available.min.y),
        egui::vec2(task_panel_width, available.height()),
    );

    let session_action = ui
        .scope_builder(egui::UiBuilder::new().max_rect(sidebar_rect), |ui| {
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
                    SessionListUi::new(session_manager, focus_queue, collapse_state, ctrl_held)
                        .chord_cursor(normal_mode.is_some_and(|view| view.pane == Pane::Sessions))
                        .ui(ui)
                })
                .inner
        })
        .inner;

    // Render the task panel (immutable borrow) before the mutable chat borrow.
    if show_task_panel {
        if let Some(session) = session_manager.get_active() {
            let chat = &session.chat;
            ui.scope_builder(egui::UiBuilder::new().max_rect(task_panel_rect), |ui| {
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .inner_margin(egui::Margin::symmetric(8, 12))
                    .show(ui, |ui| {
                        TaskListPanel::ui(ui, chat);
                    });
            });
        }
    }

    let chat_response = ui
        .scope_builder(egui::UiBuilder::new().max_rect(chat_rect), |ui| {
            if let Some(session) = session_manager.get_active_mut() {
                build_dave_ui(
                    session,
                    model_config,
                    auto_steal_focus,
                    normal_mode,
                    run_configs,
                    running_sessions,
                    account_rate_limit,
                )
                .ui(app_ctx, ui)
            } else {
                DaveResponse::default()
            }
        })
        .inner;

    ListUiResponse {
        response: chat_response,
        session_action,
        toggle_scene,
    }
}

/// Narrow/mobile layout - shows either session list or chat.
#[allow(clippy::too_many_arguments)]
pub fn narrow_ui(
    session_manager: &mut SessionManager,
    focus_queue: &FocusQueue,
    collapse_state: &crate::collapse_state::CollapseState,
    model_config: &ModelConfig,
    auto_steal_focus: bool,
    normal_mode: Option<NormalView>,
    run_configs: &std::collections::HashMap<std::path::PathBuf, Vec<crate::config::RunConfig>>,
    running_sessions: &std::collections::HashMap<SessionId, std::collections::HashSet<String>>,
    account_rate_limit: Option<crate::messages::RateLimitInfo>,
    show_session_list: bool,
    app_ctx: &mut notedeck::AppContext,
    ui: &mut egui::Ui,
) -> ListUiResponse {
    if show_session_list {
        let ctrl_held = ui.input(|i| i.modifiers.ctrl);
        let session_action = egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::symmetric(8, 12))
            .show(ui, |ui| {
                SessionListUi::new(session_manager, focus_queue, collapse_state, ctrl_held).ui(ui)
            })
            .inner;
        ListUiResponse {
            response: DaveResponse::default(),
            session_action,
            toggle_scene: false,
        }
    } else if let Some(session) = session_manager.get_active_mut() {
        let dot_color = focus_queue.current().map(|e| e.priority.color());
        let fq_info = focus_queue.ui_info();
        let response = build_dave_ui(
            session,
            model_config,
            auto_steal_focus,
            normal_mode,
            run_configs,
            running_sessions,
            account_rate_limit,
        )
        .status_dot_color(dot_color)
        .focus_queue_info(fq_info)
        .ui(app_ctx, ui);
        ListUiResponse {
            response,
            session_action: None,
            toggle_scene: false,
        }
    } else {
        ListUiResponse {
            response: DaveResponse::default(),
            session_action: None,
            toggle_scene: false,
        }
    }
}

/// Result from handling a key action
pub enum KeyActionResult {
    None,
    ToggleView,
    CloneAgent,
    NewAgent,
    DeleteSession(SessionId),
    ClearAgent,
    SetAutoSteal(bool),
    /// Permission response needs relay publishing.
    PublishPermissionResponse(update::PermissionPublish),
    /// Permission mode command needs relay publishing (observer → host).
    PublishModeCommand(update::ModeCommandPublish),
    /// Interrupt command needs relay publishing (observer → host).
    PublishInterruptCommand(update::InterruptPublish),
    /// Open this URI (the active session's issue) through chrome, which routes
    /// a `headway:` ref to its card.
    OpenUri(String),
}

/// Run a block-cursor action against the active chat's [`BlockNav`].
///
/// Deliberately leaves `focus_requested` alone, unlike most key actions:
/// pulling focus back into the input would end normal mode mid-stride.
fn with_block_nav(
    session_manager: &mut SessionManager,
    f: impl FnOnce(&mut BlockNav),
) -> KeyActionResult {
    if let Some(session) = session_manager.get_active_mut() {
        f(&mut session.block_nav);
    }
    KeyActionResult::None
}

/// Handle a keybinding action.
#[allow(clippy::too_many_arguments)]
pub fn handle_key_action(
    key_action: KeyAction,
    session_manager: &mut SessionManager,
    scene: &mut AgentScene,
    focus_queue: &mut FocusQueue,
    collapse_state: &crate::collapse_state::CollapseState,
    backend: &dyn crate::backend::AiBackend,
    show_scene: bool,
    auto_steal_focus: bool,
    home_session: &mut Option<SessionId>,
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
        KeyAction::AllowAlways => {
            if let Some(request_id) = update::first_pending_permission(session_manager) {
                update::allow_always(session_manager, request_id);
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
        KeyAction::TentativeAllowAlways => {
            set_tentative_state(
                session_manager,
                PermissionMessageState::TentativeAllowAlways,
            );
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
            update::switch_to_agent_by_index(
                session_manager,
                collapse_state,
                scene,
                show_scene,
                index,
                InputFocus::Request,
            );
            KeyActionResult::None
        }
        KeyAction::NextAgent => {
            update::cycle_next_agent(
                session_manager,
                collapse_state,
                scene,
                show_scene,
                InputFocus::Request,
            );
            KeyActionResult::None
        }
        KeyAction::PreviousAgent => {
            update::cycle_prev_agent(
                session_manager,
                collapse_state,
                scene,
                show_scene,
                InputFocus::Request,
            );
            KeyActionResult::None
        }
        // The session-list motions of normal mode: the same moves as
        // Ctrl+J / Ctrl+K, minus the focus grab, since normal mode still holds
        // the keyboard. It hands focus to the new session's input when it ends
        // (`settle_normal_mode_focus`).
        KeyAction::SessionPaneNext => {
            update::cycle_next_agent(
                session_manager,
                collapse_state,
                scene,
                show_scene,
                InputFocus::Leave,
            );
            KeyActionResult::None
        }
        KeyAction::SessionPanePrev => {
            update::cycle_prev_agent(
                session_manager,
                collapse_state,
                scene,
                show_scene,
                InputFocus::Leave,
            );
            KeyActionResult::None
        }
        KeyAction::SessionPaneFirst => {
            update::switch_to_agent_by_index(
                session_manager,
                collapse_state,
                scene,
                show_scene,
                0,
                InputFocus::Leave,
            );
            KeyActionResult::None
        }
        KeyAction::SessionPaneLast => {
            update::switch_to_last_agent(
                session_manager,
                collapse_state,
                scene,
                show_scene,
                InputFocus::Leave,
            );
            KeyActionResult::None
        }
        // The pane and the mode live on NormalMode; there is nothing to
        // dispatch.
        KeyAction::FocusSessionsPane | KeyAction::FocusChatPane | KeyAction::InsertMode => {
            KeyActionResult::None
        }
        KeyAction::NewAgent => KeyActionResult::NewAgent,
        KeyAction::CloneAgent => KeyActionResult::CloneAgent,
        // The same path as the Stop button's `DaveAction::Interrupt`.
        KeyAction::Interrupt => match update::execute_interrupt(session_manager, backend, ctx) {
            Some(cmd) => KeyActionResult::PublishInterruptCommand(cmd),
            None => KeyActionResult::None,
        },
        KeyAction::OpenIssue => session_manager
            .get_active()
            .and_then(|s| s.details.issue_url.clone())
            .map_or(KeyActionResult::None, KeyActionResult::OpenUri),
        KeyAction::ToggleView => KeyActionResult::ToggleView,
        KeyAction::ShowDetails => {
            if let Some(session) = session_manager.get_active_mut() {
                session.inspector_view.show_details();
            }
            KeyActionResult::None
        }
        KeyAction::ShowChat => {
            if let Some(session) = session_manager.get_active_mut() {
                session.inspector_view.show_chat();
            }
            KeyActionResult::None
        }
        KeyAction::CyclePermissionMode => {
            let publish = update::cycle_permission_mode(session_manager, backend, ctx);
            if let Some(session) = session_manager.get_active_mut() {
                session.focus_requested = true;
            }
            match publish {
                Some(cmd) => KeyActionResult::PublishModeCommand(cmd),
                None => KeyActionResult::None,
            }
        }
        KeyAction::DeleteActiveSession => {
            if let Some(id) = session_manager.active_id() {
                KeyActionResult::DeleteSession(id)
            } else {
                KeyActionResult::None
            }
        }
        KeyAction::ClearAgent => KeyActionResult::ClearAgent,
        KeyAction::BlockCursorDown => with_block_nav(session_manager, BlockNav::down),
        KeyAction::BlockCursorUp => with_block_nav(session_manager, BlockNav::up),
        KeyAction::BlockCursorFirst => with_block_nav(session_manager, BlockNav::first),
        KeyAction::BlockCursorLast => with_block_nav(session_manager, BlockNav::last),
        KeyAction::BlockCursorClear => with_block_nav(session_manager, BlockNav::clear_cursor),
        KeyAction::BlockToggle => with_block_nav(session_manager, |nav| nav.toggle_cursor(ctx)),
        KeyAction::BlockOpen => with_block_nav(session_manager, |nav| nav.open_cursor(ctx)),
        KeyAction::BlockClose => with_block_nav(session_manager, |nav| nav.close_cursor(ctx)),
        KeyAction::BlockExpandAll => with_block_nav(session_manager, |nav| nav.expand_all(ctx)),
        KeyAction::BlockCollapseAll => with_block_nav(session_manager, |nav| nav.collapse_all(ctx)),
        KeyAction::RenameAgent => {
            if let Some(id) = session_manager.active_id() {
                if let Some(session) = session_manager.get(id) {
                    let rename_id = egui::Id::unique("session_rename_state");
                    let rename_state = (id, session.details.display_title().to_string());
                    ctx.data_mut(|d| d.insert_temp(rename_id, rename_state));
                }
            }
            KeyActionResult::None
        }
        KeyAction::FocusQueueNext => {
            update::focus_queue_next(
                session_manager,
                focus_queue,
                collapse_state,
                scene,
                show_scene,
            );
            KeyActionResult::None
        }
        KeyAction::FocusQueuePrev => {
            update::focus_queue_prev(
                session_manager,
                focus_queue,
                collapse_state,
                scene,
                show_scene,
            );
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
        KeyAction::OpenTerminal => {
            dispatch_open_terminal(session_manager, update::open_terminal);
            KeyActionResult::None
        }
    }
}

/// Keep normal mode in charge of keyboard focus, and hand focus to the active
/// input when it ends. Runs every frame, after the keybindings are dispatched.
///
/// While normal mode is on, an action's request to focus the active input (a
/// focus-queue jump, cycling the permission mode) is dropped: focusing the
/// input mid-mode would put it back under the bare keys, and the input is
/// focused anyway when normal mode ends — whichever session is active then.
pub fn settle_normal_mode_focus(
    normal_mode: &mut NormalMode,
    session_manager: &mut SessionManager,
) {
    if normal_mode.is_normal() {
        if let Some(session) = session_manager.get_active_mut() {
            session.focus_requested = false;
        }
        return;
    }

    if !normal_mode.take_input_focus() {
        return;
    }
    if let Some(id) = session_manager.active_id() {
        update::request_input_focus(session_manager, id);
    }
}

fn dispatch_open_terminal(
    session_manager: &SessionManager,
    mut open_terminal: impl FnMut(&std::path::Path),
) {
    if let Some(session) = session_manager.get_active() {
        if session.is_remote() {
            return;
        }
        if let Some(cwd) = session.cwd() {
            open_terminal(cwd);
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
        PermissionMessageState::TentativeAccept | PermissionMessageState::TentativeAllowAlways => {
            let is_exit_plan_mode = update::has_pending_exit_plan_mode(session_manager);
            if let Some(request_id) = update::first_pending_permission(session_manager) {
                // The grant lands with the answer, never before it: see
                // `PermissionMessageState::TentativeAllowAlways`.
                if tentative_state == PermissionMessageState::TentativeAllowAlways {
                    update::allow_always(session_manager, request_id);
                }
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
                    .unwrap_or_else(|| crate::messages::DEFAULT_DENY_REASON.into());
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
    /// New chat requested — caller routes through handle_new_chat()
    NewChat,
    /// Trigger manual context compaction
    Compact,
    /// Permission mode command needs relay publishing (observer → host).
    PublishModeCommand(update::ModeCommandPublish),
    /// Interrupt command needs relay publishing (observer → host).
    PublishInterruptCommand(update::InterruptPublish),
    /// Navigate to next focus queue item (mobile)
    FocusQueueNext,
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
        DaveAction::NewChat => UiActionResult::NewChat,
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
        DaveAction::Interrupt => match update::execute_interrupt(session_manager, backend, ctx) {
            Some(cmd) => UiActionResult::PublishInterruptCommand(cmd),
            None => UiActionResult::Handled,
        },
        DaveAction::ExitToolCall { request_id } => {
            update::exit_tool_call(session_manager, request_id).map_or(
                UiActionResult::Handled,
                UiActionResult::PublishPermissionResponse,
            )
        }
        DaveAction::TentativeAccept => {
            set_tentative_state(session_manager, PermissionMessageState::TentativeAccept);
            UiActionResult::Handled
        }
        DaveAction::TentativeDeny => {
            set_tentative_state(session_manager, PermissionMessageState::TentativeDeny);
            UiActionResult::Handled
        }
        DaveAction::AllowAlways { request_id } => {
            update::allow_always(session_manager, request_id);
            update::handle_permission_response(
                session_manager,
                request_id,
                PermissionResponse::Allow { message: None },
            )
            .map_or(
                UiActionResult::Handled,
                UiActionResult::PublishPermissionResponse,
            )
        }
        DaveAction::TentativeAllowAlways => {
            set_tentative_state(
                session_manager,
                PermissionMessageState::TentativeAllowAlways,
            );
            UiActionResult::Handled
        }
        DaveAction::QuestionResponse {
            request_id,
            answers,
        } => update::handle_question_response(session_manager, request_id, answers).map_or(
            UiActionResult::Handled,
            UiActionResult::PublishPermissionResponse,
        ),
        DaveAction::CyclePermissionMode => {
            let publish = update::cycle_permission_mode(session_manager, backend, ctx);
            if let Some(session) = session_manager.get_active_mut() {
                session.focus_requested = true;
            }
            match publish {
                Some(cmd) => UiActionResult::PublishModeCommand(cmd),
                None => UiActionResult::Handled,
            }
        }
        DaveAction::SetPermissionMode(mode) => {
            let publish = update::set_permission_mode(session_manager, backend, mode, ctx);
            if let Some(session) = session_manager.get_active_mut() {
                session.focus_requested = true;
            }
            match publish {
                Some(cmd) => UiActionResult::PublishModeCommand(cmd),
                None => UiActionResult::Handled,
            }
        }
        DaveAction::ToggleAutoSteal => UiActionResult::ToggleAutoSteal,
        DaveAction::FocusQueueNext => UiActionResult::FocusQueueNext,
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
                PermissionResponse::Allow { message: None },
            );
            if let Some(session) = session_manager.get_active_mut() {
                if let Some(agentic) = &mut session.agentic {
                    agentic.compact_intent =
                        Some(crate::session::CompactIntent::ProceedAfterStreamEnd);
                }
            }
            result.map_or(
                UiActionResult::Handled,
                UiActionResult::PublishPermissionResponse,
            )
        }
        DaveAction::Compact => UiActionResult::Compact,
        DaveAction::OpenInspector(section) => {
            if let Some(session) = session_manager.get_active_mut() {
                session.inspector_view.open(section);
            }
            UiActionResult::Handled
        }
        DaveAction::ShowChat => {
            if let Some(session) = session_manager.get_active_mut() {
                session.inspector_view.show_chat();
                session.focus_requested = true;
            }
            UiActionResult::Handled
        }
        DaveAction::ShowSubagent(task_id) => {
            if let Some(session) = session_manager.get_active_mut() {
                session.inspector_view.show_chat();
                session.block_nav.seek(dave::subagent_block_id(&task_id));
            }
            UiActionResult::Handled
        }
        DaveAction::OpenPath(path) => {
            // xdg-open can wait on the app it launches, so off the UI thread.
            std::thread::spawn(move || {
                if let Err(err) = opener::open(&path) {
                    tracing::warn!("failed to open {}: {err}", path.display());
                }
            });
            UiActionResult::Handled
        }
        // All run actions are intercepted and handled in lib.rs before reaching here
        DaveAction::Run(_) => UiActionResult::Handled,
    }
}

#[cfg(test)]
mod tests {
    use super::dave::input_id;
    use super::{
        dispatch_open_terminal, handle_key_action, settle_normal_mode_focus, KeyActionResult,
    };
    use crate::backend::RemoteOnlyBackend;
    use crate::collapse_state::CollapseState;
    use crate::config::AiMode;
    use crate::focus_queue::FocusQueue;
    use crate::session::PermissionMessageState;
    use crate::session::{SessionId, SessionManager};
    use crate::ui::keybindings::{
        check_keybindings, ActiveOffers, KeyAction, KeyContext, NormalMode,
    };
    use crate::ui::AgentScene;
    use crate::ui_tools::chrome_keys::ChromeKeys;
    use crate::update;
    use egui::{Key, Modifiers};
    use egui_kittest::Harness;
    use notedeck::test_harness::PressKey;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    /// Everything a key action dispatches against, with two agentic sessions
    /// in separate directories and the first one active.
    struct Dispatch {
        session_manager: SessionManager,
        scene: AgentScene,
        focus_queue: FocusQueue,
        collapse_state: CollapseState,
        normal_mode: NormalMode,
        /// Chrome's keymap, ahead of Dave's keybindings.
        chrome: ChromeKeys,
        sessions: [SessionId; 2],
    }

    impl Dispatch {
        fn new() -> Self {
            let mut session_manager = SessionManager::new();
            let sessions = ["/tmp/chord-a", "/tmp/chord-b"].map(|cwd| {
                session_manager.new_session(
                    PathBuf::from(cwd),
                    AiMode::Agentic,
                    crate::backend::BackendType::Claude,
                )
            });
            let [first, _] = sessions;
            session_manager.switch_to(first);
            for id in sessions {
                session_manager.get_mut(id).unwrap().focus_requested = false;
            }
            Dispatch {
                session_manager,
                scene: AgentScene::new(),
                focus_queue: FocusQueue::new(),
                collapse_state: CollapseState::new(),
                normal_mode: NormalMode::default(),
                chrome: ChromeKeys::new(),
                sessions,
            }
        }

        fn dispatch(&mut self, action: KeyAction, ctx: &egui::Context) -> KeyActionResult {
            handle_key_action(
                action,
                &mut self.session_manager,
                &mut self.scene,
                &mut self.focus_queue,
                &self.collapse_state,
                &RemoteOnlyBackend,
                false,
                false,
                &mut None,
                ctx,
            )
        }

        fn focus_requested(&self, id: SessionId) -> bool {
            self.session_manager.get(id).unwrap().focus_requested
        }
    }

    /// `s` hands chrome the active session's issue URI verbatim, and does
    /// nothing for a session spawned without one.
    #[test]
    fn open_issue_opens_the_active_sessions_issue() {
        let mut d = Dispatch::new();
        let ctx = egui::Context::default();
        assert!(matches!(
            d.dispatch(KeyAction::OpenIssue, &ctx),
            KeyActionResult::None
        ));

        let [first, _] = d.sessions;
        d.session_manager.get_mut(first).unwrap().details.issue_url =
            Some("headway:dave/receive-east-neutral".to_string());
        assert!(matches!(
            d.dispatch(KeyAction::OpenIssue, &ctx),
            KeyActionResult::OpenUri(uri) if uri == "headway:dave/receive-east-neutral"
        ));
    }

    #[test]
    fn a_sessions_pane_move_switches_without_grabbing_focus() {
        let mut d = Dispatch::new();
        let [first, second] = d.sessions;
        d.dispatch(KeyAction::SessionPaneNext, &egui::Context::default());

        assert_eq!(d.session_manager.active_id(), Some(second));
        assert!(!d.focus_requested(first));
        assert!(
            !d.focus_requested(second),
            "normal mode still holds the keyboard"
        );

        // Ctrl+J, the same move outside normal mode, does take focus.
        d.dispatch(KeyAction::NextAgent, &egui::Context::default());
        assert_eq!(d.session_manager.active_id(), Some(first));
        assert!(d.focus_requested(first));
    }

    /// Dave's frame, cut down: chrome's keymap and the tool it calls, then
    /// the keybindings and their dispatch, then a stand-in for the active
    /// session's chat input that takes focus when asked, as
    /// `DaveUi::inputbox` does.
    fn normal_mode_frame(ui: &mut egui::Ui, d: &mut Dispatch) {
        let offers = ActiveOffers {
            ai_mode: AiMode::Agentic,
            interruptible: false,
            has_issue: false,
        };
        if let Some(action) = d.chrome.frame(ui.ctx(), &mut d.normal_mode, offers, false) {
            d.dispatch(action, ui.ctx());
        }
        let in_tentative_state = d
            .session_manager
            .get_active()
            .and_then(|s| s.agentic.as_ref())
            .is_some_and(|a| a.permission_message_state != PermissionMessageState::None);
        let keys = KeyContext {
            ai_mode: AiMode::Agentic,
            sessions_shown: true,
            has_pending_permission: update::first_pending_permission(&d.session_manager).is_some(),
            has_pending_question: false,
            in_tentative_state,
            overlay_open: false,
            renaming: false,
        };
        if let Some(action) = check_keybindings(ui.ctx(), &mut d.normal_mode, keys) {
            d.dispatch(action, ui.ctx());
        }
        settle_normal_mode_focus(&mut d.normal_mode, &mut d.session_manager);

        let Some(session) = d.session_manager.get_active_mut() else {
            return;
        };
        let input = ui
            .add(egui::TextEdit::singleline(&mut session.input).id(input_id(session.id)))
            .accessible_name("test field");
        if std::mem::take(&mut session.focus_requested) {
            input.request_focus();
        }
    }

    /// Esc, then `!` on a pending permission: the answer waits for a message,
    /// so the input takes focus and its letters type (`dd` must not delete
    /// the session). Esc then cancels the answer.
    #[test]
    fn a_tentative_answer_from_normal_mode_takes_its_message_in_the_input() {
        let mut harness = Harness::new_ui_state(normal_mode_frame, Dispatch::new());
        let [first, _] = harness.state().sessions;
        let (sender, _receiver) = tokio::sync::oneshot::channel();
        let agentic = harness
            .state_mut()
            .session_manager
            .get_mut(first)
            .and_then(|s| s.agentic.as_mut())
            .unwrap();
        agentic
            .permissions
            .pending
            .insert(uuid::Uuid::new_v4(), sender);
        harness.run();

        harness.press_key_modifiers(Modifiers::NONE, Key::Escape);
        harness.press_key_modifiers(Modifiers::NONE, Key::Exclamationmark);
        harness.run();
        let tentative = |d: &Dispatch, want: PermissionMessageState| {
            d.session_manager
                .get(first)
                .and_then(|s| s.agentic.as_ref())
                .is_some_and(|a| a.permission_message_state == want)
        };
        assert!(tentative(
            harness.state(),
            PermissionMessageState::TentativeAccept
        ));
        assert_eq!(harness.ctx.memory(|m| m.focused()), Some(input_id(first)));

        for _ in 0..2 {
            harness
                .input_mut()
                .events
                .push(egui::Event::Text("d".to_owned()));
            harness.press_key_modifiers(Modifiers::NONE, Key::D);
        }
        assert_eq!(
            harness.state().session_manager.get(first).unwrap().input,
            "dd"
        );

        harness.press_key_modifiers(Modifiers::NONE, Key::Escape);
        assert!(
            tentative(harness.state(), PermissionMessageState::None),
            "Esc cancels the answer rather than toggling the side menu"
        );
    }

    #[test]
    fn leaving_normal_mode_after_a_session_switch_focuses_the_new_input() {
        let mut harness = Harness::new_ui_state(normal_mode_frame, Dispatch::new());
        let [first, second] = harness.state().sessions;
        harness.ctx.memory_mut(|m| m.request_focus(input_id(first)));
        harness.run();

        harness.press_key_modifiers(Modifiers::NONE, Key::Escape);
        harness.press_key_modifiers(Modifiers::NONE, Key::H);
        harness.press_key_modifiers(Modifiers::NONE, Key::J);
        assert_eq!(harness.state().session_manager.active_id(), Some(second));
        assert_eq!(
            harness.ctx.memory(|m| m.focused()),
            None,
            "in normal mode, the new session's input stays unfocused"
        );

        harness.press_key_modifiers(Modifiers::NONE, Key::I);
        harness.run();
        assert_eq!(harness.ctx.memory(|m| m.focused()), Some(input_id(second)));
    }

    /// egui drops the input's focus on Esc before Dave reads the key, so
    /// normal mode has nothing to hand back: `i` focuses the active input.
    #[test]
    fn escape_then_i_puts_focus_back_on_the_input() {
        let mut harness = Harness::new_ui_state(normal_mode_frame, Dispatch::new());
        let [first, _] = harness.state().sessions;
        let input = input_id(first);
        harness.ctx.memory_mut(|m| m.request_focus(input));
        harness.run();

        harness.press_key_modifiers(Modifiers::NONE, Key::Escape);
        assert_eq!(harness.ctx.memory(|m| m.focused()), None);

        harness.press_key_modifiers(Modifiers::NONE, Key::I);
        harness.run();
        assert_eq!(harness.ctx.memory(|m| m.focused()), Some(input));
    }

    #[test]
    fn a_focus_request_in_normal_mode_waits_for_it_to_end() {
        let mut harness = Harness::new_ui_state(normal_mode_frame, Dispatch::new());
        let [first, second] = harness.state().sessions;
        harness.run();

        harness.press_key_modifiers(Modifiers::NONE, Key::Escape);
        // `]q` jumps through the focus queue with a plain focus request.
        harness
            .state_mut()
            .focus_queue
            .enqueue(second, crate::focus_queue::FocusPriority::NeedsInput);
        harness.press_key_modifiers(Modifiers::NONE, Key::CloseBracket);
        harness.press_key_modifiers(Modifiers::NONE, Key::Q);
        assert_eq!(harness.state().session_manager.active_id(), Some(second));
        assert_eq!(harness.ctx.memory(|m| m.focused()), None);
        assert!(!harness.state().focus_requested(first));

        harness.press_key_modifiers(Modifiers::NONE, Key::I);
        harness.run();
        assert_eq!(harness.ctx.memory(|m| m.focused()), Some(input_id(second)));
    }

    #[test]
    fn open_terminal_key_action_dispatches_active_session_cwd() {
        let mut session_manager = SessionManager::new();
        let expected_cwd = PathBuf::from("/tmp/terminal-cwd");
        let session_id = session_manager.new_session(
            expected_cwd.clone(),
            AiMode::Agentic,
            crate::backend::BackendType::Claude,
        );
        session_manager.switch_to(session_id);

        let launched_cwd = RefCell::new(None::<PathBuf>);
        dispatch_open_terminal(&session_manager, |cwd: &Path| {
            launched_cwd.replace(Some(cwd.to_path_buf()));
        });

        assert_eq!(launched_cwd.into_inner(), Some(expected_cwd));
    }

    #[test]
    fn open_terminal_key_action_ignores_remote_sessions() {
        let mut session_manager = SessionManager::new();
        let session_id = session_manager.new_session(
            PathBuf::from("/tmp/remote-cwd"),
            AiMode::Agentic,
            crate::backend::BackendType::Claude,
        );
        let session = session_manager
            .get_mut(session_id)
            .expect("remote test session should exist");
        session.source = crate::session::SessionSource::Remote;
        session_manager.switch_to(session_id);

        let launched_cwd = RefCell::new(None::<PathBuf>);
        dispatch_open_terminal(&session_manager, |cwd: &Path| {
            launched_cwd.replace(Some(cwd.to_path_buf()));
        });

        assert!(launched_cwd.into_inner().is_none());
    }

    /// `gd` opens the active session's Details tab; the other session keeps its
    /// chat, and coming back finds Details still open, until `gc`.
    #[test]
    fn session_tab_survives_switching_sessions() {
        use crate::inspector::SessionTab;

        let mut d = Dispatch::new();
        let ctx = egui::Context::default();
        let [first, second] = d.sessions;
        let tab = |d: &Dispatch, id| d.session_manager.get(id).unwrap().inspector_view.tab;

        d.dispatch(KeyAction::ShowDetails, &ctx);
        assert_eq!(tab(&d, first), SessionTab::Details);

        d.dispatch(KeyAction::SessionPaneNext, &ctx);
        assert_eq!(d.session_manager.active_id(), Some(second));
        assert_eq!(tab(&d, second), SessionTab::Chat);

        d.dispatch(KeyAction::SessionPanePrev, &ctx);
        assert_eq!(d.session_manager.active_id(), Some(first));
        assert_eq!(tab(&d, first), SessionTab::Details);

        d.dispatch(KeyAction::ShowChat, &ctx);
        assert_eq!(tab(&d, first), SessionTab::Chat);
    }

    /// A Dave session view rendered with a real `AppContext`, with the UI's
    /// actions fed back through [`super::handle_ui_action`] as the app does.
    struct SessionViewState {
        notedeck: notedeck::Notedeck,
        _tmpdir: tempfile::TempDir,
        session_manager: SessionManager,
        model_config: crate::config::ModelConfig,
        set_up: bool,
        /// The account's rate limits, as `Dave` would hand them in.
        rate_limit: Option<crate::messages::RateLimitInfo>,
        /// The clock the Details tab reads, pinned for snapshots.
        now_unix: Option<u64>,
    }

    fn session_view_state() -> SessionViewState {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let ctx = egui::Context::default();
        // `--testrunner` gives the account no relays, so nothing connects.
        let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
        let notedeck = notedeck::Notedeck::init(&ctx, tmpdir.path(), &args);
        let mut session_manager = SessionManager::new();
        let id = session_manager.new_session(
            PathBuf::from("/tmp/inspector"),
            AiMode::Agentic,
            crate::backend::BackendType::Claude,
        );
        session_manager.switch_to(id);
        // A fixed d-tag, so the Overview's `agentium:` ref is the same in
        // every snapshot.
        session_manager
            .get_mut(id)
            .and_then(|session| session.agentic.as_mut())
            .unwrap()
            .event_id = "inspector-test-session".into();
        SessionViewState {
            notedeck,
            _tmpdir: tmpdir,
            session_manager,
            model_config: crate::config::ModelConfig::default(),
            set_up: false,
            rate_limit: None,
            now_unix: None,
        }
    }

    fn render_session_view(ui: &mut egui::Ui, state: &mut SessionViewState) {
        notedeck::test_harness::full_window(ui, |ui| {
            if !state.set_up {
                state.notedeck.setup(ui.ctx());
                state.set_up = true;
            }
            // The app's root panel paints this; a bare harness doesn't.
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);
            let no_configs = std::collections::HashMap::new();
            let none_running = std::collections::HashMap::new();
            let mut app_ctx = state.notedeck.app_context();
            let Some(session) = state.session_manager.get_active_mut() else {
                return;
            };
            let mut dave_ui = super::build_dave_ui(
                session,
                &state.model_config,
                false,
                None,
                &no_configs,
                &none_running,
                state.rate_limit,
            );
            if let Some(now) = state.now_unix {
                dave_ui = dave_ui.now_unix(now);
            }
            let response = dave_ui.ui(&mut app_ctx, ui);
            if let Some(action) = response.action {
                super::handle_ui_action(
                    action,
                    &mut state.session_manager,
                    &RemoteOnlyBackend,
                    &mut crate::DaveOverlay::None,
                    &mut false,
                    ui.ctx(),
                );
            }
        });
    }

    fn session_view_harness(size: egui::Vec2) -> Harness<'static, SessionViewState> {
        notedeck::test_harness::lenient_builder()
            .with_size(size)
            .build_ui_state(render_session_view, session_view_state())
    }

    /// The number of nodes labelled `label`.
    fn count_labels(harness: &Harness<'static, SessionViewState>, label: &str) -> usize {
        use egui_kittest::kittest::Queryable;
        harness.query_all_by_label(label).count()
    }

    /// Clicking Details swaps the chat for the rail and the section cards
    /// (the two without a view yet are stubs); clicking Chat brings the
    /// input box back.
    #[test]
    fn details_tab_shows_every_section_and_chat_brings_the_input_back() {
        use crate::inspector::InspectorSection;
        use egui_kittest::kittest::Queryable;

        let mut harness = session_view_harness(egui::vec2(1100.0, 900.0));
        harness.run_ok();
        assert!(
            harness.query_by_label("Nothing yet").is_none(),
            "the chat tab shows no inspector"
        );
        assert_eq!(
            count_labels(&harness, "Ask"),
            1,
            "the chat tab has the input box"
        );

        harness.get_by_label("Details").click_accesskit();
        harness.run_ok();

        assert_eq!(count_labels(&harness, "Nothing yet"), 2);
        assert_eq!(count_labels(&harness, "No subagents yet"), 1);
        assert_eq!(count_labels(&harness, "No Claude session yet"), 1);
        assert_eq!(count_labels(&harness, "No tasks yet"), 1);
        assert_eq!(
            count_labels(&harness, "Nothing running in the background"),
            1
        );
        assert_eq!(count_labels(&harness, "No usage reported yet"), 1);
        assert_eq!(count_labels(&harness, "No rate-limit report yet"), 1);
        let mut i18n = notedeck::Localization::default();
        for section in InspectorSection::ALL {
            let title = super::inspector::section_title(section, &mut i18n);
            // Once on the rail, once on its card.
            assert_eq!(count_labels(&harness, &title), 2, "{title}");
        }
        assert_eq!(count_labels(&harness, "Ask"), 0, "Details has no input box");

        harness.get_by_label("Chat").click_accesskit();
        harness.run_ok();
        assert_eq!(count_labels(&harness, "Nothing yet"), 0);
        assert_eq!(count_labels(&harness, "Ask"), 1);
    }

    /// Give the active session what a Claude CLI's init would: a model, two
    /// MCP servers (one failed), tools, skills, a plugin that loaded and one
    /// that didn't. Parked on two background tasks with its CLI stopped, so
    /// nothing on the card depends on the clock or the machine.
    fn seed_overview(state: &mut SessionViewState) {
        use crate::messages::{
            CliState, DaveApiResponse, McpServerInfo, McpServerStatus, PluginError, PluginInfo,
            SessionInfo, WaitingOn,
        };

        let info = SessionInfo {
            tools: vec!["Bash".into(), "Read".into(), "Edit".into()],
            model: Some("claude-opus-5-5".into()),
            agents: vec!["general-purpose".into(), "Explore".into()],
            cli_version: Some("2.1.288".into()),
            claude_session_id: Some("0b8c1a2e-5f3d-4c7a-9e21-6d4f8b0a1c3e".into()),
            mcp_servers: vec![
                McpServerInfo {
                    name: "linear-server".into(),
                    status: McpServerStatus::Connected,
                    source: Some("user".into()),
                },
                McpServerInfo {
                    name: "beads".into(),
                    status: McpServerStatus::Failed,
                    source: Some("user".into()),
                },
            ],
            skills: vec!["headway".into(), "autowork".into()],
            plugins: vec![PluginInfo {
                name: "notedeck".into(),
                path: None,
                version: Some("0.3.1".into()),
            }],
            plugin_errors: vec![PluginError {
                plugin: "broken".into(),
                kind: "manifest".into(),
                message: "plugin.json is missing \"name\"".into(),
            }],
            output_style: Some("default".into()),
            api_key_source: Some("none".into()),
            ..Default::default()
        };
        let session = state.session_manager.get_active_mut().unwrap();
        session.details.model = info.model.clone();
        session.details.hostname = "monad".into();
        session.inspector.session_ref = Some("agentium:hood-skirt-lazy".into());
        session
            .inspector
            .apply(&DaveApiResponse::SessionInfo(info.clone()));
        session
            .inspector
            .apply(&DaveApiResponse::CliState(CliState {
                connected: false,
                reap_at: None,
            }));
        session.agentic.as_mut().unwrap().session_info = Some(info);
        session.waiting_on = WaitingOn {
            background_tasks: 2,
            ..Default::default()
        };
        session.update_status();
        session.inspector_view.show_details();
    }

    /// The number of nodes whose label contains `text`. A localized string
    /// with an argument wraps it in bidi isolation marks, so "2 background
    /// tasks" can't be matched whole.
    fn count_labels_containing(harness: &Harness<'static, SessionViewState>, text: &str) -> usize {
        use egui_kittest::kittest::{By, Queryable};
        harness.query_all(By::new().label_contains(text)).count()
    }

    /// The Overview shows what the init carried: a chip per MCP server, and
    /// a count chip that expands to its list on click and folds on a second.
    #[test]
    fn overview_shows_init_and_expands_counts() {
        use egui_kittest::kittest::Queryable;

        let mut harness = session_view_harness(egui::vec2(1100.0, 900.0));
        seed_overview(harness.state_mut());
        harness.run_ok();

        assert_eq!(count_labels(&harness, "beads"), 1);
        assert_eq!(count_labels(&harness, "linear-server"), 1);
        assert_eq!(count_labels(&harness, "2.1.288"), 1);
        assert_eq!(count_labels(&harness, "agentium:hood-skirt-lazy"), 1);
        assert_eq!(count_labels_containing(&harness, "background tasks"), 1);
        assert_eq!(
            count_labels(&harness, "Stopped · the next message starts it"),
            1
        );
        assert_eq!(count_labels(&harness, "Edit"), 0, "lists start folded");

        harness
            .get(egui_kittest::kittest::By::new().label_contains(" tools"))
            .click_accesskit();
        harness.run_ok();
        assert_eq!(count_labels(&harness, "Edit"), 1);

        harness
            .get(egui_kittest::kittest::By::new().label_contains(" tools"))
            .click_accesskit();
        harness.run_ok();
        assert_eq!(count_labels(&harness, "Edit"), 0);
    }

    /// A remote session's CLI runs on its own host, so the Overview has no
    /// process line to show for it.
    #[test]
    fn overview_hides_the_cli_process_of_a_remote_session() {
        let mut harness = session_view_harness(egui::vec2(1100.0, 900.0));
        seed_overview(harness.state_mut());
        harness.run_ok();
        assert_eq!(count_labels(&harness, "Process"), 1);

        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .source = crate::session::SessionSource::Remote;
        harness.run_ok();
        assert_eq!(count_labels(&harness, "Process"), 0);
    }

    /// Give the active session four turns of usage: the context climbs, is
    /// compacted after the third turn and starts again from a summary, with
    /// the fourth turn's request still live.
    fn seed_usage(state: &mut SessionViewState) {
        use crate::messages::{CompactionInfo, DaveApiResponse, UsageInfo};

        let request = |input, cache_creation, cache_read, output| {
            DaveApiResponse::UsageUpdate(UsageInfo {
                input_tokens: input,
                cache_creation_input_tokens: cache_creation,
                cache_read_input_tokens: cache_read,
                output_tokens: output,
                ..Default::default()
            })
        };
        let turn_end = |cost, num_turns| {
            DaveApiResponse::QueryComplete(UsageInfo {
                cost_usd: Some(cost),
                num_turns,
                ..Default::default()
            })
        };
        let session = state.session_manager.get_active_mut().unwrap();
        let usage = &mut session.inspector.usage;
        let responses = [
            request(3_000, 18_000, 0, 900),
            turn_end(0.21, 3),
            request(1_200, 6_000, 64_000, 2_400),
            turn_end(0.58, 9),
            request(800, 4_000, 141_000, 1_800),
            turn_end(1.12, 12),
            DaveApiResponse::CompactionComplete(CompactionInfo {
                pre_tokens: 145_800,
            }),
            request(2_000, 22_000, 0, 600),
            turn_end(1.34, 2),
            request(1_500, 9_000, 24_000, 1_100),
        ];
        for (at, res) in responses.iter().enumerate() {
            usage.apply(res, at as u64);
        }
        let agentic = session.agentic.as_mut().unwrap();
        agentic.usage = UsageInfo {
            input_tokens: 1_500,
            cache_creation_input_tokens: 9_000,
            cache_read_input_tokens: 24_000,
            output_tokens: 1_100,
            cost_usd: Some(1.34),
            num_turns: 2,
        };
    }

    /// The Usage card reads the live context, the session's cost and turns,
    /// and the compaction with the context it left.
    #[test]
    fn usage_shows_context_cost_and_compactions() {
        let mut harness = session_view_harness(egui::vec2(1100.0, 900.0));
        seed_usage(harness.state_mut());
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .show_details();
        harness.run_ok();

        assert_eq!(count_labels(&harness, "No usage reported yet"), 0);
        assert_eq!(
            count_labels_containing(&harness, "· 17%"),
            1,
            "live context"
        );
        assert_eq!(count_labels_containing(&harness, "cache read"), 1);
        assert_eq!(count_labels_containing(&harness, "$1.34 · "), 1);
        // Fluent wraps each argument in bidi isolation marks.
        assert_eq!(
            count_labels_containing(&harness, "\u{2068}4\u{2069} turns"),
            1
        );
        assert_eq!(
            count_labels_containing(&harness, "\u{2068}1\u{2069} compaction"),
            1
        );
        assert_eq!(
            count_labels_containing(&harness, "\u{2068}145K\u{2069} → \u{2068}24K\u{2069}"),
            1
        );
    }

    /// Clicking the status bar's usage meter opens Details at Usage.
    #[test]
    fn the_usage_meter_opens_the_usage_card() {
        use crate::inspector::{InspectorSection, SessionTab};
        use egui_kittest::kittest::Queryable;

        let mut harness = session_view_harness(egui::vec2(1100.0, 900.0));
        seed_usage(harness.state_mut());
        harness.run_ok();

        harness
            .get(egui_kittest::kittest::By::new().label_contains("/ 200K"))
            .click();
        harness.run_ok();

        let view = harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view;
        assert_eq!(view.tab, SessionTab::Details);
        assert_eq!(view.section, InspectorSection::Usage);
    }

    /// Rate limits near the five-hour cap, resetting at fixed times far
    /// ahead so the labels and snapshot don't move with the clock.
    fn seed_rate_limits(state: &mut SessionViewState) {
        use crate::messages::{
            RateLimitInfo, RateLimitStatus, RateLimitWindow, RateLimitWindowKind,
        };

        // 2100-01-01 00:00 UTC (a Friday) and a week later.
        const FIVE_HOUR_RESET: u64 = 4_102_444_800;
        const SEVEN_DAY_RESET: u64 = FIVE_HOUR_RESET + 7 * 24 * 3600;
        state.rate_limit = Some(RateLimitInfo {
            status: RateLimitStatus::AllowedWarning,
            limiting: Some(RateLimitWindowKind::FiveHour),
            utilization: Some(0.82),
            resets_at: Some(FIVE_HOUR_RESET),
            five_hour: Some(RateLimitWindow {
                utilization: 0.82,
                resets_at: FIVE_HOUR_RESET,
            }),
            seven_day: Some(RateLimitWindow {
                utilization: 0.41,
                resets_at: SEVEN_DAY_RESET,
            }),
            seven_day_overage_included: None,
        });
    }

    /// The Rate limits card shows the account's windows, from the value
    /// handed in rather than the session's own, with the account-wide note.
    #[test]
    fn rate_limits_show_each_window_and_the_account_note() {
        let mut harness = session_view_harness(egui::vec2(1100.0, 900.0));
        seed_rate_limits(harness.state_mut());
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .show_details();
        harness.run_ok();

        assert_eq!(count_labels(&harness, "No rate-limit report yet"), 0);
        assert_eq!(count_labels(&harness, "Near a limit"), 1);
        assert_eq!(count_labels(&harness, "5-hour"), 1);
        assert_eq!(count_labels(&harness, "7-day"), 1);
        // Fluent wraps each argument in bidi isolation marks.
        assert_eq!(
            count_labels_containing(&harness, "\u{2068}82\u{2069}% · resets"),
            1
        );
        assert_eq!(
            count_labels_containing(&harness, "\u{2068}41\u{2069}% · resets"),
            1
        );
        assert_eq!(count_labels_containing(&harness, "Account-wide"), 1);
    }

    /// Near a limit, the status bar shows a pill, and clicking it opens
    /// Details at the Rate limits card. Within limits it shows nothing.
    #[test]
    fn the_rate_limit_pill_opens_the_rate_limits_card() {
        use crate::inspector::{InspectorSection, SessionTab};
        use egui_kittest::kittest::Queryable;

        let mut harness = session_view_harness(egui::vec2(1100.0, 900.0));
        harness.state_mut().rate_limit = Some(crate::messages::RateLimitInfo::default());
        harness.run_ok();
        assert_eq!(count_labels_containing(&harness, "Limit "), 0);

        seed_rate_limits(harness.state_mut());
        harness.run_ok();
        harness
            .get(egui_kittest::kittest::By::new().label_contains("Limit \u{2068}82"))
            .click();
        harness.run_ok();

        let view = harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view;
        assert_eq!(view.tab, SessionTab::Details);
        assert_eq!(view.section, InspectorSection::RateLimits);
    }

    /// One of each kind of background entry, as of `now`: a Bash task with
    /// an output file (written to `dir`), a Monitor, a background subagent
    /// with progress, a wake-up and a cron. Also marks the session Waiting
    /// on them.
    fn seed_background(state: &mut SessionViewState, now: u64, dir: &std::path::Path) {
        use crate::messages::{
            BackgroundEntry, BackgroundTask, BackgroundTaskKind, BackgroundWork, DaveApiResponse,
            TaskUsage, WaitingOn,
        };

        let output = dir.join("b7k2q.output");
        std::fs::write(
            &output,
            "card    headway:dave/trade-marine-bless\ncolumn  In Progress\n\
             title   dave: a Waiting status\nchecked 22 times\nstill waiting\n",
        )
        .unwrap();
        let task = |id: &str, ago: u64, task: BackgroundTask| BackgroundEntry {
            id: id.into(),
            started_at: now - ago,
            work: BackgroundWork::Task(task),
        };
        let entries = vec![
            BackgroundEntry {
                id: "wakeup".into(),
                started_at: now - 30,
                work: BackgroundWork::Wakeup {
                    due_at: now + 6 * 60 + 12,
                    reason: "check the CI run".into(),
                    prompt: "/loop check CI".into(),
                },
            },
            task(
                "m41xd",
                8 * 60,
                BackgroundTask {
                    kind: BackgroundTaskKind::Monitor,
                    description: "watch CI for failures".into(),
                    command: Some("tail -f target/ci.log | grep --line-buffered FAIL".into()),
                    ..Default::default()
                },
            ),
            task(
                "a9f3",
                12 * 60,
                BackgroundTask {
                    kind: BackgroundTaskKind::Agent,
                    description: "Survey CLI schemas".into(),
                    usage: Some(TaskUsage {
                        total_tokens: 12_400,
                        tool_uses: 7,
                        duration_ms: 720_000,
                    }),
                    last_tool_name: Some("Grep".into()),
                    ..Default::default()
                },
            ),
            BackgroundEntry {
                id: "3fa9".into(),
                started_at: now - 20 * 60,
                work: BackgroundWork::Cron {
                    expr: "*/30 * * * *".into(),
                    prompt: "summarize new comments on the epic".into(),
                    recurring: true,
                    durable: false,
                },
            },
            task(
                "b7k2q",
                22 * 60,
                BackgroundTask {
                    kind: BackgroundTaskKind::Bash,
                    description: "wait for review".into(),
                    command: Some(
                        "until headway show trade-marine-bless | grep -q in-review; do sleep 60; done"
                            .into(),
                    ),
                    output_file: Some(output.to_string_lossy().into_owned()),
                    ..Default::default()
                },
            ),
        ];

        state.now_unix = Some(now);
        let session = state.session_manager.get_active_mut().unwrap();
        session
            .inspector
            .apply(&DaveApiResponse::BackgroundWork(entries));
        session.waiting_on = WaitingOn {
            background_tasks: 3,
            wakeup_at: Some(crate::session::now_unix() + 3600),
            crons: 1,
        };
        session.update_status();
    }

    /// The Background card lists every entry with its badge, title and age
    /// or countdown, and a local task's output tail once its read lands.
    #[test]
    fn background_lists_each_entry_with_its_tail() {
        let dir = tempfile::tempdir().unwrap();
        let mut harness = session_view_harness(egui::vec2(1100.0, 1400.0));
        let now = crate::session::now_unix();
        seed_background(harness.state_mut(), now, dir.path());
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .show_details();
        harness.run_ok();

        assert_eq!(
            count_labels(&harness, "Nothing running in the background"),
            0
        );
        for badge in ["bash", "monitor", "agent", "wake-up", "cron"] {
            assert_eq!(count_labels(&harness, badge), 1, "{badge}");
        }
        assert_eq!(count_labels(&harness, "22m"), 1);
        assert_eq!(count_labels(&harness, "recurring"), 1);
        assert_eq!(
            count_labels_containing(&harness, "in \u{2068}6m 12s\u{2069}"),
            1
        );
        assert_eq!(count_labels(&harness, "check the CI run"), 1);
        assert_eq!(count_labels(&harness, "*/30 * * * *"), 1);
        assert_eq!(count_labels_containing(&harness, "last \u{2068}Grep"), 1);

        // The tail is read off the UI thread; wait for it to land.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while count_labels_containing(&harness, "still waiting") == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the output tail never landed"
            );
            harness.step();
        }
        assert_eq!(
            count_labels_containing(&harness, "headway:dave/trade-marine-bless"),
            1,
            "the tail is the last five lines"
        );
    }

    /// A remote session's tasks show no tail: their files are on its host.
    #[test]
    fn a_remote_session_shows_no_output_tail() {
        let dir = tempfile::tempdir().unwrap();
        let mut harness = session_view_harness(egui::vec2(1100.0, 1400.0));
        seed_background(harness.state_mut(), crate::session::now_unix(), dir.path());
        let session = harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap();
        session.source = crate::session::SessionSource::Remote;
        session.inspector_view.show_details();
        for _ in 0..20 {
            harness.step();
        }
        assert_eq!(count_labels(&harness, "bash"), 1);
        assert_eq!(count_labels_containing(&harness, "still waiting"), 0);
    }

    /// A Waiting session's status bar shows how many things will resume
    /// it, and clicking the pill opens Details at the Background card.
    #[test]
    fn the_waiting_pill_opens_the_background_card() {
        use crate::inspector::{InspectorSection, SessionTab};
        use egui_kittest::kittest::Queryable;

        let dir = tempfile::tempdir().unwrap();
        let mut harness = session_view_harness(egui::vec2(1100.0, 900.0));
        harness.run_ok();
        assert_eq!(count_labels_containing(&harness, "Waiting · "), 0);

        seed_background(harness.state_mut(), crate::session::now_unix(), dir.path());
        harness.run_ok();
        harness
            .get(egui_kittest::kittest::By::new().label_contains("Waiting · \u{2068}5"))
            .click();
        harness.run_ok();

        let view = harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view;
        assert_eq!(view.tab, SessionTab::Details);
        assert_eq!(view.section, InspectorSection::Background);
    }

    /// Three subagents in the active session's chat, oldest first: an
    /// Explore that finished, a general-purpose one that failed, and a Plan
    /// still running in the background, with progress for the first and
    /// last.
    fn seed_subagents(state: &mut SessionViewState) {
        use crate::messages::{
            DaveApiResponse, SubagentInfo, SubagentProgress, SubagentStatus, TaskUsage,
        };

        let subagent = |task_id: &str, kind: &str, description: &str, status, background| {
            crate::Message::Subagent(SubagentInfo {
                task_id: task_id.into(),
                description: description.into(),
                subagent_type: kind.into(),
                status,
                output: String::new(),
                max_output_size: 4000,
                tool_results: Vec::new(),
                background,
            })
        };
        let session = state.session_manager.get_active_mut().unwrap();
        session.replace_chat(vec![
            subagent(
                "toolu_explore",
                "Explore",
                "Find where task_progress is parsed",
                SubagentStatus::Completed,
                false,
            ),
            subagent(
                "toolu_tests",
                "general-purpose",
                "Write the inspector tests",
                SubagentStatus::Failed,
                false,
            ),
            subagent(
                "toolu_plan",
                "Plan",
                "Survey the CLI's task message schemas",
                SubagentStatus::Running,
                true,
            ),
        ]);
        let progress = |task_id: &str, tokens, tools, secs: u64, last: &str| {
            DaveApiResponse::SubagentProgress(SubagentProgress {
                task_id: task_id.into(),
                usage: TaskUsage {
                    total_tokens: tokens,
                    tool_uses: tools,
                    duration_ms: secs * 1000,
                },
                last_tool_name: Some(last.into()),
            })
        };
        session
            .inspector
            .apply(&progress("toolu_explore", 41_300, 18, 95, "Grep"));
        session
            .inspector
            .apply(&progress("toolu_plan", 12_400, 7, 61, "Read"));
    }

    /// The Subagents card lists every subagent newest first with its status
    /// and numbers, and a click on one goes back to the chat with the block
    /// cursor on its row.
    #[test]
    fn subagents_card_lists_each_and_jumps_to_its_chat_row() {
        use crate::inspector::SessionTab;
        use egui_kittest::kittest::Queryable;

        let mut harness = session_view_harness(egui::vec2(1100.0, 1400.0));
        seed_subagents(harness.state_mut());
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .show_details();
        harness.run_ok();

        assert_eq!(count_labels(&harness, "No subagents yet"), 0);
        for label in ["Explore", "general-purpose", "Plan", "done", "failed"] {
            assert_eq!(count_labels(&harness, label), 1, "{label}");
        }
        assert_eq!(count_labels(&harness, "in background"), 1);
        // The meta line and the summary; their arguments may sit in bidi
        // isolation marks, so match around them.
        assert_eq!(count_labels_containing(&harness, "Grep"), 1);
        assert_eq!(count_labels_containing(&harness, "41K"), 1);
        assert_eq!(count_labels_containing(&harness, "subagents"), 1);

        harness.get_by_label("Write the inspector tests").click();
        harness.run_ok();

        let session = harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap();
        assert_eq!(session.inspector_view.tab, SessionTab::Chat);
        assert_eq!(
            session.block_nav.cursor(),
            Some(1),
            "the cursor is on the second subagent row in the chat"
        );
    }

    /// The Subagents card with a finished, a failed and a background
    /// subagent.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_session_subagents() {
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::vec2(1100.0, 760.0))
            .renderer(notedeck::software_renderer())
            .build_ui_state(render_session_view, session_view_state());
        seed_subagents(harness.state_mut());
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .open(crate::inspector::InspectorSection::Subagents);
        harness.run_ok();
        harness.snapshot("session_subagents");
    }

    /// A task list in the active session's chat: an older list the agent
    /// has since replaced, then the latest with two finished items, one
    /// under way and two to go.
    fn seed_tasks(state: &mut SessionViewState) {
        use serde_json::json;

        let session = state.session_manager.get_active_mut().unwrap();
        session.replace_chat(vec![
            crate::Message::TodoUpdate(json!({"todos": [
                {"content": "Superseded plan", "activeForm": "Planning", "status": "in_progress"},
            ]})),
            crate::Message::TodoUpdate(json!({"todos": [
                {"content": "Read the task tracker", "activeForm": "Reading the task tracker", "status": "completed"},
                {"content": "Share the todo row", "activeForm": "Sharing the todo row", "status": "completed"},
                {"content": "Draw the Tasks card", "activeForm": "Drawing the Tasks card", "status": "in_progress"},
                {"content": "Snapshot the card", "activeForm": "Snapshotting the card", "status": "pending"},
                {"content": "Write the explainer", "activeForm": "Writing the explainer", "status": "pending"},
            ]})),
        ]);
    }

    /// The Tasks card shows the latest list only, finished items included,
    /// under its done/total header and the item under way.
    #[test]
    fn tasks_card_shows_the_latest_list_with_progress() {
        let mut harness = session_view_harness(egui::vec2(1100.0, 1400.0));
        seed_tasks(harness.state_mut());
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .show_details();
        harness.run_ok();

        assert_eq!(count_labels(&harness, "No tasks yet"), 0);
        assert_eq!(count_labels(&harness, "Superseded plan"), 0);
        for label in [
            "Read the task tracker",
            "Share the todo row",
            "Snapshot the card",
            "Write the explainer",
        ] {
            assert_eq!(count_labels(&harness, label), 1, "{label}");
        }
        // The row and the header both name the item under way.
        assert_eq!(count_labels(&harness, "Drawing the Tasks card"), 2);
        assert_eq!(count_labels(&harness, "2/5"), 1);
    }

    /// The Tasks card two items in, one under way and two to go.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_session_tasks() {
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::vec2(1100.0, 760.0))
            .renderer(notedeck::software_renderer())
            .build_ui_state(render_session_view, session_view_state());
        seed_tasks(harness.state_mut());
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .open(crate::inspector::InspectorSection::Tasks);
        harness.run_ok();
        harness.snapshot("session_tasks");
    }

    /// The CLI session id the Files card's seeded temp dir is named for.
    const FILES_CLI_SESSION: &str = "4b1c9e02-7d3a-4f6e-9a51-c0ffee5e5510";

    /// A Claude Code temp dir for the active session under `root`, as of
    /// `now`: two scratch files and a folder, and two task output logs.
    /// Returns the newer log's path.
    fn seed_files(state: &mut SessionViewState, root: &std::path::Path, now: u64) -> PathBuf {
        let session = state.session_manager.get_active_mut().unwrap();
        let cwd = session.details.cwd.clone().unwrap();
        session.agentic.as_mut().unwrap().resume_session_id = Some(FILES_CLI_SESSION.into());
        session.inspector.files = crate::inspector::FilesState::with_root(root.to_path_buf());

        let dir = crate::inspector::session_temp_dir(root, &cwd, FILES_CLI_SESSION);
        let write = |rel: &str, body: &str, ago: u64| {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
            let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(now - ago);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
            path
        };
        write(
            "scratchpad/plan.md",
            &"- [x] list the temp dir\n".repeat(180),
            4 * 60,
        );
        write("scratchpad/strings.txt", &"x".repeat(612), 52 * 60);
        write("scratchpad/diffs/files.patch", "", 0);
        // The folder's own mtime, which writing into it just moved.
        std::fs::File::open(dir.join("scratchpad/diffs"))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(now - 30))
            .unwrap();
        let log = write(
            "tasks/bxhiddwx5.output",
            "Compiling notedeck_dave v0.7.1\nFinished `dev` profile in 3m 38s\n\
             running 12 tests\ntest inspector::files::tests::lists_both_groups_newest_first ... ok\n\
             test result: ok. 12 passed; 0 failed\n",
            3 * 60 + 20,
        );
        write("tasks/b7k2q.output", "still waiting\n", 2 * 3600 + 5 * 60);
        log
    }

    /// Step the harness until a label containing `text` appears.
    fn step_until_label(harness: &mut Harness<'static, SessionViewState>, text: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while count_labels_containing(harness, text) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "{text:?} never appeared"
            );
            harness.step();
        }
    }

    /// The Files card lists the session's scratchpad and task output newest
    /// first, and Tail shows an output log's last lines.
    #[test]
    fn files_card_lists_the_temp_dir_and_tails_an_output_log() {
        let root = tempfile::tempdir().unwrap();
        let mut harness = session_view_harness(egui::vec2(1100.0, 1600.0));
        let now = crate::session::now_unix();
        seed_files(harness.state_mut(), root.path(), now);
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .show_details();
        step_until_label(&mut harness, "plan.md");
        harness.run_ok();

        for label in ["plan.md", "strings.txt", "bxhiddwx5.output", "b7k2q.output"] {
            assert_eq!(count_labels(&harness, label), 1, "{label}");
        }
        assert_eq!(count_labels_containing(&harness, "scratchpad"), 1);
        assert_eq!(count_labels_containing(&harness, "612 B"), 1);
        assert_eq!(count_labels_containing(&harness, "4.2 KB"), 1);
        // Only the two logs get a Tail button.
        assert_eq!(count_labels(&harness, "Tail"), 2);

        // The scratchpad's files sort newest first: the folder, then plan.md.
        let y = |label: &str| {
            use egui_kittest::kittest::Queryable;
            harness.get_by_label(label).rect().min.y
        };
        assert!(y("plan.md") < y("strings.txt"));
        assert!(y("bxhiddwx5.output") < y("b7k2q.output"));

        {
            use egui_kittest::kittest::Queryable;
            harness.get_all_by_label("Tail").next().unwrap().click();
        }
        step_until_label(&mut harness, "12 passed");
        assert_eq!(count_labels(&harness, "Hide"), 1);
    }

    /// A remote session's temp files are on its host: the card says so
    /// rather than listing this machine's.
    #[test]
    fn files_card_of_a_remote_session_names_its_host() {
        let root = tempfile::tempdir().unwrap();
        let mut harness = session_view_harness(egui::vec2(1100.0, 1600.0));
        seed_files(harness.state_mut(), root.path(), crate::session::now_unix());
        let session = harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap();
        session.source = crate::session::SessionSource::Remote;
        session.details.hostname = "monad".into();
        session.inspector_view.show_details();
        harness.run_ok();

        assert_eq!(count_labels_containing(&harness, "Files live on"), 1);
        assert_eq!(count_labels(&harness, "plan.md"), 0);
    }

    /// The Files card: the scratchpad with a folder and two files, and the
    /// task logs with the newer one's tail open.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_session_files() {
        // A fixed root, so the card's path line is the same in every run.
        let root = std::env::temp_dir().join("dave-inspector-files-snapshot");
        let _ = std::fs::remove_dir_all(&root);
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::vec2(1100.0, 760.0))
            .renderer(notedeck::software_renderer())
            .build_ui_state(render_session_view, session_view_state());
        // 2100-01-01 00:00 UTC, so the ages hold still.
        let now = 4_102_444_800;
        harness.state_mut().now_unix = Some(now);
        let log = seed_files(harness.state_mut(), &root, now);
        let session = harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap();
        session
            .inspector_view
            .open(crate::inspector::InspectorSection::Files);
        // Opened once the listing lands: the first listing of a session
        // starts with every tail closed.
        step_until_label(&mut harness, "plan.md");
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector
            .files
            .tail_open = Some(log);
        step_until_label(&mut harness, "12 passed");
        harness.run_ok();
        harness.snapshot("session_files");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The Background card with one entry of each kind: a Bash task with
    /// its output tail, a Monitor, a subagent, a wake-up and a cron.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_session_background() {
        let dir = tempfile::tempdir().unwrap();
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::vec2(1100.0, 760.0))
            .renderer(notedeck::software_renderer())
            .build_ui_state(render_session_view, session_view_state());
        // 2100-01-01 00:00 UTC, so the wake-up's due time holds still.
        seed_background(harness.state_mut(), 4_102_444_800, dir.path());
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .open(crate::inspector::InspectorSection::Background);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while count_labels_containing(&harness, "still waiting") == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the tail never landed"
            );
            harness.step();
        }
        harness.run_ok();
        harness.snapshot("session_background");
    }

    /// The Rate limits card near the five-hour cap: the status, a bar per
    /// window with its reset, and the account-wide note.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_session_rate_limits() {
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::vec2(1100.0, 760.0))
            .renderer(notedeck::software_renderer())
            .build_ui_state(render_session_view, session_view_state());
        seed_rate_limits(harness.state_mut());
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .open(crate::inspector::InspectorSection::RateLimits);
        harness.run_ok();
        harness.snapshot("session_rate_limits");
    }

    /// The Usage card after a compaction: the stacked context bar, the
    /// legend, the totals and the sparkline with its compaction mark.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_session_usage() {
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::vec2(1100.0, 760.0))
            .renderer(notedeck::software_renderer())
            .build_ui_state(render_session_view, session_view_state());
        seed_usage(harness.state_mut());
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .show_details();
        harness.run_ok();
        harness.snapshot("session_usage");
    }

    /// The Overview card with a failed MCP server chip and a plugin error,
    /// the skills list expanded.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_session_overview() {
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::vec2(1100.0, 760.0))
            .renderer(notedeck::software_renderer())
            .build_ui_state(render_session_view, session_view_state());
        seed_overview(harness.state_mut());
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .toggle_overview_list(crate::inspector::OverviewList::Skills);
        harness.run_ok();
        harness.snapshot("session_overview");
    }

    /// An empty session's two tabs: the chat under the tab strip, then
    /// Details with the rail, an Overview, Usage, Rate limits, Background,
    /// Subagents and Tasks with nothing reported yet and the three stub cards.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_session_details_tab() {
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::vec2(1100.0, 760.0))
            .renderer(notedeck::software_renderer())
            .build_ui_state(render_session_view, session_view_state());
        harness.run_ok();
        harness.snapshot("session_chat_tab");
        harness
            .state_mut()
            .session_manager
            .get_active_mut()
            .unwrap()
            .inspector_view
            .show_details();
        harness.run_ok();
        harness.snapshot("session_details_tab");
    }
}
