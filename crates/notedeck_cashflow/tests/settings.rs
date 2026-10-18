//! The Settings view: layer switches that follow the account through the
//! vault, the home currency, the journal's sources with a stale-source
//! warning, and the configured sync transports.
//!
//! The model and behavioural tests run under a plain `cargo test`. The
//! snapshots are `#[ignore]`d and run via `scripts/snapshot-test` (lavapipe).

mod common;

use std::time::Duration;

use cashflow::settings::LayerSwitch;
use cashflow::{Books, Settings, Vault, kinds};
use common::{DESKTOP, JournalFixture, MOBILE, harness, loaded_harness, snapshot_harness, today};
use egui_kittest::kittest::Queryable;
use ledgerdb::{Date, Edits, Journal, JournalStore, LayerFilter, SegmentHeader};
use nostrdb::{Filter, Transaction};
use notedeck_cashflow::views::settings::{STALE_AFTER_DAYS, model};
use notedeck_cashflow::{CashflowApp, View};

/// One import: which source and layer it came from, the day it ran, and a
/// transaction dated that day.
struct Import {
    source: &'static str,
    layer: &'static str,
    day: &'static str,
}

/// Amex last imported in August, two months before [`common::TODAY`]; RBC
/// and CLN this month; CLN's on-chain moves in their own layer, off by
/// default. Every payee and amount is made up.
const IMPORTS: &[Import] = &[
    Import {
        source: "amex",
        layer: "amex",
        day: "2024-07-30",
    },
    Import {
        source: "amex",
        layer: "amex",
        day: "2024-08-01",
    },
    Import {
        source: "rbc",
        layer: "rbc",
        day: "2024-10-02",
    },
    Import {
        source: "cln",
        layer: "cln",
        day: "2024-10-15",
    },
    Import {
        source: "cln",
        layer: "cln-chain",
        day: "2024-10-15",
    },
];

/// [`IMPORTS`] written as segments and loaded as books.
fn books() -> Books {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let store = JournalStore::new(dir.path());
    for (at, import) in IMPORTS.iter().enumerate() {
        let day = Date::parse(import.day).expect("a date");
        let header = SegmentHeader {
            source: import.source.to_owned(),
            imported: u64::from(day.days()) * 86_400 + 3_600,
            layer: import.layer.to_owned(),
        };
        let text = format!(
            "{} * Shop\n  ; id: {}:{at}\n  expenses:shop  12.00 CAD\n  assets:checking\n",
            import.day, import.source
        );
        store
            .write_segment(&header, &Journal::parse(&text).journal)
            .expect("write a segment");
    }
    Books::load(
        &store,
        LayerFilter::All,
        &Default::default(),
        &Edits::default(),
        "CAD",
    )
    .expect("load the books")
}

/// An app on [`common::TODAY`] showing Settings over [`books`].
fn app() -> CashflowApp {
    let mut app = CashflowApp::new();
    app.set_today(today());
    app.set_books(books());
    app.set_view(View::Settings);
    app
}

/// Every layer and source is listed once, and only the source whose last
/// import is more than [`STALE_AFTER_DAYS`] old is flagged.
#[test]
fn a_source_not_imported_in_five_weeks_is_stale() {
    let model = model(&books(), today());
    let layers: Vec<&str> = model.layers.iter().map(|l| l.label.as_str()).collect();
    assert_eq!(
        layers,
        [
            "amex · 2 segments",
            "cln · 1 segment",
            "cln-chain · 1 segment",
            "rbc · 1 segment"
        ]
    );
    let stale: Vec<(&str, Option<&str>)> = model
        .sources
        .iter()
        .map(|s| (s.source.as_str(), s.stale.as_deref()))
        .collect();
    assert_eq!(
        stale,
        [
            ("amex", Some("amex not imported since 2024-08-01")),
            ("cln", None),
            ("rbc", None),
        ]
    );
    assert_eq!(
        model.sources[0].detail,
        "2 segments · last import 2024-08-01"
    );
    assert!(model.problems.is_empty());

    let on_the_line = today().days() - STALE_AFTER_DAYS;
    let fresh = model_with_amex_on(Date::from_days(on_the_line));
    assert!(fresh.sources[0].stale.is_none(), "exactly 35 days is fine");
}

/// [`model`] over one amex import on `day`, as of today.
fn model_with_amex_on(day: Date) -> notedeck_cashflow::views::settings::Model {
    let dir = tempfile::TempDir::new().expect("tmpdir");
    let store = JournalStore::new(dir.path());
    let header = SegmentHeader {
        source: "amex".to_owned(),
        imported: u64::from(day.days()) * 86_400,
        layer: "amex".to_owned(),
    };
    let text =
        format!("{day} * Shop\n  ; id: amex:1\n  expenses:shop  1.00 CAD\n  assets:checking\n");
    store
        .write_segment(&header, &Journal::parse(&text).journal)
        .expect("write a segment");
    let books = Books::load(
        &store,
        LayerFilter::All,
        &Default::default(),
        &Edits::default(),
        "CAD",
    )
    .expect("load");
    model(&books, today())
}

/// The pane draws the stale warning, the home currency and a toggle per
/// layer: `cln-chain` starts off, and clicking a toggle flips the layer.
#[test]
fn layers_toggle_from_settings() {
    let mut harness = harness(app(), DESKTOP);
    harness.get_by_label("amex not imported since 2024-08-01");
    harness.get_by_label("CAD");
    harness.get_by_label("This app was handed its books: there is no journal to sync.");
    assert!(!harness.state().app.layers().is_on("cln-chain"));
    assert!(harness.state().app.layers().is_on("rbc"));

    harness.get_by_label("rbc · 1 segment").click();
    harness.run();
    assert!(!harness.state().app.layers().is_on("rbc"));
    harness.get_by_label("cln-chain · 1 segment").click();
    harness.run();
    assert!(harness.state().app.layers().is_on("cln-chain"));
}

/// An app loading its own journal lists `config.toml`'s transports and
/// offers a sync; a layer switched in Settings lands in the account's
/// vault, so every machine loads the same layers.
#[test]
fn transports_are_listed_and_a_layer_switch_is_saved() {
    let fixture = JournalFixture::synthetic().with_transport();
    let mut app = fixture.app();
    app.set_view(View::Settings);
    let mut harness = loaded_harness(app, DESKTOP, false);
    harness.get_by_label("bigbox:cashflow/journal");
    harness.get_by_label("Sync now");

    let landed = {
        let ctx = harness.state_mut().notedeck.app_context();
        // As `common::publish_missing_segment` does: the test host doesn't
        // unwrap the vault's envelopes on its own.
        let secret = common::account().secret_key.secret_bytes();
        Vault::new(&secret).expect("a vault").register(ctx.ndb);
        let filter = Filter::new().kinds([kinds::SETTING as u64]).build();
        ctx.ndb.subscribe(&[filter]).expect("subscribe")
    };
    harness.get_by_label("synthetic · 1 segment").click();
    // The click lands in one frame's render; the next one's update writes.
    harness.step();
    harness.step();
    let ctx = harness.state_mut().notedeck.app_context();
    let wait = ctx
        .ndb
        .wait_for_all_notes_within(landed, 1, Duration::from_secs(30));
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime")
        .block_on(wait)
        .expect("the switch reached the vault");
    let txn = Transaction::new(ctx.ndb).expect("txn");
    let settings = Settings::load(ctx.ndb, &txn, &common::account().pubkey);
    let switches: Vec<_> = settings.layer_switches().collect();
    assert_eq!(
        switches,
        [LayerSwitch {
            layer: "synthetic",
            on: false
        }]
    );
    drop(txn);
    drop(ctx);
    assert!(!harness.state().app.layers().is_on("synthetic"));
}

/// The pane: layers with `cln-chain` off, CAD, the sources with Amex
/// flagged stale, and the no-journal sync line.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_settings() {
    let mut harness = snapshot_harness(app(), DESKTOP);
    harness.snapshot("cashflow_settings");
}

/// The pane at phone width, where the stale warning wraps under its source.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_settings_mobile() {
    let mut harness = snapshot_harness(app(), MOBILE);
    harness.snapshot("cashflow_settings_mobile");
}
