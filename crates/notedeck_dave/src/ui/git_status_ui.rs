use crate::git_status::GitStatusCache;
use egui::{Color32, RichText, Ui};

const MODIFIED_COLOR: Color32 = Color32::from_rgb(200, 170, 50);
const ADDED_COLOR: Color32 = Color32::from_rgb(60, 180, 60);
const DELETED_COLOR: Color32 = Color32::from_rgb(200, 60, 60);
const UNTRACKED_COLOR: Color32 = Color32::from_rgb(128, 128, 128);
const CONFLICT_COLOR: Color32 = Color32::from_rgb(220, 120, 40);

/// Snapshot of git status data extracted from the cache to avoid
/// borrow conflicts when mutating `cache.expanded`.
pub struct StatusSnapshot {
    branch: Option<String>,
    modified: usize,
    added: usize,
    deleted: usize,
    untracked: usize,
    /// Total inserted lines vs HEAD (line churn, not file count).
    added_lines: usize,
    /// Total deleted lines vs HEAD (line churn, not file count).
    deleted_lines: usize,
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
                added_lines: data.added_lines,
                deleted_lines: data.deleted_lines,
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

fn is_conflict(status: &str) -> bool {
    matches!(status.trim(), "UU" | "UD" | "DU" | "AA" | "DD")
}

fn status_color(status: &str) -> Color32 {
    if is_conflict(status) {
        CONFLICT_COLOR
    } else if status.starts_with('?') {
        UNTRACKED_COLOR
    } else if status.contains('D') {
        DELETED_COLOR
    } else if status.contains('A') || status.contains('C') {
        ADDED_COLOR
    } else {
        MODIFIED_COLOR
    }
}

/// Maps git porcelain XY status codes to compact UI symbols:
/// - `?` → `○` (untracked)
/// - `A` → `+` (added), `C` → `+` (copied, effectively added)
/// - `D` → `-` (deleted)
/// - `UU`/`UD`/`DU`/`AA`/`DD` → `⚠` (merge conflict)
/// - `M`, `R`, `T`, and others → `●` (modified)
fn status_symbol(status: &str) -> &'static str {
    if is_conflict(status) {
        "⚠"
    } else if status.starts_with('?') {
        "○"
    } else if status.contains('D') {
        "-"
    } else if status.contains('A') || status.contains('C') {
        "+"
    } else {
        "●"
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
                count_label(ui, "●", snap.modified, MODIFIED_COLOR);
                count_label(ui, "+", snap.added, ADDED_COLOR);
                count_label(ui, "-", snap.deleted, DELETED_COLOR);
                count_label(ui, "○", snap.untracked, UNTRACKED_COLOR);

                // Line churn vs HEAD (distinct from the file counts above), set
                // off by a separator so the +/- here reads as lines, not files.
                if snap.added_lines > 0 || snap.deleted_lines > 0 {
                    ui.separator();
                    count_label(ui, "+", snap.added_lines, ADDED_COLOR);
                    count_label(ui, "-", snap.deleted_lines, DELETED_COLOR);
                }
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
                                            RichText::new(status_symbol(status))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_status::GitStatusCache;
    use egui_kittest::Harness;
    use std::path::PathBuf;

    /// Render the git status content line in isolation so the layout — branch,
    /// file counts, and the line-churn group set off by a separator — can be
    /// verified visually. A leading `ui.separator()` mirrors the real toolbar
    /// (the divider after the Run button) so both separators can be compared.
    #[allow(clippy::too_many_arguments)]
    fn status_line_harness(
        modified: usize,
        added: usize,
        deleted: usize,
        untracked: usize,
        added_lines: usize,
        deleted_lines: usize,
    ) -> Harness<'static> {
        Harness::builder()
            .with_size(egui::Vec2::new(360.0, 28.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                let snap = StatusSnapshot {
                    branch: Some("dave".to_string()),
                    modified,
                    added,
                    deleted,
                    untracked,
                    added_lines,
                    deleted_lines,
                    is_clean: false,
                    files: Vec::new(),
                };
                let mut cache = GitStatusCache::new(PathBuf::from("/tmp/project"));
                let snapshot = Some(Ok(snap));
                ui.horizontal(|ui| {
                    ui.label("Run");
                    ui.separator();
                    git_status_content_ui(&mut cache, &snapshot, ui);
                });
            })
    }

    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_git_status_line_churn() {
        let mut harness = status_line_harness(2, 0, 0, 1, 142, 37);
        harness.run();
        harness.snapshot("git_status_line_churn");
    }
}
