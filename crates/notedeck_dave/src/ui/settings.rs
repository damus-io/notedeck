use crate::config::{AiProvider, DaveSettings, LeaderKey};
use crate::ui::keybind_hint::keybind_hint;
use notedeck::{tr, Localization};

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
        self.open = true;
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
            action = Some(SettingsPanelAction::Save(self.editing.clone()));
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
                                action = Some(SettingsPanelAction::Save(self.editing.clone()));
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;

    /// The panel plus whatever `overlay_ui` last returned.
    struct State {
        panel: DaveSettingsPanel,
        i18n: Localization,
        action: Option<SettingsPanelAction>,
    }

    /// A settings overlay over default settings, with the leader button
    /// already clicked so it is waiting for a key.
    fn capturing_harness() -> Harness<'static, State> {
        let mut harness = Harness::new_ui_state(
            |ui, state: &mut State| {
                if let Some(action) =
                    state
                        .panel
                        .overlay_ui(ui, &DaveSettings::default(), &mut state.i18n)
                {
                    state.action = Some(action);
                }
            },
            State {
                panel: DaveSettingsPanel::new(),
                i18n: Localization::default(),
                action: None,
            },
        );
        harness.run();
        harness.get_by_label("Ctrl+;").click();
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
}
