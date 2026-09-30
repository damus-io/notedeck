//! [`git_patch_ui`]: a whole [`GitPatch`] in one scroll area — a summary of
//! the changed files, then each file as a collapsible section of hunks, whose
//! lines look like [`DiffLines`](super::DiffLines)' rows.
//!
//! Every row (summary line, file header, hunk header, diff line) is the same
//! height, so the patch is one virtual list: only the rows in the viewport are
//! laid out, and jumping to a file is arithmetic on row indices. A 5k-line
//! patch costs what its visible screenful costs. Diff lines are laid out once
//! as they scroll into view and kept in [`GitPatchState`], so a frame that
//! repaints the same view lays none of them out again.

use super::patch::{FilePatch, FileStatus, GitPatch, LineKind};
use super::{
    file_extension, DiffTag, RowGalleys, DELETE_COLOR, DIFF_FONT_SIZE, INSERT_COLOR,
    LINE_NUMBER_COLOR,
};
use egui::emath::GuiRounding;
use egui::epaint::{mutex::Mutex, TextShape, TextureAtlas};
use egui::text::{LayoutJob, TextWrapping};
use egui::text_selection::LabelSelectionState;
use egui::{
    Color32, FontId, Label, Rect, RichText, ScrollArea, Sense, Stroke, Ui, UiBuilder, WidgetInfo,
    WidgetType,
};
use notedeck::{tr, tr_plural, Localization};
use std::collections::HashMap;
use std::sync::Arc;

/// Files longer than this (in diff lines) start collapsed.
const COLLAPSE_LINES: usize = 1000;

/// Width of the file table's status column (`A`, `M`, `D`, ...).
const STATUS_WIDTH: f32 = 22.0;
/// Gap between the file table's path and stats columns, and between the
/// stats columns themselves.
const COLUMN_GAP: f32 = 8.0;
/// Blocks in a file's add/delete proportion bar.
const BAR_BLOCKS: usize = 5;
/// Side of one block of the bar, and the gap between blocks.
const BAR_BLOCK: f32 = 7.0;
const BAR_BLOCK_GAP: f32 = 2.0;
/// Width of the whole bar.
const BAR_WIDTH: f32 = BAR_BLOCKS as f32 * (BAR_BLOCK + BAR_BLOCK_GAP) - BAR_BLOCK_GAP;

/// Gap between a diff line's line numbers and its `+`/`-` marker.
const GUTTER_GAP: f32 = 8.0;
/// Gap between a diff line's marker and its content.
const MARKER_GAP: f32 = 6.0;

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
    /// Move by this many pages (negative scrolls up). A whole page is exact:
    /// forward, the first row that wasn't fully shown lands as the first one
    /// readable (just under the pinned file header, when one is pinned);
    /// back is its inverse. So paging neither skips a line nor shows one
    /// twice. A fractional part moves by that share of the view's height.
    /// Either way the view lands on a row boundary.
    Pages(f32),
}

/// View state for one [`GitPatch`]: which files are collapsed, where the view
/// is, and the labels that don't change per frame. Build it with
/// [`GitPatchState::new`] when the patch changes.
#[derive(Debug, Clone, Default)]
pub struct GitPatchState {
    collapsed: Vec<bool>,
    /// Per file, what its row in the file table shows besides its name,
    /// formatted once.
    rows: Vec<FileRow>,
    /// "N files changed", formatted once.
    summary: String,
    /// The patch's totals, formatted once.
    totals: Stats,
    /// How wide the stats columns are, so they line up down the table.
    columns: StatColumns,
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
    /// The diff lines in view, laid out.
    galleys: LineGalleys,
}

/// Laid-out diff lines, kept across passes so a line is laid out once, when
/// it scrolls into view, rather than on every pass it stays there: repainting
/// the same view builds no gutter strings, layout jobs or galleys.
///
/// Holds only the lines drawn in the latest pass, so its size follows the
/// view, not the patch.
#[derive(Clone, Default)]
struct LineGalleys {
    /// What the galleys were laid out against; when it changes they are
    /// dropped and laid out again.
    key: Option<GalleyKey>,
    /// Per `(file, index into its lines)`.
    lines: HashMap<(usize, usize), CachedLine>,
    /// Counts passes, to tell the lines drawn in this one from the rest.
    pass: u64,
}

/// What a laid-out diff line depends on besides its text.
#[derive(Clone)]
struct GalleyKey {
    /// The font atlas the glyphs were placed in. egui replaces it when the
    /// scale changes or it fills up, and a galley laid out against the old
    /// one points at glyphs that are gone. Held (not just its address) so the
    /// comparison can't be fooled by a new atlas reusing a freed one's memory.
    atlas: Arc<Mutex<TextureAtlas>>,
    /// Picks the syntax theme.
    dark_mode: bool,
}

impl GalleyKey {
    fn of(ui: &Ui) -> Self {
        Self {
            atlas: ui.fonts(|f| f.texture_atlas()),
            dark_mode: ui.visuals().dark_mode,
        }
    }

    fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.atlas, &other.atlas) && self.dark_mode == other.dark_mode
    }
}

#[derive(Clone)]
struct CachedLine {
    galleys: RowGalleys,
    /// The last pass that drew it.
    pass: u64,
}

impl LineGalleys {
    /// Start a pass: drop everything if what it was laid out against changed.
    fn begin_pass(&mut self, ui: &Ui) {
        let key = GalleyKey::of(ui);
        if !self.key.as_ref().is_some_and(|k| k.same_as(&key)) {
            self.lines.clear();
            self.key = Some(key);
        }
        self.pass += 1;
    }

    /// File `f`'s line `i`, laid out now if it wasn't in view last pass.
    fn get(&mut self, patch: &GitPatch, f: usize, i: usize, ui: &Ui) -> Option<RowGalleys> {
        let pass = self.pass;
        if let Some(line) = self.lines.get_mut(&(f, i)) {
            line.pass = pass;
            return Some(line.galleys.clone());
        }
        let file = &patch.files()[f];
        let row = patch.diff_row(&file.lines[i])?;
        let lang = file_extension(file.path()).unwrap_or("text");
        let galleys = RowGalleys::layout(&row, lang, ui);
        self.lines.insert(
            (f, i),
            CachedLine {
                galleys: galleys.clone(),
                pass,
            },
        );
        Some(galleys)
    }

    /// End a pass: forget the lines it didn't draw.
    fn end_pass(&mut self) {
        let pass = self.pass;
        self.lines.retain(|_, line| line.pass == pass);
    }
}

impl std::fmt::Debug for LineGalleys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LineGalleys")
            .field("lines", &self.lines.len())
            .field("pass", &self.pass)
            .finish()
    }
}

/// A file's row in the file table, less its name (the tail of
/// [`FilePatch::path`], borrowed when drawn).
#[derive(Debug, Clone, Default)]
struct FileRow {
    /// The muted part before the name: the directory with its trailing `/`,
    /// after `old → ` for a rename or copy. Empty for a file at the root.
    dir: String,
    stats: Stats,
}

impl FileRow {
    fn new(file: &FilePatch) -> Self {
        let (dir, _) = split_path(file.path());
        let dir = if file.old_path != file.new_path {
            format!("{} → {dir}", file.old_path)
        } else {
            dir.to_owned()
        };
        Self {
            dir,
            stats: Stats::new(file.additions, file.deletions),
        }
    }
}

/// `+a` and `−d`, each empty when its count is zero, and the counts for the
/// proportion bar.
#[derive(Debug, Clone, Default)]
struct Stats {
    additions: String,
    deletions: String,
    /// Added and deleted line counts.
    counts: (usize, usize),
}

impl Stats {
    fn new(additions: usize, deletions: usize) -> Self {
        Self {
            additions: stat_label('+', additions),
            deletions: stat_label('−', deletions),
            counts: (additions, deletions),
        }
    }
}

/// The widths of the table's `+a` and `−d` columns: the widest label of each
/// across the patch (totals included), measured once per font.
#[derive(Debug, Clone, Default)]
struct StatColumns {
    /// The font and scale they were measured in.
    key: Option<(FontId, f32)>,
    additions: f32,
    deletions: f32,
}

impl StatColumns {
    /// Measure the labels again if the stat font or scale changed.
    fn update(&mut self, rows: &[FileRow], totals: &Stats, ui: &Ui) {
        let key = (stat_font(ui), ui.ctx().pixels_per_point());
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let widest = |label: fn(&Stats) -> &String| {
            let stats = rows.iter().map(|r| &r.stats).chain(std::iter::once(totals));
            ui.fonts(|f| {
                stats
                    .map(label)
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        f.layout_no_wrap(s.clone(), key.0.clone(), Color32::PLACEHOLDER)
                            .size()
                            .x
                    })
                    .fold(0.0, f32::max)
            })
        };
        self.additions = widest(|s| &s.additions);
        self.deletions = widest(|s| &s.deletions);
        self.key = Some(key);
    }

    /// The stats block's width: the two columns and the bar, with the gaps
    /// between them (none for a column no file uses).
    fn width(&self) -> f32 {
        [self.additions, self.deletions]
            .iter()
            .filter(|&&w| w > 0.0)
            .map(|w| w + COLUMN_GAP)
            .sum::<f32>()
            + BAR_WIDTH
    }
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
            rows: files.iter().map(FileRow::new).collect(),
            summary,
            totals: Stats::new(patch.additions(), patch.deletions()),
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

    /// How many rows at the top of a view whose top row is `top` the pinned
    /// file header hides: one while it's pinned (see [`sticky_header`]), none
    /// on a file's own header row or in the summary.
    fn pinned_rows(&self, top: usize) -> usize {
        match self.file_at(top) {
            Some(f) if self.file_rows[f] != top => 1,
            _ => 0,
        }
    }

    /// The offset one page on from `offset`: the first row not fully shown
    /// becomes the first readable one. When a header would be pinned over
    /// it, the view starts a row higher so the header covers a row already
    /// read instead.
    fn page_down(&self, offset: f32, rows: RowMetrics) -> f32 {
        let top = row_at(offset, rows.step);
        // The first row whose bottom edge is below the view's.
        let bottom = offset + self.viewport_height + ROW_SLACK;
        let next = ((bottom - rows.height) / rows.step).floor() as usize + 1;
        let new_top = next - self.pinned_rows(next).min(next);
        new_top.max(top + 1) as f32 * rows.step
    }

    /// The offset one page back from `offset`, the inverse of
    /// [`Self::page_down`]: the row above the first readable one becomes the
    /// last fully shown.
    fn page_up(&self, offset: f32, rows: RowMetrics) -> f32 {
        let top = row_at(offset, rows.step);
        let hidden = self.pinned_rows(top) as f32 * rows.height;
        let first_read = ((offset + hidden - ROW_SLACK) / rows.step).ceil() as usize;
        // Rows fully shown below an aligned top row.
        let below = ((self.viewport_height + ROW_SLACK - rows.height) / rows.step).floor() as usize;
        let new_top = first_read.saturating_sub(below + 1);
        new_top.min(top.saturating_sub(1)) as f32 * rows.step
    }

    /// The target of a [`PatchScroll::Pages`] request.
    fn pages_target(&self, pages: f32, total_rows: usize, rows: RowMetrics) -> f32 {
        let mut offset = self.offset;
        for _ in 0..(pages.trunc().abs() as usize) {
            offset = if pages > 0.0 {
                self.page_down(offset, rows)
            } else {
                self.page_up(offset, rows)
            };
            offset = self.clamp(offset, total_rows, rows.step);
        }
        let rest = offset + pages.fract() * self.viewport_height;
        (rest / rows.step).round() * rows.step
    }

    /// Turn the pending request into a target offset. Runs after
    /// [`Self::layout`], against this pass's rows.
    fn take_target(&mut self, content_rows: usize, rows: RowMetrics) -> Option<f32> {
        let row_step = rows.step;
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
            PatchScroll::Pages(pages) => self.pages_target(pages, total_rows, rows),
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
    ((offset + ROW_SLACK) / row_step) as usize
}

/// Slack, in points, when asking which row an edge falls on, so an edge that
/// is exactly on a row boundary isn't rounded to its neighbour.
const ROW_SLACK: f32 = 0.5;

/// The geometry every row shares: drawn `height` tall, one every `step`
/// (the height plus the item spacing below it).
#[derive(Debug, Clone, Copy)]
struct RowMetrics {
    height: f32,
    step: f32,
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

    state.galleys.begin_pass(ui);
    state.columns.update(&state.rows, &state.totals, ui);
    let row_height = row_height(ui);
    let row_step = row_height + ui.spacing().item_spacing.y;
    let content_rows = state.layout(patch);
    let rows = RowMetrics {
        height: row_height,
        step: row_step,
    };
    let target = state.take_target(content_rows, rows);
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

    state.galleys.end_pass();
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
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.strong(&state.summary);
                let right = row_right(ui);
                ui.add_space((right - ui.cursor().left() - state.columns.width()).max(COLUMN_GAP));
                stats_ui(&state.totals, &state.columns, ui);
            });
        }
        Row::SummaryFile(f) => {
            let file = &patch.files()[f];
            let row = file_row_ui(file, &state.rows[f], &state.columns, None, ui);
            if clickable(row, WidgetType::Link, file.path()) {
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
                // Under the path column.
                ui.add_space(ui.spacing().icon_width + STATUS_WIDTH);
                ui.weak(note);
            });
        }
        Row::HunkHeader(f, h) => {
            let header = patch.text(patch.files()[f].hunks[h].header);
            ui.horizontal(|ui| {
                // A band with a rule along its top, and text a step brighter
                // than the line numbers, so each hunk reads as a section.
                let rect = full_row(ui);
                let visuals = ui.visuals();
                ui.painter().rect_filled(rect, 0.0, visuals.faint_bg_color);
                ui.painter().hline(
                    rect.x_range(),
                    rect.top(),
                    visuals.widgets.noninteractive.bg_stroke,
                );
                ui.label(
                    RichText::new(header)
                        .monospace()
                        .size(DIFF_FONT_SIZE)
                        .color(visuals.text_color()),
                );
            });
        }
        Row::Line(f, i) => match state.galleys.get(patch, f, i, ui) {
            Some(galleys) => diff_line_ui(galleys, ui),
            None => {
                let line = &patch.files()[f].lines[i];
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
        },
    }
}

/// One diff line from its laid-out galleys, in three columns at fixed gaps:
/// the line numbers, the `+`/`-` marker, then the content. Placed as a
/// `horizontal` of labels would place them (left to right, vertically centred
/// in the row) but without one, because a child `Ui` allocates, and so does
/// building the labels' text. The gaps are constants, not the parent's item
/// spacing, which the chrome sets to zero.
///
/// A changed line is tinted across the whole row, out to the view's right
/// edge, with a stronger tint behind its line numbers. The tint also fills
/// the item spacing above and below, so a run of changed lines is one band.
///
/// The content is selectable like a label's; the numbers and marker are only
/// painted, so selecting code picks up neither.
fn diff_line_ui(galleys: RowGalleys, ui: &mut Ui) {
    let RowGalleys {
        tag,
        gutter,
        marker,
        content,
    } = galleys;
    let height = ui.spacing().interact_size.y;
    let min = ui.cursor().min;
    let marker_x = min.x + gutter.size().x + GUTTER_GAP;
    let content_x = marker_x + marker.size().x + MARKER_GAP;
    let row = Rect::from_min_size(
        min,
        egui::vec2(content_x - min.x + content.size().x, height),
    );
    let id = ui.advance_cursor_after_rect(row);

    if let Some((line_bg, gutter_bg)) = tag.tints() {
        let tint = row
            .expand2(egui::vec2(0.0, ui.spacing().item_spacing.y / 2.0))
            .with_max_x(row.right().max(ui.clip_rect().right()));
        let split = marker_x - GUTTER_GAP / 2.0;
        let painter = ui.painter();
        painter.rect_filled(tint.with_max_x(split), 0.0, gutter_bg);
        painter.rect_filled(tint.with_min_x(split), 0.0, line_bg);
    }

    let centred = |x: f32, size: egui::Vec2| {
        Rect::from_min_size(egui::pos2(x, row.center().y - size.y / 2.0), size).round_ui()
    };
    let gutter_rect = centred(min.x, gutter.size());
    let marker_rect = centred(marker_x, marker.size());
    let content_rect = centred(content_x, content.size());

    let response = ui.interact(gutter_rect, id.with("gutter"), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, gutter.text()));
    ui.painter()
        .add(TextShape::new(gutter_rect.min, gutter, LINE_NUMBER_COLOR));
    if tag != DiffTag::Equal {
        let response = ui.interact(marker_rect, id.with("marker"), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, marker.text()));
    }
    ui.painter()
        .add(TextShape::new(marker_rect.min, marker, LINE_NUMBER_COLOR));

    let selectable = ui.style().interaction.selectable_labels;
    let mut sense = Sense::hover();
    if selectable {
        // As a selectable `Label` senses: drag selects, except on touch
        // screens where it scrolls, and never take keyboard focus.
        let select = if ui.input(|i| i.has_touch_screen()) {
            Sense::click()
        } else {
            Sense::click_and_drag()
        };
        sense = sense.union(select - Sense::FOCUSABLE);
    }
    let response = ui.interact(content_rect, id.with("content"), sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, content.text()));
    let color = ui.visuals().text_color();
    if selectable {
        LabelSelectionState::label_text_selection(
            ui,
            &response,
            content_rect.min,
            content,
            color,
            Stroke::NONE,
        );
    } else {
        ui.painter()
            .add(TextShape::new(content_rect.min, content, color));
    }
}

/// A file's header row: collapse arrow, then the same columns as its row in
/// the summary above, so paths and stats line up between the two. Returns
/// whether it was clicked.
fn file_header_ui(patch: &GitPatch, state: &GitPatchState, f: usize, ui: &mut Ui) -> bool {
    let file = &patch.files()[f];
    let openness = if state.is_collapsed(f) { 0.0 } else { 1.0 };
    let row = file_row_ui(file, &state.rows[f], &state.columns, Some(openness), ui);
    clickable(row, WidgetType::Button, file.path())
}

/// One row of the file table, shared by the summary and the file headers:
/// a lead column, the status letter, the path — directory muted and cut short
/// to fit, the name whole — and the stats right-aligned in `columns`. Hovering
/// a row whose path was cut shows it whole.
///
/// `openness` is set for a file header: the row gets the header's opaque
/// background (so it can be pinned over the rows it scrolls past), its
/// collapse arrow in the lead column and a strong name. The summary's rows
/// leave the lead column blank.
///
/// Sets its own gaps rather than inheriting the parent's: the chrome gives
/// apps no horizontal item spacing.
fn file_row_ui(
    file: &FilePatch,
    row: &FileRow,
    columns: &StatColumns,
    openness: Option<f32>,
    ui: &mut Ui,
) -> egui::Response {
    let inner = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let height = ui.spacing().interact_size.y;
        if openness.is_some() {
            let rect = full_row(ui);
            let visuals = ui.visuals();
            ui.painter().rect_filled(rect, 0.0, visuals.panel_fill);
            ui.painter()
                .rect_filled(rect, 0.0, visuals.extreme_bg_color);
        }
        let lead = egui::vec2(ui.spacing().icon_width, height);
        let (_, lead) = ui.allocate_exact_size(lead, Sense::hover());
        if let Some(openness) = openness {
            egui::collapsing_header::paint_default_icon(ui, openness, &lead);
        }
        status_ui(file.status, height, ui);

        let body = egui::TextStyle::Body.resolve(ui.style());
        let visuals = ui.visuals();
        let name_color = if openness.is_some() {
            visuals.strong_text_color()
        } else {
            visuals.text_color()
        };
        let dir_color = visuals.weak_text_color();
        let (_, name) = split_path(file.path());
        let name = ui.fonts(|f| f.layout_no_wrap(name.to_owned(), body.clone(), name_color));
        let right = row_right(ui);
        let room = right - ui.cursor().left() - name.size().x - COLUMN_GAP - columns.width();
        let mut elided = false;
        if !row.dir.is_empty() {
            let mut job = LayoutJob::simple_singleline(row.dir.clone(), body, dir_color);
            job.wrap = TextWrapping::truncate_at_width(room.max(0.0));
            let dir = ui.fonts(|f| f.layout_job(job));
            elided = dir.elided;
            ui.add(Label::new(dir));
        }
        ui.add(Label::new(name));
        if file.hunks.is_empty() {
            return elided;
        }
        ui.add_space((right - ui.cursor().left() - columns.width()).max(COLUMN_GAP));
        stats_ui(&row.stats, columns, ui);
        elided
    });
    if inner.inner {
        inner.response.on_hover_text(file.path())
    } else {
        inner.response
    }
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

/// Where the file table's rows end: the right edge of the view. Not the
/// `max_rect`'s, which grows past the view once a wide diff line has been
/// laid out above, so rows drawn after one would end further right.
fn row_right(ui: &Ui) -> f32 {
    ui.max_rect().right().min(ui.clip_rect().right())
}

/// The rect of the current `horizontal` row, stretched to the full width.
fn full_row(ui: &Ui) -> Rect {
    let min = ui.max_rect().min;
    Rect::from_min_size(
        min,
        egui::vec2(ui.clip_rect().right() - min.x, ui.spacing().interact_size.y),
    )
}

/// The file table's status column: the letter, centred in [`STATUS_WIDTH`].
fn status_ui(status: FileStatus, height: f32, ui: &mut Ui) {
    let (letter, color) = match status {
        FileStatus::Added => ("A", INSERT_COLOR),
        FileStatus::Deleted => ("D", DELETE_COLOR),
        FileStatus::Modified => ("M", ui.visuals().weak_text_color()),
        FileStatus::Renamed { .. } => ("R", ui.visuals().warn_fg_color),
        FileStatus::Copied { .. } => ("C", ui.visuals().warn_fg_color),
    };
    ui.add_sized(
        [STATUS_WIDTH, height],
        Label::new(RichText::new(letter).strong().color(color)),
    );
}

/// The stats columns: `+a` and `−d`, each right-aligned in its column and
/// left blank when zero, then the proportion bar.
fn stats_ui(stats: &Stats, columns: &StatColumns, ui: &mut Ui) {
    let font = stat_font(ui);
    let cells = [
        (&stats.additions, columns.additions, INSERT_COLOR),
        (&stats.deletions, columns.deletions, DELETE_COLOR),
    ];
    for (label, width, color) in cells {
        if width <= 0.0 {
            continue;
        }
        if label.is_empty() {
            ui.add_space(width + COLUMN_GAP);
            continue;
        }
        let galley = ui.fonts(|f| f.layout_no_wrap(label.clone(), font.clone(), color));
        ui.add_space(width - galley.size().x);
        ui.add(Label::new(galley));
        ui.add_space(COLUMN_GAP);
    }
    stat_bar_ui(stats.counts, ui);
}

/// GitHub's five-block bar: one block per changed line up to
/// [`BAR_BLOCKS`], green for additions and red for deletions in proportion,
/// the rest grey. Painted, not laid out, so it costs no text.
fn stat_bar_ui((additions, deletions): (usize, usize), ui: &mut Ui) {
    let height = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(BAR_WIDTH, height), Sense::hover());
    let (green, red) = bar_blocks(additions, deletions);
    let grey = ui.visuals().widgets.inactive.bg_fill;
    for i in 0..BAR_BLOCKS {
        let color = if i < green {
            INSERT_COLOR
        } else if i < green + red {
            DELETE_COLOR
        } else {
            grey
        };
        let x = rect.left() + i as f32 * (BAR_BLOCK + BAR_BLOCK_GAP);
        let block = Rect::from_min_size(
            egui::pos2(x, rect.center().y - BAR_BLOCK / 2.0),
            egui::vec2(BAR_BLOCK, BAR_BLOCK),
        );
        ui.painter().rect_filled(block.round_ui(), 1.5, color);
    }
}

/// How many of the bar's blocks are green and how many red: one per changed
/// line up to [`BAR_BLOCKS`], split in proportion (rounded), with at least
/// one for each side that changed anything.
fn bar_blocks(additions: usize, deletions: usize) -> (usize, usize) {
    let total = additions + deletions;
    let lit = total.min(BAR_BLOCKS);
    if lit == 0 {
        return (0, 0);
    }
    let mut green = (additions * lit + total / 2) / total;
    if additions > 0 {
        green = green.max(1);
    }
    if deletions > 0 {
        green = green.min(lit - 1);
    }
    (green, lit - green)
}

/// The stats' font: the body font, so they sit on the path's baseline. (A
/// monospace one doesn't, even at the same size: its ascent differs, and the
/// row centres each label's box, not its baseline.) Each stat is
/// right-aligned in its column, so the digits needn't be tabular.
fn stat_font(ui: &Ui) -> FontId {
    egui::TextStyle::Body.resolve(ui.style())
}

/// `sign` then `n` with thousands separators (`+1,655`), or nothing for zero.
fn stat_label(sign: char, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let digits = n.to_string();
    let mut label = String::with_capacity(sign.len_utf8() + digits.len() * 4 / 3);
    label.push(sign);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            label.push(',');
        }
        label.push(digit);
    }
    label
}

/// `path` split after its last `/`: the directory (with the slash, or empty)
/// and the file name.
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => path.split_at(i + 1),
        None => ("", path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::{kittest::Queryable, Harness};

    const MULTI: &str = include_str!("testdata/multi.patch");
    const NESTED: &str = include_str!("testdata/nested.patch");

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
        sized_harness(patch, egui::vec2(800.0, height))
    }

    fn sized_harness(
        patch: GitPatch,
        size: egui::Vec2,
    ) -> Harness<'static, (GitPatch, GitPatchState)> {
        let state = GitPatchState::new(&patch, &mut Localization::default());
        Harness::builder().with_size(size).build_ui_state(
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
        assert_eq!(shown(&harness, "    new();"), 1);
        assert_eq!(shown(&harness, "Binary file not shown"), 1);
        assert_eq!(shown(&harness, "No content changes"), 1);

        // Stats drop a zero side: added file.rs is `+2` alone (in the summary
        // and its header), gone.txt `−1` alone; the totals keep both.
        assert_eq!(shown(&harness, "−0"), 0);
        assert_eq!(shown(&harness, "+0"), 0);
        assert_eq!(shown(&harness, "+8"), 1);
        assert_eq!(shown(&harness, "−5"), 1);
        // The rename's old path leads its muted part, in both rows.
        assert_eq!(shown(&harness, "old name.txt → "), 2);
        assert_eq!(shown(&harness, "new name.txt"), 4);
    }

    #[test]
    fn narrow_rows_cut_the_directory_and_keep_the_name() {
        const WIDTH: f32 = 280.0;
        let mut harness = sized_harness(GitPatch::parse(NESTED), egui::vec2(WIDTH, 600.0));
        harness.run();
        // The row widgets carry the whole path.
        harness.get_by_role_and_label(Role::Link, "crates/notedeck_ui/src/diff/patch_view.rs");
        harness.get_by_role_and_label(Role::Button, "crates/notedeck_ui/src/diff/patch_view.rs");

        // The labels are named by their whole text even when cut, so check
        // where they landed: in both rows, directory then name then `+2`, in
        // order and inside the view, so the directory gave up its end.
        let spans = |label: &str| -> Vec<(f64, f64)> {
            harness
                .query_all_by_label(label)
                .map(|n| {
                    let b = n.bounding_box().expect("laid out");
                    (b.x0, b.x1)
                })
                .collect()
        };
        let dirs = spans("crates/notedeck_ui/src/diff/");
        let names = spans("patch_view.rs");
        let adds = spans("+2");
        assert_eq!((dirs.len(), names.len(), adds.len()), (2, 2, 2));
        for ((dir, name), add) in dirs.iter().zip(&names).zip(&adds) {
            assert!(dir.1 <= name.0 + 0.5, "{dir:?} overlaps {name:?}");
            assert!(name.1 < add.0, "{name:?} runs into {add:?}");
            assert!(add.1 <= WIDTH as f64, "{add:?} is past the view");
        }
        // A root file has no directory part at all.
        assert_eq!(harness.query_all_by_label("README.md").count(), 4);
    }

    #[test]
    fn stat_labels_group_thousands_and_drop_zero() {
        assert_eq!(stat_label('+', 0), "");
        assert_eq!(stat_label('+', 17), "+17");
        assert_eq!(stat_label('−', 1655), "−1,655");
        assert_eq!(stat_label('+', 1_234_567), "+1,234,567");
        assert_eq!(stat_label('+', 100_000), "+100,000");
    }

    #[test]
    fn bar_blocks_split_in_proportion() {
        assert_eq!(bar_blocks(0, 0), (0, 0));
        // Fewer changed lines than blocks: one block each.
        assert_eq!(bar_blocks(2, 0), (2, 0));
        assert_eq!(bar_blocks(1, 1), (1, 1));
        assert_eq!(bar_blocks(1655, 0), (5, 0));
        assert_eq!(bar_blocks(0, 40), (0, 5));
        assert_eq!(bar_blocks(30, 20), (3, 2));
        // A lopsided change still shows its minority side.
        assert_eq!(bar_blocks(1000, 1), (4, 1));
        assert_eq!(bar_blocks(1, 1000), (1, 4));
    }

    #[test]
    fn split_path_keeps_the_slash_with_the_directory() {
        assert_eq!(split_path("a/b/c.rs"), ("a/b/", "c.rs"));
        assert_eq!(split_path("c.rs"), ("", "c.rs"));
    }

    #[test]
    fn only_rows_in_view_are_laid_out_and_jumps_land() {
        let mut harness = harness(tall_patch(600), 400.0);
        harness.run();
        assert_eq!(shown(&harness, "big 1"), 1);
        assert_eq!(shown(&harness, "big 600"), 0, "off-screen rows are virtual");
        assert_eq!(harness.state().1.current_file(), None);

        harness.state_mut().1.scroll(PatchScroll::File(1));
        harness.run();
        assert_eq!(shown(&harness, "new"), 1);
        assert_eq!(shown(&harness, "big 1"), 0);
        assert_eq!(harness.state().1.current_file(), Some(1));

        harness.state_mut().1.scroll(PatchScroll::PrevFile);
        harness.run();
        assert_eq!(harness.state().1.current_file(), Some(0));
        assert_eq!(shown(&harness, "big 1"), 1);

        harness.state_mut().1.scroll(PatchScroll::Rows(100));
        harness.run();
        assert_eq!(harness.state().1.current_file(), Some(0));
        // Deep in big.txt: its header is pinned over the top row.
        harness.get_by_role_and_label(Role::Button, "big.txt");
        assert_eq!(shown(&harness, "big 100"), 1);

        harness.state_mut().1.scroll(PatchScroll::NextFile);
        harness.run();
        assert_eq!(harness.state().1.current_file(), Some(1));

        harness.state_mut().1.scroll(PatchScroll::Top);
        harness.run();
        assert_eq!(harness.state().1.current_file(), None);
    }

    /// The rows a reader can read in full at `offset`, worked out row by row
    /// from the geometry rather than by the paging arithmetic: each row's
    /// drawn rect must sit inside the view and below the pinned header.
    /// `None` when no row is readable.
    fn readable(
        state: &GitPatchState,
        offset: f32,
        rows: RowMetrics,
        content: usize,
    ) -> Option<(usize, usize)> {
        let pinned = state.pinned_rows(row_at(offset, rows.step)) as f32 * rows.height;
        let mut shown = (0..content).filter(|&r| {
            let top = r as f32 * rows.step - offset;
            top >= pinned - ROW_SLACK && top + rows.height <= state.viewport_height + ROW_SLACK
        });
        let first = shown.next()?;
        Some((first, shown.next_back().unwrap_or(first)))
    }

    /// Paging a page down and back up, from aligned and unaligned offsets,
    /// in views that fit a whole number of rows and views that don't: a page
    /// down starts reading on the row after the last one read (or on that
    /// row's own file header, which a pinned header would otherwise hide),
    /// and a page up ends on the row before the first one read. So no line
    /// is skipped and none is read twice.
    #[test]
    fn a_page_neither_skips_nor_repeats_a_row() {
        let rows = RowMetrics {
            height: 18.0,
            step: 21.0,
        };
        for patch in [GitPatch::parse(MULTI), tall_patch(120)] {
            let mut state = GitPatchState::new(&patch, &mut Localization::default());
            let content = state.layout(&patch);
            let max = |state: &GitPatchState| content as f32 * rows.step - state.viewport_height;
            for viewport in [100.0, 147.0, 210.0, 333.0] {
                state.viewport_height = viewport;
                for start in (0..content).map(|r| r as f32 * rows.step + [0.0, 7.5][r % 2]) {
                    if start > max(&state) {
                        break;
                    }
                    let Some((_, last)) = readable(&state, start, rows, content) else {
                        continue;
                    };
                    let down = state.page_down(start, rows);
                    assert_eq!(down % rows.step, 0.0, "page down lands on a row");
                    if down <= max(&state) {
                        let (first, _) = readable(&state, down, rows, content).unwrap();
                        let header = state.file_rows.contains(&first);
                        assert!(
                            first == last + 1 || (first == last && header),
                            "viewport {viewport}, from {start}: read to {last}, then from {first}"
                        );
                    }

                    let (first, _) = readable(&state, start, rows, content).unwrap();
                    let up = state.page_up(start, rows);
                    assert_eq!(up % rows.step, 0.0, "page up lands on a row");
                    if up > 0.0 {
                        let (_, back_last) = readable(&state, up, rows, content).unwrap();
                        assert_eq!(
                            back_last + 1,
                            first,
                            "viewport {viewport}, from {start}: read from {first}, back to {back_last}"
                        );
                    }
                }
            }
        }
    }

    /// In the widget, pages move by whole rows: a page down lands on a row
    /// boundary short of a full view, two pages up return to the top, and a
    /// half page lands on a row too.
    #[test]
    fn pages_scroll_the_view_by_whole_rows() {
        let mut harness = harness(tall_patch(600), 400.0);
        harness.run();
        let offset = |h: &Harness<'_, (GitPatch, GitPatchState)>| h.state().1.offset;
        // One row's step, as the widget lays it out.
        harness.state_mut().1.scroll(PatchScroll::Rows(1));
        harness.run();
        let step = offset(&harness);
        harness.state_mut().1.scroll(PatchScroll::Top);
        harness.run();
        let on_a_row = |y: f32| (y / step - (y / step).round()).abs() < 1e-3;

        harness.state_mut().1.scroll(PatchScroll::Pages(1.0));
        harness.run();
        let paged = offset(&harness);
        let viewport = harness.state().1.viewport_height;
        assert!(on_a_row(paged), "{paged} is on a {step}pt row");
        assert!(
            paged <= viewport && paged > viewport - 2.0 * step,
            "{paged} vs {viewport}"
        );

        harness.state_mut().1.scroll(PatchScroll::Pages(1.0));
        harness.run();
        assert!(on_a_row(offset(&harness)));
        harness.state_mut().1.scroll(PatchScroll::Pages(-1.0));
        harness.run();
        harness.state_mut().1.scroll(PatchScroll::Pages(-1.0));
        harness.run();
        assert_eq!(offset(&harness), 0.0, "back at the top");

        harness.state_mut().1.scroll(PatchScroll::Pages(0.5));
        harness.run();
        let half = offset(&harness);
        assert!(
            on_a_row(half) && half > 0.0 && half < paged,
            "half a page: {half}"
        );
    }

    #[test]
    fn huge_files_start_collapsed_and_expand_on_click() {
        let mut harness = harness(tall_patch(COLLAPSE_LINES + 1), 400.0);
        harness.run();
        assert!(harness.state().1.is_collapsed(0));
        assert!(!harness.state().1.is_collapsed(1));
        assert_eq!(shown(&harness, "big 1"), 0);
        assert_eq!(shown(&harness, "new"), 1);

        harness
            .get_by_role_and_label(Role::Button, "big.txt")
            .click();
        harness.run();
        assert!(!harness.state().1.is_collapsed(0));
        assert_eq!(shown(&harness, "big 1"), 1);
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
        assert_eq!(shown(&harness, "no eol now"), 1);
    }

    /// One changed line each way, numbered so its gutter labels are unique.
    const ONE_CHANGE: &str =
        "diff --git a/x.txt b/x.txt\n--- a/x.txt\n+++ b/x.txt\n@@ -10 +10 @@\n-old\n+new\n";

    /// A harness with the chrome's zero horizontal item spacing, which the
    /// diff rows must not depend on; `render` attaches the software renderer.
    fn chrome_harness(
        patch: GitPatch,
        render: bool,
    ) -> Harness<'static, (GitPatch, GitPatchState)> {
        let state = GitPatchState::new(&patch, &mut Localization::default());
        let mut builder = Harness::builder().with_size(egui::vec2(400.0, 300.0));
        if render {
            builder = builder.renderer(notedeck::software_renderer());
        }
        builder.build_ui_state(
            |ui, (patch, state)| {
                ui.spacing_mut().item_spacing.x = 0.0;
                git_patch_ui(patch, state, ui)
            },
            (patch, state),
        )
    }

    /// Screen x-extent of the one node labelled `label`.
    fn span(harness: &Harness<'_, (GitPatch, GitPatchState)>, label: &str) -> (f64, f64, f64) {
        let b = harness
            .get_by_label(label)
            .bounding_box()
            .expect("laid out");
        (b.x0, b.x1, (b.y0 + b.y1) / 2.0)
    }

    #[test]
    fn diff_line_columns_sit_at_fixed_gaps() {
        let mut harness = chrome_harness(GitPatch::parse(ONE_CHANGE), false);
        harness.run();
        // Numbers, marker, content: each column a fixed gap after the last,
        // even with no item spacing to lean on. (Rounding to pixels moves an
        // edge by up to one.)
        for (gutter, marker, content) in [("  10     ", "-", "old"), ("       10", "+", "new")] {
            let (gutter, marker, content) = (
                span(&harness, gutter),
                span(&harness, marker),
                span(&harness, content),
            );
            let gap = marker.0 - gutter.1;
            assert!((gap - GUTTER_GAP as f64).abs() <= 1.0, "gutter gap {gap}");
            let gap = content.0 - marker.1;
            assert!((gap - MARKER_GAP as f64).abs() <= 1.0, "marker gap {gap}");
        }
    }

    #[test]
    #[ignore]
    fn diff_row_tints_rasterize_across_the_row() {
        let mut harness = chrome_harness(GitPatch::parse(ONE_CHANGE), true);
        harness.run();
        let old = span(&harness, "old");
        let new = span(&harness, "new");
        // Rasterize: tessellates the row fills, which a harness that only
        // runs never does.
        let image = harness.render().expect("software render");
        let ppp = harness.ctx.pixels_per_point() as f64;
        // Well past the end of the text, the row is still tinted its hue.
        let at = |y: f64| image.get_pixel((300.0 * ppp) as u32, (y * ppp) as u32).0;
        let [r, g, _, _] = at(old.2);
        assert!(r > g + 8, "deleted row not red at the far edge: {r},{g}");
        let [r, g, _, _] = at(new.2);
        assert!(g > r + 8, "inserted row not green at the far edge: {r},{g}");
    }
}
