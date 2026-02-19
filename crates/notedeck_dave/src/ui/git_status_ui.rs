use crate::git_status::GitStatusCache;
use egui::{Color32, RichText, Ui};

const MODIFIED_COLOR: Color32 = Color32::from_rgb(200, 170, 50);
const ADDED_COLOR: Color32 = Color32::from_rgb(60, 180, 60);
const DELETED_COLOR: Color32 = Color32::from_rgb(200, 60, 60);
const UNTRACKED_COLOR: Color32 = Color32::from_rgb(128, 128, 128);

/// Snapshot of git status data extracted from the cache to avoid
/// borrow conflicts when mutating `cache.expanded`.
pub struct StatusSnapshot {
    branch: Option<String>,
    modified: usize,
    added: usize,
    deleted: usize,
    untracked: usize,
    is_clean: bool,
    files: Vec<(String, String)>, // (status, path)
}

impl StatusSnapshot {
    pub fn from_cache(cache: &GitStatusCache) -> Option<Result<Self, ()>> {
        match cache.current() {
            Some(Ok(data)) => Some(Ok(StatusSnapshot {
                branch: data.branch.clone(),
                modified: data.modified_count(),
                added: data.added_count(),
                deleted: data.deleted_count(),
                untracked: data.untracked_count(),
                is_clean: data.is_clean(),
                files: data
                    .files
                    .iter()
                    .map(|f| (f.status.clone(), f.path.clone()))
                    .collect(),
            })),
            Some(Err(_)) => Some(Err(())),
            None => None,
        }
    }
}

fn count_label(ui: &mut Ui, prefix: &str, count: usize, color: Color32) {
    if count > 0 {
        ui.label(
            RichText::new(format!("{}{}", prefix, count))
                .color(color)
                .monospace()
                .size(11.0),
        );
    }
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

/// Render the left-side git status content (expand arrow, branch, counts).
pub fn git_status_content_ui(
    cache: &mut GitStatusCache,
    snapshot: &Option<Result<StatusSnapshot, ()>>,
    ui: &mut Ui,
) {
    match snapshot {
        Some(Ok(snap)) => {
            // Show expand arrow only when dirty
            if !snap.is_clean {
                let arrow = if cache.expanded {
                    "\u{25BC}"
                } else {
                    "\u{25B6}"
                };
                if ui
                    .add(
                        egui::Label::new(RichText::new(arrow).weak().monospace().size(9.0))
                            .sense(egui::Sense::click()),
                    )
                    .clicked()
                {
                    cache.expanded = !cache.expanded;
                }
            }

            // Branch name
            let branch_text = snap.branch.as_deref().unwrap_or("detached");
            ui.label(RichText::new(branch_text).weak().monospace().size(11.0));

            if snap.is_clean {
                ui.label(RichText::new("clean").weak().size(11.0));
            } else {
                count_label(ui, "~", snap.modified, MODIFIED_COLOR);
                count_label(ui, "+", snap.added, ADDED_COLOR);
                count_label(ui, "-", snap.deleted, DELETED_COLOR);
                count_label(ui, "?", snap.untracked, UNTRACKED_COLOR);
            }
        }
        Some(Err(_)) => {
            ui.label(RichText::new("git: not available").weak().size(11.0));
        }
        None => {
            ui.spinner();
            ui.label(RichText::new("checking git...").weak().size(11.0));
        }
    }
}

/// Render the expanded file list portion of git status.
pub fn git_expanded_files_ui(
    cache: &GitStatusCache,
    snapshot: &Option<Result<StatusSnapshot, ()>>,
    ui: &mut Ui,
) {
    if cache.expanded {
        if let Some(Ok(snap)) = snapshot {
            if !snap.files.is_empty() {
                ui.add_space(4.0);

                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .corner_radius(4.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(150.0)
                            .show(ui, |ui| {
                                for (status, path) in &snap.files {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 8.0;
                                        let color = status_color(status);
                                        ui.label(
                                            RichText::new(status)
                                                .monospace()
                                                .size(11.0)
                                                .color(color),
                                        );
                                        ui.label(RichText::new(path).monospace().size(11.0).weak());
                                    });
                                }
                            });
                    });
            }
        }
    }
}
