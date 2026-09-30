use crate::config::{AiProvider, DaveSettings, LeaderKey};
use crate::ui::keybind_hint::keybind_hint;
use notedeck::{tr, Localization};
use std::collections::BTreeMap;

/// Tracks the state of the settings panel
pub struct DaveSettingsPanel {
    /// Whether the panel is currently open
    open: bool,
    /// Working copy of settings being edited
    editing: DaveSettings,
    /// Custom model input (when user wants to type a model not in the list)
    custom_model: String,
    /// Whether to use custom model input
    use_custom_model: bool,
    /// The leader-key button was clicked: the next key press becomes the leader.
    capturing_leader: bool,
    /// The last press while capturing had no Ctrl or Alt, so it was refused.
    leader_rejected: bool,
    /// `editing.leader_key` for display, rebuilt only when it changes.
    leader_label: String,
    /// `editing.session_env` as editable rows. Seeded when the panel opens and
    /// written back into the settings only on save, so typing never rebuilds
    /// the map.
    env_rows: Vec<EnvRow>,
    /// The next [`EnvRow::id`] to hand out.
    next_env_row_id: u64,
    /// A row was just added: focus its name field on the next frame.
    focus_new_env_row: bool,
}

/// One editable `KEY = value` row of the session environment editor.
struct EnvRow {
    /// Stable egui id salt, so a row keeps its text focus when a row above it
    /// is removed.
    id: u64,
    key: String,
    value: String,
}

/// Why a session environment row won't be saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvKeyProblem {
    /// A value with no name to export it under.
    Missing,
    /// The name has whitespace, `=` or NUL, so it can't be an env var name.
    Malformed,
    /// An earlier row already sets this name; the first one wins.
    Duplicate,
}

/// What's wrong with row `index`'s name, if anything. A fully blank row is not
/// a problem, just an unused row that saving drops.
fn env_key_problem(rows: &[EnvRow], index: usize) -> Option<EnvKeyProblem> {
    let row = &rows[index];
    if row.key.is_empty() {
        return (!row.value.is_empty()).then_some(EnvKeyProblem::Missing);
    }
    if row
        .key
        .chars()
        .any(|c| c.is_whitespace() || c == '=' || c == '\0')
    {
        return Some(EnvKeyProblem::Malformed);
    }
    if rows[..index].iter().any(|earlier| earlier.key == row.key) {
        return Some(EnvKeyProblem::Duplicate);
    }
    None
}

/// The session env the rows describe: every row with a valid, first-seen name.
/// Blank and flagged rows are dropped, exactly the rows the editor warns about.
fn session_env_from_rows(rows: &[EnvRow]) -> BTreeMap<String, String> {
    (0..rows.len())
        .filter(|&i| !rows[i].key.is_empty() && env_key_problem(rows, i).is_none())
        .map(|i| (rows[i].key.clone(), rows[i].value.clone()))
        .collect()
}

/// Actions that can result from the settings panel
#[derive(Debug)]
pub enum SettingsPanelAction {
    /// User saved the settings
    Save(DaveSettings),
    /// User cancelled the settings panel
    Cancel,
}

impl Default for DaveSettingsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl DaveSettingsPanel {
    pub fn new() -> Self {
        DaveSettingsPanel {
            open: false,
            editing: DaveSettings::default(),
            custom_model: String::new(),
            use_custom_model: false,
            capturing_leader: false,
            leader_rejected: false,
            leader_label: String::new(),
            env_rows: Vec::new(),
            next_env_row_id: 0,
            focus_new_env_row: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the panel is waiting for a key press to record as the leader.
    /// While it is, the app's own keybindings must stand aside.
    pub fn is_capturing_leader(&self) -> bool {
        self.open && self.capturing_leader
    }

    /// Open the panel with a copy of current settings to edit
    pub fn open(&mut self, current: &DaveSettings) {
        self.editing = current.clone();
        self.custom_model = current.model.clone();
        // Check if current model is in the available list
        self.use_custom_model = !current
            .provider
            .available_models()
            .contains(&current.model.as_str());
        self.capturing_leader = false;
        self.leader_rejected = false;
        self.leader_label = current.leader_key.to_string();
        self.env_rows.clear();
        for (key, value) in &current.session_env {
            self.push_env_row(key.clone(), value.clone());
        }
        self.focus_new_env_row = false;
        self.open = true;
    }

    /// Append a session environment row with a fresh id.
    fn push_env_row(&mut self, key: String, value: String) {
        self.env_rows.push(EnvRow {
            id: self.next_env_row_id,
            key,
            value,
        });
        self.next_env_row_id += 1;
    }

    /// The edited settings to save: the working copy with the session env
    /// rows folded back in.
    fn saved_settings(&self) -> DaveSettings {
        let mut settings = self.editing.clone();
        settings.session_env = session_env_from_rows(&self.env_rows);
        settings
    }

    pub fn close(&mut self) {
        self.open = false;
        self.capturing_leader = false;
    }

    /// Prepare editing state for overlay mode
    pub fn prepare_edit(&mut self, current: &DaveSettings) {
        if !self.open {
            self.open(current);
        }
    }

    /// While capturing, record this frame's first key press as the leader.
    ///
    /// Esc cancels the capture. A press without Ctrl or Alt is refused, since
    /// a bare key is something you type. Every key and text event is
    /// swallowed either way, so the press can neither save (Ctrl+S) nor
    /// close (Esc) the panel.
    fn capture_leader(&mut self, ctx: &egui::Context) {
        let Some((modifiers, key)) = ctx.input(|i| {
            i.events.iter().find_map(|event| match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } => Some((*modifiers, *key)),
                _ => None,
            })
        }) else {
            return;
        };
        ctx.input_mut(|i| {
            i.events
                .retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)))
        });

        if key == egui::Key::Escape {
            self.capturing_leader = false;
            self.leader_rejected = false;
            return;
        }
        if !LeaderKey::is_valid_press(modifiers) {
            self.leader_rejected = true;
            return;
        }

        self.editing.leader_key = LeaderKey::from_press(modifiers, key);
        self.leader_label = self.editing.leader_key.to_string();
        self.capturing_leader = false;
        self.leader_rejected = false;
    }

    /// Render settings as a full-panel overlay (replaces the main content)
    pub fn overlay_ui(
        &mut self,
        ui: &mut egui::Ui,
        current: &DaveSettings,
        i18n: &mut Localization,
    ) -> Option<SettingsPanelAction> {
        // Initialize editing state if not already set
        self.prepare_edit(current);

        // Before anything else reads this frame's keys.
        if self.capturing_leader {
            self.capture_leader(ui.ctx());
        }

        let mut action: Option<SettingsPanelAction> = None;
        let is_narrow = notedeck::ui::is_narrow(ui.ctx());
        let ctrl_held = ui.input(|i| i.modifiers.ctrl);

        // Handle Ctrl+S to save
        if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::S)) {
            action = Some(SettingsPanelAction::Save(self.saved_settings()));
        }

        // Full panel frame with padding
        egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(egui::Margin::symmetric(if is_narrow { 16 } else { 40 }, 20))
            .show(ui, |ui| {
                // Header with back button
                ui.horizontal(|ui| {
                    if ui.button("< Back").clicked() {
                        action = Some(SettingsPanelAction::Cancel);
                    }
                    if ctrl_held {
                        keybind_hint(ui, "Esc");
                    }
                    ui.add_space(16.0);
                    ui.heading("Settings");
                });

                ui.add_space(24.0);

                // Centered content container (max width for readability on desktop)
                let max_content_width = if is_narrow {
                    ui.available_width()
                } else {
                    500.0
                };
                ui.allocate_ui_with_layout(
                    egui::vec2(max_content_width, ui.available_height()),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        self.settings_form(ui, i18n);

                        ui.add_space(24.0);

                        // Action buttons with keyboard hints
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                action = Some(SettingsPanelAction::Save(self.saved_settings()));
                            }
                            if ctrl_held {
                                keybind_hint(ui, "S");
                            }
                            ui.add_space(8.0);
                            if ui.button("Cancel").clicked() {
                                action = Some(SettingsPanelAction::Cancel);
                            }
                            if ctrl_held {
                                keybind_hint(ui, "Esc");
                            }
                        });
                    },
                );
            });

        // Handle Escape key
        if ui
            .ctx()
            .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            action = Some(SettingsPanelAction::Cancel);
        }

        if action.is_some() {
            self.close();
        }

        action
    }

    /// Render the settings form content (shared between overlay and window modes)
    fn settings_form(&mut self, ui: &mut egui::Ui, i18n: &mut Localization) {
        egui::Grid::new("settings_grid")
            .num_columns(2)
            .spacing([10.0, 12.0])
            .show(ui, |ui| {
                // Provider dropdown
                ui.label("Provider:");
                let prev_provider = self.editing.provider;
                egui::ComboBox::from_id_salt("provider_combo")
                    .selected_text(self.editing.provider.name())
                    .show_ui(ui, |ui| {
                        for provider in AiProvider::ALL {
                            ui.selectable_value(
                                &mut self.editing.provider,
                                provider,
                                provider.name(),
                            );
                        }
                    });
                ui.end_row();

                // If provider changed, reset to provider defaults
                if self.editing.provider != prev_provider {
                    self.editing.model = self.editing.provider.default_model().to_string();
                    self.editing.endpoint = self
                        .editing
                        .provider
                        .default_endpoint()
                        .map(|s| s.to_string());
                    self.custom_model = self.editing.model.clone();
                    self.use_custom_model = false;
                }

                // Model selection
                ui.label("Model:");
                ui.vertical(|ui| {
                    // Checkbox for custom model
                    ui.checkbox(&mut self.use_custom_model, "Custom model");

                    if self.use_custom_model {
                        // Custom text input
                        let response = ui.text_edit_singleline(&mut self.custom_model);
                        if response.changed() {
                            self.editing.model = self.custom_model.clone();
                        }
                    } else {
                        // Dropdown with available models
                        egui::ComboBox::from_id_salt("model_combo")
                            .selected_text(&self.editing.model)
                            .show_ui(ui, |ui| {
                                for model in self.editing.provider.available_models() {
                                    ui.selectable_value(
                                        &mut self.editing.model,
                                        model.to_string(),
                                        *model,
                                    );
                                }
                            });
                    }
                });
                ui.end_row();

                // Endpoint field
                ui.label("Endpoint:");
                let mut endpoint_str = self.editing.endpoint.clone().unwrap_or_default();
                if ui.text_edit_singleline(&mut endpoint_str).changed() {
                    self.editing.endpoint = if endpoint_str.is_empty() {
                        None
                    } else {
                        Some(endpoint_str)
                    };
                }
                ui.end_row();

                // API Key field (only shown when required)
                if self.editing.provider.requires_api_key() {
                    ui.label("API Key:");
                    let mut key_str = self.editing.api_key.clone().unwrap_or_default();
                    if ui
                        .add(egui::TextEdit::singleline(&mut key_str).password(true))
                        .changed()
                    {
                        self.editing.api_key = if key_str.is_empty() {
                            None
                        } else {
                            Some(key_str)
                        };
                    }
                    ui.end_row();
                }

                // Leader key: click the button, then press the new binding.
                ui.label(tr!(
                    i18n,
                    "Leader key:",
                    "Settings label for the key that starts a Dave keyboard chord"
                ));
                ui.vertical(|ui| {
                    let button = if self.capturing_leader {
                        ui.button(tr!(
                            i18n,
                            "Press a key…",
                            "Leader key button while it waits for the new binding"
                        ))
                    } else {
                        ui.button(self.leader_label.as_str())
                    };
                    if button.clicked() {
                        self.capturing_leader = !self.capturing_leader;
                        self.leader_rejected = false;
                    }

                    let hint = if self.leader_rejected {
                        tr!(
                            i18n,
                            "Hold Ctrl or Alt with the key. Esc cancels.",
                            "Shown when a leader key was pressed without Ctrl or Alt"
                        )
                    } else if self.capturing_leader {
                        tr!(
                            i18n,
                            "Esc cancels.",
                            "Hint under the leader key button while it waits for a key"
                        )
                    } else {
                        tr!(
                            i18n,
                            "Click, then press a key.",
                            "Hint under the leader key button explaining how to rebind it"
                        )
                    };
                    ui.weak(hint);
                });
                ui.end_row();
            });

        ui.add_space(20.0);
        self.session_env_ui(ui, i18n);
    }

    /// The "Session environment" section: one `KEY = value` row per variable
    /// exported into every agent session, with remove and add buttons. On a
    /// narrow screen each row wraps its value onto a second line.
    fn session_env_ui(&mut self, ui: &mut egui::Ui, i18n: &mut Localization) {
        let is_narrow = notedeck::ui::is_narrow(ui.ctx());

        ui.strong(tr!(
            i18n,
            "Session environment",
            "Settings section for environment variables exported into agent sessions"
        ));
        ui.weak(tr!(
            i18n,
            "Exported into every agent session this host starts, e.g. HEADWAY_COMMENT_NSEC_FILE pointing at an agent key file. Applies to sessions started after saving. Dave's own AGENTIUM_SESSION variables always win.",
            "Hint under the session environment settings heading"
        ));
        ui.add_space(8.0);

        let text = EnvRowText {
            key_hint: tr!(
                i18n,
                "NAME",
                "Placeholder for a session environment variable's name"
            ),
            value_hint: tr!(
                i18n,
                "value",
                "Placeholder for a session environment variable's value"
            ),
            remove_hover: tr!(
                i18n,
                "Remove variable",
                "Tooltip on the button that removes a session environment variable"
            ),
        };

        let mut remove = None;
        let last = self.env_rows.len().checked_sub(1);
        for index in 0..self.env_rows.len() {
            let problem = env_key_problem(&self.env_rows, index);
            let focus = self.focus_new_env_row && Some(index) == last;
            let row = &mut self.env_rows[index];

            ui.push_id(row.id, |ui| {
                let row_response = env_row_ui(ui, row, &text, is_narrow);
                if row_response.remove {
                    remove = Some(index);
                }
                if focus {
                    row_response.key.request_focus();
                }

                let Some(problem) = problem else {
                    return;
                };
                let message = match problem {
                    EnvKeyProblem::Missing => tr!(
                        i18n,
                        "Needs a name. This row won't be saved.",
                        "Session environment row with a value but no variable name"
                    ),
                    EnvKeyProblem::Malformed => tr!(
                        i18n,
                        "Names can't contain spaces or =. This row won't be saved.",
                        "Session environment row whose variable name is invalid"
                    ),
                    EnvKeyProblem::Duplicate => tr!(
                        i18n,
                        "Already set above. This row won't be saved.",
                        "Session environment row repeating an earlier variable name"
                    ),
                };
                ui.colored_label(ui.visuals().warn_fg_color, message);
            });
        }
        self.focus_new_env_row = false;

        if let Some(index) = remove {
            self.env_rows.remove(index);
        }

        ui.add_space(4.0);
        if ui
            .button(tr!(
                i18n,
                "+ Add variable",
                "Button that adds a session environment variable row"
            ))
            .clicked()
        {
            self.push_env_row(String::new(), String::new());
            self.focus_new_env_row = true;
        }
    }
}

/// The localized strings every session environment row shows, looked up once
/// per frame rather than once per row.
struct EnvRowText {
    key_hint: String,
    value_hint: String,
    remove_hover: String,
}

/// What one session environment row's widgets reported this frame.
struct EnvRowResponse {
    /// The name field, so a just-added row can take focus.
    key: egui::Response,
    /// The row's remove button was clicked.
    remove: bool,
}

/// One `KEY = value` row: name, value and a remove button on one line, or on a
/// narrow screen the name and remove button above the value. Every widget gets
/// an exact size (the remove button is a square), so rows line up and Tab goes
/// name, value, remove.
fn env_row_ui(
    ui: &mut egui::Ui,
    row: &mut EnvRow,
    text: &EnvRowText,
    is_narrow: bool,
) -> EnvRowResponse {
    let height = ui.spacing().interact_size.y;
    // The remove button's square plus the gap before it.
    let remove_width = height + ui.spacing().item_spacing.x;
    let key_edit = |ui: &mut egui::Ui, key: &mut String, width: f32| {
        ui.add_sized(
            [width, height],
            egui::TextEdit::singleline(key)
                .hint_text(text.key_hint.as_str())
                .font(egui::TextStyle::Monospace),
        )
    };
    let value_edit = |ui: &mut egui::Ui, value: &mut String, width: f32| {
        ui.add_sized(
            [width, height],
            egui::TextEdit::singleline(value).hint_text(text.value_hint.as_str()),
        )
    };
    let remove_button = |ui: &mut egui::Ui| {
        ui.add_sized([height, height], egui::Button::new("×"))
            .on_hover_text(text.remove_hover.as_str())
            .clicked()
    };

    if is_narrow {
        let response = ui
            .horizontal(|ui| {
                let key = key_edit(ui, &mut row.key, ui.available_width() - remove_width);
                let remove = remove_button(ui);
                EnvRowResponse { key, remove }
            })
            .inner;
        ui.horizontal(|ui| {
            ui.label("=");
            value_edit(ui, &mut row.value, ui.available_width());
        });
        ui.add_space(6.0);
        return response;
    }

    ui.horizontal(|ui| {
        let key_width = (ui.available_width() * 0.45).min(240.0);
        let key = key_edit(ui, &mut row.key, key_width);
        ui.label("=");
        value_edit(ui, &mut row.value, ui.available_width() - remove_width);
        let remove = remove_button(ui);
        EnvRowResponse { key, remove }
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui::{Key, Modifiers};
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;
    use notedeck::test_harness::PressKey;

    /// The panel, the settings it edits, and whatever `overlay_ui` last
    /// returned.
    struct State {
        panel: DaveSettingsPanel,
        settings: DaveSettings,
        i18n: Localization,
        action: Option<SettingsPanelAction>,
    }

    impl State {
        fn new(settings: DaveSettings) -> Self {
            State {
                panel: DaveSettingsPanel::new(),
                settings,
                i18n: Localization::default(),
                action: None,
            }
        }
    }

    /// Draw the settings overlay over `state.settings`, keeping its action.
    fn overlay(ui: &mut egui::Ui, state: &mut State) {
        if let Some(action) = state.panel.overlay_ui(ui, &state.settings, &mut state.i18n) {
            state.action = Some(action);
        }
    }

    /// Settings whose session env has the shape of a real `dave_settings.json`:
    /// an agent key file for headway comments, plus one more entry.
    fn settings_with_env() -> DaveSettings {
        DaveSettings {
            session_env: BTreeMap::from([
                (
                    "HEADWAY_COMMENT_NSEC_FILE".to_string(),
                    "/keys/agent".to_string(),
                ),
                ("RUST_LOG".to_string(), "debug".to_string()),
            ]),
            ..DaveSettings::default()
        }
    }

    /// A settings overlay over default settings, with the leader button
    /// already clicked so it is waiting for a key.
    fn capturing_harness() -> Harness<'static, State> {
        let mut harness = notedeck::test_harness::lenient_builder()
            .build_ui_state(overlay, State::new(DaveSettings::default()));
        harness.run();
        harness.get_by_label("Ctrl+;").click_accesskit();
        harness.run();
        assert!(harness.state().panel.is_capturing_leader());
        harness
    }

    #[test]
    fn click_then_press_rebinds_the_leader() {
        let mut harness = capturing_harness();
        harness.press_key_modifiers(Modifiers::ALT, Key::X);

        let panel = &harness.state().panel;
        assert!(!panel.is_capturing_leader());
        assert_eq!(
            panel.editing.leader_key,
            LeaderKey::from_press(Modifiers::ALT, Key::X)
        );
        assert_eq!(panel.leader_label, "Alt+X");
        // The new binding is only staged: it persists on Save.
        assert!(harness.state().action.is_none());
    }

    #[test]
    fn a_bare_key_is_refused_and_capture_continues() {
        let mut harness = capturing_harness();
        harness.press_key_modifiers(Modifiers::NONE, Key::J);

        let panel = &harness.state().panel;
        assert!(panel.is_capturing_leader());
        assert!(panel.leader_rejected);
        assert_eq!(panel.editing.leader_key, LeaderKey::default());
    }

    #[test]
    fn escape_cancels_the_capture_but_not_the_panel() {
        let mut harness = capturing_harness();
        harness.press_key_modifiers(Modifiers::NONE, Key::Escape);

        let state = harness.state();
        assert!(!state.panel.is_capturing_leader());
        assert!(state.panel.is_open(), "Esc must not close the panel");
        assert!(state.action.is_none());
        assert_eq!(state.panel.editing.leader_key, LeaderKey::default());
    }

    #[test]
    fn ctrl_s_while_capturing_is_recorded_not_saved() {
        let mut harness = capturing_harness();
        harness.press_key_modifiers(Modifiers::CTRL, Key::S);

        let state = harness.state();
        assert!(
            state.action.is_none(),
            "Ctrl+S was the new leader, not Save"
        );
        assert_eq!(
            state.panel.editing.leader_key,
            LeaderKey::from_press(Modifiers::CTRL, Key::S)
        );
    }

    /// Editing, removing and adding session env rows all land in the settings
    /// that Save hands back, and nothing changes until then.
    #[test]
    fn session_env_edits_round_trip_into_saved_settings() {
        let mut harness = notedeck::test_harness::lenient_builder()
            .build_ui_state(overlay, State::new(settings_with_env()));
        harness.run();

        // Edit: replace the key file path.
        let path = harness.get_by(|node| {
            node.role() == Role::TextInput && node.value().as_deref() == Some("/keys/agent")
        });
        path.focus();
        harness.run();
        harness.press_key_modifiers(Modifiers::COMMAND, Key::A);
        harness
            .get_by(|node| node.is_focused())
            .type_text("/keys/jex0");
        harness.run();

        // Remove: rows follow the map's key order, so RUST_LOG is the second.
        harness
            .get_all_by_label("×")
            .nth(1)
            .unwrap()
            .click_accesskit();
        harness.run();

        // Add: the new row's name field takes focus; Tab moves to its value.
        harness.get_by_label("+ Add variable").click_accesskit();
        harness.run();
        harness.get_by(|node| node.is_focused()).type_text("FOO");
        harness.run();
        harness.press_key(Key::Tab);
        harness.run();
        harness
            .get_by(|node| node.is_focused())
            .type_text("bar baz");
        harness.run();

        assert!(harness.state().action.is_none(), "only Save applies edits");
        harness.get_by_label("Save").click_accesskit();
        harness.run();

        let Some(SettingsPanelAction::Save(saved)) = &harness.state().action else {
            panic!("expected Save, got {:?}", harness.state().action);
        };
        assert_eq!(
            saved.session_env,
            BTreeMap::from([
                (
                    "HEADWAY_COMMENT_NSEC_FILE".to_string(),
                    "/keys/jex0".to_string()
                ),
                ("FOO".to_string(), "bar baz".to_string()),
            ])
        );
        // Everything else passes through untouched.
        assert_eq!(saved.leader_key, LeaderKey::default());
        assert_eq!(saved.model, DaveSettings::default().model);
    }

    /// Rows the editor flags are exactly the rows saving drops: blank rows,
    /// a value with no name, names with whitespace or `=`, and repeats (the
    /// first row with a name wins).
    #[test]
    fn flagged_env_rows_are_not_saved() {
        let rows: Vec<EnvRow> = [
            ("", ""),
            ("", "orphan"),
            ("MY VAR", "x"),
            ("A=B", "x"),
            ("KEEP", "first"),
            ("KEEP", "second"),
            ("EMPTY_OK", ""),
        ]
        .into_iter()
        .enumerate()
        .map(|(id, (key, value))| EnvRow {
            id: id as u64,
            key: key.to_string(),
            value: value.to_string(),
        })
        .collect();

        let problems: Vec<_> = (0..rows.len()).map(|i| env_key_problem(&rows, i)).collect();
        assert_eq!(
            problems,
            [
                None,
                Some(EnvKeyProblem::Missing),
                Some(EnvKeyProblem::Malformed),
                Some(EnvKeyProblem::Malformed),
                None,
                Some(EnvKeyProblem::Duplicate),
                None,
            ]
        );
        assert_eq!(
            session_env_from_rows(&rows),
            BTreeMap::from([
                ("KEEP".to_string(), "first".to_string()),
                ("EMPTY_OK".to_string(), String::new()),
            ])
        );
    }

    /// Render the settings overlay at `size` with the fixture env plus one
    /// flagged row, and snapshot it as `name`.
    fn snapshot_session_env(name: &str, size: egui::Vec2) {
        let mut state = State::new(settings_with_env());
        state.panel.open(&state.settings);
        state
            .panel
            .push_env_row("MY VAR".to_string(), "oops".to_string());

        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(size)
            .renderer(notedeck::software_renderer())
            .build_ui_state(overlay, state);
        harness.run();
        harness.snapshot(name);
    }

    /// The session environment section on a desktop-width panel.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_settings_session_env() {
        snapshot_session_env("settings_session_env", egui::vec2(720.0, 560.0));
    }

    /// The session environment section on a phone-width panel, where each
    /// row's value wraps under its name.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_settings_session_env_narrow() {
        snapshot_session_env("settings_session_env_narrow", egui::vec2(380.0, 620.0));
    }
}
