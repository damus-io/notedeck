use crate::config::{AiProvider, DaveSettings};

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

    /// Render the settings panel UI
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<SettingsPanelAction> {
        let mut action: Option<SettingsPanelAction> = None;

        if !self.open {
            return None;
        }

        let mut open = self.open;
        egui::Window::new("Dave Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_min_width(300.0);

                egui::Grid::new("settings_grid")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
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

                ui.add_space(16.0);

                // Save/Cancel buttons
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        action = Some(SettingsPanelAction::Save(self.editing.clone()));
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(SettingsPanelAction::Cancel);
                    }
                });
            });

        // Handle window close button
        if !open {
            action = Some(SettingsPanelAction::Cancel);
        }

        // Close panel if we have an action
        if action.is_some() {
            self.close();
        }

        action
    }
}
