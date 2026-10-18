//! One module per [`View`], and the dispatch from a view to its module.
//!
//! Every view has the same shape — `pub(crate) fn ui(cx: &mut ViewCx<'_>, ui)`
//! — so the router is one `match` and a new view is a new module plus an arm
//! here.

use cashflow::Books;
use egui::RichText;
use ledgerdb::Date;

use crate::{Period, View, ui::palette::Palette};

mod adventure;
pub mod categories;
mod goals;
mod overview;
mod projects;
mod settings;
mod transactions;
pub mod trends;

/// What a view draws from. Every view takes this one struct, so handing views
/// something new (the host's `AppContext`) is a field here, not a new
/// parameter on every view.
pub struct ViewCx<'a> {
    /// The colors to draw with.
    pub palette: &'a Palette,
    /// The books, once some are loaded.
    pub ledger: Option<LedgerCx<'a>>,
    /// The date [`Self::period`] is resolved against.
    pub today: Date,
    /// The span of days every view reports on, shared so picking one in a
    /// view picks it in all of them.
    pub period: &'a mut Period,
    /// What each view keeps between frames.
    pub states: &'a mut ViewStates,
}

/// The loaded books, and which load they came from.
#[derive(Clone, Copy)]
pub struct LedgerCx<'a> {
    /// The books.
    pub books: &'a Books,
    /// Changes whenever the books are replaced, so a view knows its cached
    /// results are stale.
    pub generation: u64,
}

/// What each view keeps between frames: its cached results and where the
/// user is in it. One field per view that needs one.
#[derive(Default)]
pub struct ViewStates {
    /// The Categories view's drill-down and its cached breakdowns.
    pub(crate) categories: categories::CategoriesState,
    /// The Trends view's span, picks and cached series.
    pub(crate) trends: trends::TrendsState,
}

/// Draw `view` into `ui`.
#[profiling::function]
pub fn show(view: View, cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    match view {
        View::Overview => overview::ui(cx, ui),
        View::Categories => categories::ui(cx, ui),
        View::Trends => trends::ui(cx, ui),
        View::Transactions => transactions::ui(cx, ui),
        View::Projects => projects::ui(cx, ui),
        View::Goals => goals::ui(cx, ui),
        View::Adventure => adventure::ui(cx, ui),
        View::Settings => settings::ui(cx, ui),
    }
}

/// A view that has no content yet: its heading and one line on what it will
/// show.
fn placeholder(cx: &ViewCx<'_>, ui: &mut egui::Ui, view: View, what: &'static str) {
    ui.label(RichText::new(view.label()).heading().color(cx.palette.text));
    ui.add_space(8.0);
    ui.label(RichText::new(what).color(cx.palette.text_muted));
}
