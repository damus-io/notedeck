//! A review record's explainer sources in the review pane (see
//! [`review_walk`](crate::review_walk)): the explainer's Markdown as a tab
//! beside the diff, and the code walk's notes threaded into the diff — above
//! the hunks they introduce, or at its top when they introduce none. Either
//! may be the commit's explain git note, when the record has no source of its
//! own.

use notedeck::ColorTheme;
use notedeck::media::AnimationMode;
use notedeck::media::images::ImageType;
use notedeck::tokens::{RADIUS_MD, SPACING_MD, SPACING_SM, SPACING_XS, STROKE_THICK};

use super::widgets::{text_pill, tinted_pill};
use crate::explain_palette::Palette;
use crate::review_figures::{
    Block, Failure, Figure, FigureBody, FigureView, Figured, Figures, fit,
};
use crate::review_walk::{CodeWalk, ExplainerPage, SourceText};

/// What the review pane shows under the record's summary.
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

/// The explainer source `source` as a page filling the rest of the pane: its
/// front matter's title, src and facts as a header, then the Markdown, with
/// `headway:` refs drawn as their chips.
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
            let front = &page.front;
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
            let figured = Page {
                palette: &page.palette,
                figures: &mut page.figures,
            };
            figured_ui(ui, theme, app_ctx, None, figured, &page.body);
        });
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
            figured_ui(ui, theme, app_ctx, None, page, &note.body);
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
    walk_frame(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let page = Page { palette, figures };
        figured_ui(ui, theme, app_ctx, txn, page, &note.body);
    });
}

/// The page a body's figures belong to: the palette they're drawn in and
/// their loads.
struct Page<'a> {
    palette: &'a Palette,
    figures: &'a mut Figures,
}

/// `body`, block by block: its runs of Markdown with refs resolved through
/// `txn` (or one opened here when the caller holds none; without one refs
/// draw as plain text), and its figures drawn ([`figure_ui`]).
fn figured_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    txn: Option<&nostrdb::Transaction>,
    mut page: Page<'_>,
    body: &Figured,
) {
    let opened = match txn {
        Some(_) => None,
        None => nostrdb::Transaction::new(app_ctx.ndb).ok(),
    };
    let txn = txn.or(opened.as_ref());
    for block in &body.blocks {
        match block {
            Block::Markdown(range) => {
                let mut note_ctx = app_ctx.note_context();
                notedeck_ui::markdown::render_parsed_markdown(
                    &body.md.elements[range.clone()],
                    None,
                    &body.md.source,
                    Some(&mut note_ctx),
                    txn,
                    ui,
                );
            }
            Block::Figure(figure) => figure_ui(ui, theme, app_ctx, txn, &mut page, figure),
        }
    }
}

/// A figure, centred in a thin frame at no more than its own size or the
/// pane's width, its caption small and muted under it. A figure that can't
/// show draws a placeholder saying why; one loading, a spinner.
fn figure_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    txn: Option<&nostrdb::Transaction>,
    page: &mut Page<'_>,
    figure: &Figure,
) {
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
                    figured_ui(ui, theme, app_ctx, txn, inner_page, inner);
                });
            }
            FigureBody::Image { alt, src } => {
                let ctx = ui.ctx().clone();
                let view = match page
                    .figures
                    .image(&ctx, page.palette, figure.index, dark, src)
                {
                    Some(view) => view,
                    None => match app_ctx.img_cache.latest_texture(
                        app_ctx.media_jobs.sender(),
                        ui,
                        src,
                        ImageType::Content(None),
                        AnimationMode::NoAnimation,
                    ) {
                        Some(texture) => {
                            let [w, h] = texture.size();
                            FigureView::Ready {
                                texture,
                                size: egui::vec2(w as f32, h as f32),
                            }
                        }
                        None => FigureView::Pending,
                    },
                };
                figure_view_ui(ui, theme, view, available, Some(alt.as_str()), src);
            }
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
            let size = fit(size, available);
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
