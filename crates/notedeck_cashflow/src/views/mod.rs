//! One module per [`View`], and the dispatch from a view to its module.
//!
//! Every view has the same shape — `pub(crate) fn ui(cx: &mut ViewCx<'_>, ui)`
//! — so the router is one `match` and a new view is a new module plus an arm
//! here.

use cashflow::Books;
use cashflow::chart::Labels;
use cashflow::triage::{Action, Triage};
use ledgerdb::Date;

use crate::{Layers, Period, View, ui::palette::Palette};

pub mod adventure;
pub mod categories;
pub mod goals;
pub mod overview;
pub mod projects;
pub mod settings;
pub mod transactions;
pub mod trends;

/// What a view draws from. Every view takes this one struct, so handing views
/// something new (the host's `AppContext`) is a field here, not a new
/// parameter on every view.
pub struct ViewCx<'a> {
    /// The colors to draw with.
    pub palette: &'a Palette,
    /// The books, once some are loaded.
    pub ledger: Option<LedgerCx<'a>>,
    /// The chart's display names and icons for accounts.
    pub labels: &'a Labels,
    /// Vault writes to make once this frame is drawn: a view queues what
    /// the person asked for, and the app writes it with the host's account.
    pub outbox: &'a mut Vec<Action>,
    /// One line on what the books hold and how long they took to load
    /// ([`crate::ui::journal_status::summary`]); empty with no books.
    pub summary: &'a str,
    /// Where the books come from, when the app loads its own: the journal's
    /// state and its sync. `None` for an app handed its books.
    pub journal: Option<&'a mut crate::journal::JournalSource>,
    /// Which project each account is in, from the chart of accounts. A
    /// change bumps [`LedgerCx::generation`] too.
    pub projects: &'a cashflow::projects::Projects,
    /// Which Headway boards each project links, and their progress.
    pub boards: &'a crate::boards::Boards,
    /// The date [`Self::period`] is resolved against.
    pub today: Date,
    /// The span of days every view reports on, shared so picking one in a
    /// view picks it in all of them.
    pub period: &'a mut Period,
    /// The journal layers switched on and off.
    pub layers: &'a mut Layers,
    /// Segments the vault lists that the local journal lacks.
    pub missing_segments: usize,
    /// What each view keeps between frames.
    pub states: &'a mut ViewStates,
    /// The view to switch to once this frame is drawn, set by a view that
    /// links to another.
    pub open: Option<View>,
    /// The goals on the Headway boards.
    pub goals: GoalsCx<'a>,
    /// A `headway:<board>/<word-id>` card to open in notedeck once this
    /// frame is drawn, set by a view that links to one.
    pub open_card: Option<String>,
}

/// The goals found on the Headway boards, and which find they came from.
#[derive(Clone, Copy)]
pub struct GoalsCx<'a> {
    /// The goals, in the order they were found.
    pub goals: &'a [cashflow::goals::Goal],
    /// Changes whenever the goals are replaced, so a view knows its cached
    /// results are stale.
    pub generation: u64,
    /// The board slugs goals are looked for on, for the empty state.
    pub boards: &'a [String],
}

/// The loaded books, their triage, and which load they came from.
#[derive(Clone, Copy)]
pub struct LedgerCx<'a> {
    /// The books.
    pub books: &'a Books,
    /// Changes whenever the books (or the project attribution) are
    /// replaced, so a view knows its cached results are stale.
    /// What is left to categorize in them.
    pub triage: &'a Triage,
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
    /// The Overview's cached figures.
    pub(crate) overview: overview::OverviewState,
    /// The Projects view's cached figures and the project open.
    pub(crate) projects: projects::ProjectsState,
    /// The register's filters and rows, and the triage inbox.
    pub(crate) transactions: transactions::TransactionsState,
    /// The Settings view's cached journal status.
    pub(crate) settings: settings::SettingsState,
    /// The goals measured, and the goal open; the Overview's strip reads
    /// it too.
    pub(crate) goals: goals::GoalsState,
    /// The Adventure's fold.
    pub(crate) adventure: adventure::AdventureState,
}

impl ViewStates {
    /// A vault write a view queued failed with `err`: say so, and show
    /// again whatever was hidden waiting for it.
    pub(crate) fn write_failed(&mut self, err: String) {
        self.transactions.write_failed(err);
    }
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
