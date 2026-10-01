//! Diff line rendering: one row per line with a line-number gutter, a `+`/`-`
//! prefix, a red/green background tint and syntax-highlighted content.
//!
//! This is the renderer Dave uses for tool-call file edits, lifted here so any
//! crate (headway's review pane, ...) can draw a diff. It knows nothing about
//! where the lines came from: callers describe each line as a borrowed
//! [`DiffRow`], numbering them with [`DiffNumbering`] when the numbers are
//! sequential from one start line.
//!
//! On top of it, [`GitPatch`] parses a multi-file git patch and
//! [`git_patch_ui`] draws one, virtualized, with collapsible files.

mod patch;
mod patch_images;
mod patch_view;

pub use patch::{
    DiffSide, FilePatch, FileStatus, GitPatch, Hunk, LineKind, LineSpan, PatchLine, Span,
};
pub use patch_images::{FileImages, ImageSide, PatchImage};
pub use patch_view::{
    git_patch_ui, git_patch_ui_with, GitPatchState, NoteDrawer, PatchNote, PatchNoteKind,
    PatchScroll, PatchSelection,
};

use crate::markdown::{tokenize_code, SandCodeTheme};
use egui::text::LayoutJob;
use egui::{Color32, FontId, Galley, RichText, TextFormat, Ui};
use std::fmt::Write;
use std::sync::Arc;

/// Strong colour of a deleted line's `-` prefix.
pub const DELETE_COLOR: Color32 = Color32::from_rgb(200, 60, 60);
/// Strong colour of an inserted line's `+` prefix.
pub const INSERT_COLOR: Color32 = Color32::from_rgb(60, 180, 60);
/// Colour of the line-number gutter (and other diff chrome, like expand links).
pub const LINE_NUMBER_COLOR: Color32 = Color32::from_rgb(128, 128, 128);

/// Font size (monospace) of diff content.
pub(crate) const DIFF_FONT_SIZE: f32 = 12.0;
/// Font size (monospace) of the line-number gutter.
const GUTTER_FONT_SIZE: f32 = 11.0;

/// Soft background tints for syntax-highlighted diff lines.
/// Uses premultiplied alpha: rgb(200,60,60) @ alpha=40 and rgb(60,180,60) @ alpha=40.
const DELETE_BG: Color32 = Color32::from_rgba_premultiplied(31, 9, 9, 40);
const INSERT_BG: Color32 = Color32::from_rgba_premultiplied(9, 28, 9, 40);
/// Stronger tints of the same hues, for the line-number gutter of a changed
/// row: rgb(200,60,60) @ alpha=70 and rgb(60,180,60) @ alpha=70.
const DELETE_GUTTER_BG: Color32 = Color32::from_rgba_premultiplied(55, 16, 16, 70);
const INSERT_GUTTER_BG: Color32 = Color32::from_rgba_premultiplied(16, 49, 16, 70);

/// Whether a diff line is unchanged context, removed, or added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffTag {
    Equal,
    Delete,
    Insert,
}

/// One line of a diff, as the renderer needs it. Borrows its text, so building
/// rows on the fly each frame costs nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffRow<'a> {
    pub tag: DiffTag,
    /// 1-based line number in the old file; `None` for an inserted line.
    pub old_no: Option<usize>,
    /// 1-based line number in the new file; `None` for a deleted line.
    pub new_no: Option<usize>,
    /// The line's content. A trailing newline is ignored.
    pub text: &'a str,
}

/// Numbers a run of diff lines that are contiguous in both files: context
/// lines advance both counters, deletions only the old one, insertions only
/// the new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffNumbering {
    old: usize,
    new: usize,
}

impl DiffNumbering {
    /// Start numbering at the given 1-based old/new file line numbers.
    pub fn starting_at(old: usize, new: usize) -> Self {
        Self { old, new }
    }

    /// Number the next line and advance the counters it consumes.
    pub fn row<'a>(&mut self, tag: DiffTag, text: &'a str) -> DiffRow<'a> {
        let (old_no, new_no) = match tag {
            DiffTag::Equal => (Some(self.old), Some(self.new)),
            DiffTag::Delete => (Some(self.old), None),
            DiffTag::Insert => (None, Some(self.new)),
        };
        if old_no.is_some() {
            self.old += 1;
        }
        if new_no.is_some() {
            self.new += 1;
        }
        DiffRow {
            tag,
            old_no,
            new_no,
            text,
        }
    }
}

/// Renders diff rows, one `ui.horizontal` per line.
pub struct DiffLines<'a> {
    lang: &'a str,
    gutter: bool,
}

impl<'a> DiffLines<'a> {
    /// Highlight content as `lang` (a [`tokenize_code`] language name or file
    /// extension). The line-number gutter is shown by default.
    pub fn new(lang: &'a str) -> Self {
        Self { lang, gutter: true }
    }

    /// Highlight content by the extension of `path` (plain text if it has none).
    pub fn for_path(path: &'a str) -> Self {
        Self::new(file_extension(path).unwrap_or("text"))
    }

    /// Show or hide the old/new line-number gutter.
    pub fn gutter(mut self, gutter: bool) -> Self {
        self.gutter = gutter;
        self
    }

    /// Draw `rows` into `ui`.
    pub fn show<'r>(self, rows: impl IntoIterator<Item = DiffRow<'r>>, ui: &mut Ui) {
        let font_id = diff_font();
        let theme = SandCodeTheme::from_visuals(ui.visuals());

        for row in rows {
            ui.horizontal(|ui| self.row_ui(&row, &theme, &font_id, ui));
        }
    }

    fn row_ui(&self, row: &DiffRow<'_>, theme: &SandCodeTheme, font_id: &FontId, ui: &mut Ui) {
        if self.gutter {
            ui.label(
                RichText::new(gutter_text(row.old_no, row.new_no))
                    .font(gutter_font())
                    .color(LINE_NUMBER_COLOR),
            );
        }

        let plain = ui.visuals().text_color();
        ui.label(row_job(row, self.lang, theme, font_id, plain));
    }
}

/// Font of diff content.
fn diff_font() -> FontId {
    FontId::monospace(DIFF_FONT_SIZE)
}

/// Font of the line-number gutter.
fn gutter_font() -> FontId {
    FontId::monospace(GUTTER_FONT_SIZE)
}

/// The content of `row` as one layout job: its prefix (with its trailing
/// space) in the prefix's strong colour, then its syntax-highlighted tokens,
/// all on the background tint that signals diff status across the whole
/// line. `plain` colours an unchanged line's blank prefix.
fn row_job(
    row: &DiffRow<'_>,
    lang: &str,
    theme: &SandCodeTheme,
    font_id: &FontId,
    plain: Color32,
) -> LayoutJob {
    let (prefix, prefix_color, line_bg) = match row.tag {
        DiffTag::Equal => ("  ", plain, Color32::TRANSPARENT),
        DiffTag::Delete => ("- ", DELETE_COLOR, DELETE_BG),
        DiffTag::Insert => ("+ ", INSERT_COLOR, INSERT_BG),
    };

    let mut job = LayoutJob::default();
    job.append(
        prefix,
        0.0,
        TextFormat {
            font_id: font_id.clone(),
            color: prefix_color,
            background: line_bg,
            ..Default::default()
        },
    );
    append_tokens(&mut job, row.text, lang, theme, font_id, line_bg);
    job
}

/// Append `text`'s syntax-highlighted tokens to `job`, each on `background`.
/// A trailing newline is dropped.
fn append_tokens(
    job: &mut LayoutJob,
    text: &str,
    lang: &str,
    theme: &SandCodeTheme,
    font_id: &FontId,
    background: Color32,
) {
    let content = text.trim_end_matches('\n');
    for (token, text) in tokenize_code(content, lang) {
        let mut fmt = theme.format(token, font_id);
        fmt.background = background;
        job.append(text, 0.0, fmt);
    }
}

/// A diff row laid out once, for callers that keep it across frames instead
/// of laying the row out every frame like [`DiffLines`] does. Unlike
/// [`DiffLines`]' rows, the `+`/`-` marker is its own galley and nothing
/// carries a background: the caller places the columns and tints the whole
/// row (see [`DiffTag::tints`]). All three galleys are unwrapped.
#[derive(Clone)]
pub(crate) struct RowGalleys {
    pub tag: DiffTag,
    /// The old/new line numbers, as [`DiffLines`]' gutter shows them.
    pub gutter: Arc<Galley>,
    /// `+`, `-`, or a space for an unchanged line, in the content's font so
    /// the marker column is one content glyph wide.
    pub marker: Arc<Galley>,
    /// The highlighted content, without the marker.
    pub content: Arc<Galley>,
}

impl RowGalleys {
    /// Lay out `row`, highlighting its content as `lang`.
    pub fn layout(row: &DiffRow<'_>, lang: &str, ui: &Ui) -> Self {
        let theme = SandCodeTheme::from_visuals(ui.visuals());
        let font_id = diff_font();
        let mut job = LayoutJob::default();
        append_tokens(
            &mut job,
            row.text,
            lang,
            &theme,
            &font_id,
            Color32::TRANSPARENT,
        );
        let (marker, marker_color) = match row.tag {
            DiffTag::Equal => (" ", LINE_NUMBER_COLOR),
            DiffTag::Delete => ("-", DELETE_COLOR),
            DiffTag::Insert => ("+", INSERT_COLOR),
        };
        let gutter = gutter_text(row.old_no, row.new_no);
        ui.fonts_mut(|fonts| Self {
            tag: row.tag,
            gutter: fonts.layout_no_wrap(gutter, gutter_font(), LINE_NUMBER_COLOR),
            marker: fonts.layout_no_wrap(marker.to_owned(), font_id.clone(), marker_color),
            content: fonts.layout_job(job),
        })
    }
}

impl DiffTag {
    /// The row tint and the stronger gutter tint of a changed line; `None`
    /// for an unchanged one, which isn't tinted.
    pub(crate) fn tints(self) -> Option<(Color32, Color32)> {
        match self {
            DiffTag::Equal => None,
            DiffTag::Delete => Some((DELETE_BG, DELETE_GUTTER_BG)),
            DiffTag::Insert => Some((INSERT_BG, INSERT_GUTTER_BG)),
        }
    }
}

/// The gutter label for one row: old and new line numbers, each right-aligned
/// in four columns (blank when absent), separated by a space.
fn gutter_text(old: Option<usize>, new: Option<usize>) -> String {
    let mut s = String::with_capacity(9);
    push_line_no(&mut s, old);
    s.push(' ');
    push_line_no(&mut s, new);
    s
}

fn push_line_no(s: &mut String, n: Option<usize>) {
    match n {
        Some(n) => {
            let _ = write!(s, "{n:4}");
        }
        None => s.push_str("    "),
    }
}

/// Extract the file extension from a path.
pub fn file_extension(path: &str) -> Option<&str> {
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    #[test]
    fn numbering_advances_per_tag() {
        let mut n = DiffNumbering::starting_at(10, 10);
        let rows = [
            n.row(DiffTag::Equal, "a"),
            n.row(DiffTag::Delete, "b"),
            n.row(DiffTag::Delete, "c"),
            n.row(DiffTag::Insert, "d"),
            n.row(DiffTag::Equal, "e"),
        ];
        let nums: Vec<_> = rows.iter().map(|r| (r.old_no, r.new_no)).collect();
        assert_eq!(
            nums,
            [
                (Some(10), Some(10)),
                (Some(11), None),
                (Some(12), None),
                (None, Some(11)),
                (Some(13), Some(12)),
            ]
        );
    }

    #[test]
    fn gutter_right_aligns_and_blanks_missing_numbers() {
        assert_eq!(gutter_text(Some(3), Some(3)), "   3    3");
        assert_eq!(gutter_text(Some(12), None), "  12     ");
        assert_eq!(gutter_text(None, Some(1234)), "     1234");
    }

    #[test]
    fn renders_gutter_and_prefixed_lines() {
        let harness = Harness::new_ui(|ui| {
            let mut n = DiffNumbering::starting_at(1, 1);
            let rows = [
                (DiffTag::Equal, "fn main() {\n"),
                (DiffTag::Delete, "    old();\n"),
                (DiffTag::Insert, "    new();\n"),
            ];
            DiffLines::for_path("src/main.rs").show(rows.map(|(t, s)| n.row(t, s)), ui);
        });

        harness.get_by_label("   1    1");
        harness.get_by_label("   2     ");
        harness.get_by_label("        2");
        harness.get_by_label("-     old();");
        harness.get_by_label("+     new();");
    }

    #[test]
    fn gutter_can_be_hidden() {
        let harness = Harness::new_ui(|ui| {
            let mut n = DiffNumbering::starting_at(1, 1);
            DiffLines::for_path("notes.txt")
                .gutter(false)
                .show([n.row(DiffTag::Insert, "hello")], ui);
        });

        harness.get_by_label("+ hello");
        assert!(harness.query_by_label("        1").is_none());
    }
}
