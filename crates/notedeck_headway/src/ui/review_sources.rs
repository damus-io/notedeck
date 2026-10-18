//! A review record's explainer sources in the review pane (see
//! [`review_walk`](crate::review_walk)): the explainer's Markdown as a tab
//! beside the diff, and the code walk's notes threaded into the diff — above
//! the hunks they introduce, or at its top when they introduce none.

use headway::event::{ReviewSourceRole, ReviewView};
use md_stream::ParsedMarkdown;
use notedeck::ColorTheme;
use notedeck::tokens::{RADIUS_MD, SPACING_MD, SPACING_SM, SPACING_XS, STROKE_THICK};

use super::widgets::{text_pill, tinted_pill};
use crate::review_walk::{CodeWalk, ExplainerPage};

/// What the review pane shows under the record's summary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReviewTab {
    /// The commit's diff, with the code walk threaded in.
    #[default]
    Diff,
    /// The record's explainer source, rendered.
    Explainer,
}

/// The pane's explainer page, parsed once per source: kept on the
/// [`ReviewUi`](super::review::ReviewUi) across frames.
#[derive(Default)]
pub(crate) struct ExplainerCache {
    page: Option<ExplainerPage>,
}

impl ExplainerCache {
    /// `record`'s explainer page, parsed now if it changed since the last
    /// frame; `None` when the record has no explainer source.
    fn page(&mut self, record: &ReviewView) -> Option<&ExplainerPage> {
        let Some(source) = record.source(ReviewSourceRole::Explainer) else {
            self.page = None;
            return None;
        };
        if self.page.as_ref().map(|p| p.source) != Some(source.id) {
            self.page = Some(ExplainerPage::new(source));
        }
        self.page.as_ref()
    }
}

/// Whether `record` has an explainer source, so the pane offers its tab.
pub(super) fn has_explainer(record: Option<&ReviewView>) -> bool {
    record.is_some_and(|r| r.source(ReviewSourceRole::Explainer).is_some())
}

/// The tab to show for `record`: the one picked, but always
/// [`ReviewTab::Diff`] for a record without an explainer, whatever was picked
/// on another.
pub(super) fn shown_tab(record: Option<&ReviewView>, tab: ReviewTab) -> ReviewTab {
    if has_explainer(record) {
        tab
    } else {
        ReviewTab::Diff
    }
}

/// The Diff / Explainer switch as Linear's view tabs: plain text, the shown
/// tab in primary text over an accent underline, the other muted. Each is
/// `height` tall, so the underline lands on the bottom of the row the caller
/// rules off under them.
pub(super) fn review_tabs_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    tab: &mut ReviewTab,
    height: f32,
) {
    let tabs = [
        (ReviewTab::Diff, "Diff"),
        (ReviewTab::Explainer, "Explainer source"),
    ];
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

/// `record`'s explainer source as a page filling the rest of the pane: its
/// front matter's title, src and facts as a header, then the Markdown, with
/// `headway:` refs drawn as their chips.
pub(super) fn explainer_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    cache: &mut ExplainerCache,
    record: &ReviewView,
) {
    let Some(page) = cache.page(record) else {
        return;
    };
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
            markdown_ui(ui, app_ctx, None, &page.body);
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
    walk: &CodeWalk,
) {
    if walk.top().next().is_none() {
        return;
    }
    for note in walk.top() {
        let bar = ui.visuals().hyperlink_color;
        let shown = walk_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(missing) = &note.missing {
                tinted_pill(ui, missing, theme.warning);
                ui.add_space(SPACING_XS);
            }
            markdown_ui(ui, app_ctx, None, &note.body);
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
    app_ctx: &mut notedeck::AppContext,
    txn: Option<&nostrdb::Transaction>,
    walk: &CodeWalk,
    key: usize,
) {
    let Some(note) = walk.notes.get(key) else {
        return;
    };
    walk_frame(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        markdown_ui(ui, app_ctx, txn, &note.body);
    });
}

/// `md`, with refs resolved through `txn`, or one opened here when the caller
/// holds none. Without a transaction refs draw as plain text.
fn markdown_ui(
    ui: &mut egui::Ui,
    app_ctx: &mut notedeck::AppContext,
    txn: Option<&nostrdb::Transaction>,
    md: &ParsedMarkdown,
) {
    let mut note_ctx = app_ctx.note_context();
    let opened = match txn {
        Some(_) => None,
        None => nostrdb::Transaction::new(note_ctx.ndb).ok(),
    };
    let txn = txn.or(opened.as_ref());
    notedeck_ui::markdown::render_parsed_markdown(
        &md.elements,
        None,
        &md.source,
        Some(&mut note_ctx),
        txn,
        ui,
    );
}
