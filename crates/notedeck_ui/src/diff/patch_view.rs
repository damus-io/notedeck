//! [`git_patch_ui`]: a whole [`GitPatch`] in one scroll area — a summary of
//! the changed files, then each file as a collapsible section of hunks drawn
//! with [`DiffLines`].
//!
//! Every row (summary line, file header, hunk header, diff line) is the same
//! height, so the patch is one virtual list: only the rows in the viewport are
//! laid out, and jumping to a file is arithmetic on row indices. A 5k-line
//! patch costs what its visible screenful costs.

use super::patch::{FilePatch, FileStatus, GitPatch, LineKind};
use super::{DiffLines, DELETE_COLOR, INSERT_COLOR, LINE_NUMBER_COLOR};
use egui::{
    Color32, FontId, Rect, RichText, ScrollArea, Sense, Ui, UiBuilder, WidgetInfo, WidgetType,
};
use notedeck::{tr, tr_plural, Localization};

/// Files longer than this (in diff lines) start collapsed.
const COLLAPSE_LINES: usize = 1000;

/// Font size of diff content, as [`DiffLines`] draws it.
const DIFF_FONT_SIZE: f32 = 12.0;

/// A scroll the caller asks for; applied on the next [`git_patch_ui`] pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PatchScroll {
    Top,
    Bottom,
    /// The header of file `n`.
    File(usize),
    /// The file after the one at the top of the view.
    NextFile,
    /// The start of the file at the top of the view, or the one before it if
    /// its header is already at the top.
    PrevFile,
    /// Move by this many rows (negative scrolls up).
    Rows(isize),
    /// Move by this fraction of the view's height (negative scrolls up).
    Pages(f32),
}

/// View state for one [`GitPatch`]: which files are collapsed, where the view
/// is, and the labels that don't change per frame. Build it with
/// [`GitPatchState::new`] when the patch changes.
#[derive(Debug, Clone, Default)]
pub struct GitPatchState {
    collapsed: Vec<bool>,
    /// Per file, its `+a −d` counts, formatted once.
    stats: Vec<String>,
    /// "N files changed", formatted once.
    summary: String,
    /// The patch's total `+a` and `−d`, formatted once.
    total_additions: String,
    total_deletions: String,
    /// "Binary file not shown" and friends, localized once.
    notes: Notes,
    /// The virtual row of each file's header. Recomputed every pass (its
    /// capacity is kept, so that costs no allocation).
    file_rows: Vec<usize>,
    pending: Option<PatchScroll>,
    /// Scroll offset and view height from the last pass.
    offset: f32,
    viewport_height: f32,
    /// The file whose row is at the top of the view, if any.
    current_file: Option<usize>,
}

/// Localized one-line bodies for files without hunks.
#[derive(Debug, Clone, Default)]
struct Notes {
    binary: String,
    renamed: String,
    unchanged: String,
}

impl GitPatchState {
    /// Fresh state for `patch`: everything expanded except files over
    /// [`COLLAPSE_LINES`], scrolled to the top.
    pub fn new(patch: &GitPatch, i18n: &mut Localization) -> Self {
        let files = patch.files();
        let summary = tr_plural!(
            i18n,
            "{count} file changed",
            "{count} files changed",
            "Summary line above a commit's diff: how many files it touches",
            files.len(),
        );
        let notes = Notes {
            binary: tr!(
                i18n,
                "Binary file not shown",
                "Diff placeholder for a changed binary file"
            ),
            renamed: tr!(
                i18n,
                "File renamed without changes",
                "Diff placeholder for a file moved with identical content"
            ),
            unchanged: tr!(
                i18n,
                "No content changes",
                "Diff placeholder for a file whose only change is its mode"
            ),
        };
        Self {
            collapsed: files
                .iter()
                .map(|f| f.lines.len() > COLLAPSE_LINES)
                .collect(),
            stats: files
                .iter()
                .map(|f| format!("+{} −{}", f.additions, f.deletions))
                .collect(),
            summary,
            total_additions: format!("+{}", patch.additions()),
            total_deletions: format!("−{}", patch.deletions()),
            notes,
            file_rows: Vec::with_capacity(files.len()),
            ..Default::default()
        }
    }

    /// Ask for a scroll on the next pass. A later request replaces an earlier
    /// one that hasn't been applied yet.
    pub fn scroll(&mut self, request: PatchScroll) {
        self.pending = Some(request);
    }

    /// The file at the top of the view as of the last pass; `None` while the
    /// summary is at the top.
    pub fn current_file(&self) -> Option<usize> {
        self.current_file
    }

    /// Whether file `n`'s hunks are hidden.
    pub fn is_collapsed(&self, n: usize) -> bool {
        self.collapsed.get(n).copied().unwrap_or(false)
    }

    /// Show or hide file `n`'s hunks.
    pub fn set_collapsed(&mut self, n: usize, collapsed: bool) {
        if let Some(c) = self.collapsed.get_mut(n) {
            *c = collapsed;
        }
    }

    /// Lay the rows out for this pass: fill `file_rows` and return the
    /// content's row count. The summary takes the first `1 + files` rows.
    fn layout(&mut self, patch: &GitPatch) -> usize {
        let files = patch.files();
        self.file_rows.clear();
        let mut row = 1 + files.len();
        for (file, collapsed) in files.iter().zip(&self.collapsed) {
            self.file_rows.push(row);
            row += 1;
            if !collapsed {
                row += body_rows(file);
            }
        }
        row
    }

    /// What virtual row `row` shows.
    fn locate(&self, patch: &GitPatch, row: usize) -> Row {
        let n = patch.files().len();
        if row == 0 {
            return Row::Summary;
        }
        if row <= n {
            return Row::SummaryFile(row - 1);
        }
        let f = self.file_rows.partition_point(|&r| r <= row) - 1;
        let body = row - self.file_rows[f];
        if body == 0 {
            return Row::FileHeader(f);
        }
        let file = &patch.files()[f];
        if file.hunks.is_empty() {
            return Row::Note(f);
        }
        let b = body - 1;
        let h = hunk_at(file, b);
        let first = hunk_row(file, h);
        if b == first {
            Row::HunkHeader(f, h)
        } else {
            Row::Line(f, file.hunks[h].lines.start + (b - first - 1))
        }
    }

    /// The file whose section contains `row`.
    fn file_at(&self, row: usize) -> Option<usize> {
        self.file_rows.partition_point(|&r| r <= row).checked_sub(1)
    }

    /// Blank rows after the content, so that even the last file's header can
    /// scroll to the top of the view: a jump always lands the same way, and
    /// [`Self::current_file`] always names the file jumped to.
    fn padding_rows(&self, row_step: f32) -> usize {
        ((self.viewport_height / row_step) as usize).saturating_sub(1)
    }

    /// Turn the pending request into a target offset. Runs after
    /// [`Self::layout`], against this pass's rows.
    fn take_target(&mut self, content_rows: usize, row_step: f32) -> Option<f32> {
        let total_rows = content_rows + self.padding_rows(row_step);
        let request = self.pending.take()?;
        let n = self.file_rows.len();
        let top_row = row_at(self.offset, row_step);
        let file = match request {
            PatchScroll::File(i) => Some(i),
            PatchScroll::NextFile => Some(self.current_file.map_or(0, |f| f + 1)),
            PatchScroll::PrevFile => match self.current_file {
                Some(f) if self.file_rows.get(f) == Some(&top_row) => f.checked_sub(1),
                Some(f) => Some(f),
                None => None,
            },
            _ => None,
        };
        if let Some(f) = file.filter(|&f| f < n) {
            let target = self.file_rows[f] as f32 * row_step;
            return Some(self.clamp(target, total_rows, row_step));
        }

        let target = match request {
            PatchScroll::Top | PatchScroll::PrevFile => 0.0,
            PatchScroll::Bottom => content_rows as f32 * row_step - self.viewport_height,
            PatchScroll::Rows(rows) => self.offset + rows as f32 * row_step,
            PatchScroll::Pages(pages) => self.offset + pages * self.viewport_height,
            // Past the last file: stay put.
            PatchScroll::File(_) | PatchScroll::NextFile => self.offset,
        };
        Some(self.clamp(target, total_rows, row_step))
    }

    fn clamp(&self, offset: f32, total_rows: usize, row_step: f32) -> f32 {
        let max = total_rows as f32 * row_step - self.viewport_height;
        offset.min(max).max(0.0)
    }
}

/// What one virtual row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// "N files changed", then the totals.
    Summary,
    /// File `n`'s line in the summary.
    SummaryFile(usize),
    FileHeader(usize),
    /// A file with no hunks: binary, a pure rename, or a mode change.
    Note(usize),
    /// File, hunk index.
    HunkHeader(usize, usize),
    /// File, index into its `lines`.
    Line(usize, usize),
}

/// The row at scroll offset `offset`. The half-pixel slack keeps an offset
/// that is exactly a row's top (a jump) from rounding down to the row above.
fn row_at(offset: f32, row_step: f32) -> usize {
    ((offset + 0.5) / row_step) as usize
}

/// Rows a file's body takes when expanded.
fn body_rows(file: &FilePatch) -> usize {
    if file.hunks.is_empty() {
        1
    } else {
        file.hunks.len() + file.lines.len()
    }
}

/// Body row (0-based, after the file header) of hunk `h`'s header: every hunk
/// before it took one header row plus its lines.
fn hunk_row(file: &FilePatch, h: usize) -> usize {
    h + file.hunks[h].lines.start
}

/// The hunk whose rows contain body row `b` (binary search on [`hunk_row`],
/// which grows with `h`). `file` must have hunks.
fn hunk_at(file: &FilePatch, b: usize) -> usize {
    let (mut lo, mut hi) = (0, file.hunks.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if hunk_row(file, mid) <= b {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo.saturating_sub(1)
}

/// Draw `patch` with `state`, filling the available space.
#[profiling::function]
pub fn git_patch_ui(patch: &GitPatch, state: &mut GitPatchState, ui: &mut Ui) {
    if state.collapsed.len() != patch.files().len() {
        // The caller swapped the patch without new state. Recover rather than
        // index out of bounds; the labels fall back to the source strings.
        *state = GitPatchState {
            pending: state.pending,
            ..GitPatchState::new(patch, &mut Localization::default())
        };
    }

    let row_height = row_height(ui);
    let row_step = row_height + ui.spacing().item_spacing.y;
    let content_rows = state.layout(patch);
    let target = state.take_target(content_rows, row_step);
    let total_rows = content_rows + state.padding_rows(row_step);

    let mut area = ScrollArea::both()
        .id_salt("git_patch")
        .auto_shrink([false, false]);
    if let Some(y) = target {
        area = area.vertical_scroll_offset(y);
    }

    let out = ui
        .scope(|ui| {
            // Every row is a `horizontal`, whose minimum height this is, so
            // they all come out exactly `row_height` tall, and row `n` sits at
            // `n * row_step`.
            ui.spacing_mut().interact_size.y = row_height;
            area.show_viewport(ui, |ui, viewport| {
                let spacing = ui.spacing().item_spacing.y;
                ui.set_height((total_rows as f32 * row_step - spacing).max(0.0));

                // Lay out only the rows the viewport touches (padding rows
                // draw nothing), each at its fixed place.
                let first = (viewport.min.y / row_step) as usize;
                let end = ((viewport.max.y / row_step) as usize + 1).min(content_rows);
                let top = ui.max_rect().top();
                let rows_rect = Rect::from_x_y_ranges(
                    ui.max_rect().x_range(),
                    top + first as f32 * row_step..=top + end as f32 * row_step,
                );
                ui.scope_builder(UiBuilder::new().max_rect(rows_rect), |ui| {
                    ui.skip_ahead_auto_ids(first); // Stable ids as rows scroll.
                    for row in first..end {
                        let row = state.locate(patch, row);
                        row_ui(patch, state, row, ui);
                    }
                });

                let view_top = top + viewport.min.y;
                sticky_header(patch, state, row_at(viewport.min.y, row_step), view_top, ui);
            })
        })
        .inner;

    state.offset = out.state.offset.y;
    state.viewport_height = out.inner_rect.height();
    let top_row = row_at(state.offset, row_step);
    state.current_file = state.file_at(top_row);
    if state.pending.is_some() {
        // A row asked for a scroll this pass; apply it on the next one.
        ui.ctx().request_repaint();
    }
}

/// The one height every row is drawn at: tall enough for a diff line, a
/// body-text label and egui's own minimum interactive height.
fn row_height(ui: &Ui) -> f32 {
    let mono = ui.fonts(|f| f.row_height(&FontId::monospace(DIFF_FONT_SIZE)));
    let body = ui.text_style_height(&egui::TextStyle::Body);
    ui.spacing().interact_size.y.max(mono).max(body)
}

fn row_ui(patch: &GitPatch, state: &mut GitPatchState, row: Row, ui: &mut Ui) {
    match row {
        Row::Summary => {
            ui.horizontal(|ui| {
                ui.strong(&state.summary);
                ui.label(
                    RichText::new(&state.total_additions)
                        .monospace()
                        .color(INSERT_COLOR),
                );
                ui.label(
                    RichText::new(&state.total_deletions)
                        .monospace()
                        .color(DELETE_COLOR),
                );
            });
        }
        Row::SummaryFile(f) => {
            let file = &patch.files()[f];
            let row = ui.horizontal(|ui| {
                ui.add_space(8.0);
                status_ui(file.status, ui);
                ui.label(file.path());
                stat_ui(file, &state.stats[f], ui);
            });
            if clickable(row.response, WidgetType::Link, file.path()) {
                state.set_collapsed(f, false);
                state.scroll(PatchScroll::File(f));
            }
        }
        Row::FileHeader(f) => {
            if file_header_ui(patch, state, f, ui) {
                let collapsed = state.is_collapsed(f);
                state.set_collapsed(f, !collapsed);
            }
        }
        Row::Note(f) => {
            let file = &patch.files()[f];
            let note = if file.binary {
                &state.notes.binary
            } else if matches!(file.status, FileStatus::Renamed { .. }) {
                &state.notes.renamed
            } else {
                &state.notes.unchanged
            };
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.weak(note);
            });
        }
        Row::HunkHeader(f, h) => {
            let header = patch.text(patch.files()[f].hunks[h].header);
            ui.horizontal(|ui| {
                let rect = full_row(ui);
                ui.painter()
                    .rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
                ui.label(
                    RichText::new(header)
                        .monospace()
                        .size(DIFF_FONT_SIZE)
                        .color(LINE_NUMBER_COLOR),
                );
            });
        }
        Row::Line(f, i) => {
            let file = &patch.files()[f];
            let line = &file.lines[i];
            match patch.diff_row(line) {
                Some(diff_row) => {
                    DiffLines::for_path(file.path()).show(std::iter::once(diff_row), ui);
                }
                None => {
                    debug_assert_eq!(line.kind, LineKind::NoNewline);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(patch.text(line.text))
                                .monospace()
                                .size(DIFF_FONT_SIZE)
                                .color(LINE_NUMBER_COLOR),
                        );
                    });
                }
            }
        }
    }
}

/// A file's header row: collapse arrow, status, path (old → new for a
/// rename), counts. Returns whether it was clicked.
fn file_header_ui(patch: &GitPatch, state: &GitPatchState, f: usize, ui: &mut Ui) -> bool {
    let file = &patch.files()[f];
    let openness = if state.is_collapsed(f) { 0.0 } else { 1.0 };
    let row = ui
        .horizontal(|ui| {
            let rect = full_row(ui);
            let visuals = ui.visuals();
            ui.painter().rect_filled(rect, 0.0, visuals.panel_fill);
            ui.painter()
                .rect_filled(rect, 0.0, visuals.extreme_bg_color);
            let icon = egui::vec2(ui.spacing().icon_width, ui.spacing().interact_size.y);
            let (_, icon) = ui.allocate_exact_size(icon, Sense::hover());
            egui::collapsing_header::paint_default_icon(ui, openness, &icon);
            status_ui(file.status, ui);
            if file.old_path != file.new_path {
                ui.label(RichText::new(&file.old_path).weak());
                ui.label(RichText::new("->").weak());
            }
            ui.strong(file.path());
            stat_ui(file, &state.stats[f], ui);
        })
        .response;
    clickable(row, WidgetType::Button, file.path())
}

/// Make a whole row clickable, as one accessible widget named `label` (the
/// labels inside it are only text). Returns whether it was clicked.
fn clickable(row: egui::Response, typ: WidgetType, label: &str) -> bool {
    let row = row
        .interact(Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    row.widget_info(|| WidgetInfo::labeled(typ, true, label));
    row.clicked()
}

/// Pin the header of the file at the top of the view over the first row, so
/// the path stays visible while scrolling through its hunks.
///
/// `top_row` is the row at the top of the view and `view_top` the screen y of
/// the view's top edge.
fn sticky_header(
    patch: &GitPatch,
    state: &mut GitPatchState,
    top_row: usize,
    view_top: f32,
    ui: &mut Ui,
) {
    let Some(f) = state.file_at(top_row) else {
        return;
    };
    if state.file_rows[f] == top_row {
        return;
    }
    let rect = Rect::from_min_size(
        egui::pos2(ui.max_rect().left(), view_top),
        egui::vec2(ui.max_rect().width(), ui.spacing().interact_size.y),
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    if file_header_ui(patch, state, f, &mut child) {
        // Collapsing from inside the file: land on its header, not wherever
        // the rows after it happen to end up.
        state.set_collapsed(f, true);
        state.scroll(PatchScroll::File(f));
    }
}

/// The rect of the current `horizontal` row, stretched to the full width.
fn full_row(ui: &Ui) -> Rect {
    let min = ui.max_rect().min;
    Rect::from_min_size(
        min,
        egui::vec2(ui.clip_rect().right() - min.x, ui.spacing().interact_size.y),
    )
}

fn status_ui(status: FileStatus, ui: &mut Ui) {
    let (letter, color) = match status {
        FileStatus::Added => ("A", INSERT_COLOR),
        FileStatus::Deleted => ("D", DELETE_COLOR),
        FileStatus::Modified => ("M", ui.visuals().weak_text_color()),
        FileStatus::Renamed { .. } => ("R", ui.visuals().warn_fg_color),
        FileStatus::Copied { .. } => ("C", ui.visuals().warn_fg_color),
    };
    ui.label(RichText::new(letter).monospace().strong().color(color));
}

/// A file's `+a −d`, preformatted in `stats`; nothing for a file without
/// hunks (binary, pure rename, mode change).
fn stat_ui(file: &FilePatch, stats: &str, ui: &mut Ui) {
    if file.hunks.is_empty() {
        return;
    }
    let color: Color32 = if file.deletions == 0 {
        INSERT_COLOR
    } else if file.additions == 0 {
        DELETE_COLOR
    } else {
        LINE_NUMBER_COLOR
    };
    ui.label(RichText::new(stats).monospace().size(11.0).color(color));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::{kittest::Queryable, Harness};

    const MULTI: &str = include_str!("testdata/multi.patch");

    /// A patch whose first file is `lines` inserted lines, then a small file.
    fn tall_patch(lines: usize) -> GitPatch {
        let mut text = format!(
            "diff --git a/big.txt b/big.txt\n--- a/big.txt\n+++ b/big.txt\n@@ -0,0 +1,{lines} @@\n"
        );
        for i in 1..=lines {
            text.push_str(&format!("+big {i}\n"));
        }
        text.push_str("diff --git a/small.rs b/small.rs\n--- a/small.rs\n+++ b/small.rs\n@@ -1 +1 @@\n-old\n+new\n");
        GitPatch::parse(text)
    }

    fn harness(patch: GitPatch, height: f32) -> Harness<'static, (GitPatch, GitPatchState)> {
        let state = GitPatchState::new(&patch, &mut Localization::default());
        Harness::builder()
            .with_size(egui::vec2(800.0, height))
            .build_ui_state(
                |ui, (patch, state)| git_patch_ui(patch, state, ui),
                (patch, state),
            )
    }

    fn shown(harness: &Harness<'_, (GitPatch, GitPatchState)>, label: &str) -> usize {
        harness.query_all_by_label(label).count()
    }

    #[test]
    fn rows_map_to_summary_headers_hunks_and_lines() {
        let patch = GitPatch::parse(MULTI);
        let mut state = GitPatchState::new(&patch, &mut Localization::default());
        let total = state.layout(&patch);
        let rows: Vec<_> = (0..total).map(|r| state.locate(&patch, r)).collect();

        // Summary: heading + 8 files.
        assert_eq!(rows[0], Row::Summary);
        assert_eq!(rows[8], Row::SummaryFile(7));
        // added file.rs: header, hunk header, 2 lines.
        assert_eq!(
            &rows[9..13],
            &[
                Row::FileHeader(0),
                Row::HunkHeader(0, 0),
                Row::Line(0, 0),
                Row::Line(0, 1),
            ]
        );
        // blob.bin: header + note.
        assert_eq!(&rows[13..15], &[Row::FileHeader(1), Row::Note(1)]);
        // long.txt (file 4) has two hunks of 7 and 8 lines.
        let long = state.file_rows[4];
        assert_eq!(rows[long + 1], Row::HunkHeader(4, 0));
        assert_eq!(rows[long + 9], Row::HunkHeader(4, 1));
        assert_eq!(rows[long + 10], Row::Line(4, 7));
        assert_eq!(*rows.last().unwrap(), Row::Line(7, 3));
        assert_eq!(total, rows.len());

        state.set_collapsed(4, true);
        let collapsed_total = state.layout(&patch);
        assert_eq!(collapsed_total, total - 17);
        assert_eq!(state.locate(&patch, long + 1), Row::FileHeader(5));
    }

    #[test]
    fn renders_summary_files_hunks_and_markers() {
        // Tall enough for the whole patch.
        let mut harness = harness(GitPatch::parse(MULTI), 1600.0);
        harness.run();

        // Fluent wraps the count in bidi isolation marks once the ftl has
        // the key, so match around them.
        harness.get_by_label_contains("files changed");
        // The summary link, then the header button, each with its path as
        // plain text inside.
        assert_eq!(shown(&harness, "main.rs"), 4);
        harness.get_by_role_and_label(Role::Link, "main.rs");
        harness.get_by_role_and_label(Role::Button, "main.rs");
        assert_eq!(shown(&harness, "@@ -1,3 +1,4 @@"), 1);
        assert_eq!(shown(&harness, "+     new();"), 1);
        assert_eq!(shown(&harness, "Binary file not shown"), 1);
        assert_eq!(shown(&harness, "No content changes"), 1);
    }

    #[test]
    fn only_rows_in_view_are_laid_out_and_jumps_land() {
        let mut harness = harness(tall_patch(600), 400.0);
        harness.run();
        assert_eq!(shown(&harness, "+ big 1"), 1);
        assert_eq!(
            shown(&harness, "+ big 600"),
            0,
            "off-screen rows are virtual"
        );
        assert_eq!(harness.state().1.current_file(), None);

        harness.state_mut().1.scroll(PatchScroll::File(1));
        harness.run();
        assert_eq!(shown(&harness, "+ new"), 1);
        assert_eq!(shown(&harness, "+ big 1"), 0);
        assert_eq!(harness.state().1.current_file(), Some(1));

        harness.state_mut().1.scroll(PatchScroll::PrevFile);
        harness.run();
        assert_eq!(harness.state().1.current_file(), Some(0));
        assert_eq!(shown(&harness, "+ big 1"), 1);

        harness.state_mut().1.scroll(PatchScroll::Rows(100));
        harness.run();
        assert_eq!(harness.state().1.current_file(), Some(0));
        // Deep in big.txt: its header is pinned over the top row.
        harness.get_by_role_and_label(Role::Button, "big.txt");
        assert_eq!(shown(&harness, "+ big 100"), 1);

        harness.state_mut().1.scroll(PatchScroll::NextFile);
        harness.run();
        assert_eq!(harness.state().1.current_file(), Some(1));

        harness.state_mut().1.scroll(PatchScroll::Top);
        harness.run();
        assert_eq!(harness.state().1.current_file(), None);
    }

    #[test]
    fn huge_files_start_collapsed_and_expand_on_click() {
        let mut harness = harness(tall_patch(COLLAPSE_LINES + 1), 400.0);
        harness.run();
        assert!(harness.state().1.is_collapsed(0));
        assert!(!harness.state().1.is_collapsed(1));
        assert_eq!(shown(&harness, "+ big 1"), 0);
        assert_eq!(shown(&harness, "+ new"), 1);

        harness
            .get_by_role_and_label(Role::Button, "big.txt")
            .click();
        harness.run();
        assert!(!harness.state().1.is_collapsed(0));
        assert_eq!(shown(&harness, "+ big 1"), 1);
    }

    #[test]
    fn pinned_header_names_the_file_under_the_top_edge() {
        let mut harness = harness(GitPatch::parse(MULTI), 500.0);
        // Row 27 is inside long.txt, whose header (row 20) is above the view.
        harness.state_mut().1.scroll(PatchScroll::Rows(27));
        harness.run();
        assert_eq!(harness.state().1.current_file(), Some(4));
        harness.get_by_role_and_label(Role::Button, "long.txt");
        assert!(harness
            .query_by_role_and_label(Role::Button, "keep.txt")
            .is_none());

        harness.state_mut().1.scroll(PatchScroll::Bottom);
        harness.run();
        assert_eq!(shown(&harness, "+ no eol now"), 1);
    }
}
