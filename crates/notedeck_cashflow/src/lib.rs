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
//! [`CashflowApp::set_books`], over the one [`Period`] every view shares.
//! In the chrome they come from a [`journal::JournalSource`]
//! ([`CashflowApp::from_env`]), which loads `$CASHFLOW_DIR`'s journal off the
//! UI thread and hands the app new books whenever a segment arrives or an
//! edit lands; until the first load, or while the journal is empty or
//! unreadable, the shell draws that state in place of every view. It loads
//! the layers [`CashflowApp::layers`] leaves on, and tells the app what the
//! journal is missing with [`CashflowApp::set_missing_segments`].

use std::time::Duration;

use cashflow::{Books, CashflowDir};
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

use journal::{JournalSource, JournalState};
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
    /// Where the books come from, when the app loads its own: `None` for an
    /// app handed its books with [`Self::set_books`].
    journal: Option<JournalSource>,
    /// [`ui::journal_status::summary`] of the books, written when they
    /// change.
    summary: String,
    /// Bumped every time [`Self::books`] is replaced, so a view's cached
    /// results know to recompute.
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
            journal: None,
            summary: String::new(),
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

    /// Report on `books` from now on. Every view recomputes what it shows
    /// from them on its next frame.
    pub fn set_books(&mut self, books: Books) {
        let load_time = self.journal.as_ref().and_then(JournalSource::load_time);
        self.summary = ui::journal_status::summary(&books, load_time);
        self.books = Some(books);
        self.generation += 1;
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
            journal,
            summary,
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
                    ledger: books.as_ref().map(|books| views::LedgerCx {
                        books,
                        generation: *generation,
                    }),
                    summary,
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
}

impl notedeck::App for CashflowApp {
    /// Poll the journal source, and take any books it finished deriving.
    fn update(&mut self, ctx: &mut AppContext<'_>) {
        let Some(journal) = self.journal.as_mut() else {
            return;
        };
        let books = journal.update(ctx, &self.layers, self.books.as_ref());
        self.missing_segments = journal.missing().hashes.len();
        if let Some(books) = books {
            self.set_books(books);
        }
    }

    fn render(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.ui(ui);
        AppResponse::none()
    }
}
