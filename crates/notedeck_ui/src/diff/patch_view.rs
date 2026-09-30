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
use super::patch_images::{images_ui, FileImages, ImageGeometry, ShownImages};
use super::{
    file_extension, DiffTag, RowGalleys, DELETE_COLOR, DIFF_FONT_SIZE, INSERT_COLOR,
    LINE_NUMBER_COLOR,
};
use egui::emath::GuiRounding;
use egui::epaint::{text::TextOptions, TextShape};
use egui::text::{LayoutJob, TextWrapping};
use egui::text_selection::LabelSelectionState;
use egui::{
    Color32, FontId, Galley, Label, Rect, RichText, Role, ScrollArea, Sense, Stroke, Ui, UiBuilder,
    WidgetInfo,
};
use notedeck::{tr, tr_plural, Localization};
use std::collections::HashMap;
use std::ops::Range;
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

/// Lines picked in a [`git_patch_ui`] for a comment: a run of one file's
/// lines, inside one hunk. A click on a line's numbers picks it, a shift-click
/// stretches the pick to it and a click on a hunk's header picks the whole
/// hunk; the widget only reports it (see [`GitPatchState::selection`]), and
/// what a comment is, and where it goes, is the caller's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchSelection {
    pub file: usize,
    /// Indices into the file's [`lines`](FilePatch::lines), in order.
    pub lines: Range<usize>,
}

/// Whether a [`PatchNote`] has been sent yet: a draft is drawn warmer, so the
/// reviewer can tell what is still theirs to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchNoteKind {
    /// Written but not sent: marked in the warning colour.
    Draft,
    /// Sent: marked in the selection's colour.
    Posted,
}

/// A note the caller asks for under a file's lines — a review comment, say —
/// set with [`GitPatchState::set_notes`]. It sits under the last of its
/// `lines`, which get a bar in their gutter.
///
/// By default it takes one row and shows the first line of `text`. With
/// `caller_draws`, [`git_patch_ui_with`] hands its space to the caller's
/// drawer instead — a whole note with its author, say — and gives it as many
/// rows as the drawer turns out to need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchNote {
    /// The file (index into the patch's files) it's on.
    pub file: usize,
    /// Indices into the file's [`lines`](FilePatch::lines) the note is about.
    pub lines: Range<usize>,
    /// What the note says: the one line a plain note shows (hovering shows it
    /// all), and its accessible label.
    pub text: String,
    /// How its bar is coloured.
    pub kind: PatchNoteKind,
    /// The caller draws it (see [`git_patch_ui_with`]); `false` for the
    /// built-in one-line row.
    pub caller_draws: bool,
    /// The caller's own handle for it — an index into its comments, say —
    /// so its drawer knows what to draw. The widget never reads it.
    pub key: usize,
}

/// The pick being made: where it started and where it stretches to, both
/// line indices of `file` inside hunk `hunk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    file: usize,
    hunk: usize,
    anchor: usize,
    head: usize,
}

impl Selection {
    fn lines(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head) + 1
    }

    fn contains(&self, file: usize, line: usize) -> bool {
        self.file == file && self.lines().contains(&line)
    }
}

/// View state for one [`GitPatch`]: which files are collapsed, where the view
/// is, and the labels that don't change per frame. Build it with
/// [`GitPatchState::new`] when the patch changes, and give each patch its own
/// [`with_id_salt`](Self::with_id_salt) when one view shows several in turn.
#[derive(Debug, Clone, Default)]
pub struct GitPatchState {
    /// The scroll area's id salt. egui keeps a scroll area's offset in its
    /// memory under the area's id, not here, so two patches drawn at the same
    /// place with the same salt share one offset. `None` is `"git_patch"`.
    id_salt: Option<egui::Id>,
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
    /// Whether `offset` and `viewport_height` come from a pass yet. A new
    /// state reads 0 for both while egui may hold another offset for its
    /// salt (a reloaded diff reopens where it was left), so a request made
    /// before the first pass waits for it rather than being measured from 0.
    synced: bool,
    /// The file whose row is at the top of the view, if any.
    current_file: Option<usize>,
    /// The diff lines in view, laid out.
    galleys: LineGalleys,
    /// The lines picked for a comment, if any.
    selection: Option<Selection>,
    /// The caller's rows under diff lines, sorted by file and then by the
    /// line they sit under.
    line_notes: Vec<PatchNote>,
    /// Per file, its run of `line_notes`.
    note_spans: Vec<Range<usize>>,
    /// How many rows each of `line_notes` takes: one for a plain note, what
    /// its drawer last needed for one the caller draws.
    note_rows: Vec<usize>,
    /// Running totals of `note_rows`, one longer than it: the rows every note
    /// before index `n` takes is `note_prefix[n]`.
    note_prefix: Vec<usize>,
    /// Drawn notes whose drawer used a different number of rows than they
    /// had, as `(note, rows)`: applied once the pass's rows are all placed,
    /// since every row of a pass is located against the sizes it began with.
    resized: Vec<(usize, usize)>,
    /// What the caller said `line_notes` were built from (see
    /// [`GitPatchState::set_notes`]); `None` until it sets any.
    notes_stamp: Option<u64>,
    /// Per file, its changed images, when the caller set any (see
    /// [`GitPatchState::set_file_images`]). Empty until it does.
    images: Vec<Option<ShownImages>>,
    /// How big image rows are this pass; measured before the rows are laid
    /// out, so a file's row count and its drawing agree.
    image_geometry: ImageGeometry,
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
    /// The caller's note rows, one line each, per index into
    /// `GitPatchState::line_notes`.
    notes: HashMap<usize, CachedNote>,
    /// Image captions, per `(file, column)`.
    captions: HashMap<(usize, usize), CachedNote>,
    /// Counts passes, to tell the lines drawn in this one from the rest.
    pass: u64,
}

/// What a laid-out diff line depends on besides its text.
#[derive(Clone)]
struct GalleyKey {
    /// The scale the glyphs were rasterized at.
    pixels_per_point: f32,
    /// The text options the atlas was built with. epaint throws the atlas
    /// away when they change.
    text_options: TextOptions,
    /// How full the font atlas was. epaint clears the atlas once it is over
    /// 80% full, and a galley laid out against the old one points at glyphs
    /// that are gone. The atlas only ever grows between clears, so a lower
    /// ratio than last pass means it was cleared.
    atlas_fill: f32,
    /// Picks the syntax theme.
    dark_mode: bool,
}

impl GalleyKey {
    fn of(ui: &Ui) -> Self {
        let (text_options, atlas_fill) = ui.fonts(|f| (*f.options(), f.font_atlas_fill_ratio()));
        Self {
            pixels_per_point: ui.ctx().pixels_per_point(),
            text_options,
            atlas_fill,
            dark_mode: ui.visuals().dark_mode,
        }
    }

    /// Are galleys laid out under `self` still valid under `now`?
    fn still_valid(&self, now: &Self) -> bool {
        self.pixels_per_point == now.pixels_per_point
            && self.text_options == now.text_options
            && self.atlas_fill <= now.atlas_fill
            && self.dark_mode == now.dark_mode
    }
}

#[derive(Clone)]
struct CachedLine {
    galleys: RowGalleys,
    /// The last pass that drew it.
    pass: u64,
}

#[derive(Clone)]
struct CachedNote {
    /// The note's first line, unwrapped; the row clips it at the view's edge.
    galley: Arc<Galley>,
    pass: u64,
}

impl LineGalleys {
    /// Start a pass: drop everything if what it was laid out against changed.
    fn begin_pass(&mut self, ui: &Ui) {
        // The key is refreshed every pass, not only on a miss, so the atlas
        // fill it remembers is last pass's and a clear shows up as a drop.
        let key = GalleyKey::of(ui);
        if !self.key.as_ref().is_some_and(|k| k.still_valid(&key)) {
            self.lines.clear();
            self.notes.clear();
            self.captions.clear();
        }
        self.key = Some(key);
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

    /// Note `n`'s first line, laid out now if it wasn't in view last pass.
    fn note(&mut self, n: usize, note: &PatchNote, ui: &Ui) -> Arc<Galley> {
        let pass = self.pass;
        if let Some(cached) = self.notes.get_mut(&n) {
            cached.pass = pass;
            return cached.galley.clone();
        }
        let first = note.text.lines().next().unwrap_or_default().to_owned();
        let font = egui::TextStyle::Body.resolve(ui.style());
        let galley = ui.fonts_mut(|f| f.layout_no_wrap(first, font, ui.visuals().text_color()));
        self.notes.insert(
            n,
            CachedNote {
                galley: galley.clone(),
                pass,
            },
        );
        galley
    }

    /// Column `side` of file `f`'s image captions, `text`, laid out now if
    /// it wasn't in view last pass.
    fn caption(&mut self, f: usize, side: usize, text: &str, ui: &Ui) -> Arc<Galley> {
        let pass = self.pass;
        if let Some(cached) = self.captions.get_mut(&(f, side)) {
            cached.pass = pass;
            return cached.galley.clone();
        }
        let font = egui::TextStyle::Small.resolve(ui.style());
        let galley =
            ui.fonts_mut(|fonts| fonts.layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER));
        self.captions.insert(
            (f, side),
            CachedNote {
                galley: galley.clone(),
                pass,
            },
        );
        galley
    }

    /// End a pass: forget the lines, notes and captions it didn't draw.
    fn end_pass(&mut self) {
        let pass = self.pass;
        self.lines.retain(|_, line| line.pass == pass);
        self.notes.retain(|_, note| note.pass == pass);
        self.captions.retain(|_, caption| caption.pass == pass);
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
            ui.fonts_mut(|f| {
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

    /// Salt this patch's scroll area with `salt`, so its offset is its own.
    /// A view that swaps one patch for another in the same place (a review
    /// queue stepping cards) passes something that names the patch: a new
    /// one then opens at the top, and going back to one returns to where it
    /// was left. Without it every patch there shares one offset.
    pub fn with_id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.id_salt = Some(egui::Id::unique(salt));
        self
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

    /// The lines picked for a comment, if any (see [`PatchSelection`]).
    pub fn selection(&self) -> Option<PatchSelection> {
        self.selection.map(|s| PatchSelection {
            file: s.file,
            lines: s.lines(),
        })
    }

    /// Drop the pick, e.g. once a comment on it is written.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Show `notes` under their lines, replacing any set before. `stamp` is
    /// whatever the caller built them from, handed back by
    /// [`notes_stamp`](Self::notes_stamp), so it can tell when they're stale
    /// without rebuilding them every frame. A note on a file the patch
    /// doesn't have, on no lines, or on a file without hunks is dropped.
    pub fn set_notes(&mut self, patch: &GitPatch, mut notes: Vec<PatchNote>, stamp: u64) {
        let files = patch.files();
        notes.retain(|n| {
            files.get(n.file).is_some_and(|f| {
                !f.hunks.is_empty() && !n.lines.is_empty() && n.lines.end <= f.lines.len()
            })
        });
        // Stable, so notes on the same line keep the caller's order.
        notes.sort_by_key(|n| (n.file, n.lines.end));
        self.note_spans.clear();
        for f in 0..files.len() {
            let start = notes.partition_point(|n| n.file < f);
            let end = notes.partition_point(|n| n.file <= f);
            self.note_spans.push(start..end);
        }
        self.note_rows = vec![1; notes.len()];
        self.line_notes = notes;
        self.sum_note_rows();
        self.notes_stamp = Some(stamp);
        // Laid out per index, and the indices just changed.
        self.galleys.notes.clear();
    }

    /// Show file `file`'s changed images in place of its "Binary file not
    /// shown", replacing any set before: its before and after side by side,
    /// each captioned with its size. A side that couldn't be shown is
    /// captioned with why; when neither can, the file keeps its one note row
    /// and it says why instead. Captions are formatted here, once.
    ///
    /// The textures are the caller's to upload, once, when its load lands
    /// (see [`FileImages`]). A file index past the patch's files is ignored.
    pub fn set_file_images(&mut self, file: usize, images: FileImages, i18n: &mut Localization) {
        let files = self.collapsed.len();
        if file >= files {
            return;
        }
        if self.images.len() < files {
            self.images.resize(files, None);
        }
        self.images[file] = ShownImages::new(images, i18n);
        self.galleys.captions.retain(|&(f, _), _| f != file);
    }

    /// File `f`'s images, if it has any side to draw.
    fn shown_images(&self, f: usize) -> Option<&ShownImages> {
        self.images
            .get(f)
            .and_then(Option::as_ref)
            .filter(|i| i.any_shown())
    }

    /// The `stamp` the notes were last [set](Self::set_notes) with; `None`
    /// on a fresh state, so a reloaded patch asks for its notes again.
    pub fn notes_stamp(&self) -> Option<u64> {
        self.notes_stamp
    }

    /// File `f`'s notes.
    fn file_notes(&self, f: usize) -> &[PatchNote] {
        self.note_spans
            .get(f)
            .map_or(&[], |span| &self.line_notes[span.clone()])
    }

    /// Recount [`note_prefix`](Self::note_prefix) from `note_rows`.
    fn sum_note_rows(&mut self) {
        self.note_prefix.clear();
        self.note_prefix.push(0);
        let mut total = 0;
        for rows in &self.note_rows {
            total += rows;
            self.note_prefix.push(total);
        }
    }

    /// Give note `n` `rows` rows, as its drawer turned out to need.
    fn set_note_rows(&mut self, n: usize, rows: usize) {
        if let Some(slot) = self.note_rows.get_mut(n) {
            *slot = rows.max(1);
            self.sum_note_rows();
        }
    }

    /// How many of file `f`'s notes sit above line `i`: those under a line
    /// before it.
    fn notes_before(&self, f: usize, i: usize) -> usize {
        self.file_notes(f).partition_point(|n| n.lines.end <= i)
    }

    /// Rows the notes of file `f` above line `i` take (`i` past the end for
    /// all of them).
    fn note_rows_before(&self, f: usize, i: usize) -> usize {
        let Some(span) = self.note_spans.get(f) else {
            return 0;
        };
        let upto = span.start + self.notes_before(f, i);
        self.note_prefix[upto] - self.note_prefix[span.start]
    }

    /// Rows file `f`'s body takes when expanded: its hunk headers, lines and
    /// notes, or for a file without hunks its note or its images.
    fn body_rows(&self, f: usize, file: &FilePatch) -> usize {
        if file.hunks.is_empty() {
            self.shown_images(f)
                .map_or(1, |images| images.rows(self.image_geometry))
        } else {
            file.hunks.len() + file.lines.len() + self.note_rows_before(f, usize::MAX)
        }
    }

    /// Body row (0-based, after the file header) of hunk `h`'s header: every
    /// hunk before it took one header row plus its lines and their notes.
    fn hunk_row(&self, f: usize, file: &FilePatch, h: usize) -> usize {
        let start = file.hunks[h].lines.start;
        h + start + self.note_rows_before(f, start)
    }

    /// Body row of line `i`, which is in hunk `h`.
    fn line_row(&self, f: usize, h: usize, i: usize) -> usize {
        h + 1 + i + self.note_rows_before(f, i)
    }

    /// The hunk whose rows contain body row `b` (binary search on
    /// [`hunk_row`](Self::hunk_row), which grows with `h`). `file` must have
    /// hunks.
    fn hunk_at(&self, f: usize, file: &FilePatch, b: usize) -> usize {
        let (mut lo, mut hi) = (0, file.hunks.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.hunk_row(f, file, mid) <= b {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo.saturating_sub(1)
    }

    /// What body row `b` of hunk `h` shows, past its header: the last line
    /// at or above it, or one of that line's notes.
    fn hunk_body_row(&self, f: usize, file: &FilePatch, h: usize, b: usize) -> Row {
        let lines = file.hunks[h].lines.clone();
        // The last line whose row is at or above `b`.
        let (mut lo, mut hi) = (lines.start, lines.end);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.line_row(f, h, mid) <= b {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let i = lo.saturating_sub(1).max(lines.start);
        let at = self.line_row(f, h, i);
        if b == at {
            return Row::Line(f, i);
        }
        // Row `k` of the notes under line `i`: find the note it falls in.
        let k = b - at - 1;
        let first = self.note_spans[f].start + self.notes_before(f, i);
        let base = self.note_prefix[first];
        let past = self.note_prefix[first..].partition_point(|&p| p - base <= k);
        let note = first + past - 1;
        Row::Comment(note, k - (self.note_prefix[note] - base))
    }

    /// A click on line `i` of file `f`, in hunk `hunk`: pick it, or with
    /// `extend` stretch the pick to it when it's in the same hunk. A click on
    /// the one line already picked drops the pick.
    fn click_line(&mut self, f: usize, hunk: usize, i: usize, extend: bool) {
        let fresh = Selection {
            file: f,
            hunk,
            anchor: i,
            head: i,
        };
        self.selection = match self.selection {
            Some(s) if extend && s.file == f && s.hunk == hunk => Some(Selection { head: i, ..s }),
            Some(s) if !extend && s.file == f && s.anchor == i && s.head == i => None,
            _ => Some(fresh),
        };
    }

    /// A click on hunk `h`'s header in file `f`: pick all its lines.
    fn click_hunk(&mut self, patch: &GitPatch, f: usize, h: usize) {
        let lines = patch.files()[f].hunks[h].lines.clone();
        if lines.is_empty() {
            return;
        }
        self.selection = Some(Selection {
            file: f,
            hunk: h,
            anchor: lines.start,
            head: lines.end - 1,
        });
    }

    /// Lay the rows out for this pass: fill `file_rows` and return the
    /// content's row count. The summary takes the first `1 + files` rows.
    fn layout(&mut self, patch: &GitPatch) -> usize {
        let files = patch.files();
        self.file_rows.clear();
        let mut row = 1 + files.len();
        for (f, (file, collapsed)) in files.iter().zip(&self.collapsed).enumerate() {
            self.file_rows.push(row);
            row += 1;
            if !collapsed {
                row += self.body_rows(f, file);
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
            if self.shown_images(f).is_some() {
                return Row::Image(f, body - 1);
            }
            return Row::Note(f);
        }
        let b = body - 1;
        let h = self.hunk_at(f, file, b);
        if b == self.hunk_row(f, file, h) {
            Row::HunkHeader(f, h)
        } else {
            self.hunk_body_row(f, file, h, b)
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
    /// [`Self::layout`], against this pass's rows. Before the first pass the
    /// request is kept: every request is measured from the view (its offset,
    /// its height, the file at its top), which isn't known yet.
    fn take_target(&mut self, content_rows: usize, rows: RowMetrics) -> Option<f32> {
        if !self.synced {
            return None;
        }
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
    /// Row `.1` of a binary file's before and after images (see
    /// [`GitPatchState::set_file_images`]).
    Image(usize, usize),
    /// File, hunk index.
    HunkHeader(usize, usize),
    /// File, index into its `lines`.
    Line(usize, usize),
    /// Row `.1` of a caller's note, by index into
    /// `GitPatchState::line_notes`.
    Comment(usize, usize),
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

/// Draw `patch` with `state`, filling the available space. Every note gets
/// the built-in one-line row; see [`git_patch_ui_with`] to draw them.
pub fn git_patch_ui(patch: &GitPatch, state: &mut GitPatchState, ui: &mut Ui) {
    git_patch_ui_with(patch, state, ui, None);
}

/// Draws a [`PatchNote`] with `caller_draws` set, in a `Ui` as wide as the
/// diff past its gutter; the note takes as many rows as the drawer used.
pub type NoteDrawer<'a> = &'a mut dyn FnMut(&mut Ui, &PatchNote);

/// [`git_patch_ui`], with `draw_note` drawing the notes marked
/// `caller_draws` (the rest keep the one-line row). A drawn note starts one
/// row tall and grows to what its drawer used, a pass later.
#[profiling::function]
pub fn git_patch_ui_with(
    patch: &GitPatch,
    state: &mut GitPatchState,
    ui: &mut Ui,
    mut draw_note: Option<NoteDrawer<'_>>,
) {
    if state.collapsed.len() != patch.files().len() {
        // The caller swapped the patch without new state. Recover rather than
        // index out of bounds; the labels fall back to the source strings.
        *state = GitPatchState {
            id_salt: state.id_salt,
            pending: state.pending,
            offset: state.offset,
            viewport_height: state.viewport_height,
            synced: state.synced,
            ..GitPatchState::new(patch, &mut Localization::default())
        };
    }

    state.galleys.begin_pass(ui);
    state.columns.update(&state.rows, &state.totals, ui);
    let row_height = row_height(ui);
    let row_step = row_height + ui.spacing().item_spacing.y;
    state.image_geometry = ImageGeometry::new(
        ui,
        ui.cursor().left(),
        ui.max_rect().right(),
        image_indent(ui),
        row_step,
    );
    let content_rows = state.layout(patch);
    let rows = RowMetrics {
        height: row_height,
        step: row_step,
    };
    let target = state.take_target(content_rows, rows);
    let total_rows = content_rows + state.padding_rows(row_step);

    let id_salt = state
        .id_salt
        .unwrap_or_else(|| egui::Id::unique("git_patch"));
    let mut area = ScrollArea::both()
        .id_salt(id_salt)
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
                        match state.locate(patch, row) {
                            // A drawn note is drawn whole from its first row,
                            // or from the top of the view when that's above it.
                            Row::Comment(n, part) if state.line_notes[n].caller_draws => {
                                match draw_note.as_deref_mut() {
                                    Some(draw) if part == 0 || row == first => {
                                        drawn_note_ui(state, n, part, draw, ui)
                                    }
                                    Some(_) => skip_row(ui),
                                    None => row_ui(patch, state, Row::Comment(n, part), ui),
                                }
                            }
                            // Images are drawn whole the same way.
                            Row::Image(_, part) if part > 0 && row != first => skip_row(ui),
                            located => row_ui(patch, state, located, ui),
                        }
                    }
                });

                let view_top = top + viewport.min.y;
                sticky_header(patch, state, row_at(viewport.min.y, row_step), view_top, ui);
            })
        })
        .inner;

    state.galleys.end_pass();
    if !state.resized.is_empty() {
        for i in 0..state.resized.len() {
            let (n, rows) = state.resized[i];
            state.set_note_rows(n, rows);
        }
        state.resized.clear();
        ui.ctx().request_repaint();
    }
    state.offset = out.state.offset.y;
    state.viewport_height = out.inner_rect.height();
    state.synced = true;
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
    let mono = ui.fonts_mut(|f| f.row_height(&FontId::monospace(DIFF_FONT_SIZE)));
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
            if clickable(row, Role::Link, file.path()) {
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
            let images_note = state.images.get(f).and_then(Option::as_ref);
            let note: &str = if let Some(note) = images_note.and_then(ShownImages::note) {
                note
            } else if file.binary {
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
        Row::Image(f, part) => {
            let Some(images) = state.images.get(f).and_then(Option::as_ref) else {
                return skip_row(ui);
            };
            let galleys = &mut state.galleys;
            let mut caption = |side: usize| {
                let text = &images.sides[side].as_ref()?.caption;
                Some(galleys.caption(f, side, text, ui))
            };
            let captions = [caption(0), caption(1)];
            images_ui(
                images,
                captions,
                part,
                state.image_geometry,
                image_indent(ui),
                ui,
            );
        }
        Row::HunkHeader(f, h) => {
            let header = patch.text(patch.files()[f].hunks[h].header);
            let row = ui.horizontal(|ui| {
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
            // Takes the whole hunk for a comment.
            if clickable(row.response, Role::Button, header) {
                state.click_hunk(patch, f, h);
            }
        }
        Row::Comment(n, _) => {
            let galley = state.galleys.note(n, &state.line_notes[n], ui);
            note_row_ui(&state.line_notes[n], galley, ui);
        }
        Row::Line(f, i) => match state.galleys.get(patch, f, i, ui) {
            Some(galleys) => {
                let height = ui.spacing().interact_size.y;
                let min = ui.cursor().min;
                let row = Rect::from_min_size(
                    min,
                    egui::vec2((ui.clip_rect().right() - min.x).max(0.0), height),
                );
                let selected = state.selection.is_some_and(|s| s.contains(f, i));
                if selected {
                    // Under the diff's own tint and faint, so the picked
                    // lines still read as added or removed.
                    let tint = row.expand2(egui::vec2(0.0, ui.spacing().item_spacing.y / 2.0));
                    let color = ui.visuals().selection.bg_fill.gamma_multiply(SELECTED_TINT);
                    ui.painter().rect_filled(tint, 0.0, color);
                }
                let numbers = diff_line_ui(galleys, ui);
                // A note's lines get a bar down their left edge.
                let noted = state
                    .file_notes(f)
                    .iter()
                    .find(|n| n.lines.contains(&i))
                    .map(|n| n.kind);
                if let Some(kind) = noted {
                    let bar = Rect::from_min_size(row.min, egui::vec2(NOTE_BAR, height));
                    ui.painter()
                        .rect_filled(bar, 0.0, note_color(kind, ui.visuals()));
                }
                if numbers.clicked() {
                    let extend = ui.input(|input| input.modifiers.shift);
                    let hunk = hunk_of(&patch.files()[f], i);
                    state.click_line(f, hunk, i, extend);
                }
            }
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
///
/// Returns the line numbers' response: a click there picks the line for a
/// comment (see [`PatchSelection`]).
fn diff_line_ui(galleys: RowGalleys, ui: &mut Ui) -> egui::Response {
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

    let numbers = ui
        .interact(gutter_rect, id.with("gutter"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    numbers.widget_info(|| WidgetInfo::labeled(Role::Button, true, gutter.text()));
    ui.painter()
        .add(TextShape::new(gutter_rect.min, gutter, LINE_NUMBER_COLOR));
    if tag != DiffTag::Equal {
        let response = ui.interact(marker_rect, id.with("marker"), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, marker.text()));
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
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, content.text()));
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
    numbers
}

/// How much of the selection colour a picked line is tinted with.
const SELECTED_TINT: f32 = 0.35;

/// Width of the bar down the left edge of a note's lines and its row.
const NOTE_BAR: f32 = 3.0;

/// The colour a note of `kind` is marked in: a draft warm, so what is still
/// to send stands out, and a posted one in the selection's colour.
fn note_color(kind: PatchNoteKind, visuals: &egui::Visuals) -> Color32 {
    match kind {
        PatchNoteKind::Draft => visuals.warn_fg_color,
        PatchNoteKind::Posted => visuals.selection.stroke.color,
    }
}

/// A caller's note under its lines: a faint band with the note's bar on the
/// left, then its first line from `galley`, clipped at the view's edge.
/// Hovering shows the whole note. Placed without a child `Ui`, as a diff line
/// is, and laid out once (see [`LineGalleys::note`]).
fn note_row_ui(note: &PatchNote, galley: Arc<Galley>, ui: &mut Ui) {
    let height = ui.spacing().interact_size.y;
    let min = ui.cursor().min;
    let right = ui.clip_rect().right();
    let row = Rect::from_min_size(min, egui::vec2((right - min.x).max(0.0), height));
    let id = ui.advance_cursor_after_rect(row);
    let visuals = ui.visuals();
    let accent = note_color(note.kind, visuals);
    let painter = ui.painter();
    painter.rect_filled(row, 0.0, visuals.faint_bg_color);
    painter.rect_filled(
        Rect::from_min_size(row.min, egui::vec2(NOTE_BAR, height)),
        0.0,
        accent,
    );
    let text_min = egui::pos2(
        row.left() + ui.spacing().icon_width + STATUS_WIDTH,
        row.center().y - galley.size().y / 2.0,
    );
    painter
        .with_clip_rect(row.intersect(ui.clip_rect()))
        .galley(text_min.round_ui(), galley, visuals.text_color());
    let response = ui.interact(row, id.with("note"), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &note.text));
    response.on_hover_ui(|ui| {
        ui.label(&note.text);
    });
}

/// Note `n`, drawn by the caller's `draw` from row `part` of it (the row this
/// pass is at). Its band and bar are painted across every row it takes, the
/// drawer gets the width past the gutter, and when the drawer used more or
/// fewer rows than the note has, the note is resized once the pass is done.
/// Advances one row, as every row does; the note's later rows skip.
fn drawn_note_ui(
    state: &mut GitPatchState,
    n: usize,
    part: usize,
    draw: &mut dyn FnMut(&mut Ui, &PatchNote),
    ui: &mut Ui,
) {
    let height = ui.spacing().interact_size.y;
    let spacing = ui.spacing().item_spacing.y;
    let step = height + spacing;
    let min = ui.cursor().min;
    let rows = state.note_rows[n];
    let top = min.y - part as f32 * step;
    let right = ui.clip_rect().right().max(min.x);
    let note_rect = Rect::from_min_max(
        egui::pos2(min.x, top),
        egui::pos2(right, top + rows as f32 * step - spacing),
    );
    let visuals = ui.visuals();
    let accent = note_color(state.line_notes[n].kind, visuals);
    let painter = ui.painter();
    painter.rect_filled(note_rect, 0.0, visuals.faint_bg_color);
    painter.rect_filled(note_rect.with_max_x(min.x + NOTE_BAR), 0.0, accent);

    let inner = note_rect.with_min_x(min.x + ui.spacing().icon_width + STATUS_WIDTH);
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt(("patch_note", n))
            .max_rect(inner)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(note_rect.intersect(ui.clip_rect()));
    draw(&mut child, &state.line_notes[n]);

    let used = child.min_rect().height();
    let needed = (((used + spacing) / step).ceil() as usize).max(1);
    if needed != rows {
        // Not now: the rows after this one are still to be located against
        // the size this pass began with.
        state.resized.push((n, needed));
    }
    skip_row(ui);
}

/// How far right of a row's start an image column begins: under the path,
/// as a file's note is.
fn image_indent(ui: &Ui) -> f32 {
    ui.spacing().icon_width + STATUS_WIDTH
}

/// Take one row's space, drawing nothing: a drawn note's later rows.
fn skip_row(ui: &mut Ui) {
    let height = ui.spacing().interact_size.y;
    let row = Rect::from_min_size(ui.cursor().min, egui::vec2(0.0, height));
    ui.advance_cursor_after_rect(row);
}

/// The hunk of `file` that line `i` is in.
fn hunk_of(file: &FilePatch, i: usize) -> usize {
    file.hunks
        .partition_point(|h| h.lines.start <= i)
        .saturating_sub(1)
}

/// A file's header row: collapse arrow, then the same columns as its row in
/// the summary above, so paths and stats line up between the two. Returns
/// whether it was clicked.
fn file_header_ui(patch: &GitPatch, state: &GitPatchState, f: usize, ui: &mut Ui) -> bool {
    let file = &patch.files()[f];
    let openness = if state.is_collapsed(f) { 0.0 } else { 1.0 };
    let row = file_row_ui(file, &state.rows[f], &state.columns, Some(openness), ui);
    clickable(row, Role::Button, file.path())
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
        let name = ui.fonts_mut(|f| f.layout_no_wrap(name.to_owned(), body.clone(), name_color));
        let right = row_right(ui);
        let room = right - ui.cursor().left() - name.size().x - COLUMN_GAP - columns.width();
        let mut elided = false;
        if !row.dir.is_empty() {
            let mut job = LayoutJob::simple_singleline(row.dir.clone(), body, dir_color);
            job.wrap = TextWrapping::truncate_at_width(room.max(0.0));
            let dir = ui.fonts_mut(|f| f.layout_job(job));
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
fn clickable(row: egui::Response, typ: Role, label: &str) -> bool {
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
        let galley = ui.fonts_mut(|f| f.layout_no_wrap(label.clone(), font.clone(), color));
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
    use crate::diff::{DiffSide, FileImages, ImageSide, LineSpan, PatchImage};
    use egui::accesskit::Role;
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };

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
        // egui's default fonts lack the rename arrow `→`; it draws as tofu
        // here, as before egui 0.36 made a missing glyph panic under kittest.
        Harness::builder()
            .with_size(size)
            .allow_missing_glyphs()
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

    /// A note takes a row under the last of its lines, after any before it
    /// on the same line, and the rows after it shift down, across a hunk
    /// boundary too. Collapsing the file hides its notes with it.
    #[test]
    fn notes_take_rows_under_their_lines() {
        let patch = GitPatch::parse(MULTI);
        let mut state = GitPatchState::new(&patch, &mut Localization::default());
        let plain = state.layout(&patch);
        let note = |lines: Range<usize>, text: &str| PatchNote {
            file: 4,
            lines,
            text: text.to_string(),
            kind: PatchNoteKind::Posted,
            caller_draws: false,
            key: 0,
        };
        // long.txt: hunk 0 is lines 0..7, hunk 1 lines 7..15. Given out of
        // order; one on a file that doesn't exist is dropped.
        let notes = vec![
            note(7..8, "b"),
            note(5..7, "a"),
            note(7..8, "c"),
            PatchNote {
                file: 99,
                ..note(0..1, "gone")
            },
        ];
        state.set_notes(&patch, notes, 7);
        assert_eq!(state.notes_stamp(), Some(7));
        let total = state.layout(&patch);
        assert_eq!(total, plain + 3);

        let rows: Vec<_> = (0..total).map(|r| state.locate(&patch, r)).collect();
        let long = state.file_rows[4];
        let texts = |r: Row| match r {
            Row::Comment(n, _) => state.line_notes[n].text.as_str(),
            _ => "",
        };
        assert_eq!(rows[long + 8], Row::Line(4, 6));
        assert_eq!(texts(rows[long + 9]), "a");
        assert_eq!(rows[long + 10], Row::HunkHeader(4, 1));
        assert_eq!(rows[long + 11], Row::Line(4, 7));
        assert_eq!(texts(rows[long + 12]), "b");
        assert_eq!(texts(rows[long + 13]), "c");
        assert_eq!(rows[long + 14], Row::Line(4, 8));
        assert_eq!(*rows.last().unwrap(), Row::Line(7, 3));

        state.set_collapsed(4, true);
        assert_eq!(state.layout(&patch), plain - 17);
        assert_eq!(state.locate(&patch, long + 1), Row::FileHeader(5));

        // A note grown to three rows (as a drawn one does) takes all three,
        // each naming its row of the note, and pushes the rest down.
        state.set_collapsed(4, false);
        state.set_note_rows(0, 3);
        assert_eq!(state.layout(&patch), plain + 5);
        for part in 0..3 {
            assert_eq!(state.locate(&patch, long + 9 + part), Row::Comment(0, part));
        }
        assert_eq!(state.locate(&patch, long + 12), Row::HunkHeader(4, 1));
        assert_eq!(state.locate(&patch, long + 14), Row::Comment(1, 0));
    }

    /// A drawn note that turns out shorter than the rows it had (a note
    /// whose content got shorter, say) is resized after the pass, not in the
    /// middle of it: the rows below it in the same pass were placed against
    /// the old size, and resizing under them mapped one past the last note
    /// (index out of bounds in the row loop).
    #[test]
    fn a_drawn_note_that_shrinks_is_resized_after_the_pass() {
        let patch = GitPatch::parse(MULTI);
        let mut state = GitPatchState::new(&patch, &mut Localization::default());
        let note = PatchNote {
            file: 4,
            lines: 0..1,
            text: "drawn".to_string(),
            kind: PatchNoteKind::Posted,
            caller_draws: true,
            key: 0,
        };
        state.set_notes(&patch, vec![note], 1);
        // As tall as a longer note was last pass.
        state.set_note_rows(0, 4);
        let size = egui::vec2(800.0, 1600.0);
        // MULTI renames a file; see `sized_harness` for the tofu `→`.
        let mut harness = Harness::builder()
            .with_size(size)
            .allow_missing_glyphs()
            .build_ui_state(
                |ui, (patch, state): &mut (GitPatch, GitPatchState)| {
                    let mut draw = |ui: &mut Ui, _: &PatchNote| {
                        ui.label("one line now");
                    };
                    git_patch_ui_with(patch, state, ui, Some(&mut draw));
                },
                (patch, state),
            );
        harness.run();
        assert!(harness.query_by_label("one line now").is_some());
        let (_, state) = harness.state();
        assert!(state.note_rows[0] < 4, "shrank to {}", state.note_rows[0]);
    }

    /// A note the caller draws is handed to its drawer, and grows to the rows
    /// the drawer used: the line after it moves down by as many.
    #[test]
    fn a_drawn_note_grows_to_what_its_drawer_used() {
        let patch = GitPatch::parse(MULTI);
        let mut state = GitPatchState::new(&patch, &mut Localization::default());
        let note = PatchNote {
            file: 4,
            lines: 0..1,
            text: "drawn".to_string(),
            kind: PatchNoteKind::Posted,
            caller_draws: true,
            key: 0,
        };
        state.set_notes(&patch, vec![note], 1);
        let size = egui::vec2(800.0, 1600.0);
        // MULTI renames a file; see `sized_harness` for the tofu `→`.
        let mut harness = Harness::builder()
            .with_size(size)
            .allow_missing_glyphs()
            .build_ui_state(
                |ui, (patch, state): &mut (GitPatch, GitPatchState)| {
                    let mut draw = |ui: &mut Ui, note: &PatchNote| {
                        ui.label(format!("{} by someone", note.text));
                        ui.label("second line");
                        ui.label("third line");
                    };
                    git_patch_ui_with(patch, state, ui, Some(&mut draw));
                },
                (patch, state),
            );
        harness.run();
        assert!(harness.query_by_label("drawn by someone").is_some());
        assert!(harness.query_by_label("third line").is_some());
        let (_, state) = harness.state();
        assert!(state.note_rows[0] >= 3, "grew to {}", state.note_rows[0]);
    }

    /// A click picks one line, a shift-click stretches the pick within its
    /// hunk, a shift-click into another hunk starts over there, a click on
    /// the one picked line drops it, and a hunk header takes the whole hunk.
    #[test]
    fn clicks_pick_lines_within_one_hunk() {
        let patch = GitPatch::parse(MULTI);
        let mut state = GitPatchState::new(&patch, &mut Localization::default());
        let picked = |state: &GitPatchState| state.selection().map(|s| (s.file, s.lines));

        state.click_line(4, 0, 4, false);
        assert_eq!(picked(&state), Some((4, 4..5)));
        state.click_line(4, 0, 2, true);
        assert_eq!(picked(&state), Some((4, 2..5)), "stretched up to line 2");
        state.click_line(4, 1, 9, true);
        assert_eq!(picked(&state), Some((4, 9..10)), "another hunk starts over");
        state.click_line(4, 1, 9, false);
        assert_eq!(picked(&state), None);

        state.click_hunk(&patch, 4, 1);
        assert_eq!(picked(&state), Some((4, 7..15)));
        state.clear_selection();
        assert_eq!(picked(&state), None);
    }

    /// Clicking a hunk's header in the drawn patch picks its lines.
    #[test]
    fn clicking_a_hunk_header_picks_the_hunk() {
        let mut harness = harness(GitPatch::parse(MULTI), 1600.0);
        harness.run();
        harness
            .get_by_role_and_label(Role::Button, "@@ -1,3 +1,4 @@")
            .click_accesskit();
        harness.run();
        let (patch, state) = harness.state();
        let main = patch.file_named("main.rs").unwrap();
        let picked = state.selection().unwrap();
        assert_eq!(picked.file, main);
        assert_eq!(picked.lines, patch.files()[main].hunks[0].lines);
    }

    /// A pick's line numbers are the new file's when it has any, else the
    /// old file's for deletions alone; `lines_in` maps them back.
    #[test]
    fn line_spans_name_a_side_and_map_back() {
        let patch = GitPatch::parse(MULTI);
        let long = &patch.files()[patch.file_named("long.txt").unwrap()];
        let new3 = LineSpan {
            side: DiffSide::New,
            start: 3,
            end: 3,
        };
        let old3 = LineSpan {
            side: DiffSide::Old,
            ..new3
        };
        // -line 3 / +LINE 3 together: the new side's 3.
        assert_eq!(long.line_span(2..4), Some(new3));
        // The deletion alone: the old side's 3.
        assert_eq!(long.line_span(2..3), Some(old3));
        // A context run: new 4-6.
        assert_eq!(
            long.line_span(4..7),
            Some(LineSpan {
                start: 4,
                end: 6,
                ..new3
            })
        );
        assert_eq!(long.lines_in(new3), Some(3..4));
        assert_eq!(long.lines_in(old3), Some(2..3));
        assert_eq!(
            long.lines_in(LineSpan {
                start: 900,
                end: 901,
                ..new3
            }),
            None
        );
        assert_eq!(long.line_span(3..3), None);
    }

    #[test]
    fn renders_summary_files_hunks_and_markers() {
        // Tall enough for the whole patch.
        let mut harness = harness(GitPatch::parse(MULTI), 1600.0);
        harness.run();

        // Fluent wraps the count in bidi isolation marks once the ftl has
        // the key, so match around them.
        harness.get_by_label_contains("files changed");
        // The summary link, then the header button. Since egui 0.36 the path
        // text drawn inside each is no longer a node of its own, so each row
        // reads its path once.
        assert_eq!(shown(&harness, "main.rs"), 2);
        harness.get_by_role_and_label(Role::Link, "main.rs");
        harness.get_by_role_and_label(Role::Button, "main.rs");
        // The hunk header is the button that picks the whole hunk; its text
        // is drawn inside it, so (as the paths above) it reads once.
        assert_eq!(shown(&harness, "@@ -1,3 +1,4 @@"), 1);
        harness.get_by_role_and_label(Role::Button, "@@ -1,3 +1,4 @@");
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
        assert_eq!(shown(&harness, "new name.txt"), 2);
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
                    let b = n.accesskit_node().bounding_box().expect("laid out");
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
        // A root file has no directory part at all: its link and its header
        // button, each read once.
        assert_eq!(harness.query_all_by_label("README.md").count(), 2);
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

    /// A patch swapped in where another was scrolled opens at the top when
    /// each has its own salt, and swapping the first back returns to where it
    /// was left, not to where the second was scrolled. With one shared salt
    /// the new patch would inherit the old one's offset from egui's memory:
    /// the review queue's "next card opens halfway down its diff".
    #[test]
    fn a_salted_patch_keeps_its_own_scroll() {
        let (patch, state) = salted_tall_patch("first");
        let mut harness = Harness::builder()
            .with_size(egui::vec2(800.0, 400.0))
            .build_ui_state(
                |ui, (patch, state): &mut (GitPatch, GitPatchState)| git_patch_ui(patch, state, ui),
                (patch, state),
            );
        harness.run();
        harness.state_mut().1.scroll(PatchScroll::Pages(2.0));
        harness.run();
        let scrolled = harness.state().1.offset;
        assert!(scrolled > 0.0, "the first patch scrolled");

        let first = std::mem::replace(harness.state_mut(), salted_tall_patch("second"));
        harness.run();
        assert_eq!(
            harness.state().1.offset,
            0.0,
            "a new patch opens at the top"
        );
        harness.state_mut().1.scroll(PatchScroll::Pages(1.0));
        harness.run();
        let second = harness.state().1.offset;
        assert!(
            second > 0.0 && second != scrolled,
            "the second scrolled elsewhere"
        );

        *harness.state_mut() = first;
        harness.run();
        assert_eq!(harness.state().1.offset, scrolled, "back where it was left");
    }

    /// A state rebuilt for a patch egui already scrolled (a reloaded diff
    /// under the same salt) measures a request made before its first pass
    /// from where the view really is, not from its own zeroed offset.
    #[test]
    fn a_rebuilt_state_scrolls_from_the_real_offset() {
        let (patch, state) = salted_tall_patch("reloaded");
        let mut harness = Harness::builder()
            .with_size(egui::vec2(800.0, 400.0))
            .build_ui_state(
                |ui, (patch, state): &mut (GitPatch, GitPatchState)| git_patch_ui(patch, state, ui),
                (patch, state),
            );
        harness.run();
        harness.state_mut().1.scroll(PatchScroll::Rows(1));
        harness.run();
        let step = harness.state().1.offset;
        harness.state_mut().1.scroll(PatchScroll::Pages(2.0));
        harness.run();
        let scrolled = harness.state().1.offset;
        assert!(scrolled > 3.0 * step, "paged well past three rows");

        *harness.state_mut() = salted_tall_patch("reloaded");
        harness.state_mut().1.scroll(PatchScroll::Rows(3));
        harness.run();
        assert_eq!(
            harness.state().1.offset,
            scrolled + 3.0 * step,
            "three rows on from where the view was"
        );
    }

    /// A 600-line patch and a fresh state for it, salted with `salt`.
    fn salted_tall_patch(salt: &str) -> (GitPatch, GitPatchState) {
        let patch = tall_patch(600);
        let state = GitPatchState::new(&patch, &mut Localization::default()).with_id_salt(salt);
        (patch, state)
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
            .click_accesskit();
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
        let mut builder = Harness::builder()
            .with_size(egui::vec2(400.0, 300.0))
            .allow_missing_glyphs();
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
            .accesskit_node()
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

    /// A commit changing one image in place, adding another, and changing a
    /// binary file that isn't one.
    const IMAGES: &str = "\
diff --git a/shot.png b/shot.png
index 1111111..2222222 100644
Binary files a/shot.png and b/shot.png differ
diff --git a/new.png b/new.png
new file mode 100644
index 0000000..3333333
Binary files /dev/null and b/new.png differ
diff --git a/data.bin b/data.bin
index 4444444..5555555 100644
Binary files a/data.bin and b/data.bin differ
";

    /// A flat `w`×`h` side, uploaded to `ctx`.
    fn image_side(ctx: &egui::Context, w: usize, h: usize, bytes: u64) -> ImageSide {
        let pixels = egui::ColorImage::filled([w, h], Color32::from_rgb(200, 80, 40));
        ImageSide::Shown(PatchImage {
            texture: notedeck::media::load_texture_checked(ctx, "side", pixels, Default::default()),
            width: w as u32,
            height: h as u32,
            bytes,
        })
    }

    /// The images patch in a harness, with `shot.png` given both sides and
    /// `new.png` its after.
    fn image_harness() -> Harness<'static, (GitPatch, GitPatchState)> {
        let mut harness = harness(GitPatch::parse(IMAGES), 900.0);
        let ctx = harness.ctx.clone();
        let mut i18n = Localization::no_bidi();
        let (_, state) = harness.state_mut();
        let shot = FileImages {
            old: Some(image_side(&ctx, 120, 80, 142 * 1024)),
            new: Some(image_side(&ctx, 160, 90, 9_542_000)),
        };
        state.set_file_images(0, shot, &mut i18n);
        let added = FileImages {
            old: None,
            new: Some(image_side(&ctx, 64, 64, 812)),
        };
        state.set_file_images(1, added, &mut i18n);
        harness.run();
        harness
    }

    /// A changed image shows its before and after, captioned with their
    /// sizes; an added one only its after; a binary file with no images
    /// keeps its note.
    #[test]
    fn binary_images_show_before_and_after() {
        let harness = image_harness();
        for caption in [
            "before 120×80 · 142 KB",
            "after 160×90 · 9.1 MB",
            "after 64×64 · 812 B",
        ] {
            assert_eq!(shown(&harness, caption), 2, "{caption}: image + caption");
        }
        assert_eq!(
            harness.query_all_by_role(Role::Image).count(),
            3,
            "one image per side shown"
        );
        assert_eq!(shown(&harness, "Binary file not shown"), 1, "data.bin");
    }

    /// An image file's rows follow its header, as many as its tallest side
    /// and caption need, and the next file's header comes after them.
    /// Collapsing the file hides them.
    #[test]
    fn image_rows_span_the_tallest_side() {
        let mut harness = image_harness();
        let (patch, state) = harness.state_mut();
        let total = state.layout(patch);
        let rows: Vec<_> = (0..total).map(|r| state.locate(patch, r)).collect();
        let shot = state.file_rows[0];
        let added = state.file_rows[1];
        assert_eq!(rows[shot], Row::FileHeader(0));
        let image_rows = added - shot - 1;
        assert!(
            image_rows > 3,
            "90pt image + caption span rows: {image_rows}"
        );
        for (part, row) in rows[shot + 1..added].iter().enumerate() {
            assert_eq!(*row, Row::Image(0, part));
        }
        assert_eq!(rows[added], Row::FileHeader(1));
        assert_eq!(rows[state.file_rows[2] + 1], Row::Note(2));

        state.set_collapsed(0, true);
        assert_eq!(state.layout(patch), total - image_rows);
    }

    /// With no side it can draw, the file keeps one note row, saying why.
    #[test]
    fn unshown_images_say_why_in_the_note() {
        let mut harness = harness(GitPatch::parse(IMAGES), 900.0);
        let mut i18n = Localization::no_bidi();
        let big = FileImages {
            old: Some(ImageSide::TooLarge { bytes: 9_542_000 }),
            new: Some(ImageSide::Omitted),
        };
        harness.state_mut().1.set_file_images(0, big, &mut i18n);
        harness.run();
        assert_eq!(shown(&harness, "before: too large to show (9.1 MB)"), 1);
        assert_eq!(shown(&harness, "Binary file not shown"), 2);
        assert_eq!(harness.query_all_by_role(Role::Image).count(), 0);
    }
}
