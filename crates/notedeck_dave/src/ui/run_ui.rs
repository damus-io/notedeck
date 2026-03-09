use super::dave::{DaveAction, RunAction};
use crate::config::RunConfig;

/// Renders the named run-config buttons and the "+ Run" button in the status bar.
///
/// Layout: `[+ Run] [▶ name] [▶ name] ...`
///
/// - Click stopped config → `RunAction::Launch { config_id }`
/// - Click running config → `RunAction::Stop { config_id }`
/// - Ctrl+click any config → `RunAction::OpenEdit { cwd, config_id }`
/// - Click "+ Run" → `RunAction::OpenNew { cwd }`
pub fn run_configs_ui(
    configs: &[RunConfig],
    running_ids: Option<&std::collections::HashSet<String>>,
    cwd: &std::path::Path,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let ctrl_held = ui.input(|i| i.modifiers.ctrl);
    let mut action = None;

    ui.spacing_mut().item_spacing.x = 4.0;

    // "+ Run" button — always first
    let plus_btn = egui::Button::new(
        egui::RichText::new("+ Run")
            .monospace()
            .size(11.0)
            .color(ui.visuals().weak_text_color()),
    )
    .fill(egui::Color32::TRANSPARENT)
    .corner_radius(4.0)
    .stroke(egui::Stroke::NONE);

    if ui
        .add(plus_btn)
        .on_hover_text("Add a run configuration")
        .clicked()
    {
        action = Some(DaveAction::Run(RunAction::OpenNew {
            cwd: cwd.to_path_buf(),
        }));
    }

    for cfg in configs {
        let is_running = running_ids.is_some_and(|s| s.contains(&cfg.id));

        let (label, color, tooltip) = if is_running {
            (
                format!("■ {}", cfg.name),
                egui::Color32::from_rgb(200, 60, 60),
                "Stop this process (Ctrl+click to reconfigure)",
            )
        } else {
            (
                format!("▶ {}", cfg.name),
                egui::Color32::from_rgb(60, 180, 60),
                "Run this config (Ctrl+click to edit)",
            )
        };

        let btn = egui::Button::new(
            egui::RichText::new(&label)
                .monospace()
                .size(11.0)
                .color(color),
        )
        .fill(egui::Color32::TRANSPARENT)
        .corner_radius(4.0)
        .stroke(egui::Stroke::NONE);

        let resp = ui.add(btn).on_hover_text(tooltip);

        if resp.clicked() {
            if ctrl_held {
                action = Some(DaveAction::Run(RunAction::OpenEdit {
                    cwd: cwd.to_path_buf(),
                    config_id: cfg.id.clone(),
                }));
            } else if is_running {
                action = Some(DaveAction::Run(RunAction::Stop {
                    config_id: cfg.id.clone(),
                }));
            } else {
                action = Some(DaveAction::Run(RunAction::Launch {
                    config_id: cfg.id.clone(),
                }));
            }
        }
    }

    action
}
