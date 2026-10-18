//! The harness every Cashflow UI test builds on: a real [`Notedeck`] host in a
//! throwaway data dir, driving one [`CashflowApp`] through `update` and
//! `render` the way the chrome does.
//!
//! [`synthetic_books`] is the journal every view's tests draw: made-up
//! accounts, payees and amounts, never real data. Later cards extend this
//! harness (an edits vault in the host's nostrdb) rather than each suite
//! growing its own.

#![allow(dead_code)]

use cashflow::Books;
use egui_kittest::Harness;
use ledgerdb::{Date, Edits, Journal, JournalStore, LayerFilter, SegmentHeader};
use notedeck::{App, Notedeck};
use notedeck_cashflow::CashflowApp;

/// The date [`synthetic_books`] tests run on: mid-October 2024, so "This
/// month" is half a month and has September's first half to compare with.
pub const TODAY: &str = "2024-10-17";

/// One posting of the synthetic journal: a payee paid from (or into)
/// checking, on a day, to an account.
struct Entry {
    date: &'static str,
    payee: &'static str,
    account: &'static str,
    amount: &'static str,
}

/// The synthetic journal's postings, each balanced against
/// `assets:checking`. Ten expense categories, so one tail goes to "Other";
/// payees repeat in `expenses:food:groceries`; one posting goes straight to
/// `expenses:food`; September and October so deltas have something to
/// compare.
const ENTRIES: &[Entry] = &[
    // September, first half.
    e("2024-09-01", "Landlord", "expenses:housing:rent", "1800.00"),
    e(
        "2024-09-02",
        "Corner Grocer",
        "expenses:food:groceries",
        "84.20",
    ),
    e(
        "2024-09-04",
        "Golden Arches",
        "expenses:food:restaurant:burgers",
        "12.40",
    ),
    e(
        "2024-09-05",
        "Transit Card",
        "expenses:transport:transit",
        "50.00",
    ),
    e(
        "2024-09-06",
        "Big Box Market",
        "expenses:food:groceries",
        "142.75",
    ),
    e(
        "2024-09-08",
        "Sushi Bar",
        "expenses:food:restaurant:sushi",
        "48.00",
    ),
    e("2024-09-10", "Netstream", "expenses:subscriptions", "16.99"),
    e(
        "2024-09-12",
        "Power Co",
        "expenses:utilities:power",
        "92.10",
    ),
    e("2024-09-15", "Acme Corp", "income:salary", "-4200.00"),
    e(
        "2024-09-16",
        "Client A",
        "income:freelance:client-a",
        "-900.00",
    ),
    // September, second half.
    e(
        "2024-09-20",
        "Fuel Stop",
        "expenses:transport:fuel",
        "61.30",
    ),
    e("2024-09-25", "Bookshop", "expenses:shopping", "38.00"),
    e("2024-09-30", "Acme Corp", "income:salary", "-4200.00"),
    // October, through the 17th.
    e("2024-10-01", "Landlord", "expenses:housing:rent", "1800.00"),
    e(
        "2024-10-02",
        "Corner Grocer",
        "expenses:food:groceries",
        "96.40",
    ),
    e(
        "2024-10-03",
        "Golden Arches",
        "expenses:food:restaurant:burgers",
        "15.80",
    ),
    e("2024-10-03", "Snack Stand", "expenses:food", "6.50"),
    e(
        "2024-10-04",
        "Big Box Market",
        "expenses:food:groceries",
        "121.10",
    ),
    e(
        "2024-10-05",
        "Farmers Market",
        "expenses:food:groceries",
        "34.00",
    ),
    e(
        "2024-10-06",
        "Sushi Bar",
        "expenses:food:restaurant:sushi",
        "62.50",
    ),
    e(
        "2024-10-07",
        "Fuel Stop",
        "expenses:transport:fuel",
        "58.90",
    ),
    e(
        "2024-10-08",
        "Transit Card",
        "expenses:transport:transit",
        "50.00",
    ),
    e("2024-10-09", "Netstream", "expenses:subscriptions", "16.99"),
    e(
        "2024-10-10",
        "Power Co",
        "expenses:utilities:power",
        "88.45",
    ),
    e(
        "2024-10-10",
        "Fibre Net",
        "expenses:utilities:internet",
        "70.00",
    ),
    e("2024-10-11", "Game Store", "expenses:fun:games", "79.99"),
    e("2024-10-12", "Cinema", "expenses:fun:movies", "24.00"),
    e("2024-10-12", "Pharmacy", "expenses:health", "31.25"),
    e("2024-10-13", "Bookshop", "expenses:shopping", "45.60"),
    e("2024-10-14", "Florist", "expenses:gifts", "28.00"),
    e("2024-10-14", "Railway", "expenses:travel", "112.00"),
    e("2024-10-15", "Acme Corp", "income:salary", "-4200.00"),
    e(
        "2024-10-15",
        "Client A",
        "income:freelance:client-a",
        "-650.00",
    ),
    e(
        "2024-10-16",
        "Client B",
        "income:freelance:client-b",
        "-1200.00",
    ),
    e("2024-10-16", "Bank", "income:interest", "-12.34"),
];

/// An [`Entry`], short enough to keep the table one row per posting.
const fn e(
    date: &'static str,
    payee: &'static str,
    account: &'static str,
    amount: &'static str,
) -> Entry {
    Entry {
        date,
        payee,
        account,
        amount,
    }
}

/// The synthetic journal, as ledger-cli text.
pub fn synthetic_journal() -> String {
    let mut text = String::new();
    for (at, entry) in ENTRIES.iter().enumerate() {
        text.push_str(&format!(
            "{} * {}\n  ; id: synth:{at}\n  {}  {} CAD\n  assets:checking\n\n",
            entry.date, entry.payee, entry.account, entry.amount
        ));
    }
    text
}

/// `text` (ledger-cli) loaded as books in CAD, through the same pipeline the
/// app and the CLI run: written to a journal store as one segment, loaded
/// and derived with no edits.
pub fn books_from(text: &str) -> Books {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let store = JournalStore::new(dir.path());
    let header = SegmentHeader {
        source: "synthetic".to_owned(),
        imported: 1,
        layer: "synthetic".to_owned(),
    };
    store
        .write_segment(&header, &Journal::parse(text).journal)
        .expect("write the synthetic segment");
    Books::load(
        &store,
        LayerFilter::All,
        &Default::default(),
        &Edits::default(),
        "CAD",
    )
    .expect("load the books")
}

/// [`synthetic_journal`], loaded.
pub fn synthetic_books() -> Books {
    books_from(&synthetic_journal())
}

/// [`TODAY`] as a date.
pub fn today() -> Date {
    Date::parse(TODAY).expect("TODAY is a date")
}

/// A fresh app on [`TODAY`] reporting on [`synthetic_books`].
pub fn synthetic_app() -> CashflowApp {
    let mut app = CashflowApp::new();
    app.set_today(today());
    app.set_books(synthetic_books());
    app
}

/// A desktop window, wide enough for the nav rail.
pub const DESKTOP: egui::Vec2 = egui::Vec2::new(1200.0, 800.0);

/// A phone window, below `notedeck::ui::is_narrow`'s breakpoint, so the nav
/// collapses to a tab strip.
pub const MOBILE: egui::Vec2 = egui::Vec2::new(400.0, 800.0);

/// What a Cashflow harness holds between frames.
pub struct CashflowTestState {
    /// The host: nostrdb, accounts, job pool, all in [`Self::tmpdir`].
    pub notedeck: Notedeck,
    /// The app under test.
    pub app: CashflowApp,
    /// The host's data dir; dropped (and deleted) with the harness.
    pub tmpdir: tempfile::TempDir,
    /// Whether the first frame has installed notedeck's fonts and styles.
    setup_done: bool,
}

/// One frame: install notedeck's look on the first, then `update` and
/// `render` the app on every later one.
fn render_cashflow(ui: &mut egui::Ui, state: &mut CashflowTestState) {
    notedeck::test_harness::full_window(ui, |ui| {
        if !state.setup_done {
            let ctx = ui.ctx().clone();
            state.notedeck.setup(&ctx);
            ctx.global_style_mut(|s| s.animation_time = 0.0);
            state.setup_done = true;
            // Fonts installed during a frame apply from the next one.
            ctx.request_repaint();
            return;
        }

        let mut app_ctx = state.notedeck.app_context();
        state.app.update(&mut app_ctx);
        // Edge to edge, as the chrome hands an app its `Ui`.
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                state.app.render(&mut app_ctx, ui);
            });
    });
}

/// A booted host in a fresh data dir, running `app`.
pub fn test_state(app: CashflowApp) -> CashflowTestState {
    let tmpdir = tempfile::TempDir::new().expect("tmpdir");
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);
    CashflowTestState {
        notedeck,
        app,
        tmpdir,
        setup_done: false,
    }
}

/// A harness of `size` running `app`, with no renderer: for behavioural tests
/// that query and click, and run under a plain `cargo test`.
pub fn harness(app: CashflowApp, size: egui::Vec2) -> Harness<'static, CashflowTestState> {
    let mut harness = Harness::builder()
        .with_size(size)
        .build_ui_state(render_cashflow, test_state(app));
    harness.run();
    harness
}

/// A harness of `size` running `app` on the software renderer, for
/// snapshots. Needs lavapipe: run its tests via `scripts/snapshot-test`.
pub fn snapshot_harness(app: CashflowApp, size: egui::Vec2) -> Harness<'static, CashflowTestState> {
    let mut harness = Harness::builder()
        .with_size(size)
        .renderer(notedeck::software_renderer())
        .build_ui_state(render_cashflow, test_state(app));
    harness.run();
    harness
}
