use crate::git_status::GitStatusCache;
use agentium_core::inspector_snapshot::GitStatus;
use egui::{Color32, RichText, Ui};

const MODIFIED_COLOR: Color32 = Color32::from_rgb(200, 170, 50);
const ADDED_COLOR: Color32 = Color32::from_rgb(60, 180, 60);
const DELETED_COLOR: Color32 = Color32::from_rgb(200, 60, 60);
const UNTRACKED_COLOR: Color32 = Color32::from_rgb(128, 128, 128);
const CONFLICT_COLOR: Color32 = Color32::from_rgb(220, 120, 40);

/// Longest branch name (in bytes) shown in the status line before
/// middle-truncation kicks in, so a pathological branch name can't crowd
/// the rest of the toolbar into a vertical character wrap.
const MAX_BRANCH_LEN: usize = 24;

/// Middle-truncate a branch name to roughly [`MAX_BRANCH_LEN`], keeping the
/// head and tail (ticket prefixes and distinguishing suffixes both survive).
fn abbreviate_branch(branch: &str) -> String {
    if branch.len() <= MAX_BRANCH_LEN {
        return branch.to_string();
    }
    let head = MAX_BRANCH_LEN / 2;
    let tail = MAX_BRANCH_LEN - head - 1;
    let head_end = notedeck::abbrev::floor_char_boundary(branch, head);
    let tail_start = notedeck::abbrev::ceil_char_boundary(branch, branch.len() - tail);
    format!("{}…{}", &branch[..head_end], &branch[tail_start..])
}

/// Snapshot of git status data extracted from the cache to avoid
/// borrow conflicts when mutating `cache.expanded`.
pub struct StatusSnapshot {
    branch: Option<String>,
    /// Total inserted lines vs HEAD (line churn, not file count).
    added_lines: usize,
    /// Total deleted lines vs HEAD (line churn, not file count).
    deleted_lines: usize,
    /// Every changed path, untracked included.
    changed: usize,
    is_clean: bool,
    files: Vec<(String, String)>, // (status, path)
}

impl StatusSnapshot {
    pub fn from_cache(cache: &GitStatusCache) -> Option<Result<Self, ()>> {
        let tree = match cache.current()? {
            GitStatus::WorkTree(tree) => tree,
            GitStatus::NotARepo | GitStatus::Failed(_) => return Some(Err(())),
        };
        Some(Ok(StatusSnapshot {
            branch: tree.branch.clone(),
            added_lines: tree.added_lines,
            deleted_lines: tree.deleted_lines,
            changed: tree.changed,
            is_clean: tree.is_clean(),
            files: tree
                .files
                .iter()
                .map(|f| (f.status.clone(), f.path.clone()))
                .collect(),
        }))
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

/// Render the left-side git status content: the branch, then the line churn
/// against HEAD, the same `+N -M` the session list shows. A dirty tree's
/// branch carries a chevron that opens the changed files; the per-kind file
/// counts live in that list, not on the bar. A tree changed only by files
/// git doesn't track yet, which have no lines to count, says how many.
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

            // Branch name, abbreviated so long names can't break the layout
            let branch_text = snap.branch.as_deref().unwrap_or("detached");
            let abbreviated = abbreviate_branch(branch_text);
            let response = ui.label(RichText::new(&abbreviated).weak().monospace().size(11.0));
            if abbreviated != branch_text {
                response.on_hover_text(branch_text);
            }

            if snap.is_clean {
                return;
            }
            if snap.added_lines > 0 || snap.deleted_lines > 0 {
                count_label(ui, "+", snap.added_lines, ADDED_COLOR);
                count_label(ui, "-", snap.deleted_lines, DELETED_COLOR);
            } else {
                ui.label(
                    RichText::new(format!("{} changed", snap.changed))
                        .weak()
                        .size(11.0),
                );
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

    /// Render the git status content line in isolation so its layout —
    /// chevron, branch and line churn — can be verified visually. A leading
    /// `ui.separator()` mirrors the real toolbar (the divider after the Run
    /// button). `render` gives it lavapipe, for a snapshot.
    fn status_line_harness(
        branch: &str,
        changed: usize,
        added_lines: usize,
        deleted_lines: usize,
        render: bool,
    ) -> Harness<'static> {
        let branch = branch.to_string();
        let mut builder = Harness::builder().with_size(egui::Vec2::new(360.0, 28.0));
        if render {
            builder = builder.renderer(notedeck::software_renderer());
        }
        notedeck::test_harness::with_notedeck_style(builder.build_ui(move |ui| {
            let snap = StatusSnapshot {
                branch: Some(branch.clone()),
                added_lines,
                deleted_lines,
                changed,
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
        }))
    }

    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_git_status_line_churn() {
        let mut harness = status_line_harness("dave", 3, 142, 37, true);
        harness.run();
        harness.snapshot("git_status_line_churn");
    }

    /// A pathological branch name must be middle-truncated so the churn to
    /// its right stays on the status line instead of wrapping.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_git_status_long_branch() {
        let mut harness = status_line_harness(
            "rental-614-requirements-generalize-evaluateonboarding-derived-gate",
            3,
            142,
            37,
            true,
        );
        harness.run();
        harness.snapshot("git_status_long_branch");
    }

    /// A tree changed only by untracked files has no lines to count, so the
    /// bar says how many files changed instead.
    #[test]
    fn untracked_only_tree_counts_files() {
        use egui_kittest::kittest::Queryable;
        let mut harness = status_line_harness("dave", 2, 0, 0, false);
        harness.run();
        let _ = harness.get_by_label("2 changed");
    }

    #[test]
    fn short_branch_unchanged() {
        assert_eq!(abbreviate_branch("dave"), "dave");
        // Exactly at the limit stays untouched.
        let at_limit = "a".repeat(MAX_BRANCH_LEN);
        assert_eq!(abbreviate_branch(&at_limit), at_limit);
    }

    #[test]
    fn long_branch_middle_truncated() {
        let branch = "rental-614-requirements-generalize-evaluateonboarding-derived-gate";
        let abbreviated = abbreviate_branch(branch);
        assert_eq!(abbreviated, "rental-614-r…erived-gate");
        assert_eq!(abbreviated.chars().count(), MAX_BRANCH_LEN);
    }

    #[test]
    fn multibyte_branch_truncates_on_char_boundaries() {
        // 40 chars of 3 bytes each. The head cut at byte 12 already lands on a
        // boundary, but the tail cut at byte 120 - 11 = 109 falls inside a char
        // and has to be ceiled to 111 — so the tail is three chars, not four,
        // and the result is one char shorter than the ASCII case.
        let branch = "ブランチ".repeat(10);
        assert_eq!(abbreviate_branch(&branch), "ブランチ…ランチ");
    }
}
