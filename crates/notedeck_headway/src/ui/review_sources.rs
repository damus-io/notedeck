//! A review record's explainer sources in the review pane (see
//! [`review_walk`](crate::review_walk)): the explainer's Markdown beside the
//! commit message at the top of the diff on a wide pane, or as a tab beside
//! the diff on a narrow one, and the code walk's notes threaded into the
//! diff — above the hunks they introduce, or at its top when they introduce
//! none. Either
//! may be the commit's explain git note, when the record has no source of its
//! own.

use notedeck::tokens::{RADIUS_MD, SPACING_MD, SPACING_SM, SPACING_XS, STROKE_THICK};
use notedeck::{ColorTheme, NoteContext};
use notedeck_ui::markdown::{BlockHook, MdBlock, fit_image};

use super::widgets::{section_label, text_pill, tinted_pill};
use crate::explain_flow::BACKDROP;
use crate::explain_palette::Palette;
use crate::review_figures::{
    Failure, Figure, FigureBody, FigureView, Figured, Figures, FittedFlow, FlowFigure, Pane,
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
    /// `source`'s page, parsed now if it changed since the last frame.
    fn page(&mut self, source: SourceText<'_>) -> &mut ExplainerPage {
        if self.page.as_ref().map(|p| p.source) != Some(source.key) {
            self.page = None;
        }
        self.page
            .get_or_insert_with(|| ExplainerPage::new(source.text, source.key))
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
/// pane, in a scroll of its own ([`explainer_page_ui`]). The tab above it
/// says when it is the commit's git note.
pub(super) fn explainer_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    cache: &mut ExplainerCache,
    source: SourceText<'_>,
) {
    let page = cache.page(source);
    egui::ScrollArea::vertical()
        .id_salt("headway-review-explainer")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            explainer_page_ui(ui, theme, app_ctx, page, PageHead::Tab)
        });
}

/// The explainer source `source` as a column of a wide pane's diff header,
/// beside the commit message: the page whole ([`explainer_page_ui`]), for
/// the caller to clamp, under a small "Explainer" label as the message
/// column is under "Commit". With no tab to say so, a "git note" pill by its
/// title says when it is the commit's note.
pub(super) fn explainer_column_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    cache: &mut ExplainerCache,
    source: SourceText<'_>,
) {
    let page = cache.page(source);
    let head = PageHead::Column {
        note: source.is_note(),
    };
    explainer_page_ui(ui, theme, app_ctx, page, head);
}

/// How much of a page's front matter [`explainer_page_ui`] draws over it.
#[derive(Clone, Copy)]
enum PageHead {
    /// A narrow pane's Explainer tab, where the page is all there is: its
    /// title large, then its src line and fact pills.
    Tab,
    /// A column beside the commit message, under the card's title and
    /// beside the commit's own byline: a small "Explainer" label and the
    /// title at [`COLUMN_TITLE_SIZE`], with no src line (the byline has the
    /// sha) or fact pills. `note` puts a "git note" pill by the title.
    Column { note: bool },
}

/// The explainer's title in a column beside the commit message: a step
/// over body text, since the card's title above is the pane's heading.
const COLUMN_TITLE_SIZE: f32 = 17.0;

/// `page`: its front matter as `head` says, then the Markdown, with
/// `headway:` refs drawn as their chips.
fn explainer_page_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    page: &mut ExplainerPage,
    head: PageHead,
) {
    let front = &page.front;
    match head {
        PageHead::Tab => tab_head_ui(ui, theme, front),
        PageHead::Column { note } => {
            section_label(ui, theme, "Explainer");
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
        let page = Page { palette, figures };
        page_body_ui(ui, theme, app_ctx, txn, page, &note.body);
    });
    let card = shown.response.rect;
    let card = card.with_max_y(card.bottom().max(ui.max_rect().bottom()));
    ui.painter().set(
        fill,
        egui::epaint::RectShape::filled(card, frame.corner_radius, frame.fill),
    );
}

/// The page a body's figures belong to: the palette they're drawn in and
/// their loads.
struct Page<'a> {
    palette: &'a Palette,
    figures: &'a mut Figures,
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
    let mut hook = FigureHook { theme, page, body };
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
}

impl BlockHook for FigureHook<'_> {
    fn block_ui(&mut self, ui: &mut egui::Ui, block: MdBlock<'_, '_, '_>) -> bool {
        let Some(figure) = self.body.figure_at(block.index) else {
            return false;
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
/// without a caption is no figure in explain, so it draws bare.
fn figure_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note: Option<&mut NoteContext>,
    txn: Option<&nostrdb::Transaction>,
    page: &mut Page<'_>,
    figure: &Figure,
) {
    if let (FigureBody::Panes(panes), None) = (&figure.body, &figure.caption) {
        ui.add_space(SPACING_XS);
        panes_ui(ui, theme, note, txn, page, panes);
        ui.add_space(SPACING_SM);
        return;
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
                    let inner_page = Page {
                        palette: page.palette,
                        figures: &mut *page.figures,
                    };
                    figured_ui(ui, theme, note, txn, inner_page, inner);
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
            FigureBody::Flow(flow) => flow_ui(ui, theme, page, figure.index, flow, available),
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

/// A flow figure's drawing at 1:1: the wide layout where `available` holds
/// it, else the narrow one, scrolling sideways if even that is wider. It is
/// never scaled, so its text stays at explain's 12px. Its labels sit on the
/// pane's own background.
fn flow_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    page: &mut Page<'_>,
    index: usize,
    flow: &FlowFigure,
    available: f32,
) {
    let dark = ui.visuals().dark_mode;
    let backdrop = ui.visuals().panel_fill;
    let FittedFlow { index, drawing } = flow.fitting(index, available);
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
            placeholder_ui(ui, theme, Some(flow.flow.label.as_str()), None, &failure);
            return;
        }
        FigureView::Ready { texture, .. } => texture,
    };
    let image = egui::Image::new(texture).fit_to_exact_size(size);
    let label = flow.flow.label.as_str();
    let add = |ui: &mut egui::Ui| {
        let response = ui.add(image);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, label));
    };
    if drawing.width <= available {
        add(ui);
        return;
    }
    egui::ScrollArea::horizontal()
        .id_salt(("explainer-flow", index))
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
                    palette: page.palette,
                    figures: &mut *page.figures,
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
