//! The harness every Cashflow UI test builds on: a real [`Notedeck`] host in a
//! throwaway data dir, driving one [`CashflowApp`] through `update` and
//! `render` the way the chrome does.
//!
//! [`synthetic_books`] is the journal every view's tests draw: made-up
//! accounts, payees and amounts, never real data. Later cards extend this
//! harness rather than each suite growing its own.
//!
//! An app handed its books with `set_books` never touches disk. One that
//! loads its own ([`JournalFixture`], [`journal_harness`]) reads a journal
//! written to a temp `$CASHFLOW_DIR`, and the vault of [`account`], which
//! the host is logged in as.

#![allow(dead_code)]

use std::path::Path;
use std::time::{Duration, Instant};

use cashflow::manifest::{self, Manifest};
use cashflow::{Books, CashflowDir, Vault, kinds};
use egui_kittest::Harness;
use ledgerdb::{Date, Edits, Journal, JournalStore, LayerFilter, SegmentHash, SegmentHeader};
use nostrdb::Filter;
use nostrdb_net::FullKeypair;
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

/// Write `text` (ledger-cli) to `store` as one segment of `layer`.
fn write_segment(store: &JournalStore, layer: &str, text: &str) -> SegmentHash {
    let header = SegmentHeader {
        source: layer.to_owned(),
        imported: 1,
        layer: layer.to_owned(),
    };
    store
        .write_segment(&header, &Journal::parse(text).journal)
        .expect("write the synthetic segment")
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
        write_segment(&store, segment.layer, &segment.text);
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

/// The secret of the account a journal harness's host is logged in as,
/// whose vault the app reads.
const ACCOUNT_SECRET: [u8; 32] = [7u8; 32];

/// The account a journal harness's host is logged in as.
pub fn account() -> FullKeypair {
    FullKeypair::from_secret_bytes(&ACCOUNT_SECRET).expect("a valid secret")
}

/// A `$CASHFLOW_DIR` in a temp dir, for an app that loads its own books.
pub struct JournalFixture {
    /// Deleted with the fixture.
    tmp: tempfile::TempDir,
}

impl JournalFixture {
    /// An empty data dir: no journal, no `config.toml`.
    pub fn empty() -> Self {
        JournalFixture {
            tmp: tempfile::TempDir::new().expect("tmpdir"),
        }
    }

    /// A data dir whose journal holds [`synthetic_journal`] as one segment.
    pub fn synthetic() -> Self {
        let fixture = Self::empty();
        fixture.write(&synthetic_journal());
        fixture
    }

    /// The data dir.
    pub fn dir(&self) -> CashflowDir {
        CashflowDir::new(self.tmp.path())
    }

    /// Add `text` (ledger-cli) to the journal as one segment, as an import
    /// or a sync from another machine would.
    pub fn write(&self, text: &str) -> SegmentHash {
        write_segment(&cashflow::journal::store(&self.dir()), "synthetic", text)
    }

    /// Give `config.toml` an rsync transport, so the app offers a sync.
    /// Nothing is run unless a test clicks it.
    pub fn with_transport(self) -> Self {
        std::fs::write(
            self.dir().config_file(),
            "[[sync.transport]]\nkind = \"rsync\"\nremote = \"bigbox:cashflow/journal\"\n",
        )
        .expect("write config.toml");
        self
    }

    /// A fresh app on [`TODAY`] loading this data dir.
    pub fn app(&self) -> CashflowApp {
        journal_app(self.tmp.path())
    }
}

/// A fresh app on [`TODAY`] loading the data dir at `dir`.
pub fn journal_app(dir: &Path) -> CashflowApp {
    let mut app = CashflowApp::with_journal(CashflowDir::new(dir));
    app.set_today(today());
    app
}

/// A data dir no machine has, so a pane that prints the journal's path
/// draws the same one in every snapshot. Loading it finds no segments.
pub const ABSENT_DIR: &str = "/nonexistent/cashflow";

/// Publish a manifest in [`account`]'s vault for a segment no local journal
/// has, as if `host` had imported it, and wait until nostrdb has it.
pub fn publish_missing_segment(state: &mut CashflowTestState, host: &str) -> SegmentHash {
    let app_ctx = state.notedeck.app_context();
    let ndb = &*app_ctx.ndb;
    let vault = Vault::new(&ACCOUNT_SECRET).expect("a vault");
    vault.register(ndb);
    let hash = SegmentHash::of(b"a segment only bigbox has");
    let body = Manifest {
        source: "bank",
        layer: "banks",
        imported: 1_700_000_000,
        first: None,
        last: None,
        txns: 12,
        prices: 0,
        bytes: 900,
        host,
        retired: false,
    };
    let landed = ndb
        .subscribe(&[Filter::new().kinds([kinds::SEGMENT as u64]).build()])
        .expect("subscribe");
    manifest::write(&vault, ndb, hash, &body, 0).expect("write the manifest");
    let wait = ndb.wait_for_all_notes_within(landed, 1, INGEST_TIMEOUT);
    block_on(wait).expect("the manifest landed");
    hash
}

/// How long a journal test waits for nostrdb or the job pool.
const INGEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Run `future` to completion on a throwaway runtime.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime")
        .block_on(future)
}

/// Step `harness` until `done` holds of its state, or fail after
/// [`INGEST_TIMEOUT`]. For an app whose results arrive from the job pool,
/// which `Harness::run`'s few steps don't wait for.
pub fn step_until(
    harness: &mut Harness<'_, CashflowTestState>,
    what: &str,
    done: impl Fn(&CashflowTestState) -> bool,
) {
    let deadline = Instant::now() + INGEST_TIMEOUT;
    while !done(harness.state()) {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        harness.step();
    }
    // One more frame, so what arrived is drawn.
    harness.step();
}

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
    /// Skip the app's `update`, so it renders but loads nothing: what a
    /// journal harness is built with, since building runs frames until
    /// the harness settles and the load would finish meanwhile.
    hold_updates: bool,
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
        if !state.hold_updates {
            state.app.update(&mut app_ctx);
        }
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
    let mut notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);
    {
        let account = account();
        let mut app_ctx = notedeck.app_context();
        // The response asks to fetch the account's profile, which a host
        // that dials no relay has no use for.
        let _ = app_ctx
            .accounts
            .add_account(nostrdb_net::Keypair::from_secret(
                account.secret_key.clone(),
            ));
        app_ctx.select_account(&account.pubkey);
    }
    CashflowTestState {
        notedeck,
        app,
        tmpdir,
        setup_done: false,
        hold_updates: false,
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

/// A harness of `size` running `app`, which loads its own books, built
/// with its `update` held back and then given one: the first load has
/// started and nothing has come back. With `snapshot`, on the software
/// renderer.
pub fn journal_harness(
    app: CashflowApp,
    size: egui::Vec2,
    snapshot: bool,
) -> Harness<'static, CashflowTestState> {
    let mut builder = Harness::builder().with_size(size);
    if snapshot {
        builder = builder.renderer(notedeck::software_renderer());
    }
    let state = CashflowTestState {
        hold_updates: true,
        ..test_state(app)
    };
    let mut harness = builder.build_ui_state(render_cashflow, state);
    // One frame with `update`: it starts the load, whose result only the
    // next `update` can take.
    harness.state_mut().hold_updates = false;
    harness.step();
    assert!(harness.state().app.journal().is_some_and(|j| j.is_busy()));
    harness
}

/// [`journal_harness`], stepped until the first load's books are in.
pub fn loaded_harness(
    app: CashflowApp,
    size: egui::Vec2,
    snapshot: bool,
) -> Harness<'static, CashflowTestState> {
    let mut harness = journal_harness(app, size, snapshot);
    step_until(&mut harness, "the books load", |s| s.app.books().is_some());
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

/// What a card's export holds for the triage tests: coffees and groceries
/// nothing has categorized, an airline, the card payment arriving, and a
/// bookshop the importer already knew. Made up.
pub const TRIAGE_CARD: &str = "\
2024-10-02 * SQ *COFFEE SHOP #12
  ; id: card:1
  liabilities:card  -4.50 CAD
  expenses:unknown

2024-10-03 * CORNER GROCER 1
  ; id: card:2
  liabilities:card  -84.20 CAD
  expenses:unknown

2024-10-05 * SQ *COFFEE SHOP #9
  ; id: card:3
  liabilities:card  -5.25 CAD
  expenses:unknown

2024-10-06 * Bookshop
  ; id: card:4
  liabilities:card  -38.00 CAD
  expenses:shopping

2024-10-07 * Airline tickets
  ; id: card:5
  liabilities:card  -412.00 CAD
  expenses:unknown

2024-10-09 * SQ *COFFEE SHOP #31
  ; id: card:6
  liabilities:card  -4.75 CAD
  expenses:unknown

2024-10-11 * Corner Grocer 2
  ; id: card:7
  liabilities:card  -61.10 CAD
  expenses:unknown

2024-10-14 * Payment received
  ; id: card:8
  liabilities:card  600.00 CAD
  income:unknown
";

/// What a bank's download holds for the triage tests: pay, rent, the card
/// payment leaving, and an e-transfer nothing has categorized. Made up.
const TRIAGE_BANK: &str = "\
2024-10-01 * Acme Corp payroll
  ; id: bank:1
  assets:checking  4200.00 CAD
  income:unknown

2024-10-04 * Landlord
  ; id: bank:2
  assets:checking  -1800.00 CAD
  expenses:housing:rent

2024-10-10 * Mystery e-transfer
  ; id: bank:3
  assets:checking  -45.00 CAD
  expenses:unknown

2024-10-12 * Card payment
  ; id: bank:4
  assets:checking  -600.00 CAD
  expenses:unknown
";

/// The chart labels the triage app draws with: rent and shopping get a
/// name and an icon.
pub fn triage_labels() -> cashflow::chart::Labels {
    use cashflow::Stored;
    use cashflow::chart::{AccountMeta, Chart};

    let meta = |id, name, icon| Stored {
        id,
        created_at: 1,
        body: AccountMeta {
            name: Some(name),
            icon: Some(icon),
            ..AccountMeta::default()
        },
    };
    Chart {
        accounts: vec![
            meta("expenses:housing:rent", "Rent", "🏠"),
            meta("expenses:shopping", "Shopping", "🛍"),
        ],
        aliases: Vec::new(),
    }
    .labels()
}

/// A fresh app on [`TODAY`] showing the Transactions view over the triage
/// journals ([`TRIAGE_CARD`] as source `card`, [`TRIAGE_BANK`] as `bank`),
/// derived with the default counter accounts and no vault edits, its
/// triage worked out from the raw ledger so the card payment is
/// suggested as a transfer.
pub fn triage_app() -> CashflowApp {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let store = JournalStore::new(dir.path());
    write_segment(&store, "card", TRIAGE_CARD);
    write_segment(&store, "bank", TRIAGE_BANK);
    loaded_app(&store)
}

/// A fresh app on [`TODAY`] showing the Transactions view over `store`'s
/// segments, derived and triaged as [`triage_app`]'s are.
pub fn loaded_app(store: &JournalStore) -> CashflowApp {
    use std::sync::Arc;

    let loaded = ledgerdb::Ledger::load(store, LayerFilter::All).expect("load the raw ledger");
    let edits = cashflow::rules::default_edits();
    let books = Books::from_raw(&loaded.ledger, loaded.report, &edits, None, "CAD");
    let inputs = Arc::new(cashflow::DeriveInputs::from_edits(edits));
    let triage = cashflow::triage::Triage::new(Some(&loaded.ledger), &books, inputs, 0);
    let mut app = CashflowApp::new();
    app.set_today(today());
    app.set_loaded(notedeck_cashflow::journal::Loaded { books, triage });
    app.set_labels(triage_labels());
    app.set_view(notedeck_cashflow::View::Transactions);
    app
}

/// A journal store in a temp dir, deleted with it.
pub struct TempJournal {
    /// The store.
    pub store: JournalStore,
    /// Where it is.
    _dir: tempfile::TempDir,
}

/// A journal holding `count` made-up card transactions, three a day up to
/// [`TODAY`], every fifth one uncategorized: enough rows that the register
/// has to scroll.
pub fn long_register(count: usize) -> TempJournal {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let store = JournalStore::new(dir.path());
    let payees = [
        "Corner Grocer",
        "Fuel Stop",
        "Sushi Bar",
        "Bookshop",
        "Pharmacy",
    ];
    let accounts = [
        "expenses:food:groceries",
        "expenses:transport:fuel",
        "expenses:food:restaurant",
        "expenses:shopping",
        "expenses:unknown",
    ];
    let start = today().days() - (count as u32 / 3);
    let mut text = String::new();
    for i in 0..count {
        let date = Date::from_days(start + i as u32 / 3);
        let cents = 500 + (i * 137) % 9000;
        text.push_str(&format!(
            "{date} * {} #{i}\n  ; id: long:{i}\n  liabilities:card  -{}.{:02} CAD\n  {}\n\n",
            payees[i % 5],
            cents / 100,
            cents % 100,
            accounts[i % 5],
        ));
    }
    write_segment(&store, "card", &text);
    TempJournal { store, _dir: dir }
}

/// Register [`account`]'s vault root with the host's nostrdb, so the notes
/// the app writes unwrap there, and subscribe to every cashflow vault kind:
/// call before the click that writes, then [`wait_vault`].
pub fn watch_vault(state: &mut CashflowTestState) -> nostrdb::Subscription {
    let app_ctx = state.notedeck.app_context();
    let ndb = &*app_ctx.ndb;
    Vault::new(&ACCOUNT_SECRET).expect("a vault").register(ndb);
    ndb.subscribe(&[Filter::new().kinds(kinds::ALL.map(u64::from)).build()])
        .expect("subscribe")
}

/// Wait until `notes` vault records landed on `sub`, then return the
/// newest revision of every one of [`account`]'s records of `kind`, by
/// `d`.
pub fn wait_vault(
    state: &mut CashflowTestState,
    sub: nostrdb::Subscription,
    notes: u32,
    kind: u32,
) -> Vec<VaultNote> {
    let app_ctx = state.notedeck.app_context();
    let ndb = &*app_ctx.ndb;
    let wait = ndb.wait_for_all_notes_within(sub, notes, INGEST_TIMEOUT);
    block_on(wait).expect("the vault notes landed");
    let txn = nostrdb::Transaction::new(ndb).expect("txn");
    let filter = Filter::new()
        .authors([account().pubkey.bytes()])
        .kinds([u64::from(kind)])
        .build();
    let mut found: Vec<VaultNote> = nostrdb_net::query_replaceable(ndb, &txn, &[filter])
        .into_iter()
        .filter_map(|key| ndb.get_note_by_key(&txn, key).ok())
        .map(|note| {
            let tag = |name: &str| {
                note.tags()
                    .iter()
                    .find(|t| t.count() >= 2 && t.get_str(0) == Some(name))
                    .and_then(|t| t.get_str(1).map(str::to_owned))
            };
            VaultNote {
                d: tag("d").unwrap_or_default(),
                cleared: tag("cleared").is_some(),
                account: tag("account"),
            }
        })
        .collect();
    found.sort_by(|a, b| a.d.cmp(&b.d));
    found
}

/// One vault record as [`wait_vault`] read it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultNote {
    /// Its `d` tag.
    pub d: String,
    /// Whether this revision clears it.
    pub cleared: bool,
    /// Its `account` tag, for a rule or an override.
    pub account: Option<String>,
}
