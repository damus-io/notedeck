use crate::ui::keybind_hint::paint_keybind_hint;
use egui::{RichText, Vec2};

/// Actions from the host picker overlay.
#[derive(Debug, Clone)]
pub enum HostPickerAction {
    /// User picked a host. `None` = local, `Some(hostname)` = remote.
    HostSelected(Option<String>),
    /// User cancelled.
    Cancelled,
}

/// Render the host picker as a full-panel overlay.
///
/// `known_hosts` should contain all remote hostnames (not the local one).
/// The local machine is always shown as the first option.
pub fn host_picker_overlay_ui(
    ui: &mut egui::Ui,
    local_hostname: &str,
    known_hosts: &[String],
    has_sessions: bool,
) -> Option<HostPickerAction> {
    let mut action = None;
    let is_narrow = notedeck::ui::is_narrow(ui.ctx());
    let ctrl_held = ui.input(|i| i.modifiers.ctrl);

    // Keyboard shortcuts: Ctrl+1 = local, Ctrl+2..9 = remote hosts
    if ctrl_held {
        if ui.input(|i| i.key_pressed(egui::Key::Num1)) {
            return Some(HostPickerAction::HostSelected(None));
        }
        for (idx, host) in known_hosts.iter().take(8).enumerate() {
            let key = match idx {
                0 => egui::Key::Num2,
                1 => egui::Key::Num3,
                2 => egui::Key::Num4,
                3 => egui::Key::Num5,
                4 => egui::Key::Num6,
                5 => egui::Key::Num7,
                6 => egui::Key::Num8,
                7 => egui::Key::Num9,
                _ => continue,
            };
            if ui.input(|i| i.key_pressed(key)) {
                return Some(HostPickerAction::HostSelected(Some(host.clone())));
            }
        }
    }

    egui::Frame::new()
        .fill(ui.visuals().panel_fill)
        .inner_margin(egui::Margin::symmetric(if is_narrow { 16 } else { 40 }, 20))
        .show(ui, |ui| {
            // Header
            ui.horizontal(|ui| {
                if has_sessions {
                    if ui.button("< Back").clicked() {
                        action = Some(HostPickerAction::Cancelled);
                    }
                    ui.add_space(16.0);
                }
                ui.heading("Select Host");
            });

            ui.add_space(16.0);

            let max_content_width = if is_narrow {
                ui.available_width()
            } else {
                500.0
            };
            let available_height = ui.available_height();

            ui.allocate_ui_with_layout(
                egui::vec2(max_content_width, available_height),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    let button_height = if is_narrow { 44.0 } else { 32.0 };
                    let hint_width = if ctrl_held { 24.0 } else { 0.0 };
                    let button_width = ui.available_width() - hint_width - 4.0;

                    // Local option
                    ui.horizontal(|ui| {
                        let button = egui::Button::new(
                            RichText::new(format!("{} (local)", local_hostname)).monospace(),
                        )
                        .min_size(Vec2::new(button_width, button_height))
                        .fill(ui.visuals().widgets.inactive.weak_bg_fill);

                        let response = ui.add(button);

                        if ctrl_held {
                            let hint_center = response.rect.right_center()
                                + egui::vec2(hint_width / 2.0 + 2.0, 0.0);
                            paint_keybind_hint(ui, hint_center, "1", 18.0);
                        }

                        if response.clicked() {
                            action = Some(HostPickerAction::HostSelected(None));
                        }
                    });

                    ui.add_space(4.0);

                    // Remote hosts
                    for (idx, host) in known_hosts.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let button =
                                egui::Button::new(RichText::new(host.as_str()).monospace())
                                    .min_size(Vec2::new(button_width, button_height))
                                    .fill(ui.visuals().widgets.inactive.weak_bg_fill);

                            let response = ui.add(button);

                            if ctrl_held && idx < 8 {
                                let hint_text = format!("{}", idx + 2);
                                let hint_center = response.rect.right_center()
                                    + egui::vec2(hint_width / 2.0 + 2.0, 0.0);
                                paint_keybind_hint(ui, hint_center, &hint_text, 18.0);
                            }

                            if response.clicked() {
                                action = Some(HostPickerAction::HostSelected(Some(host.clone())));
                            }
                        });

                        ui.add_space(4.0);
                    }
                },
            );
        });

    // Escape to cancel
    if has_sessions && ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        action = Some(HostPickerAction::Cancelled);
    }

    action
}
