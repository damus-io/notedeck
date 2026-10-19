//! The figures in an explainer page, drawn in the review pane's Explainer
//! tab instead of shown as their source.
//!
//! A page's Markdown is drawn by `notedeck_ui::markdown`, and the figures
//! among its top-level elements, found once when it's parsed, by a
//! [`BlockHook`](notedeck_ui::markdown::BlockHook) that claims them. They are
//! what `explain` builds:
//!
//! - a ` ```figure ` fence, its caption after a last `---` line. A body that
//!   opens with `<svg` is a hand-drawn SVG, rasterised with resvg in the
//!   pane's light or dark theme (its `var(--…)` colours from the page's
//!   [`Palette`]); any other body is Markdown, drawn in the frame.
//! - a ` ```ba ` fence (or `~~~ba`, when its panes hold code): panes, each
//!   opened by a `### Title {tone}` line, drawn side by side; two untoned
//!   panes read as before (alert) and after (moss). Its caption, after a last
//!   `---`, puts it in a figure's frame.
//! - a ` ```flow ` fence: boxes and arrows laid out by
//!   [`explain_flow`](crate::explain_flow), drawn as an SVG, with a narrower
//!   top-down layout for a column the first won't fit.
//! - a ` ```sequence ` fence: lifelines and messages, drawn by
//!   [`explain_sequence`](crate::explain_sequence) the same way, its narrow
//!   drawing with the columns squeezed to a phone's width.
//! - a ` ```wrong ` fence: how the change goes wrong and what wasn't
//!   verified, its Markdown drawn as explain's `aside.wrong`, a block toned
//!   alert. It has no caption: a `---` in it is a rule, as there.
//! - a ` ```hunk ` fence: one hunk of the commit's own diff, cited by its
//!   path and header (and a `match:` line), found once the diff has loaded
//!   ([`Figured::resolve_hunks`]) and drawn as explain's coloured block
//!   under its path. The page never copies code: before the diff is in, or
//!   when it hasn't got the hunk, it's the citation alone.
//! - a paragraph holding one image and nothing else: a screenshot, its
//!   `"title"` the caption, read from a `data:` URI or an absolute path on
//!   this host; a relative path says it can't show. One at a URL isn't a
//!   figure: the shared renderer draws it, as it does in any note. Only this
//!   pane reads local paths, since only its Markdown is the user's own.
//!
//! Every load runs on a thread of its own and lands in [`Figures`] as a
//! texture; a frame only looks it up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use base64::Engine;
use egui::{ColorImage, TextureHandle, Vec2};
use md_stream::{InlineElement, LoneImage, MdElement, ParsedMarkdown, TableCell, lone_image};
use notedeck_ui::diff::GitPatch;
use notedeck_ui::markdown::CellAt;
use resvg::usvg;

use crate::explain_diagram::{Diagram, Drawing};
use crate::explain_flow::render_flow;
use crate::explain_palette::Palette;
use crate::explain_sequence::render_sequence;
use crate::review_images::{Decoded, MAX_IMAGE_BYTES, decode_image};
use crate::review_walk::{HunkCite, HunkRef, SourceKey, find_hunk};

/// Longest edge a figure is rasterised at, in pixels.
const MAX_RASTER_EDGE: f32 = 2048.0;

/// A page's Markdown, the figures among its top-level elements, and its
/// top-level tables that mark cells.
pub(crate) struct Figured {
    pub md: ParsedMarkdown,
    /// In element order, one per element that is a figure.
    pub figures: Vec<Figure>,
    /// In element order, one per table with a cell marked.
    pub tables: Vec<MarkedTable>,
}

impl Figured {
    /// Parse `text` and find its figures, numbering them from `next` on (and
    /// leaving `next` past the last), so every figure of a page has its own,
    /// and its tables' cell marks, which come out of the cells' text.
    pub(crate) fn parse(text: &str, next: &mut usize) -> Self {
        let mut md = ParsedMarkdown::parse(text);
        let figures = figures(&md, next);
        let tables = mark_tables(&mut md);
        Self {
            md,
            figures,
            tables,
        }
    }

    /// Find every hunk fence's hunk in `patch`, the commit's diff, its own
    /// and those of the Markdown its figures hold.
    pub(crate) fn resolve_hunks(&mut self, patch: &GitPatch) {
        for figure in &mut self.figures {
            match &mut figure.body {
                FigureBody::Hunk(hunk) => hunk.resolve(patch),
                FigureBody::Markdown(inner) | FigureBody::Wrong(inner) => {
                    inner.resolve_hunks(patch)
                }
                FigureBody::Panes(panes) => {
                    for pane in panes {
                        pane.body.resolve_hunks(patch);
                    }
                }
                FigureBody::Svg { .. } | FigureBody::Image { .. } | FigureBody::Diagram(_) => {}
            }
        }
    }

    /// The figure top-level element `element` is, if it's one.
    pub(crate) fn figure_at(&self, element: usize) -> Option<&Figure> {
        let at = self
            .figures
            .binary_search_by_key(&element, |f| f.element)
            .ok()?;
        self.figures.get(at)
    }

    /// The marks of top-level element `element`'s cells, if it's a table
    /// with one.
    pub(crate) fn table_at(&self, element: usize) -> Option<&MarkedTable> {
        let at = self
            .tables
            .binary_search_by_key(&element, |t| t.element)
            .ok()?;
        self.tables.get(at)
    }
}

/// A table's marked cells: explain's `{ok}`, `{bad}`, `{hot}` or `{dim}` at
/// the end of a cell.
pub(crate) struct MarkedTable {
    /// The top-level element it is.
    pub element: usize,
    pub cells: Vec<MarkedCell>,
}

impl MarkedTable {
    /// The mark of the cell at `at`, if it has one.
    pub(crate) fn mark(&self, at: CellAt) -> Option<CellMark> {
        self.cells.iter().find(|c| c.at == at).map(|c| c.mark)
    }
}

/// One marked cell of a [`MarkedTable`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MarkedCell {
    /// Its place: row 0 is the header row.
    pub at: CellAt,
    pub mark: CellMark,
}

/// How a marked cell draws, as explain's `td.ok`, `td.bad`, `td.hot` and
/// `td.dim`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CellMark {
    /// In moss ink: it worked.
    Ok,
    /// In alert ink: it didn't.
    Bad,
    /// On brass-soft: look here.
    Hot,
    /// In the third ink: by the way.
    Dim,
}

impl CellMark {
    /// The mark a cell's `{tone}` gives it, as explain's `CELL`: slate is dim.
    fn of(tone: Tone) -> Self {
        match tone {
            Tone::Moss => Self::Ok,
            Tone::Alert => Self::Bad,
            Tone::Brass => Self::Hot,
            Tone::Slate | Tone::Dim => Self::Dim,
        }
    }
}

/// Take the marks off the cells of `md`'s top-level tables, shortening each
/// marked cell's spans so its text no longer holds the mark, and say which
/// cells had one. A cell is marked as explain's `render_cell` marks it: its
/// last inline is text ending in a `{tone}` naming a known tone, so a mark
/// in code (`` `x {bad}` ``) or an unknown word (`{nope}`) stays text.
fn mark_tables(md: &mut ParsedMarkdown) -> Vec<MarkedTable> {
    let source = md.source.as_str();
    let mut tables = Vec::new();
    for (element, block) in md.elements.iter_mut().enumerate() {
        let MdElement::Table { headers, rows } = block else {
            continue;
        };
        let rows = std::iter::once(headers).chain(rows.iter_mut());
        let cells: Vec<MarkedCell> = rows
            .enumerate()
            .flat_map(|(row, cells)| {
                cells.iter_mut().enumerate().filter_map(move |(col, cell)| {
                    let mark = take_cell_mark(cell, source)?;
                    Some(MarkedCell {
                        at: CellAt { row, col },
                        mark,
                    })
                })
            })
            .collect();
        if !cells.is_empty() {
            tables.push(MarkedTable { element, cells });
        }
    }
    tables
}

/// `cell`'s mark, taken off the end of its text (and its last inline, which
/// goes when nothing else is left of it), or `None`, leaving it as it was.
fn take_cell_mark(cell: &mut TableCell, source: &str) -> Option<CellMark> {
    let Some(InlineElement::Text(last)) = cell.content.last_mut() else {
        return None;
    };
    let toned = take_tone(last.resolve(source));
    let tone = toned.tone?;
    last.end = last.start + toned.text.len();
    if last.is_empty() {
        cell.content.pop();
    }
    cell.span.end = cell.span.start + take_tone(cell.span.resolve(source)).text.len();
    Some(CellMark::of(tone))
}

/// A figure: what it draws, and the caption under it.
pub(crate) struct Figure {
    /// The top-level element of its body it is.
    pub element: usize,
    /// Its number in the page: its load's key.
    pub index: usize,
    pub body: FigureBody,
    pub caption: Option<String>,
}

/// What a figure draws.
pub(crate) enum FigureBody {
    /// A hand-drawn `<svg>`, as written (its vars not yet substituted).
    Svg {
        source: String,
        /// Its `aria-label`, if it has one: its accessible label.
        label: Option<String>,
    },
    /// Markdown in the figure's frame.
    Markdown(Box<Figured>),
    /// An image, from a `data:` URI or a path.
    Image { alt: String, src: String },
    /// A ` ```ba ` fence's panes, in order.
    Panes(Vec<Pane>),
    /// A ` ```flow ` or ` ```sequence ` fence, laid out.
    Diagram(Box<DiagramFigure>),
    /// A ` ```wrong ` fence's Markdown, whole.
    Wrong(Box<Figured>),
    /// A ` ```hunk ` fence: the hunk of the commit's diff it cites.
    Hunk(Box<HunkFigure>),
}

/// A ` ```hunk ` fence's cite, and what the commit's diff had for it.
pub(crate) struct HunkFigure {
    pub cite: HunkCite,
    /// The path over the block, as explain's `srcname` shows it: upper case.
    pub srcname: String,
    pub found: HunkFound,
}

impl HunkFigure {
    fn new(cite: HunkCite) -> Self {
        Self {
            srcname: cite.path.to_uppercase(),
            cite,
            found: HunkFound::Unsearched,
        }
    }

    /// Look for the cited hunk in `patch`.
    fn resolve(&mut self, patch: &GitPatch) {
        self.found = match find_hunk(patch, &self.cite) {
            Some(at) => HunkFound::Found(at),
            None => HunkFound::Missing,
        };
    }
}

/// Where a hunk fence's hunk is in the commit's diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HunkFound {
    /// No diff searched yet (it's still loading, or there's none): the block
    /// is its citation, the path and the header.
    Unsearched,
    Found(HunkRef),
    /// The diff hasn't got it.
    Missing,
}

/// A diagram figure: its drawings, the wide one loaded as the figure's own
/// index and the narrow one as `narrow_index`.
pub(crate) struct DiagramFigure {
    pub diagram: Diagram,
    pub narrow_index: usize,
}

/// The drawing a diagram shows at some width, and its load's index.
pub(crate) struct FittedDrawing<'a> {
    pub index: usize,
    pub drawing: &'a Drawing,
}

impl DiagramFigure {
    /// The drawing to show in `width` points ([`Diagram::fitting`]), for the
    /// figure numbered `index`.
    pub(crate) fn fitting(&self, index: usize, width: f32) -> FittedDrawing<'_> {
        let drawing = self.diagram.fitting(width);
        let narrow = self
            .diagram
            .narrow
            .as_ref()
            .is_some_and(|n| std::ptr::eq(n, drawing));
        FittedDrawing {
            index: if narrow { self.narrow_index } else { index },
            drawing,
        }
    }
}

/// One pane of a ` ```ba ` fence.
pub(crate) struct Pane {
    /// Its `### ` line, without the tone.
    pub title: String,
    /// Its `{tone}`, or the before/after one a two-pane block gives it.
    pub tone: Option<Tone>,
    /// The Markdown under its title, up to the next pane's.
    pub body: Figured,
}

/// explain's one vocabulary of tones, for panes (and later cells, nodes and
/// edges): its `.tone-*` classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Brass,
    Slate,
    Moss,
    Alert,
    Dim,
}

impl Tone {
    /// The tone a `{word}` names, aliases included, as explain's `TONES`.
    pub(crate) fn named(word: &str) -> Option<Self> {
        Some(match word {
            "brass" | "hot" => Self::Brass,
            "slate" => Self::Slate,
            "moss" | "good" | "ok" => Self::Moss,
            "alert" | "bad" => Self::Alert,
            "dim" => Self::Dim,
            _ => return None,
        })
    }

    /// The palette tokens it draws with, as explain's `--t` (its edge) and
    /// `--t-ink` (its text): a role's own pair, or for dim the rule and the
    /// second ink.
    pub(crate) fn tokens(self) -> ToneTokens {
        let (edge, ink) = match self {
            Self::Brass => ("brass", "brass-ink"),
            Self::Slate => ("slate", "slate-ink"),
            Self::Moss => ("moss", "moss-ink"),
            Self::Alert => ("alert", "alert-ink"),
            Self::Dim => ("rule-strong", "ink-2"),
        };
        ToneTokens { edge, ink }
    }
}

/// The palette token names a [`Tone`] draws with.
pub(crate) struct ToneTokens {
    pub edge: &'static str,
    pub ink: &'static str,
}

/// A line split at a trailing `{tone}`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Toned<'a> {
    pub text: &'a str,
    pub tone: Option<Tone>,
}

/// `line` without a trailing `{tone}` naming a known tone, and that tone, as
/// explain's `take_tone`; an unknown word stays in the text.
pub(crate) fn take_tone(line: &str) -> Toned<'_> {
    let untoned = Toned {
        text: line,
        tone: None,
    };
    let trimmed = line.trim_end();
    let Some(open) = trimmed.strip_suffix('}').and_then(|t| t.rfind('{')) else {
        return untoned;
    };
    match Tone::named(&trimmed[open + 1..trimmed.len() - 1]) {
        Some(tone) => Toned {
            text: trimmed[..open].trim_end(),
            tone: Some(tone),
        },
        None => untoned,
    }
}

/// A ` ```ba ` body's panes, each opened by a `### Title {tone}` line, its
/// figures numbered from `next`. Text before the first pane is dropped, as
/// explain does (with a warning there). Two untoned panes read as before
/// (alert) and after (moss).
fn panes(body: &str, next: &mut usize) -> Vec<Pane> {
    struct Open<'a> {
        toned: Toned<'a>,
        start: usize,
    }
    let mut panes = Vec::new();
    let mut open: Option<Open> = None;
    let mut close = |open: Option<Open>, end: usize, next: &mut usize| {
        if let Some(Open { toned, start }) = open {
            panes.push(Pane {
                title: toned.text.trim().to_owned(),
                tone: toned.tone,
                body: Figured::parse(&body[start..end], next),
            });
        }
    };
    let mut at = 0;
    for line in body.split_inclusive('\n') {
        let heading = line
            .trim_end_matches(['\n', '\r'])
            .strip_prefix("###")
            .filter(|rest| rest.starts_with([' ', '\t']));
        if let Some(rest) = heading {
            close(open.take(), at, next);
            open = Some(Open {
                toned: take_tone(rest),
                start: at + line.len(),
            });
        }
        at += line.len();
    }
    close(open, body.len(), next);
    if let [before, after] = panes.as_mut_slice() {
        before.tone = before.tone.or(Some(Tone::Alert));
        after.tone = after.tone.or(Some(Tone::Moss));
    }
    panes
}

/// The figures among `md`'s top-level elements, numbered from `next`.
fn figures(md: &ParsedMarkdown, next: &mut usize) -> Vec<Figure> {
    md.elements
        .iter()
        .enumerate()
        .filter_map(|(i, element)| figure_of(i, element, &md.source, next))
        .collect()
}

/// The figure `element` (top-level element `at`) is, if it is one: a
/// ` ```figure `, ` ```ba `, ` ```flow `, ` ```sequence `, ` ```wrong ` or
/// ` ```hunk ` fence, or a paragraph holding one image and nothing else whose source
/// isn't a URL.
fn figure_of(at: usize, element: &MdElement, source: &str, next: &mut usize) -> Option<Figure> {
    match element {
        MdElement::CodeBlock(code) => {
            let lang = code.language.as_ref()?.resolve(source);
            let kind = lang.split_whitespace().next();
            if !matches!(
                kind,
                Some("figure" | "ba" | "flow" | "sequence" | "wrong" | "hunk")
            ) {
                return None;
            }
            let content = code.content.resolve(source);
            if kind == Some("hunk") {
                // Its `match:` line is in the body, above the caption.
                let Caption { body, caption } = split_caption(content);
                return Some(Figure {
                    element: at,
                    index: take(next),
                    body: FigureBody::Hunk(Box::new(HunkFigure::new(HunkCite::parse(lang, body)))),
                    caption: caption.map(str::to_owned),
                });
            }
            if kind == Some("wrong") {
                // explain's wrong_block takes no caption: its body is
                // Markdown to the fence's end.
                return Some(Figure {
                    element: at,
                    index: take(next),
                    body: FigureBody::Wrong(Box::new(Figured::parse(content, next))),
                    caption: None,
                });
            }
            let Caption { body, caption } = split_caption(content);
            let diagram = match kind {
                Some("flow") => Some(render_flow(body, lang)),
                // A sequence with no lifelines has nothing to draw: its
                // source shows as a code block.
                Some("sequence") => Some(render_sequence(body, lang)?),
                _ => None,
            };
            let index = take(next);
            if let Some(diagram) = diagram {
                // The narrow drawing loads as a figure of its own.
                let narrow_index = if diagram.narrow.is_some() {
                    take(next)
                } else {
                    index
                };
                return Some(Figure {
                    element: at,
                    index,
                    body: FigureBody::Diagram(Box::new(DiagramFigure {
                        diagram,
                        narrow_index,
                    })),
                    caption: caption.map(str::to_owned),
                });
            }
            if kind == Some("ba") {
                return Some(Figure {
                    element: at,
                    index,
                    body: FigureBody::Panes(panes(body, next)),
                    caption: caption.map(str::to_owned),
                });
            }
            let trimmed = body.trim();
            let body = if trimmed
                .get(..4)
                .is_some_and(|t| t.eq_ignore_ascii_case("<svg"))
            {
                FigureBody::Svg {
                    label: svg_label(trimmed),
                    source: trimmed.to_owned(),
                }
            } else {
                FigureBody::Markdown(Box::new(Figured::parse(body, next)))
            };
            Some(Figure {
                element: at,
                index,
                body,
                caption: caption.map(str::to_owned),
            })
        }
        MdElement::Paragraph(inlines) => {
            let LoneImage { alt, target } = lone_image(inlines, source)?;
            if matches!(image_source(target.src), ImageSource::Url(_)) {
                return None;
            }
            Some(Figure {
                element: at,
                index: take(next),
                body: FigureBody::Image {
                    alt: alt.to_owned(),
                    src: target.src.to_owned(),
                },
                caption: target.title.map(str::to_owned),
            })
        }
        _ => None,
    }
}

/// `*next`, moving it on.
fn take(next: &mut usize) -> usize {
    let n = *next;
    *next += 1;
    n
}

/// A block body split at its caption.
#[derive(Debug, PartialEq, Eq)]
struct Caption<'a> {
    body: &'a str,
    caption: Option<&'a str>,
}

/// `body` split at its last line holding only `---`: the caption after it,
/// trimmed (`None` when empty), as explain's `split_caption`.
fn split_caption(body: &str) -> Caption<'_> {
    let mut at = 0;
    let mut split = None;
    for line in body.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            split = Some(Caption {
                body: &body[..at],
                caption: Some(body[at + line.len()..].trim()).filter(|c| !c.is_empty()),
            });
        }
        at += line.len();
    }
    split.unwrap_or(Caption {
        body,
        caption: None,
    })
}

/// The `aria-label` on an SVG's opening tag.
fn svg_label(svg: &str) -> Option<String> {
    let tag = &svg[..svg.find('>')?];
    let at = tag.find("aria-label=")? + "aria-label=".len();
    let q = tag[at..]
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\''))?;
    let value = &tag[at + 1..];
    Some(value[..value.find(q)?].to_owned())
}

/// `svg` with every `var(--name)` and `var(--name, fallback)` replaced for
/// the dark or light theme: a palette token's hex, `--t…` as brass, the font
/// vars as notedeck's bundled faces; an unknown var takes its fallback, else
/// ink. `currentColor` is ink too.
pub(crate) fn substitute_vars(svg: &str, palette: &Palette, dark: bool) -> String {
    let ink = palette.get("ink", dark).unwrap_or("#000000");
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(at) = rest.find("var(") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 4..];
        let Some(close) = matching_paren(after) else {
            out.push_str(&rest[at..]);
            rest = "";
            break;
        };
        let inner = &after[..close];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        let name = name.strip_prefix("--").unwrap_or(name);
        match var_value(name, palette, dark) {
            Some(value) => out.push_str(value),
            None => match fallback {
                Some(f) => out.push_str(&substitute_vars(f, palette, dark)),
                None => out.push_str(ink),
            },
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out.replace("currentColor", ink)
}

/// The byte offset of the `)` closing an already-opened `(` in `s`.
fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(i),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// What explain's `--name` is in the pane.
fn var_value<'a>(name: &str, palette: &'a Palette, dark: bool) -> Option<&'a str> {
    match name {
        "mono" => Some("Inconsolata, monospace"),
        "body" | "disp" => Some("DejaVu Sans, sans-serif"),
        "t" => palette.get("brass", dark),
        "t-soft" => palette.get("brass-soft", dark),
        "t-ink" => palette.get("brass-ink", dark),
        _ => palette.get(name, dark),
    }
}

/// A font database holding only notedeck's bundled Inconsolata (monospace)
/// and DejaVu Sans (sans-serif and serif): figures draw the same text on
/// every host.
pub(crate) fn figure_fonts() -> Arc<usvg::fontdb::Database> {
    let mut db = usvg::fontdb::Database::new();
    for face in [notedeck::fonts::INCONSOLATA, notedeck::fonts::DEJAVU_SANS] {
        db.load_font_source(usvg::fontdb::Source::Binary(Arc::new(face)));
    }
    db.set_monospace_family("Inconsolata");
    db.set_sans_serif_family("DejaVu Sans");
    db.set_serif_family("DejaVu Sans");
    Arc::new(db)
}

/// A figure rasterised or decoded, ready to upload.
pub(crate) struct Raster {
    pub image: ColorImage,
    /// Its intrinsic size in points: the SVG's, or the image file's pixels.
    pub size: Vec2,
}

/// `svg` (its vars already substituted) drawn at its intrinsic size times
/// `pixels_per_point`, its longest edge capped at [`MAX_RASTER_EDGE`].
#[profiling::function]
pub(crate) fn rasterize_svg(
    svg: &str,
    fonts: &Arc<usvg::fontdb::Database>,
    pixels_per_point: f32,
) -> Result<Raster, Failure> {
    let options = usvg::Options {
        font_family: "DejaVu Sans".to_owned(),
        fontdb: Arc::clone(fonts),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &options).map_err(|e| Failure::Svg(e.to_string()))?;
    let size = tree.size();
    let longest = size.width().max(size.height()) * pixels_per_point;
    let scale = pixels_per_point * (MAX_RASTER_EDGE / longest.max(1.0)).min(1.0);
    let w = (size.width() * scale).ceil() as u32;
    let h = (size.height() * scale).ceil() as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| Failure::Svg(format!("empty drawing ({w}×{h})")))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(Raster {
        image: ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data()),
        size: Vec2::new(size.width(), size.height()),
    })
}

/// Why a figure can't show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// A relative path, or an absolute one this host hasn't got.
    NotOnHost,
    /// Bigger than [`MAX_IMAGE_BYTES`]; its size in bytes.
    TooLarge(u64),
    /// Read, but not an image notedeck decodes, or not read at all.
    Unreadable,
    /// An SVG resvg can't parse, and why.
    Svg(String),
}

impl Failure {
    /// The placeholder's words for it.
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::NotOnHost => "not on this host".to_owned(),
            Self::TooLarge(bytes) => {
                format!("too large ({:.1} MB)", *bytes as f64 / (1024.0 * 1024.0))
            }
            Self::Unreadable => "unreadable".to_owned(),
            Self::Svg(e) => format!("SVG didn't parse: {e}"),
        }
    }
}

/// Where an image's `src` points, as the pane reads it.
#[derive(Debug, PartialEq, Eq)]
enum ImageSource<'a> {
    Data(&'a str),
    Path(&'a Path),
    Url(&'a str),
    /// A relative path: not anywhere this host can look.
    Elsewhere,
}

/// What `src` is.
fn image_source(src: &str) -> ImageSource<'_> {
    if src.starts_with("data:") {
        return ImageSource::Data(src);
    }
    if src.starts_with("https://") || src.starts_with("http://") {
        return ImageSource::Url(src);
    }
    let path = Path::new(src.strip_prefix("file://").unwrap_or(src));
    if path.is_absolute() {
        ImageSource::Path(path)
    } else {
        ImageSource::Elsewhere
    }
}

/// The theme an SVG is drawn in.
#[derive(Clone)]
struct SvgTheme {
    palette: Palette,
    dark: bool,
    pixels_per_point: f32,
    fonts: Arc<usvg::fontdb::Database>,
}

impl SvgTheme {
    fn rasterize(&self, svg: &str) -> Result<Raster, Failure> {
        let themed = substitute_vars(svg, &self.palette, self.dark);
        rasterize_svg(&themed, &self.fonts, self.pixels_per_point)
    }
}

/// A load a worker runs.
enum Job {
    Svg(String),
    Data(String),
    File(PathBuf),
}

impl Job {
    /// Run it: read, decode or rasterise. Worker thread only.
    fn run(self, theme: &SvgTheme) -> Result<Raster, Failure> {
        match self {
            Job::Svg(svg) => theme.rasterize(&svg),
            Job::Data(uri) => load_data_uri(&uri, theme),
            Job::File(path) => load_file(&path, MAX_IMAGE_BYTES, theme),
        }
    }
}

/// A `data:` URI's image: base64 or percent-encoded, an SVG rasterised.
fn load_data_uri(uri: &str, theme: &SvgTheme) -> Result<Raster, Failure> {
    let rest = uri.strip_prefix("data:").ok_or(Failure::Unreadable)?;
    let (meta, payload) = rest.split_once(',').ok_or(Failure::Unreadable)?;
    let mut parts = meta.split(';');
    let mime = parts.next().unwrap_or("").trim();
    let bytes = if parts.any(|p| p.trim() == "base64") {
        let clean: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
        base64::engine::general_purpose::STANDARD
            .decode(clean)
            .map_err(|_| Failure::Unreadable)?
    } else {
        percent_decode(payload)
    };
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(Failure::TooLarge(bytes.len() as u64));
    }
    if mime.eq_ignore_ascii_case("image/svg+xml") {
        let svg = std::str::from_utf8(&bytes).map_err(|_| Failure::Unreadable)?;
        return theme.rasterize(svg);
    }
    decoded(&bytes)
}

/// `s` with its `%XX` escapes decoded.
fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// The image at `path`, read if it's at most `cap` bytes; an `.svg`
/// rasterised.
fn load_file(path: &Path, cap: u64, theme: &SvgTheme) -> Result<Raster, Failure> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Failure::NotOnHost,
        _ => Failure::Unreadable,
    })?;
    if meta.len() > cap {
        return Err(Failure::TooLarge(meta.len()));
    }
    let bytes = std::fs::read(path).map_err(|_| Failure::Unreadable)?;
    if crate::review_images::has_extension(&path.to_string_lossy(), &["svg"]) {
        let svg = std::str::from_utf8(&bytes).map_err(|_| Failure::Unreadable)?;
        return theme.rasterize(svg);
    }
    decoded(&bytes)
}

/// `bytes` decoded as a raster image, one pixel to a point.
fn decoded(bytes: &[u8]) -> Result<Raster, Failure> {
    let Decoded {
        image,
        width,
        height,
    } = decode_image(bytes).ok_or(Failure::Unreadable)?;
    Ok(Raster {
        image,
        size: Vec2::new(width as f32, height as f32),
    })
}

/// A figure's load, keyed by its index and theme.
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct FigureKey {
    index: usize,
    dark: bool,
}

/// Where a figure's load is.
enum FigureLoad {
    Pending(Receiver<Result<Raster, Failure>>),
    Ready { texture: TextureHandle, size: Vec2 },
    Failed(Failure),
}

/// One page's figure loads and textures, kept across frames and dropped with
/// the page (so a page key needn't be in each key).
pub(crate) struct Figures {
    /// What the page was parsed from: names its textures.
    page: SourceKey,
    /// The font database SVGs draw with, built on the first one.
    fonts: Option<Arc<usvg::fontdb::Database>>,
    loads: HashMap<FigureKey, FigureLoad>,
}

impl Figures {
    pub(crate) fn new(page: SourceKey) -> Self {
        Self {
            page,
            fonts: None,
            loads: HashMap::new(),
        }
    }

    /// Figure `index`'s load in the `dark` or light theme, started with
    /// `job` on a thread of its own the first time it's asked for, and
    /// uploaded once it lands.
    fn load(
        &mut self,
        ctx: &egui::Context,
        key: FigureKey,
        palette: &Palette,
        job: impl FnOnce() -> Job,
    ) -> &FigureLoad {
        let page = self.page;
        if !self.loads.contains_key(&key) {
            let fonts = Arc::clone(self.fonts.get_or_insert_with(figure_fonts));
            let theme = SvgTheme {
                palette: palette.clone(),
                dark: key.dark,
                pixels_per_point: ctx.pixels_per_point(),
                fonts,
            };
            let (tx, rx) = channel();
            let job = job();
            let repaint = ctx.clone();
            let spawned = std::thread::Builder::new()
                .name("explainer-figure".into())
                .spawn(move || {
                    let _ = tx.send(job.run(&theme));
                    repaint.request_repaint();
                });
            let load = match spawned {
                Ok(_) => FigureLoad::Pending(rx),
                Err(e) => {
                    tracing::warn!("spawning an explainer figure's load: {e}");
                    FigureLoad::Failed(Failure::Unreadable)
                }
            };
            self.loads.insert(key, load);
        }
        let load = self.loads.get_mut(&key).expect("inserted above");
        if let FigureLoad::Pending(rx) = load {
            match rx.try_recv() {
                Ok(Ok(Raster { image, size })) => {
                    let name = format!("explainer-figure:{page}:{}:{}", key.index, key.dark);
                    let texture =
                        notedeck::media::load_texture_checked(ctx, name, image, Default::default());
                    *load = FigureLoad::Ready { texture, size };
                }
                Ok(Err(failure)) => *load = FigureLoad::Failed(failure),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => *load = FigureLoad::Failed(Failure::Unreadable),
            }
        }
        load
    }
}

/// What a figure's body resolves to this frame.
pub(crate) enum FigureView<'a> {
    /// Still loading.
    Pending,
    Ready {
        texture: &'a TextureHandle,
        size: Vec2,
    },
    Failed(Failure),
}

impl Figures {
    /// Figure `index`, an SVG, in the `dark` or light theme.
    pub(crate) fn svg(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        index: usize,
        dark: bool,
        source: &str,
    ) -> FigureView<'_> {
        self.svg_with(ctx, palette, index, dark, || source.to_owned())
    }

    /// Figure `index`, the SVG `source` makes, called only the first time
    /// the figure is asked for in a theme.
    pub(crate) fn svg_with(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        index: usize,
        dark: bool,
        source: impl FnOnce() -> String,
    ) -> FigureView<'_> {
        let key = FigureKey { index, dark };
        view(self.load(ctx, key, palette, || Job::Svg(source())))
    }

    /// Figure `index`, an image at `src`: a `data:` URI or a path.
    pub(crate) fn image(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        index: usize,
        dark: bool,
        src: &str,
    ) -> FigureView<'_> {
        let job = match image_source(src) {
            // A URL is never a figure ([`figure_of`]): it isn't one to read
            // off this host.
            ImageSource::Url(_) | ImageSource::Elsewhere => {
                return FigureView::Failed(Failure::NotOnHost);
            }
            ImageSource::Data(uri) => Job::Data(uri.to_owned()),
            ImageSource::Path(path) => Job::File(path.to_owned()),
        };
        // A raster image looks the same in both themes, but a data: or
        // path SVG doesn't, so every image is keyed by theme.
        let key = FigureKey { index, dark };
        view(self.load(ctx, key, palette, || job))
    }
}

fn view(load: &FigureLoad) -> FigureView<'_> {
    match load {
        FigureLoad::Pending(_) => FigureView::Pending,
        FigureLoad::Ready { texture, size } => FigureView::Ready {
            texture,
            size: *size,
        },
        FigureLoad::Failed(f) => FigureView::Failed(f.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(dark: bool) -> SvgTheme {
        SvgTheme {
            palette: Palette::default(),
            dark,
            pixels_per_point: 1.0,
            fonts: figure_fonts(),
        }
    }

    #[test]
    fn split_caption_takes_the_last_rule() {
        assert_eq!(
            split_caption("a\n---\nb\n---\n  cap \n"),
            Caption {
                body: "a\n---\nb\n",
                caption: Some("cap")
            }
        );
        assert_eq!(
            split_caption("a\n---\n\n"),
            Caption {
                body: "a\n",
                caption: None
            }
        );
        assert_eq!(
            split_caption("a --- b\n"),
            Caption {
                body: "a --- b\n",
                caption: None
            }
        );
    }

    /// The figures among a body's elements: an SVG fence (its caption and
    /// label), a Markdown fence (its own image numbered after it), a lone
    /// image (its title the caption), a flow. An image mid-sentence and a
    /// lone image at a URL aren't: the shared renderer draws them.
    #[test]
    fn figures_are_found_by_element() {
        let text = "\
Intro.

```figure
<svg viewBox=\"0 0 10 10\" aria-label=\"a box\">
  <g/>

  <g/>
</svg>
---
The caption.
```

```flow
a -> b
```

See ![inline](x.png) here.

```figure id=f
Some **markdown**.

![inner](/i.png)
```

![A shot](/abs/shot.jpg \"Its caption.\")

![Hosted](https://example.com/shot.png \"On the web.\")

Outro.
";
        let mut next = 0;
        let doc = Figured::parse(text, &mut next);
        assert_eq!(next, 5);
        let shape: Vec<String> = doc
            .figures
            .iter()
            .map(|f| match &f.body {
                FigureBody::Svg { source, label } => format!(
                    "@{} svg #{} {:?} {:?} blank={}",
                    f.element,
                    f.index,
                    label,
                    f.caption,
                    source.contains("\n\n")
                ),
                FigureBody::Markdown(inner) => format!(
                    "@{} md-fig #{} {:?} inner={:?}",
                    f.element,
                    f.index,
                    f.caption,
                    inner.figures.iter().map(|f| f.element).collect::<Vec<_>>()
                ),
                FigureBody::Image { alt, src } => {
                    format!(
                        "@{} img #{} {alt} {src} {:?}",
                        f.element, f.index, f.caption
                    )
                }
                FigureBody::Panes(panes) => format!("@{} ba {}", f.element, panes.len()),
                FigureBody::Wrong(_) => format!("@{} wrong", f.element),
                FigureBody::Hunk(h) => format!("@{} hunk {}", f.element, h.cite.path),
                FigureBody::Diagram(d) => format!(
                    "@{} diagram #{} narrow={}",
                    f.element,
                    f.index,
                    d.diagram.narrow.is_some()
                ),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "@1 svg #0 Some(\"a box\") Some(\"The caption.\") blank=true",
                "@2 diagram #1 narrow=false",
                "@4 md-fig #2 None inner=[1]",
                "@5 img #4 A shot /abs/shot.jpg Some(\"Its caption.\")",
            ]
        );
        assert_eq!(doc.md.elements.len(), 8);
        assert!(doc.figure_at(5).is_some());
        assert!(doc.figure_at(6).is_none(), "the https image");
    }

    /// A ba fence's panes, as explain splits them: at `### ` lines, the
    /// text before the first dropped, a known `{tone}` taken off the title
    /// and an unknown one left on it.
    #[test]
    fn ba_panes_split_on_their_titles() {
        let text = "\
~~~ba
dropped
### Old way {bad}
It **stalls**.

```rust
let x = 1;
```
### Middle {nope}
###notatitle
### New way {hot}
It runs.
---
The caption.
~~~
";
        let mut next = 0;
        let doc = Figured::parse(text, &mut next);
        let [figure] = doc.figures.as_slice() else {
            panic!("one figure");
        };
        assert_eq!(figure.caption.as_deref(), Some("The caption."));
        let FigureBody::Panes(panes) = &figure.body else {
            panic!("panes");
        };
        let shape: Vec<String> = panes
            .iter()
            .map(|p| format!("{} {:?} {}", p.title, p.tone, p.body.md.elements.len()))
            .collect();
        assert_eq!(
            shape,
            [
                "Old way Some(Alert) 2",
                "Middle {nope} None 1",
                "New way Some(Brass) 1",
            ]
        );
        assert!(matches!(
            panes[0].body.md.elements[1],
            MdElement::CodeBlock(_)
        ));
    }

    /// Two untoned panes read as before (alert) then after (moss); one or
    /// three untoned stay untoned, and a pane's own tone wins.
    #[test]
    fn two_untoned_panes_are_before_and_after() {
        let tones = |text: &str| -> Vec<Option<Tone>> {
            let mut next = 0;
            let doc = Figured::parse(text, &mut next);
            let Some(Figure {
                body: FigureBody::Panes(panes),
                ..
            }) = doc.figures.first()
            else {
                panic!("panes in {text:?}");
            };
            panes.iter().map(|p| p.tone).collect()
        };
        assert_eq!(
            tones("```ba\n### Before\na\n### After\nb\n```\n"),
            [Some(Tone::Alert), Some(Tone::Moss)]
        );
        assert_eq!(
            tones("```ba\n### Before {slate}\na\n### After\nb\n```\n"),
            [Some(Tone::Slate), Some(Tone::Moss)]
        );
        assert_eq!(
            tones("```ba\n### A\n### B\n### C\n```\n"),
            [None, None, None]
        );
        assert_eq!(tones("```ba\n### Only\nx\n```\n"), [None]);
    }

    /// A wrong fence is its Markdown whole: no caption split off at `---`
    /// (a rule there, as in explain), and a figure in it still claimed.
    #[test]
    fn wrong_is_markdown_whole() {
        let text = "\
```wrong
### How it goes wrong
- **Loose.** It matches too much.

---

![A shot](/abs/shot.jpg)
```
";
        let mut next = 0;
        let doc = Figured::parse(text, &mut next);
        let [figure] = doc.figures.as_slice() else {
            panic!("one figure");
        };
        assert_eq!(figure.caption, None);
        let FigureBody::Wrong(body) = &figure.body else {
            panic!("a wrong block");
        };
        assert!(matches!(
            body.md.elements.first(),
            Some(MdElement::Heading { level: 3, .. })
        ));
        assert!(body.md.source.contains("---"));
        assert_eq!(body.figures.len(), 1, "the shot under the rule");
        assert_eq!(next, 2);
    }

    /// A hunk fence is a figure citing its path and header, its `match:`
    /// line read above the caption. Nothing is found until a diff is given:
    /// then the cited hunk (by its match once the header moved, inside a ba
    /// pane too), or none for a hunk the diff hasn't got.
    #[test]
    fn hunk_fences_cite_then_resolve_against_the_diff() {
        let patch = GitPatch::parse(
            "\
diff --git a/src/guard.ts b/src/guard.ts
--- a/src/guard.ts
+++ b/src/guard.ts
@@ -10,2 +10,3 @@
 one
+if (isLocal())
 two
@@ -40,2 +41,3 @@
 six
+seven
 eight
",
        );
        let text = "\
```hunk src/guard.ts @@ -8,2 +8,3 @@
match: isLocal()
---
The guard.
```

```hunk gone.rs @@ -1 +1 @@
```

~~~ba
### Before
x
### After
```hunk src/guard.ts @@ -40,2 +41,3 @@
```
~~~
";
        let mut next = 0;
        let mut doc = Figured::parse(text, &mut next);
        let found = |doc: &Figured| -> Vec<HunkFound> {
            let mut out = Vec::new();
            for figure in &doc.figures {
                match &figure.body {
                    FigureBody::Hunk(h) => out.push(h.found),
                    FigureBody::Panes(panes) => {
                        for pane in panes {
                            out.extend(pane.body.figures.iter().filter_map(|f| match &f.body {
                                FigureBody::Hunk(h) => Some(h.found),
                                _ => None,
                            }));
                        }
                    }
                    _ => {}
                }
            }
            out
        };
        let FigureBody::Hunk(first) = &doc.figures[0].body else {
            panic!("a hunk");
        };
        assert_eq!(first.cite.path, "src/guard.ts");
        assert_eq!(first.cite.header, "@@ -8,2 +8,3 @@");
        assert_eq!(first.srcname, "SRC/GUARD.TS");
        assert_eq!(doc.figures[0].caption.as_deref(), Some("The guard."));
        assert_eq!(found(&doc), [HunkFound::Unsearched; 3]);

        doc.resolve_hunks(&patch);
        let hunk = |hunk| HunkFound::Found(HunkRef { file: 0, hunk });
        assert_eq!(found(&doc), [hunk(0), HunkFound::Missing, hunk(1)]);
    }

    /// A cell's trailing `{tone}` comes off its text and marks it, as
    /// explain's own fixture (test/fixtures/build/blocks.md) has it: in
    /// code, or naming no tone, it stays text, and an unmarked table has no
    /// marks to keep.
    #[test]
    fn table_cells_lose_their_marks() {
        let figured = Figured::parse(
            "\
| one {hot} | two |
|---|---|
| a | up {ok} | `x {bad}` |
| b | down {bad} | plain {nope} |
| c {hot} | {dim} | **z** {slate} |

| plain | table |
|---|---|
| x | y |
",
            &mut 0,
        );
        let [marked] = figured.tables.as_slice() else {
            panic!("one marked table, got {}", figured.tables.len());
        };
        assert_eq!(marked.element, 0);
        let at = |row, col| CellAt { row, col };
        let marks = [
            (at(0, 0), CellMark::Hot),
            (at(1, 1), CellMark::Ok),
            (at(2, 1), CellMark::Bad),
            (at(3, 0), CellMark::Hot),
            (at(3, 1), CellMark::Dim),
            (at(3, 2), CellMark::Dim),
        ];
        assert_eq!(
            marked.cells,
            marks.map(|(at, mark)| MarkedCell { at, mark }),
        );
        assert_eq!(marked.mark(at(2, 2)), None);

        let MdElement::Table { headers, rows } = &figured.md.elements[0] else {
            panic!("a table");
        };
        let source = figured.md.source.as_str();
        let shown: Vec<Vec<&str>> = std::iter::once(headers)
            .chain(rows)
            .map(|row| row.iter().map(|c| c.span.resolve(source)).collect())
            .collect();
        let expected: [&[&str]; 4] = [
            &["one", "two"],
            &["a", "up", "`x {bad}`"],
            &["b", "down", "plain {nope}"],
            &["c", "", "**z**"],
        ];
        assert_eq!(shown, expected);
        // The bold cell's mark was a text run of its own: it's gone, the
        // bold left.
        assert!(matches!(
            rows[2][2].content.as_slice(),
            [InlineElement::Styled { .. }]
        ));
    }

    #[test]
    fn take_tone_reads_a_trailing_tone() {
        let cases = [
            ("Title {ok}", "Title", Some(Tone::Moss)),
            ("Title {dim}  ", "Title", Some(Tone::Dim)),
            ("Title {what}", "Title {what}", None),
            ("{x} Title", "{x} Title", None),
            ("Title", "Title", None),
        ];
        for (line, text, tone) in cases {
            assert_eq!(take_tone(line), Toned { text, tone }, "{line:?}");
        }
    }

    #[test]
    fn substitute_vars_in_light_and_dark() {
        let p = Palette::default();
        let svg = "<g fill=\"var(--moss-ink)\" stroke=\"var(--t)\" font-family=\"var(--mono)\">\
                   <text fill=\"var(--nope, var(--alert))\" stroke=\"var(--gone)\" color=\"currentColor\"/>";
        assert_eq!(
            substitute_vars(svg, &p, false),
            "<g fill=\"#455F40\" stroke=\"#A76603\" font-family=\"Inconsolata, monospace\">\
             <text fill=\"#C24C37\" stroke=\"#1B1E29\" color=\"#1B1E29\"/>"
        );
        assert_eq!(
            substitute_vars(svg, &p, true),
            "<g fill=\"#BBD6B6\" stroke=\"#E19C4E\" font-family=\"Inconsolata, monospace\">\
             <text fill=\"#FE836C\" stroke=\"#ECEFF7\" color=\"#ECEFF7\"/>"
        );
        assert_eq!(substitute_vars("var(--ink", &p, false), "var(--ink");
    }

    /// An SVG of only text draws pixels, with the bundled faces.
    #[test]
    fn rasterize_svg_draws_text() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 120 30\">\
                   <text x=\"2\" y=\"20\" font-family=\"JetBrains Mono, monospace\" font-size=\"14\">hello</text></svg>";
        let raster = theme(false).rasterize(svg).expect("rasterised");
        assert_eq!(raster.size, Vec2::new(120.0, 30.0));
        assert_eq!(raster.image.size, [120, 30]);
        assert!(raster.image.pixels.iter().any(|p| p.a() > 0));

        let big = rasterize_svg(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4000\" height=\"100\"/>",
            &figure_fonts(),
            2.0,
        )
        .expect("rasterised");
        assert_eq!(big.image.size, [2048, 52]);
    }

    /// Each reason a figure can't show.
    #[test]
    fn placeholder_reasons() {
        let theme = theme(true);
        assert_eq!(image_source("shot.png"), ImageSource::Elsewhere);
        assert_eq!(
            image_source("file:///a/b.png"),
            ImageSource::Path(Path::new("/a/b.png"))
        );
        assert_eq!(
            load_file(
                Path::new("/nowhere/at/all/shot.jpg"),
                MAX_IMAGE_BYTES,
                &theme
            )
            .err(),
            Some(Failure::NotOnHost)
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let junk = dir.path().join("junk.png");
        std::fs::write(&junk, b"not a png at all").expect("write");
        assert_eq!(
            load_file(&junk, MAX_IMAGE_BYTES, &theme).err(),
            Some(Failure::Unreadable)
        );
        assert_eq!(
            load_file(&junk, 4, &theme).err(),
            Some(Failure::TooLarge(16))
        );
        let bad = dir.path().join("bad.svg");
        std::fs::write(&bad, "<svg").expect("write");
        assert!(matches!(
            load_file(&bad, MAX_IMAGE_BYTES, &theme),
            Err(Failure::Svg(_))
        ));
        assert_eq!(Failure::NotOnHost.reason(), "not on this host");
        assert_eq!(Failure::TooLarge(3 << 20).reason(), "too large (3.0 MB)");
        assert_eq!(Failure::Unreadable.reason(), "unreadable");
    }

    /// A data: URI's SVG (base64 or percent-encoded) draws, its vars themed.
    #[test]
    fn data_uris_load() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"2\"><rect width=\"4\" height=\"2\" fill=\"var(--ink)\"/></svg>";
        let b64 = base64::engine::general_purpose::STANDARD.encode(svg);
        let uri = format!("data:image/svg+xml;base64,{b64}");
        let dark = load_data_uri(&uri, &theme(true)).expect("loaded");
        assert_eq!(dark.image.size, [4, 2]);
        // Dark ink is near white.
        assert!(dark.image.pixels[0].r() > 200);
        let plain = format!(
            "data:image/svg+xml,{}",
            svg.replace('#', "%23").replace('"', "%22")
        );
        let light = load_data_uri(&plain, &theme(false)).expect("loaded");
        assert!(light.image.pixels[0].r() < 60);
        assert_eq!(
            load_data_uri("data:image/png;base64,!!!", &theme(false)).err(),
            Some(Failure::Unreadable)
        );
    }
}
