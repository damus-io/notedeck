use crate::config::RunConfig;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// State for the run-config editor overlay.
/// All fields are private; construct via `new_config` or `edit_config`.
pub struct RunConfigEditor {
    cwd: PathBuf,
    /// If Some(id), we are editing the existing config with this stable UUID.
    /// If None, we are creating a new config.
    edit_id: Option<String>,
    name: String,
    command: String,
    /// Existing run configs to suggest in the editor.
    suggestions: Vec<RunConfig>,
}

/// Action returned by the run-config editor overlay.
pub(crate) enum RunConfigEditorAction {
    Added { cwd: PathBuf, config: RunConfig },
    Updated { cwd: PathBuf, config: RunConfig },
    Deleted { cwd: PathBuf, config_id: String },
    Cancelled,
}

/// What changed after processing an editor action.
pub(crate) enum RunConfigChange {
    /// A config was added or updated — publish it.
    Saved { cwd: PathBuf, config: RunConfig },
    /// A config was deleted — publish a tombstone and kill its process.
    Deleted { cwd: PathBuf, config_id: String },
    /// Nothing changed.
    None,
}

/// Collect all existing run configs as suggestions, deduplicated by
/// `(name, command)`. Optionally excludes a specific config ID when editing.
pub(crate) fn collect_run_config_suggestions(
    run_configs: &HashMap<PathBuf, Vec<RunConfig>>,
    exclude_id: Option<&str>,
) -> Vec<RunConfig> {
    let mut seen = HashSet::new();
    let mut suggestions = Vec::new();

    for configs in run_configs.values() {
        for cfg in configs {
            if exclude_id == Some(cfg.id.as_str()) {
                continue;
            }
            if seen.insert((cfg.name.clone(), cfg.command.clone())) {
                suggestions.push(cfg.clone());
            }
        }
    }

    RunConfig::sort_by_name(&mut suggestions);
    suggestions
}

impl RunConfigEditorAction {
    /// Apply this action to the run config state map.
    /// Returns what changed so the caller can publish and clean up processes.
    pub(crate) fn process(
        self,
        run_configs: &mut HashMap<PathBuf, Vec<RunConfig>>,
    ) -> RunConfigChange {
        match self {
            Self::Added { cwd, config } => {
                let configs = run_configs.entry(cwd.clone()).or_default();
                configs.push(config.clone());
                RunConfig::sort_by_name(configs);
                RunConfigChange::Saved { cwd, config }
            }
            Self::Updated { cwd, config } => {
                let found = run_configs
                    .get_mut(&cwd)
                    .and_then(|configs| {
                        let existing = configs.iter_mut().find(|c| c.id == config.id)?;
                        existing.name.clone_from(&config.name);
                        existing.command.clone_from(&config.command);
                        RunConfig::sort_by_name(configs);
                        Some(())
                    })
                    .is_some();
                if found {
                    RunConfigChange::Saved { cwd, config }
                } else {
                    RunConfigChange::None
                }
            }
            Self::Deleted { cwd, config_id } => {
                if let Some(configs) = run_configs.get_mut(&cwd) {
                    configs.retain(|c| c.id != config_id);
                }
                RunConfigChange::Deleted { cwd, config_id }
            }
            Self::Cancelled => RunConfigChange::None,
        }
    }
}

impl RunConfigEditor {
    pub fn new_config(cwd: PathBuf, suggestions: Vec<RunConfig>) -> Self {
        Self {
            cwd,
            edit_id: None,
            name: String::new(),
            command: String::new(),
            suggestions,
        }
    }

    pub fn edit_config(cwd: PathBuf, config: RunConfig, suggestions: Vec<RunConfig>) -> Self {
        Self {
            cwd,
            edit_id: Some(config.id),
            name: config.name,
            command: config.command,
            suggestions,
        }
    }

    fn is_editing(&self) -> bool {
        self.edit_id.is_some()
    }

    fn try_confirm(&self) -> Option<RunConfigEditorAction> {
        let name = self.name.trim().to_string();
        let command = self.command.trim().to_string();
        if name.is_empty() || command.is_empty() {
            return None;
        }
        let cwd = self.cwd.clone();
        match &self.edit_id {
            None => {
                let config = RunConfig::new(name, command);
                Some(RunConfigEditorAction::Added { cwd, config })
            }
            Some(id) => Some(RunConfigEditorAction::Updated {
                cwd,
                config: RunConfig {
                    id: id.clone(),
                    name,
                    command,
                    updated_at: 0,
                },
            }),
        }
    }

    fn can_save(&self) -> bool {
        !self.name.trim().is_empty() && !self.command.trim().is_empty()
    }
}

/// Render the run-config editor as a full-screen overlay.
/// Returns `Some(action)` when the user confirms, cancels, or deletes;
/// returns `None` to keep the overlay open.
pub(crate) fn run_config_editor_overlay_ui(
    editor: &mut RunConfigEditor,
    ui: &mut egui::Ui,
) -> Option<RunConfigEditorAction> {
    let is_narrow = notedeck::ui::is_narrow(ui.ctx());
    let mut action: Option<RunConfigEditorAction> = None;

    egui::Frame::new()
        .fill(ui.visuals().panel_fill)
        .inner_margin(egui::Margin::symmetric(if is_narrow { 16 } else { 40 }, 20))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("< Back").clicked() {
                    action = Some(RunConfigEditorAction::Cancelled);
                }
                let title = if editor.is_editing() {
                    "Edit Run Config"
                } else {
                    "New Run Config"
                };
                ui.heading(title);
            });

            ui.add_space(12.0);

            let max_width = if is_narrow {
                ui.available_width()
            } else {
                500.0_f32.min(ui.available_width())
            };

            ui.allocate_ui_with_layout(
                egui::vec2(max_width, ui.available_height()),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    egui::Grid::new("run_config_grid")
                        .num_columns(2)
                        .spacing([10.0, 12.0])
                        .show(ui, |ui| {
                            ui.label("Name:");
                            let name_resp = ui.add(
                                egui::TextEdit::singleline(&mut editor.name)
                                    .hint_text("e.g. server, tests, build")
                                    .desired_width(f32::INFINITY),
                            );
                            if editor.name.is_empty() && !editor.is_editing() {
                                name_resp.request_focus();
                            }
                            ui.end_row();

                            ui.label("Command:");
                            let cmd_resp = ui.add(
                                egui::TextEdit::singleline(&mut editor.command)
                                    .font(egui::TextStyle::Monospace)
                                    .hint_text("e.g. cargo run, npm start")
                                    .desired_width(f32::INFINITY),
                            );
                            if cmd_resp.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                action = editor.try_confirm();
                            }
                            ui.end_row();
                        });

                    // Suggestions section
                    if !editor.suggestions.is_empty() {
                        ui.add_space(16.0);
                        ui.label(
                            egui::RichText::new("Suggestions")
                                .color(ui.visuals().weak_text_color()),
                        );
                        ui.add_space(4.0);

                        let text_color = ui.visuals().text_color();
                        let dim = ui.visuals().weak_text_color();
                        for suggestion in &editor.suggestions {
                            let mut job = egui::text::LayoutJob::default();
                            job.append(
                                &suggestion.name,
                                0.0,
                                egui::TextFormat {
                                    font_id: egui::FontId::proportional(12.0),
                                    color: text_color,
                                    ..Default::default()
                                },
                            );
                            job.append(
                                &suggestion.command,
                                8.0,
                                egui::TextFormat {
                                    font_id: egui::FontId::monospace(11.0),
                                    color: dim,
                                    ..Default::default()
                                },
                            );

                            if ui.add(egui::SelectableLabel::new(false, job)).clicked() {
                                editor.name.clone_from(&suggestion.name);
                                editor.command.clone_from(&suggestion.command);
                            }
                        }
                    }

                    ui.add_space(16.0);

                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        action = Some(RunConfigEditorAction::Cancelled);
                    }

                    ui.horizontal(|ui| {
                        if let Some(ref id) = editor.edit_id {
                            let delete_btn = egui::Button::new(
                                egui::RichText::new("Delete")
                                    .color(egui::Color32::from_rgb(200, 60, 60)),
                            );
                            if ui.add(delete_btn).clicked() {
                                action = Some(RunConfigEditorAction::Deleted {
                                    cwd: editor.cwd.clone(),
                                    config_id: id.clone(),
                                });
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(editor.can_save(), egui::Button::new("Save"))
                                .clicked()
                            {
                                action = editor.try_confirm();
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(RunConfigEditorAction::Cancelled);
                            }
                        });
                    });
                },
            );
        });

    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    struct EditorHarnessState {
        editor: RunConfigEditor,
        action: Option<RunConfigEditorAction>,
    }

    fn test_config(id: &str, name: &str, command: &str) -> RunConfig {
        RunConfig {
            id: id.to_owned(),
            name: name.to_owned(),
            command: command.to_owned(),
            updated_at: 0,
        }
    }

    #[test]
    fn collect_suggestions_dedupes_sorts_and_excludes_current_config() {
        let run_configs = HashMap::from([
            (
                PathBuf::from("/tmp/app-a"),
                vec![
                    test_config("1", "Build", "cargo build"),
                    test_config("2", "Test", "cargo test"),
                ],
            ),
            (
                PathBuf::from("/tmp/app-b"),
                vec![
                    test_config("3", "Build", "cargo build"),
                    test_config("4", "Build", "cargo build --release"),
                    test_config("5", "Alpha", "echo alpha"),
                ],
            ),
        ]);

        let suggestions = collect_run_config_suggestions(&run_configs, Some("2"));

        let names: Vec<_> = suggestions.iter().map(|cfg| cfg.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Build", "Build"]);

        let pairs: HashSet<_> = suggestions
            .iter()
            .map(|cfg| (cfg.name.as_str(), cfg.command.as_str()))
            .collect();
        assert_eq!(
            pairs,
            HashSet::from([
                ("Alpha", "echo alpha"),
                ("Build", "cargo build"),
                ("Build", "cargo build --release"),
            ])
        );
        assert!(
            suggestions.iter().all(|cfg| cfg.id != "2"),
            "the config being edited should not be suggested"
        );
    }

    #[test]
    fn edit_mode_suggestion_save_preserves_existing_config_id() {
        let mut harness = Harness::new_ui_state(
            |ui, state: &mut EditorHarnessState| {
                if let Some(action) = run_config_editor_overlay_ui(&mut state.editor, ui) {
                    state.action = Some(action);
                }
            },
            EditorHarnessState {
                editor: RunConfigEditor::edit_config(
                    PathBuf::from("/tmp/project"),
                    test_config("edit-id", "Test", "cargo test"),
                    vec![test_config("1", "Build", "cargo build --workspace")],
                ),
                action: None,
            },
        );

        harness.run();
        harness
            .get_by_label_contains("cargo build --workspace")
            .click();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        assert_eq!(harness.state().editor.name, "Build");
        assert_eq!(harness.state().editor.command, "cargo build --workspace");

        match harness.state().action.as_ref() {
            Some(RunConfigEditorAction::Updated { cwd, config }) => {
                assert_eq!(cwd, &PathBuf::from("/tmp/project"));
                assert_eq!(config.id, "edit-id");
                assert_eq!(config.name, "Build");
                assert_eq!(config.command, "cargo build --workspace");
            }
            _ => panic!("expected Updated action"),
        }
    }
}
