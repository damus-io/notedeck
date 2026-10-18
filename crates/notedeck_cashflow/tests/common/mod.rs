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
    books_from_segments(
        &[Segment {
            layer: "synthetic",
            text: text.to_owned(),
        }],
        &Edits::default(),
    )
}

/// One segment of a journal: its layer and its ledger-cli text.
pub struct Segment {
    /// The layer it is in.
    pub layer: &'static str,
    /// Its transactions.
    pub text: String,
}

/// `segments` written to one journal store and loaded as books in CAD,
/// derived with `edits` (whose defaults decide what counts as unknown).
pub fn books_from_segments(segments: &[Segment], edits: &Edits) -> Books {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let store = JournalStore::new(dir.path());
    for segment in segments {
        let header = SegmentHeader {
            source: segment.layer.to_owned(),
            imported: 1,
            layer: segment.layer.to_owned(),
        };
        store
            .write_segment(&header, &Journal::parse(&segment.text).journal)
            .expect("write the synthetic segment");
    }
    Books::load(&store, LayerFilter::All, &Default::default(), edits, "CAD")
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

/// The first month of [`synthetic_years_journal`].
const YEARS_FROM: (u16, u8) = (2022, 1);

/// How many months [`synthetic_years_journal`] covers: January 2022 through
/// October 2024, the month of [`TODAY`].
const YEARS_MONTHS: u32 = 34;

/// Bitcoin's made-up price in CAD on the first of each month of
/// [`synthetic_years_journal`]: a fall through 2022 and a climb after, so the
/// net worth line moves with it.
const BTC_CAD: [u32; YEARS_MONTHS as usize] = [
    52_000, 48_000, 55_000, 58_000, 40_000, 26_000, 28_000, 30_000, 25_000, 26_000, 22_000, 21_000,
    28_000, 31_000, 37_000, 39_000, 36_000, 41_000, 40_000, 38_000, 35_000, 46_000, 50_000, 58_000,
    60_000, 70_000, 94_000, 88_000, 92_000, 84_000, 86_000, 80_000, 82_000, 85_000,
];

/// A made-up journal of every month from January 2022 through [`TODAY`]:
/// rent and salary that step up each year, groceries, restaurants on a
/// credit card paid off the next month, fuel, a quarterly treat, summer and
/// winter trips (months that spend more than they earn), freelance work
/// every other month, interest, and two bitcoin buys priced monthly, so the
/// Trends view's three charts all have something to show. Nothing is dated
/// after [`TODAY`].
pub fn synthetic_years_journal() -> String {
    let today = today();
    let mut text = String::new();
    let mut id = 0;
    let mut txn = |text: &mut String, date: &str, payee: &str, postings: &str| {
        id += 1;
        text.push_str(&format!(
            "{date} * {payee}\n  ; id: years:{id}\n{postings}\n"
        ));
    };
    let cad = |cents: u64| format!("{}.{:02} CAD", cents / 100, cents % 100);

    txn(
        &mut text,
        "2022-01-01",
        "Opening balance",
        "  assets:checking  12000.00 CAD\n  equity:opening\n",
    );
    for m in 0..YEARS_MONTHS {
        let month = u32::from(YEARS_FROM.1) - 1 + m;
        let year = YEARS_FROM.0 + (month / 12) as u16;
        let mon = month % 12 + 1;
        let day = |d: u8| format!("{year}-{mon:02}-{d:02}");
        let dated = |d: u8| Date::parse(&day(d)).unwrap() <= today;
        let paid =
            |account: &str, cents: u64| format!("  {account}  {}\n  assets:checking\n", cad(cents));
        let received = |account: &str, cents: u64| {
            format!("  {account}  -{}\n  assets:checking\n", cad(cents))
        };
        let step = u64::from(year - YEARS_FROM.0);
        let m = u64::from(m);

        text.push_str(&format!(
            "P {} BTC {}.00 CAD\n\n",
            day(1),
            BTC_CAD[m as usize]
        ));
        if dated(1) {
            txn(
                &mut text,
                &day(1),
                "Landlord",
                &paid("expenses:housing:rent", 180_000 + step * 10_000),
            );
        }
        if dated(3) && m > 0 {
            // Last month's restaurant bill, paid off.
            let bill = 8_000 + (m - 1) * 5_300 % 9_000;
            txn(
                &mut text,
                &day(3),
                "Card payment",
                &paid("liabilities:card", bill),
            );
        }
        if dated(5) {
            txn(
                &mut text,
                &day(5),
                "Corner Grocer",
                &paid("expenses:food:groceries", 30_000 + m * 3_700 % 12_000),
            );
        }
        if dated(10) && (m == 2 || m == 17) {
            let (btc, price) = if m == 2 {
                ("0.05000000", 55_000)
            } else {
                ("0.10000000", 41_000)
            };
            txn(
                &mut text,
                &day(10),
                "Exchange",
                &format!("  assets:btc  {btc} BTC @ {price}.00 CAD\n  assets:checking\n"),
            );
        }
        if dated(12) {
            let bill = 8_000 + m * 5_300 % 9_000;
            txn(
                &mut text,
                &day(12),
                "Bistro",
                &format!(
                    "  expenses:food:restaurant  {}\n  liabilities:card\n",
                    cad(bill)
                ),
            );
        }
        if dated(15) {
            txn(
                &mut text,
                &day(15),
                "Acme Corp",
                &received("income:salary", 420_000 + step * 30_000),
            );
        }
        if dated(18) {
            txn(
                &mut text,
                &day(18),
                "Fuel Stop",
                &paid("expenses:transport:fuel", 12_000 + m * 2_900 % 6_000),
            );
        }
        if dated(20) && m % 3 == 0 {
            txn(
                &mut text,
                &day(20),
                "Game Store",
                &paid("expenses:fun", 25_000),
            );
        }
        if dated(22) && (mon == 7 || mon == 12) {
            txn(
                &mut text,
                &day(22),
                "Airline",
                &paid("expenses:travel", 450_000),
            );
        }
        if dated(25) && m % 2 == 1 {
            let fee = 60_000 + m * 7_100 % 90_000;
            txn(
                &mut text,
                &day(25),
                "Client A",
                &received("income:freelance", fee),
            );
        }
        if dated(28) {
            txn(
                &mut text,
                &day(28),
                "Bank",
                &received("income:interest", 500 + m * 100),
            );
        }
    }
    text
}

/// [`synthetic_years_journal`], loaded.
pub fn synthetic_years_books() -> Books {
    books_from(&synthetic_years_journal())
}

/// A fresh app on [`TODAY`] reporting on [`synthetic_years_books`].
pub fn synthetic_years_app() -> CashflowApp {
    let mut app = CashflowApp::new();
    app.set_today(today());
    app.set_books(synthetic_years_books());
    app
}
