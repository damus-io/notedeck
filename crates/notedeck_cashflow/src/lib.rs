//! Cashflow — a personal finance dashboard for Notedeck.
//!
//! The ledger itself lives outside the UI: `crates/ledgerdb` is the journal
//! engine and `crates/cashflow` the importers, rules and nostr vault. This crate
//! only draws them.
//!
//! It is laid out to host more than one app's worth of views. [`View`] is the
//! router: one variant per screen, and one module under [`views`] per variant.
//! Each view draws from a [`views::ViewCx`], the one struct every view takes,
//! so a later card that hands views the ledger adds a field there rather than
//! changing eight signatures. The colors every view uses come from the
//! [`ui::palette::Palette`] held in [`CashflowApp`] (never a global), which is
//! the seam a second theme plugs into.
//!
//! The app reports on the [`cashflow::Books`] handed to it with
//! [`CashflowApp::set_books`], over the one [`Period`] every view shares,
//! with accounts put in projects by [`CashflowApp::set_projects`].
//! In the chrome they come from a [`journal::JournalSource`]
//! ([`CashflowApp::from_env`]), which loads `$CASHFLOW_DIR`'s journal off the
//! UI thread and hands the app new books whenever a segment arrives or an
//! edit lands; until the first load, or while the journal is empty or
//! unreadable, the shell draws that state in place of every view. It loads
//! the layers [`CashflowApp::layers`] leaves on, and tells the app what the
//! journal is missing with [`CashflowApp::set_missing_segments`].
//!
//! Books always come with their [`Triage`], what the Transactions view's
//! inbox burns down. A view never writes to the vault itself: it queues a
//! [`triage::Action`], and [`CashflowApp`] makes the write in `render`,
//! where it has the host's nostrdb and account. The write lands in the
//! vault, the journal source sees the edits move and re-derives, and the
//! new triage arrives a few frames later.

use std::sync::Arc;
use std::time::Duration;

use cashflow::chart::Labels;
use cashflow::projects::Projects;
use cashflow::triage::{self, Action, Triage};
use cashflow::{Books, CashflowDir, DeriveInputs, Vault};
use ledgerdb::Date;
use notedeck::{AppContext, AppResponse};

pub mod journal;
pub mod layers;
pub mod money;
pub mod period;
pub mod ui;
mod view;
pub mod views;

pub use layers::Layers;
pub use period::Period;
pub use view::View;

use journal::{JournalSource, JournalState, Loaded};
use ui::palette::Palette;

/// How soon to poll again while a load or a sync is running.
const BUSY_REPAINT: Duration = Duration::from_millis(50);

/// The Cashflow app: which view is showing, the books it reports on, and the
/// palette it is drawn in.
pub struct CashflowApp {
    /// The view the router shows in the content pane.
    view: View,
    /// The semantic colors every view draws with, re-derived from the host's
    /// `ColorTheme` when the light/dark mode flips (see [`Palette::refresh`]).
    palette: Palette,
    /// The loaded books, once there are some.
    books: Option<Books>,
    /// What is left to categorize in [`Self::books`], set with them.
    triage: Option<Triage>,
    /// The chart's display names and icons.
    labels: Arc<Labels>,
    /// Vault writes the views asked for this frame, made in `render`.
    outbox: Vec<Action>,
    /// Where the books come from, when the app loads its own: `None` for an
    /// app handed its books with [`Self::set_books`].
    journal: Option<JournalSource>,
    /// [`ui::journal_status::summary`] of the books, written when they
    /// change.
    summary: String,
    /// Which project each account is in.
    projects: Projects,
    /// Bumped every time [`Self::books`] or [`Self::projects`] is replaced,
    /// so a view's cached results know to recompute.
    generation: u64,
    /// The date periods are resolved against.
    today: Date,
    /// The span of days every view reports on.
    period: Period,
    /// The journal layers switched off.
    layers: Layers,
    /// Segments the vault says exist that the local journal lacks.
    missing_segments: usize,
    /// What each view keeps between frames.
    views: views::ViewStates,
}

impl Default for CashflowApp {
    fn default() -> Self {
        Self::new()
    }
}

impl CashflowApp {
    /// A fresh app showing [`View::Overview`], with no books yet and today's
    /// date from the system clock.
    pub fn new() -> Self {
        Self {
            view: View::Overview,
            palette: Palette::default(),
            books: None,
            triage: None,
            labels: Arc::new(Labels::default()),
            outbox: Vec::new(),
            journal: None,
            summary: String::new(),
            projects: Projects::default(),
            generation: 0,
            today: period::today(),
            period: Period::default(),
            layers: Layers::default(),
            missing_segments: 0,
            views: views::ViewStates::default(),
        }
    }

    /// An app that loads the journal in `dir` and the selected account's
    /// vault, and keeps both current.
    pub fn with_journal(dir: CashflowDir) -> Self {
        Self {
            journal: Some(JournalSource::new(dir)),
            ..Self::new()
        }
    }

    /// [`Self::with_journal`] over `$CASHFLOW_DIR`, the directory the
    /// `cashflow` CLI writes. With no data dir to default to, an app with
    /// no books.
    pub fn from_env() -> Self {
        match CashflowDir::from_env() {
            Ok(dir) => Self::with_journal(dir),
            Err(err) => {
                tracing::error!("cashflow: no journal to load: {err}");
                Self::new()
            }
        }
    }

    /// Report on `books` from now on, derived with no vault edits but the
    /// default counter accounts. Their triage is worked out here, on the
    /// calling thread, without transfer suggestions (it has no raw
    /// ledger); [`Self::set_loaded`] takes one worked out elsewhere.
    pub fn set_books(&mut self, books: Books) {
        let inputs = Arc::new(DeriveInputs::from_edits(cashflow::rules::default_edits()));
        let since = period::start_of_day(self.today);
        let triage = Triage::new(None, &books, inputs, since);
        self.set_loaded(Loaded { books, triage });
    }

    /// Report on `loaded`'s books and triage from now on. Every view
    /// recomputes what it shows from them on its next frame.
    pub fn set_loaded(&mut self, loaded: Loaded) {
        let load_time = self.journal.as_ref().and_then(JournalSource::load_time);
        self.summary = ui::journal_status::summary(&loaded.books, load_time);
        self.books = Some(loaded.books);
        self.triage = Some(loaded.triage);
        self.generation += 1;
    }

    /// Attribute accounts to projects by `projects` from now on: the chart
    /// of accounts' attribution ([`cashflow::chart::Chart::projects`]).
    /// Every view recomputes on its next frame.
    pub fn set_projects(&mut self, projects: Projects) {
        if self.projects == projects {
            return;
        }
        self.projects = projects;
        self.generation += 1;
    }

    /// Draw accounts with `labels`' names and icons. The journal source
    /// sets them from the vault's chart.
    pub fn set_labels(&mut self, labels: Labels) {
        self.labels = Arc::new(labels);
        self.generation += 1;
    }

    /// The triage of the books, if any are loaded.
    pub fn triage(&self) -> Option<&Triage> {
        self.triage.as_ref()
    }

    /// The books the app reports on, if any are loaded.
    pub fn books(&self) -> Option<&Books> {
        self.books.as_ref()
    }

    /// The journal source, when the app loads its own books.
    pub fn journal(&self) -> Option<&JournalSource> {
        self.journal.as_ref()
    }

    /// Resolve periods against `today` instead of the system clock's date,
    /// so a test gets the same ranges on every run.
    pub fn set_today(&mut self, today: Date) {
        self.today = today;
    }

    /// The span of days the views report on.
    pub fn period(&self) -> Period {
        self.period
    }

    /// Report on `period` from now on.
    pub fn set_period(&mut self, period: Period) {
        self.period = period;
    }

    /// The journal layers switched on and off. The journal source reloads
    /// with [`Layers::filter`] when [`Layers::generation`] moves; whoever
    /// hands the app its books instead does the same.
    pub fn layers(&self) -> &Layers {
        &self.layers
    }

    /// Say that the vault lists `count` segments the local journal does
    /// not have, so every figure is partial until they arrive. The journal
    /// source sets it every frame.
    pub fn set_missing_segments(&mut self, count: usize) {
        self.missing_segments = count;
    }

    /// The view currently showing.
    pub fn view(&self) -> View {
        self.view
    }

    /// Switch the router to `view`.
    pub fn set_view(&mut self, view: View) {
        self.view = view;
    }

    /// The palette the app draws with this frame.
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Draw the whole app — the view nav and the routed view — into `ui`.
    ///
    /// Takes no [`AppContext`] yet: the shell reads nothing from the host but
    /// its theme. A view that needs the host gets it through
    /// [`views::ViewCx`].
    #[profiling::function]
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.palette.refresh(ui.ctx());

        let narrow = notedeck::ui::is_narrow(ui.ctx());
        let picked = ui::nav::show(ui, &self.palette, self.view, narrow);
        if let Some(view) = picked {
            self.view = view;
        }

        let Self {
            view,
            palette,
            books,
            triage,
            labels,
            outbox,
            journal,
            summary,
            projects,
            generation,
            today,
            period,
            layers,
            missing_segments,
            views: states,
        } = self;
        egui::CentralPanel::default()
            .frame(ui::panel_frame(palette).inner_margin(egui::Margin::symmetric(24, 16)))
            .show(ui, |ui| {
                if let Some(journal) = journal {
                    let delay = if journal.is_busy() {
                        BUSY_REPAINT
                    } else {
                        journal::RESCAN_EVERY
                    };
                    ui.ctx().request_repaint_after(delay);
                    match journal.state(books.is_some()) {
                        JournalState::Loading => {
                            return ui::journal_status::loading_ui(ui, palette);
                        }
                        JournalState::Error => {
                            return ui::journal_status::error_ui(ui, palette, journal);
                        }
                        JournalState::Empty => {
                            return ui::journal_status::empty_ui(ui, palette, journal);
                        }
                        JournalState::Ready => ui::journal_status::banner_ui(ui, palette, journal),
                    }
                }
                let mut cx = views::ViewCx {
                    palette,
                    ledger: books.as_ref().zip(triage.as_ref()).map(|(books, triage)| {
                        views::LedgerCx {
                            books,
                            triage,
                            generation: *generation,
                        }
                    }),
                    labels,
                    outbox,
                    summary,
                    projects,
                    today: *today,
                    period,
                    layers,
                    missing_segments: *missing_segments,
                    states,
                    open: None,
                };
                views::show(*view, &mut cx, ui);
                if let Some(open) = cx.open {
                    *view = open;
                }
            });
    }

    /// Make the vault writes the views queued this frame, as the selected
    /// account. A write that fails is reported back to the Transactions
    /// view, which shows what it had hidden again.
    fn flush_writes(&mut self, ctx: &mut AppContext<'_>) {
        if self.outbox.is_empty() {
            return;
        }
        let vault = ctx
            .accounts
            .selected_filled()
            .and_then(|full| Vault::new(&full.secret_key.secret_bytes()));
        for action in self.outbox.drain(..) {
            let result = match &vault {
                Some(vault) => triage::apply(vault, ctx.ndb, &action).map_err(|e| e.to_string()),
                None => {
                    Err("this account has no secret key, so it can't write to the vault".to_owned())
                }
            };
            if let Err(err) = result {
                tracing::warn!("cashflow: {action:?} failed: {err}");
                self.views.write_failed(err);
            }
        }
    }
}

impl notedeck::App for CashflowApp {
    /// Poll the journal source, and take any books it finished deriving.
    fn update(&mut self, ctx: &mut AppContext<'_>) {
        let Some(journal) = self.journal.as_mut() else {
            return;
        };
        let since = period::start_of_day(self.today);
        let loaded = journal.update(ctx, &self.layers, self.books.as_ref(), since);
        self.missing_segments = journal.missing().hashes.len();
        if !Arc::ptr_eq(&self.labels, journal.labels()) {
            self.labels = journal.labels().clone();
            self.generation += 1;
        }
        if let Some(loaded) = loaded {
            self.set_loaded(loaded);
        }
    }

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.ui(ui);
        self.flush_writes(ctx);
        AppResponse::none()
    }
}
