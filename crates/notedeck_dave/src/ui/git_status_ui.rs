use crate::git_status::GitStatusCache;
use egui::{Color32, RichText, Ui};

const MODIFIED_COLOR: Color32 = Color32::from_rgb(200, 170, 50);
const ADDED_COLOR: Color32 = Color32::from_rgb(60, 180, 60);
const DELETED_COLOR: Color32 = Color32::from_rgb(200, 60, 60);
const UNTRACKED_COLOR: Color32 = Color32::from_rgb(128, 128, 128);

/// Render the git status bar. Call `cache.request_refresh()` externally if needed.
pub fn git_status_bar_ui(cache: &mut GitStatusCache, ui: &mut Ui) {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .corner_radius(6.0)
        .show(ui, |ui| {
            // vertical() forces top-down ordering inside this frame,
            // preventing the parent's bottom_up layout from pushing the
            // expanded file list above the header into the chat area.
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    // Expand/collapse toggle
                    let arrow = if cache.expanded { "\u{25BC}" } else { "\u{25B6}" };
                    if ui.small_button(arrow).clicked() {
                        cache.expanded = !cache.expanded;
                    }

                    match cache.current() {
                        Some(Ok(data)) => {
                            // Branch name
                            if let Some(branch) = &data.branch {
                                ui.label(
                                    RichText::new(format!("git: {}", branch))
                                        .monospace()
                                        .size(11.0),
                                );
                            } else {
                                ui.label(
                                    RichText::new("git: detached").monospace().size(11.0),
                                );
                            }

                            if data.is_clean() {
                                ui.label(RichText::new("clean").weak().size(11.0));
                            } else {
                                let m = data.modified_count();
                                let a = data.added_count();
                                let d = data.deleted_count();
                                let u = data.untracked_count();
                                if m > 0 {
                                    ui.label(
                                        RichText::new(format!("~{}", m))
                                            .color(MODIFIED_COLOR)
                                            .monospace()
                                            .size(11.0),
                                    );
                                }
                                if a > 0 {
                                    ui.label(
                                        RichText::new(format!("+{}", a))
                                            .color(ADDED_COLOR)
                                            .monospace()
                                            .size(11.0),
                                    );
                                }
                                if d > 0 {
                                    ui.label(
                                        RichText::new(format!("-{}", d))
                                            .color(DELETED_COLOR)
                                            .monospace()
                                            .size(11.0),
                                    );
                                }
                                if u > 0 {
                                    ui.label(
                                        RichText::new(format!("?{}", u))
                                            .color(UNTRACKED_COLOR)
                                            .monospace()
                                            .size(11.0),
                                    );
                                }
                            }
                        }
                        Some(Err(_)) => {
                            ui.label(
                                RichText::new("git: not available").weak().size(11.0),
                            );
                        }
                        None => {
                            ui.spinner();
                            ui.label(
                                RichText::new("checking git...").weak().size(11.0),
                            );
                        }
                    }

                    // Refresh button (right-aligned)
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui
                                .small_button("\u{21BB}")
                                .on_hover_text("Refresh git status")
                                .clicked()
                            {
                                cache.request_refresh();
                            }
                        },
                    );
                });

                // Expanded file list
                if cache.expanded {
                    if let Some(Ok(data)) = cache.current() {
                        if !data.files.is_empty() {
                            ui.add_space(4.0);
                            egui::ScrollArea::vertical()
                                .max_height(150.0)
                                .show(ui, |ui| {
                                    for entry in &data.files {
                                        ui.horizontal(|ui| {
                                            let color = status_color(&entry.status);
                                            ui.label(
                                                RichText::new(&entry.status)
                                                    .monospace()
                                                    .size(11.0)
                                                    .color(color),
                                            );
                                            ui.label(
                                                RichText::new(&entry.path)
                                                    .monospace()
                                                    .size(11.0),
                                            );
                                        });
                                    }
                                });
                        }
                    }
                }
            });
        });
}

fn status_color(status: &str) -> Color32 {
    if status.starts_with('?') {
        UNTRACKED_COLOR
    } else if status.contains('D') {
        DELETED_COLOR
    } else if status.contains('A') {
        ADDED_COLOR
    } else {
        MODIFIED_COLOR
    }
}
