use crate::config::{AiProvider, DaveSettings};
use crate::ui::keybind_hint::keybind_hint;

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
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
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
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Prepare editing state for overlay mode
    pub fn prepare_edit(&mut self, current: &DaveSettings) {
        if !self.open {
            self.editing = current.clone();
            self.custom_model = current.model.clone();
            self.use_custom_model = !current
                .provider
                .available_models()
                .contains(&current.model.as_str());
            self.open = true;
        }
    }

    /// Render settings as a full-panel overlay (replaces the main content)
    pub fn overlay_ui(
        &mut self,
        ui: &mut egui::Ui,
        current: &DaveSettings,
    ) -> Option<SettingsPanelAction> {
        // Initialize editing state if not already set
        self.prepare_edit(current);

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
                        self.settings_form(ui);

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
    fn settings_form(&mut self, ui: &mut egui::Ui) {
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
            });
    }
}
