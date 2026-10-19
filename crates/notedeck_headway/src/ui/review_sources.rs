//! A review record's explainer sources in the review pane (see
//! [`review_walk`](crate::review_walk)): the explainer's Markdown beside the
//! commit message at the top of the diff on a wide pane, or as a tab beside
//! the diff on a narrow one, and the code walk's notes threaded into the
//! diff — above the hunks they introduce, or at its top when they introduce
//! none. Either
//! may be the commit's explain git note, when the record has no source of its
//! own.

use md_stream::MdElement;
use notedeck::tokens::{RADIUS_MD, SPACING_MD, SPACING_SM, SPACING_XS, STROKE_THICK};
use notedeck::{ColorTheme, NoteContext};
use notedeck_ui::diff::{GitPatch, LineKind};
use notedeck_ui::eyebrow_label;
use notedeck_ui::markdown::{BlockHook, CellAt, CellLook, MdBlock, fit_image, table_ui};

use super::widgets::{text_pill, tinted_pill};
use crate::explain_diagram::BACKDROP;
use crate::explain_palette::Palette;
use crate::review_figures::{
    CellMark, DiagramFigure, Failure, Figure, FigureBody, FigureView, Figured, Figures,
    FittedDrawing, HunkFigure, HunkFound, Pane,
};
use crate::review_walk::{CodeWalk, ExplainerPage, FrontMatter, SourceText};

/// What a narrow review pane shows under the record's summary. A wide one has
/// no tabs: its explainer sits beside the commit message, over the diff.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReviewTab {
    /// The commit's diff, with the code walk threaded in.
    #[default]
    Diff,
    /// The record's explainer source (or the commit's note), rendered.
    Explainer,
}

/// The pane's explainer page, parsed once per source: kept on the
/// [`ReviewUi`](super::review::ReviewUi) across frames.
#[derive(Default)]
pub(crate) struct ExplainerCache {
    page: Option<ExplainerPage>,
}

impl ExplainerCache {
    /// `source`'s page, parsed now if it changed since the last frame, its
    /// hunk fences found in `patch` (the commit's diff) once it's loaded.
    fn page(&mut self, source: SourceText<'_>, patch: Option<&GitPatch>) -> &mut ExplainerPage {
        if self.page.as_ref().map(|p| p.source) != Some(source.key) {
            self.page = None;
        }
        let page = self
            .page
            .get_or_insert_with(|| ExplainerPage::new(source.text, source.key));
        page.resolve_hunks(patch);
        page
    }
}

/// The tab to show when the explainer source is `page`: the one picked, but
/// always [`ReviewTab::Diff`] without one, whatever was picked on another
/// record.
pub(super) fn shown_tab(page: Option<SourceText<'_>>, tab: ReviewTab) -> ReviewTab {
    if page.is_some() { tab } else { ReviewTab::Diff }
}

/// The Diff / Explainer switch as Linear's view tabs: plain text, the shown
/// tab in primary text over an accent underline, the other muted. Each is
/// `height` tall, so the underline lands on the bottom of the row the caller
/// rules off under them. The explainer's tab says "(git note)" when `page`
/// is the commit's note rather than the record's source.
pub(super) fn review_tabs_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    page: SourceText<'_>,
    tab: &mut ReviewTab,
    height: f32,
) {
    let explainer = if page.is_note() {
        "Explainer (git note)"
    } else {
        "Explainer source"
    };
    let tabs = [(ReviewTab::Diff, "Diff"), (ReviewTab::Explainer, explainer)];
    for (i, (value, label)) in tabs.into_iter().enumerate() {
        if i > 0 {
            ui.add_space(SPACING_MD);
        }
        if view_tab_ui(ui, theme, label, *tab == value, height).clicked() {
            *tab = value;
        }
    }
}

/// One of [`review_tabs_ui`]'s tabs: `label` `height` tall, its text centred,
/// and while `selected` an accent bar along its bottom edge.
fn view_tab_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    label: &str,
    selected: bool,
    height: f32,
) -> egui::Response {
    let color = if selected {
        theme.text_primary
    } else {
        theme.text_muted
    };
    let galley = egui::WidgetText::from(egui::RichText::new(label).color(color)).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Body,
    );
    let size = egui::vec2(galley.size().x, height);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, true, selected, galley.text())
    });
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let hovered = response.hovered() && !selected;
    let text_color = if hovered { theme.text_secondary } else { color };
    let origin = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(origin, galley, text_color);
    if selected {
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.bottom() - STROKE_THICK),
            rect.max,
        );
        ui.painter().rect_filled(bar, 0.0, theme.accent);
    }
    response
}

/// The explainer source `source` as a page filling the rest of a narrow
/// pane, in a scroll of its own ([`explainer_page_ui`]), its hunk fences
/// drawn from `patch`, the commit's diff (`None` while it loads). The tab
/// above it says when it is the commit's git note.
pub(super) fn explainer_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    cache: &mut ExplainerCache,
    source: SourceText<'_>,
    patch: Option<&GitPatch>,
) {
    let page = cache.page(source, patch);
    egui::ScrollArea::vertical()
        .id_salt("headway-review-explainer")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            explainer_page_ui(ui, theme, app_ctx, page, patch, PageHead::Tab)
        });
}

/// The explainer source `source` as a column of a wide pane's diff header,
/// beside the commit message: the page whole ([`explainer_page_ui`]), for
/// the caller to clamp, under a small "EXPLAINER" eyebrow as the message
/// column is under "COMMIT". With no tab to say so, a "git note" pill by its
/// title says when it is the commit's note. Its hunk fences draw from
/// `patch`, the diff under it.
pub(super) fn explainer_column_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    cache: &mut ExplainerCache,
    source: SourceText<'_>,
    patch: &GitPatch,
) {
    let page = cache.page(source, Some(patch));
    let head = PageHead::Column {
        note: source.is_note(),
    };
    explainer_page_ui(ui, theme, app_ctx, page, Some(patch), head);
}

/// How much of a page's front matter [`explainer_page_ui`] draws over it.
#[derive(Clone, Copy)]
enum PageHead {
    /// A narrow pane's Explainer tab, where the page is all there is: its
    /// title large, then its src line and fact pills.
    Tab,
    /// A column beside the commit message, under the card's title and
    /// beside the commit's own byline: a small "EXPLAINER" eyebrow and the
    /// title at [`COLUMN_TITLE_SIZE`], with no src line (the byline has the
    /// sha) or fact pills. `note` puts a "git note" pill by the title.
    Column { note: bool },
}

/// The explainer's title in a column beside the commit message: a step
/// over body text, since the card's title above is the pane's heading.
const COLUMN_TITLE_SIZE: f32 = 17.0;

/// `page`: its front matter as `head` says, then the Markdown, with
/// `headway:` refs drawn as their chips and hunk fences from `patch`.
fn explainer_page_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    page: &mut ExplainerPage,
    patch: Option<&GitPatch>,
    head: PageHead,
) {
    let front = &page.front;
    match head {
        PageHead::Tab => tab_head_ui(ui, theme, front),
        PageHead::Column { note } => {
            eyebrow_label(ui, theme, "EXPLAINER");
            if front.title.is_some() || note {
                ui.horizontal_wrapped(|ui| {
                    if let Some(title) = &front.title {
                        let title = egui::RichText::new(title)
                            .size(COLUMN_TITLE_SIZE)
                            .strong()
                            .color(theme.text_primary);
                        ui.label(title);
                    }
                    if note {
                        tinted_pill(ui, "git note", theme.warning);
                    }
                });
            }
            ui.add_space(SPACING_SM);
        }
    }
    let figured = Page {
        palette: &page.palette,
        figures: &mut page.figures,
        patch,
        in_pane: false,
    };
    page_body_ui(ui, theme, app_ctx, None, figured, &page.body);
}

/// A page's front matter over the Explainer tab: its title large, its src
/// line in small muted mono, and its facts as pills.
fn tab_head_ui(ui: &mut egui::Ui, theme: &ColorTheme, front: &FrontMatter) {
    if let Some(title) = &front.title {
        ui.label(egui::RichText::new(title).size(20.0).strong());
    }
    if let Some(src) = &front.src {
        ui.label(
            egui::RichText::new(src)
                .small()
                .monospace()
                .color(theme.text_muted),
        );
    }
    if !front.facts.is_empty() {
        ui.add_space(SPACING_XS);
        ui.horizontal_wrapped(|ui| {
            for fact in &front.facts {
                text_pill(ui, theme, fact);
            }
        });
    }
    if front.title.is_some() || front.src.is_some() || !front.facts.is_empty() {
        ui.add_space(SPACING_SM);
    }
}

/// The walk's notes that thread onto no hunk — its intro, its closing words,
/// and any whose hunk wasn't found (saying which) — in the diff's header, so
/// they scroll away with it. Each is drawn as the notes in the diff are
/// ([`walk_frame`]), with the link-coloured bar the diff gives them, so a
/// walk reads as one voice from its intro down through the hunks.
pub(super) fn walk_top_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    walk: &mut CodeWalk,
) {
    if walk.top().next().is_none() {
        return;
    }
    let CodeWalk {
        notes,
        palette,
        figures,
        ..
    } = walk;
    for note in notes.iter().filter(|n| n.at.is_none()) {
        let bar = ui.visuals().hyperlink_color;
        let shown = walk_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(missing) = &note.missing {
                tinted_pill(ui, missing, theme.warning);
                ui.add_space(SPACING_XS);
            }
            let page = Page {
                palette,
                figures: &mut *figures,
                patch: None,
                in_pane: false,
            };
            page_body_ui(ui, theme, app_ctx, None, page, &note.body);
        });
        let rect = shown.response.rect;
        let edge =
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + WALK_BAR, rect.bottom()));
        ui.painter().rect_filled(edge, 0.0, bar);
        ui.add_space(SPACING_SM);
    }
}

/// Width of the bar down a walk note's left edge in the header, as the diff
/// marks one inside it.
const WALK_BAR: f32 = 3.0;

/// The card a walk note's prose sits in: a faint tint of the link colour the
/// walk is marked in, rounded on the right where the bar doesn't run, and
/// padded so the prose doesn't touch its edges — the diff's band alone
/// left it flush against the hunks around it.
fn walk_frame(ui: &egui::Ui) -> egui::Frame {
    let tint = ui.visuals().hyperlink_color.gamma_multiply(WALK_TINT);
    let r = RADIUS_MD as u8;
    egui::Frame::new()
        .fill(tint)
        .corner_radius(egui::CornerRadius {
            nw: 0,
            sw: 0,
            ne: r,
            se: r,
        })
        .inner_margin(egui::Margin::symmetric(SPACING_MD as i8, SPACING_SM as i8))
}

/// How strongly a walk note's card is tinted with the walk's colour.
const WALK_TINT: f32 = 0.08;

/// Walk note `key` where the diff hands it its rows, above its hunk: in the
/// same card as the header's ([`walk_frame`]), as wide as the diff gives it.
pub(super) fn walk_note_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    txn: Option<&nostrdb::Transaction>,
    walk: &mut CodeWalk,
    key: usize,
) {
    let CodeWalk {
        notes,
        palette,
        figures,
        ..
    } = walk;
    let Some(note) = notes.get(key) else {
        return;
    };
    // The card's tint runs down to the bottom of the note's whole rows, so
    // the slack they leave under the prose reads as the card's, not a gap
    // before the hunk. Painted behind, after the prose is measured, so the
    // diff still sizes the note by what the prose needs.
    let fill = ui.painter().add(egui::Shape::Noop);
    let frame = walk_frame(ui);
    let shown = frame.fill(egui::Color32::TRANSPARENT).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let page = Page {
            palette,
            figures,
            patch: None,
            in_pane: false,
        };
        page_body_ui(ui, theme, app_ctx, txn, page, &note.body);
    });
    let card = shown.response.rect;
    let card = card.with_max_y(card.bottom().max(ui.max_rect().bottom()));
    ui.painter().set(
        fill,
        egui::epaint::RectShape::filled(card, frame.corner_radius, frame.fill),
    );
}

/// The page a body's figures belong to: the palette they're drawn in, their
/// loads, and the commit's diff its hunk fences draw from (`None` for a
/// walk's notes, whose fences are the diff's own hunks, or while it loads).
struct Page<'a> {
    palette: &'a Palette,
    figures: &'a mut Figures,
    patch: Option<&'a GitPatch>,
    /// Whether this is a ba pane's body, whose code blocks never fold
    /// ([`PreFold`]), as explain's `in_ba`.
    in_pane: bool,
}

impl Page<'_> {
    /// This page lent to a body nested in one of its figures.
    fn nested(&mut self) -> Page<'_> {
        Page {
            palette: self.palette,
            figures: &mut *self.figures,
            patch: self.patch,
            in_pane: self.in_pane,
        }
    }
}

/// `body` drawn with its refs resolved through `txn`, or one opened here
/// when the caller holds none (without one refs draw as plain text): see
/// [`figured_ui`].
fn page_body_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    txn: Option<&nostrdb::Transaction>,
    page: Page<'_>,
    body: &Figured,
) {
    let opened = match txn {
        Some(_) => None,
        None => nostrdb::Transaction::new(app_ctx.ndb).ok(),
    };
    let txn = txn.or(opened.as_ref());
    let mut note_ctx = app_ctx.note_context();
    figured_ui(ui, theme, Some(&mut note_ctx), txn, page, body);
}

/// `body` drawn by notedeck_ui's Markdown renderer, its figures by
/// [`FigureHook`] in their place among the rest.
fn figured_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note: Option<&mut NoteContext>,
    txn: Option<&nostrdb::Transaction>,
    page: Page<'_>,
    body: &Figured,
) {
    figured_headed_ui(ui, theme, note, txn, page, body, None);
}

/// [`figured_ui`], its `##` and `###` headings drawn in `headings`' ink
/// when it's given (see [`FigureHook::headings`]).
fn figured_headed_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note: Option<&mut NoteContext>,
    txn: Option<&nostrdb::Transaction>,
    page: Page<'_>,
    body: &Figured,
    headings: Option<egui::Color32>,
) {
    let mut hook = FigureHook {
        theme,
        page,
        body,
        headings,
    };
    notedeck_ui::markdown::render_parsed_markdown(
        &body.md.elements,
        None,
        &body.md.source,
        note,
        txn,
        Some(&mut hook),
        ui,
    );
}

/// Claims the elements of `body` that are figures and draws them
/// ([`figure_ui`]), leaving the rest to the renderer.
struct FigureHook<'a> {
    theme: &'a ColorTheme,
    page: Page<'a>,
    body: &'a Figured,
    /// The ink a `##` or `###` heading draws in, at [`WRONG_HEADING_SCALE`]
    /// times body text, when `body` is a wrong block's; `None` leaves
    /// headings to the renderer.
    headings: Option<egui::Color32>,
}

impl BlockHook for FigureHook<'_> {
    fn block_ui(&mut self, ui: &mut egui::Ui, block: MdBlock<'_, '_, '_>) -> bool {
        if let (
            Some(ink),
            MdElement::Heading {
                level: 2 | 3,
                content,
            },
        ) = (self.headings, block.element)
        {
            let size = egui::TextStyle::Body.resolve(ui.style()).size * WRONG_HEADING_SCALE;
            let text = egui::RichText::new(content.resolve(block.source))
                .size(size)
                .strong()
                .color(ink);
            ui.add(egui::Label::new(text).wrap());
            ui.add_space(SPACING_XS);
            return true;
        }
        if let (MdElement::Table { headers, rows }, Some(marked)) =
            (block.element, self.body.table_at(block.index))
        {
            let looks = CellLooks::of(self.page.palette, self.theme, ui.visuals().dark_mode);
            let look = |at: CellAt| looks.of_mark(marked.mark(at));
            table_ui(
                headers,
                rows,
                block.source,
                block.note,
                block.txn,
                &look,
                ui,
            );
            return true;
        }
        let Some(figure) = self.body.figure_at(block.index) else {
            let MdElement::CodeBlock(code) = block.element else {
                return false;
            };
            let lang = code.language.as_ref().map(|l| l.resolve(block.source));
            let content = code.content.resolve(block.source);
            code_ui(ui, self.theme, &self.page, block.index, lang, content);
            return true;
        };
        figure_ui(
            ui,
            self.theme,
            block.note,
            block.txn,
            &mut self.page,
            figure,
        );
        true
    }
}

/// A figure, centred in a thin frame at no more than its own size or the
/// pane's width, its caption small and muted under it. A figure that can't
/// show draws a placeholder saying why; one loading, a spinner. A ba block
/// without a caption is no figure in explain, so it draws bare, and a wrong
/// block is never one ([`wrong_ui`]).
fn figure_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note: Option<&mut NoteContext>,
    txn: Option<&nostrdb::Transaction>,
    page: &mut Page<'_>,
    figure: &Figure,
) {
    match (&figure.body, &figure.caption) {
        (FigureBody::Panes(panes), None) => {
            ui.add_space(SPACING_XS);
            panes_ui(ui, theme, note, txn, page, panes);
            ui.add_space(SPACING_SM);
            return;
        }
        (FigureBody::Wrong(body), _) => {
            wrong_ui(ui, theme, note, txn, page, body);
            return;
        }
        _ => {}
    }
    let dark = ui.visuals().dark_mode;
    let margin = SPACING_SM;
    ui.add_space(SPACING_XS);
    ui.vertical_centered(|ui| {
        let frame = egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, theme.border_default))
            .corner_radius(4.0)
            .inner_margin(margin);
        let available = ui.available_width() - 2.0 * margin - 2.0;
        frame.show(ui, |ui| match &figure.body {
            FigureBody::Svg { source, label } => {
                let label = label.as_deref().or(figure.caption.as_deref());
                let view = page
                    .figures
                    .svg(ui.ctx(), page.palette, figure.index, dark, source);
                if let FigureView::Failed(failure) = &view {
                    placeholder_ui(ui, theme, label, None, failure);
                    ui.label(egui::RichText::new(source.as_str()).small().monospace());
                    return;
                }
                figure_view_ui(ui, theme, view, available, label, "");
            }
            FigureBody::Markdown(inner) => {
                ui.vertical(|ui| {
                    ui.set_max_width(available);
                    figured_ui(ui, theme, note, txn, page.nested(), inner);
                });
            }
            FigureBody::Image { alt, src } => {
                let view = page
                    .figures
                    .image(ui.ctx(), page.palette, figure.index, dark, src);
                figure_view_ui(ui, theme, view, available, Some(alt.as_str()), src);
            }
            FigureBody::Panes(panes) => {
                ui.set_width(available);
                panes_ui(ui, theme, note, txn, page, panes);
            }
            FigureBody::Diagram(diagram) => {
                diagram_ui(ui, theme, page, figure.index, diagram, available)
            }
            FigureBody::Hunk(hunk) => {
                ui.vertical(|ui| {
                    ui.set_width(available);
                    hunk_ui(ui, theme, page, figure.index, hunk);
                });
            }
            // Drawn bare, above.
            FigureBody::Wrong(_) => {}
        });
        if let Some(caption) = &figure.caption {
            ui.add_space(SPACING_XS);
            ui.label(
                egui::RichText::new(caption.as_str())
                    .small()
                    .color(theme.text_muted),
            );
        }
    });
    ui.add_space(SPACING_SM);
}

/// A diagram figure's drawing at 1:1: the wide layout where `available` holds
/// it, else the narrow one, scrolling sideways if even that is wider. It is
/// never scaled, so its text stays at explain's 12px. Its labels sit on the
/// pane's own background.
fn diagram_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    page: &mut Page<'_>,
    index: usize,
    diagram: &DiagramFigure,
    available: f32,
) {
    let dark = ui.visuals().dark_mode;
    let backdrop = ui.visuals().panel_fill;
    let FittedDrawing { index, drawing } = diagram.fitting(index, available);
    let size = egui::vec2(drawing.width, drawing.height);
    let view = page
        .figures
        .svg_with(ui.ctx(), page.palette, index, dark, || {
            let [r, g, b, _] = backdrop.to_array();
            drawing
                .svg
                .replace(BACKDROP, &format!("#{r:02x}{g:02x}{b:02x}"))
        });
    let texture = match view {
        FigureView::Pending => {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(size.x.min(available), size.y),
                egui::Sense::hover(),
            );
            ui.put(rect, egui::Spinner::new());
            return;
        }
        FigureView::Failed(failure) => {
            placeholder_ui(
                ui,
                theme,
                Some(diagram.diagram.label.as_str()),
                None,
                &failure,
            );
            return;
        }
        FigureView::Ready { texture, .. } => texture,
    };
    let image = egui::Image::new(texture).fit_to_exact_size(size);
    let label = diagram.diagram.label.as_str();
    let add = |ui: &mut egui::Ui| {
        let response = ui.add(image);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, label));
    };
    if drawing.width <= available {
        add(ui);
        return;
    }
    egui::ScrollArea::horizontal()
        .id_salt(("explainer-diagram", index))
        .max_width(available)
        .show(ui, add);
}

/// The most panes a ba block lays side by side; one with more stacks.
const MAX_SIDE_PANES: usize = 4;

/// The narrowest a pane is drawn beside another: under it, they stack, as
/// explain's grid goes to one column on a narrow page.
const MIN_PANE_WIDTH: f32 = 240.0;

/// The gap between two panes, across or down.
const PANE_GAP: f32 = 10.0;

/// A ba block's panes side by side, as tall as the tallest, or stacked
/// when the column can't give each [`MIN_PANE_WIDTH`].
fn panes_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    mut note: Option<&mut NoteContext>,
    txn: Option<&nostrdb::Transaction>,
    page: &mut Page<'_>,
    panes: &[Pane],
) {
    let n = panes.len();
    let across = (ui.available_width() - PANE_GAP * n.saturating_sub(1) as f32) / n.max(1) as f32;
    if (1..=MAX_SIDE_PANES).contains(&n) && across >= MIN_PANE_WIDTH {
        pane_row_ui(ui, theme, note, txn, page, panes);
        return;
    }
    for (i, pane) in panes.iter().enumerate() {
        if i > 0 {
            ui.add_space(PANE_GAP);
        }
        let row = std::slice::from_ref(pane);
        pane_row_ui(ui, theme, note.as_deref_mut(), txn, page, row);
    }
}

/// One row of at most [`MAX_SIDE_PANES`] panes splitting the column's width:
/// each drawn in its own child, then its card painted behind it down to the
/// row's tallest — a neutral fill and border, its tone's bar down the left
/// edge and its title, small, in the tone's ink.
fn pane_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    mut note: Option<&mut NoteContext>,
    txn: Option<&nostrdb::Transaction>,
    page: &mut Page<'_>,
    panes: &[Pane],
) {
    let panes = &panes[..panes.len().min(MAX_SIDE_PANES)];
    let dark = ui.visuals().dark_mode;
    let top = ui.cursor().min;
    let n = panes.len().max(1) as f32;
    let width = (ui.available_width() - PANE_GAP * (n - 1.0)) / n;
    let margin = egui::Margin {
        left: (WALK_BAR + SPACING_MD) as i8,
        right: SPACING_MD as i8,
        top: SPACING_SM as i8,
        bottom: SPACING_SM as i8,
    };
    let mut cards = [egui::layers::ShapeIdx(0); MAX_SIDE_PANES];
    let mut bottom = top.y;
    for (i, pane) in panes.iter().enumerate() {
        cards[i] = ui.painter().add(egui::Shape::Noop);
        let left = top.x + i as f32 * (width + PANE_GAP);
        let room =
            egui::Rect::from_min_size(egui::pos2(left, top.y), egui::vec2(width, f32::INFINITY));
        // Left-aligned even in a figure's frame, which centres what it holds.
        let builder = egui::UiBuilder::new()
            .max_rect(room)
            .layout(egui::Layout::top_down(egui::Align::Min));
        let mut child = ui.new_child(builder);
        egui::Frame::new()
            .inner_margin(margin)
            .show(&mut child, |ui| {
                ui.set_width(ui.available_width());
                let ink = tone_color(page.palette, pane.tone.map(|t| t.tokens().ink), dark)
                    .unwrap_or(theme.text_primary);
                ui.label(
                    egui::RichText::new(pane.title.as_str())
                        .small()
                        .strong()
                        .color(ink),
                );
                ui.add_space(SPACING_XS);
                let inner = Page {
                    in_pane: true,
                    ..page.nested()
                };
                figured_ui(ui, theme, note.as_deref_mut(), txn, inner, &pane.body);
            });
        bottom = bottom.max(child.min_rect().bottom());
    }
    let r = RADIUS_MD as u8;
    for (i, pane) in panes.iter().enumerate() {
        let left = top.x + i as f32 * (width + PANE_GAP);
        let card =
            egui::Rect::from_min_max(egui::pos2(left, top.y), egui::pos2(left + width, bottom));
        let edge = tone_color(page.palette, pane.tone.map(|t| t.tokens().edge), dark)
            .unwrap_or(theme.border_strong);
        let fill = egui::epaint::RectShape::new(
            card,
            r,
            theme.surface_secondary,
            egui::Stroke::new(1.0, theme.border_default),
            egui::StrokeKind::Inside,
        );
        ui.painter().set(cards[i], fill);
        let bar = egui::Rect::from_min_max(card.min, egui::pos2(card.left() + WALK_BAR, bottom));
        ui.painter().rect_filled(
            bar,
            egui::CornerRadius {
                nw: r,
                sw: r,
                ne: 0,
                se: 0,
            },
            edge,
        );
    }
    let right = top.x + n * width + (n - 1.0) * PANE_GAP;
    ui.allocate_rect(
        egui::Rect::from_min_max(top, egui::pos2(right, bottom)),
        egui::Sense::hover(),
    );
}

/// A wrong block's `##` and `###` headings against body text: explain's
/// 1.14rem over its 17px body.
const WRONG_HEADING_SCALE: f32 = 1.14 * 16.0 / 17.0;

/// Under this width a wrong block takes explain's phone padding.
const WRONG_NARROW: f32 = 520.0;

/// A ` ```wrong ` block, as explain's `aside.wrong`: its Markdown on the
/// page's alert-soft fill inside a thin alert rule, its `##` and `###`
/// headings in alert, set off from the text around it.
fn wrong_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note: Option<&mut NoteContext>,
    txn: Option<&nostrdb::Transaction>,
    page: &mut Page<'_>,
    body: &Figured,
) {
    let dark = ui.visuals().dark_mode;
    let fill =
        tone_color(page.palette, Some("alert-soft"), dark).unwrap_or(theme.surface_secondary);
    let edge = tone_color(page.palette, Some("alert"), dark).unwrap_or(theme.destructive);
    // explain's 18px 20px, or 14px 16px on a phone, less the 1px rule.
    let margin = if ui.available_width() < WRONG_NARROW {
        egui::Margin::symmetric(15, 13)
    } else {
        egui::Margin::symmetric(19, 17)
    };
    ui.add_space(SPACING_MD);
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, edge))
        .corner_radius(4.0)
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            figured_headed_ui(ui, theme, note, txn, page.nested(), body, Some(edge));
        });
    ui.add_space(SPACING_MD);
}

/// explain's code against its body text: `.84rem` over its 17px.
const CODE_SCALE: f32 = 0.84 * 16.0 / 17.0;

/// explain's `pre` line height, in lines of its font.
const CODE_LINE_HEIGHT: f32 = 1.6;

/// explain's `srcname` against its body text: `.7rem` over its 17px.
const SRCNAME_SCALE: f32 = 0.7 * 16.0 / 17.0;

/// A ` ```hunk ` fence, as explain builds it: the file's path in small
/// upper-case mono (its `srcname`), then the hunk the commit's diff has for
/// it in a coloured block ([`pre_ui`]), its `@@` header first. Without the
/// diff it is the citation, the header alone; a diff without the hunk says
/// so under it. `id` is the figure's number, to tell its scroll apart. A
/// long hunk folds ([`PreFold`]), its button counting the lines under the
/// header.
fn hunk_ui(ui: &mut egui::Ui, theme: &ColorTheme, page: &Page<'_>, id: usize, hunk: &HunkFigure) {
    let dark = ui.visuals().dark_mode;
    let pre_id = egui::IdSalt::new(("hunk", id));
    let fold = PreFold::of(page, false);
    if !hunk.srcname.is_empty() {
        let size = egui::TextStyle::Body.resolve(ui.style()).size * SRCNAME_SCALE;
        let ink = tone_color(page.palette, Some("ink-3"), dark).unwrap_or(theme.text_muted);
        let srcname = egui::RichText::new(hunk.srcname.as_str())
            .monospace()
            .size(size)
            .color(ink);
        ui.label(srcname);
        ui.add_space(SPACING_SM);
    }
    if let (HunkFound::Found(at), Some(patch)) = (hunk.found, page.patch)
        && let Some(file) = patch.files().get(at.file)
        && let Some(found) = file.hunks.get(at.hunk)
    {
        let header = PreRow {
            kind: PreKind::Hunk,
            marker: "",
            text: patch.text(found.header),
        };
        let lines = file.hunk_lines(found).iter().map(|line| {
            let (kind, marker) = match line.kind {
                LineKind::Insert => (PreKind::Add, "+"),
                LineKind::Delete => (PreKind::Del, "-"),
                LineKind::Context => (PreKind::Plain, " "),
                // Its text is the whole `\ No newline …` line.
                LineKind::NoNewline => (PreKind::Plain, ""),
            };
            PreRow {
                kind,
                marker,
                text: patch.text(line.text),
            }
        });
        pre_ui(
            ui,
            theme,
            page.palette,
            pre_id,
            std::iter::once(header).chain(lines),
            fold.head(1),
        );
        return;
    }
    if !hunk.cite.header.is_empty() {
        let header = PreRow {
            kind: PreKind::Hunk,
            marker: "",
            text: hunk.cite.header.as_str(),
        };
        pre_ui(
            ui,
            theme,
            page.palette,
            pre_id,
            std::iter::once(header),
            PreFold::Whole,
        );
    }
    if hunk.found == HunkFound::Missing {
        ui.add_space(SPACING_XS);
        ui.label(
            egui::RichText::new("hunk not found in the diff")
                .small()
                .color(theme.warning),
        );
    }
}

/// A fence that's no figure, as explain's `pre`: a ` ```diff ` fence's
/// lines coloured by their first character (`@@` a hunk header, `+` added,
/// `-` removed), any other fence's in ink. It folds when long unless
/// `full` follows its language. `index` is its element's, to tell its
/// scroll and fold apart.
fn code_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    page: &Page<'_>,
    index: usize,
    lang: Option<&str>,
    content: &str,
) {
    let mut words = lang.unwrap_or("").split_whitespace();
    let diff = words.next() == Some("diff");
    let full = words.any(|w| w == "full");
    let content = content.strip_suffix('\n').unwrap_or(content);
    let rows = content.split('\n').map(move |text| PreRow {
        kind: if diff {
            PreKind::of_diff_line(text)
        } else {
            PreKind::Plain
        },
        marker: "",
        text,
    });
    let id = egui::IdSalt::new(("code", index));
    pre_ui(ui, theme, page.palette, id, rows, PreFold::of(page, full));
    ui.add_space(SPACING_SM);
}

/// How explain colours a line of a `pre`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PreKind {
    /// A hunk's `@@` header, in slate.
    Hunk,
    /// An added line, in moss ink on moss's soft fill.
    Add,
    /// A removed line, in alert ink on alert's soft fill.
    Del,
    /// Any other line, in ink.
    Plain,
}

impl PreKind {
    /// A ` ```diff ` fence's line by its first character, as explain reads
    /// it: `@@` before `+` and `-`, so a header is never an added line.
    fn of_diff_line(line: &str) -> Self {
        if line.starts_with("@@") {
            Self::Hunk
        } else if line.starts_with('+') {
            Self::Add
        } else if line.starts_with('-') {
            Self::Del
        } else {
            Self::Plain
        }
    }
}

/// A [`pre_ui`] block longer than this many lines folds.
const FOLD_AT: usize = 30;

/// The lines a folded [`pre_ui`] block shows.
const FOLD_SHOW: usize = 20;

/// The height, in lines of its font, a folded block fades out over:
/// explain's `3.2em` mask.
const FOLD_FADE: f32 = 3.2;

/// explain's `.more-btn` against its body text: `.76rem` over its 17px.
const FOLD_BUTTON_SCALE: f32 = 0.76 * 16.0 / 17.0;

/// Whether a [`pre_ui`] block folds when it's long, as explain's `pre`.
#[derive(Clone, Copy)]
enum PreFold {
    /// Always whole: a fence marked `full`, a block in a ba pane, or one
    /// that's only ever a line.
    Whole,
    /// Past [`FOLD_AT`] lines it shows its first [`FOLD_SHOW`], faded, over
    /// a "Show all N lines" button, until that's pressed. N counts the
    /// lines after the first `head` (a hunk's `@@` line).
    Long { head: usize },
}

impl PreFold {
    /// How a block on `page` folds: never in a ba pane, nor when `full`.
    fn of(page: &Page<'_>, full: bool) -> Self {
        if full || page.in_pane {
            Self::Whole
        } else {
            Self::Long { head: 0 }
        }
    }

    /// This fold with its button not counting the first `head` lines.
    fn head(self, head: usize) -> Self {
        match self {
            Self::Whole => Self::Whole,
            Self::Long { .. } => Self::Long { head },
        }
    }
}

/// One line of a [`pre_ui`] block: its marker (`+`, `-`, a space, or none)
/// and its text after it.
#[derive(Clone, Copy)]
struct PreRow<'a> {
    kind: PreKind,
    marker: &'static str,
    text: &'a str,
}

/// The page's colours a marked table cell draws in, as explain's `td.ok`,
/// `td.bad`, `td.hot` and `td.dim`, the theme's where the palette hasn't got
/// one. explain sets an ok or bad cell's text at weight 600 too; here, as
/// for `**bold**`, its ink alone sets it apart.
struct CellLooks {
    ok: egui::Color32,
    bad: egui::Color32,
    hot: egui::Color32,
    dim: egui::Color32,
}

impl CellLooks {
    fn of(palette: &Palette, theme: &ColorTheme, dark: bool) -> Self {
        let token = |name, fallback| tone_color(palette, Some(name), dark).unwrap_or(fallback);
        Self {
            ok: token("moss-ink", theme.success),
            bad: token("alert-ink", theme.destructive),
            hot: token("brass-soft", theme.surface_secondary),
            dim: token("ink-3", theme.text_muted),
        }
    }

    /// How a cell with `mark` draws: an unmarked one as any other.
    fn of_mark(&self, mark: Option<CellMark>) -> CellLook {
        match mark {
            None => CellLook::default(),
            Some(CellMark::Ok) => CellLook {
                ink: Some(self.ok),
                fill: None,
            },
            Some(CellMark::Bad) => CellLook {
                ink: Some(self.bad),
                fill: None,
            },
            Some(CellMark::Hot) => CellLook {
                ink: None,
                fill: Some(self.hot),
            },
            Some(CellMark::Dim) => CellLook {
                ink: Some(self.dim),
                fill: None,
            },
        }
    }
}

/// The page's colours a [`pre_ui`] block draws in, the theme's where the
/// palette hasn't got one.
struct PreColors {
    ink: egui::Color32,
    hunk: egui::Color32,
    add: egui::Color32,
    add_fill: egui::Color32,
    del: egui::Color32,
    del_fill: egui::Color32,
    fill: egui::Color32,
    rule: egui::Color32,
    /// A folded block's button: its text, border and fill.
    button_ink: egui::Color32,
    button_rule: egui::Color32,
    button_fill: egui::Color32,
}

impl PreColors {
    fn of(palette: &Palette, theme: &ColorTheme, dark: bool) -> Self {
        let token = |name, fallback| tone_color(palette, Some(name), dark).unwrap_or(fallback);
        Self {
            ink: token("ink", theme.text_primary),
            hunk: token("slate", theme.text_muted),
            add: token("moss-ink", theme.success),
            add_fill: token("moss-soft", theme.surface_secondary),
            del: token("alert-ink", theme.destructive),
            del_fill: token("alert-soft", theme.surface_secondary),
            fill: token("surface-2", theme.surface_secondary),
            rule: token("rule", theme.border_default),
            button_ink: token("ink-2", theme.text_secondary),
            button_rule: token("rule-strong", theme.border_strong),
            button_fill: token("surface", theme.surface_primary),
        }
    }

    /// The ink of a `kind` line, and the fill under it, if it has one.
    fn row(&self, kind: PreKind) -> RowColors {
        let (ink, fill) = match kind {
            PreKind::Hunk => (self.hunk, None),
            PreKind::Add => (self.add, Some(self.add_fill)),
            PreKind::Del => (self.del, Some(self.del_fill)),
            PreKind::Plain => (self.ink, None),
        };
        RowColors { ink, fill }
    }
}

/// A [`pre_ui`] line's ink, and the fill behind it across the block.
struct RowColors {
    ink: egui::Color32,
    fill: Option<egui::Color32>,
}

/// `rows` as explain's `pre`: mono at [`CODE_SCALE`] on the page's second
/// surface inside a 1px rule, an added or removed line's fill running the
/// block's full width as explain's inline-block does. A block wider than the
/// column scrolls sideways rather than wrapping, and a long one folds as
/// `fold` says. `id` tells its scroll and fold apart from the others'.
fn pre_ui<'a>(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    palette: &Palette,
    id: egui::IdSalt,
    rows: impl Iterator<Item = PreRow<'a>> + Clone,
    fold: PreFold,
) {
    let colors = PreColors::of(palette, theme, ui.visuals().dark_mode);
    let size = egui::TextStyle::Body.resolve(ui.style()).size * CODE_SCALE;
    let font = egui::FontId::monospace(size);
    let opened = ui.make_persistent_id((id, "opened"));
    let total = rows.clone().count();
    let folded = match fold {
        PreFold::Long { head } if total > FOLD_AT => {
            let open = ui.data(|d| d.get_temp::<bool>(opened)).unwrap_or(false);
            (!open).then_some(total - head)
        }
        _ => None,
    };
    let rows = rows.take(if folded.is_some() { FOLD_SHOW } else { total });
    let mut job = egui::text::LayoutJob::default();
    for (i, row) in rows.clone().enumerate() {
        let format = egui::TextFormat {
            font_id: font.clone(),
            color: colors.row(row.kind).ink,
            line_height: Some(size * CODE_LINE_HEIGHT),
            ..Default::default()
        };
        if i > 0 {
            job.append("\n", 0.0, format.clone());
        }
        job.append(row.marker, 0.0, format.clone());
        job.append(row.text, 0.0, format);
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let fade = FOLD_FADE * size;
    // explain's 14px 16px, less the 1px rule.
    egui::Frame::new()
        .fill(colors.fill)
        .stroke(egui::Stroke::new(1.0, colors.rule))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(15, 13))
        .show(ui, |ui| {
            let width = ui.available_width();
            let text = |ui: &mut egui::Ui| {
                let size = egui::vec2(galley.size().x.max(width), galley.size().y);
                let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
                for (placed, row) in galley.rows.iter().zip(rows.clone()) {
                    let Some(fill) = colors.row(row.kind).fill else {
                        continue;
                    };
                    let line = placed.rect().translate(rect.min.to_vec2());
                    let band = egui::Rect::from_x_y_ranges(rect.x_range(), line.y_range());
                    ui.painter().rect_filled(band, 0.0, fill);
                }
                ui.painter().galley(rect.min, galley.clone(), colors.ink);
                if folded.is_some() {
                    fade_ui(ui, rect, fade, colors.fill);
                }
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::Label, true, galley.text())
                });
            };
            if galley.size().x <= width {
                text(ui);
                return;
            }
            egui::ScrollArea::horizontal()
                .id_salt(("explainer-pre", id))
                .show(ui, text);
        });
    let Some(lines) = folded else {
        return;
    };
    ui.add_space(SPACING_XS);
    let size = egui::TextStyle::Body.resolve(ui.style()).size * FOLD_BUTTON_SCALE;
    let label = egui::RichText::new(format!("Show all {lines} lines"))
        .monospace()
        .size(size)
        .color(colors.button_ink);
    let button = egui::Button::new(label)
        .fill(colors.button_fill)
        .stroke(egui::Stroke::new(1.0, colors.button_rule))
        .corner_radius(3.0);
    if ui.add(button).clicked() {
        ui.data_mut(|d| d.insert_temp(opened, true));
    }
}

/// The bottom `height` of `rect` faded into `fill`, as explain masks a
/// folded block's last lines out.
fn fade_ui(ui: &egui::Ui, rect: egui::Rect, height: f32, fill: egui::Color32) {
    let top = (rect.bottom() - height).max(rect.top());
    let mut mesh = egui::Mesh::default();
    let clear = egui::Color32::TRANSPARENT;
    mesh.colored_vertex(egui::pos2(rect.left(), top), clear);
    mesh.colored_vertex(egui::pos2(rect.right(), top), clear);
    mesh.colored_vertex(rect.right_bottom(), fill);
    mesh.colored_vertex(rect.left_bottom(), fill);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(mesh);
}

/// Palette token `name`'s colour in the light or dark theme; `None` for no
/// token, or one the palette doesn't have.
fn tone_color(palette: &Palette, name: Option<&str>, dark: bool) -> Option<egui::Color32> {
    egui::Color32::from_hex(palette.get(name?, dark)?).ok()
}

/// A loaded figure at its fitted size (labelled `label` for a screen
/// reader), a spinner while it loads, or a placeholder naming `src` and why
/// it can't show.
fn figure_view_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: FigureView<'_>,
    available: f32,
    label: Option<&str>,
    src: &str,
) {
    match view {
        FigureView::Pending => {
            ui.spinner();
        }
        FigureView::Failed(failure) => placeholder_ui(ui, theme, label, Some(src), &failure),
        FigureView::Ready { texture, size } => {
            let size = fit_image(size, available);
            let response = ui.add(egui::Image::new(texture).fit_to_exact_size(size));
            let label = label.unwrap_or("figure");
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, label));
        }
    }
}

/// Why a figure can't show: its alt text, the `src` it names in mono (when
/// it has one) and the reason.
fn placeholder_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    alt: Option<&str>,
    src: Option<&str>,
    failure: &Failure,
) {
    ui.vertical_centered(|ui| {
        if let Some(alt) = alt.filter(|a| !a.is_empty()) {
            ui.label(egui::RichText::new(alt).color(theme.text_secondary));
        }
        if let Some(src) = src.filter(|s| !s.is_empty() && !s.starts_with("data:")) {
            ui.label(egui::RichText::new(src).small().monospace());
        }
        ui.label(
            egui::RichText::new(failure.reason())
                .small()
                .color(theme.warning),
        );
    });
}
