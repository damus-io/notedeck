//! A review record's explainer sources in the review pane (see
//! [`review_walk`](crate::review_walk)): the explainer's Markdown as a tab
//! beside the diff, and the code walk's notes threaded into the diff — above
//! the hunks they introduce, or at its top when they introduce none.

use headway::event::{ReviewSourceRole, ReviewView};
use md_stream::ParsedMarkdown;
use notedeck::ColorTheme;
use notedeck::tokens::{SPACING_SM, SPACING_XS};

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

/// The Diff / Explainer switch, drawn only when `record` has an explainer
/// source. Returns the tab to show: always [`ReviewTab::Diff`] for a record
/// without one, whatever was picked on another.
pub(super) fn review_tabs_ui(
    ui: &mut egui::Ui,
    record: Option<&ReviewView>,
    tab: &mut ReviewTab,
) -> ReviewTab {
    let has_explainer = record.is_some_and(|r| r.source(ReviewSourceRole::Explainer).is_some());
    if !has_explainer {
        return ReviewTab::Diff;
    }
    ui.horizontal(|ui| {
        ui.selectable_value(tab, ReviewTab::Diff, "Diff");
        ui.selectable_value(tab, ReviewTab::Explainer, "Explainer source");
    });
    ui.add_space(SPACING_XS);
    *tab
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
/// they scroll away with it.
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
        if let Some(missing) = &note.missing {
            tinted_pill(ui, missing, theme.warning);
        }
        markdown_ui(ui, app_ctx, None, &note.body);
        ui.add_space(SPACING_XS);
    }
    ui.add_space(SPACING_SM);
}

/// Walk note `key` where the diff hands it its rows, above its hunk.
pub(super) fn walk_note_ui(
    ui: &mut egui::Ui,
    app_ctx: &mut notedeck::AppContext,
    txn: Option<&nostrdb::Transaction>,
    walk: &CodeWalk,
    key: usize,
) {
    if let Some(note) = walk.notes.get(key) {
        markdown_ui(ui, app_ctx, txn, &note.body);
    }
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
