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
//! Whoever loads them reads [`CashflowApp::layers`] for the layers to load
//! and tells the app what the journal is missing with
//! [`CashflowApp::set_missing_segments`].

use cashflow::Books;
use ledgerdb::Date;
use notedeck::{AppContext, AppResponse};

pub mod layers;
pub mod money;
pub mod period;
pub mod ui;
mod view;
pub mod views;

pub use layers::Layers;
pub use period::Period;
pub use view::View;

use ui::palette::Palette;

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
            generation: 0,
            today: period::today(),
            period: Period::default(),
            layers: Layers::default(),
            missing_segments: 0,
            views: views::ViewStates::default(),
        }
    }

    /// Report on `books` from now on. Every view recomputes what it shows
    /// from them on its next frame.
    pub fn set_books(&mut self, books: Books) {
        self.books = Some(books);
        self.generation += 1;
    }

    /// The books the app reports on, if any are loaded.
    pub fn books(&self) -> Option<&Books> {
        self.books.as_ref()
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

    /// The journal layers switched on and off. Reload the books with
    /// [`Layers::filter`] when [`Layers::generation`] moves.
    pub fn layers(&self) -> &Layers {
        &self.layers
    }

    /// Say that the vault lists `count` segments the local journal does
    /// not have, so every figure is partial until they arrive.
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
                let mut cx = views::ViewCx {
                    palette,
                    ledger: books.as_ref().map(|books| views::LedgerCx {
                        books,
                        generation: *generation,
                    }),
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
    fn render(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.ui(ui);
        AppResponse::none()
    }
}
