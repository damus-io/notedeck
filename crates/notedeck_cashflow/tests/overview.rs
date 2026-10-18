//! The Overview: net cashflow, income, expenses and savings rate, their
//! deltas, the 12-month trend, the top categories and the badges, over the
//! synthetic journals in `common`.
//!
//! The model and behavioural tests run under a plain `cargo test`. The
//! snapshots are `#[ignore]`d and run via `scripts/snapshot-test` (lavapipe).

mod common;

use cashflow::Books;
use common::{
    DESKTOP, MOBILE, Segment, books_from_segments, harness, snapshot_harness, synthetic_app,
    synthetic_books, synthetic_years_journal, today,
};
use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use ledgerdb::LayerFilter;
use notedeck_cashflow::views::overview::model::{Inputs, Overview, overview};
use notedeck_cashflow::{CashflowApp, Period, View};

/// A window tall enough for the whole Overview.
const TALL: egui::Vec2 = egui::Vec2::new(1200.0, 1000.0);

/// What a lightning node adds to the years journal, in a layer of its own:
/// routing fees, a zap in msat the price db has no rate for (and that no
/// rule has said whose it was), and a payment nothing has categorized.
const CLN: &str = "\
2024-10-06 * Routed
  ; id: cln:1
  routed:in  -3.40 CAD
  assets:cln

2024-10-08 * Zap
  ; id: cln:2
  income:zap  -21000 msat
  assets:cln

2024-10-09 * Mystery merchant
  ; id: cln:3
  expenses:unknown  23.00 CAD
  assets:cln
";

/// The years journal in a `bank` layer and [`CLN`] in a `cln` one, derived
/// with the default counter accounts so the mystery payment is unknown.
fn layered_books() -> Books {
    books_from_segments(
        &[
            Segment {
                layer: "bank",
                text: synthetic_years_journal(),
            },
            Segment {
                layer: "cln",
                text: CLN.to_owned(),
            },
        ],
        &cashflow::rules::default_edits(),
    )
}

/// A fresh app on [`common::TODAY`] reporting on [`layered_books`], with
/// two segments missing so every badge shows.
fn layered_app() -> CashflowApp {
    let mut app = CashflowApp::new();
    app.set_today(today());
    app.set_books(layered_books());
    app.set_missing_segments(2);
    app
}

/// The Overview of `books` over `period` on [`common::TODAY`].
fn overview_of(books: &Books, period: Period) -> Overview {
    overview(Inputs {
        books,
        home: books.home().expect("books in CAD"),
        period,
        today: today(),
    })
}

/// This month (October 1st through the 17th) earned 6062.34, spent
/// 2741.48 and kept 55% of it.
#[test]
fn this_months_figures() {
    let books = synthetic_books();
    let o = overview_of(&books, Period::ThisMonth);
    assert_eq!(o.income_text, "6062.34 CAD");
    assert_eq!(o.expenses_text, "2741.48 CAD");
    assert_eq!(o.net_text, "3320.86 CAD");
    assert_eq!(o.savings_text, "55%");
    assert_eq!(o.routed_text, None);
    assert_eq!(o.unknown, None);
    assert_eq!(o.unpriced, None);
    assert_eq!(o.layers, ["synthetic"]);
}

/// Net is compared with September's first seventeen days (5100.00 earned,
/// 2246.44 spent), and with September's whole net (6954.26, the only
/// month of the trailing twelve the journal has) scaled to seventeen days
/// of thirty: 3940.74.
#[test]
fn deltas_against_the_period_before_and_the_average() {
    let books = synthetic_books();
    let o = overview_of(&books, Period::ThisMonth);
    let previous = o.vs_previous.expect("September is in the journal");
    assert_eq!(previous.amount, 332_086 - 285_356);
    assert_eq!(previous.text, "+467.30 CAD");
    let average = o.vs_average.expect("September is a whole month");
    assert_eq!(average.amount, 332_086 - 695_426 * 17 / 30);
    assert_eq!(average.text, "-619.88 CAD");
}

/// Last month has nothing before it in a journal that starts with it, and
/// the twelve-month average would only restate a twelve-month period.
#[test]
fn deltas_are_left_out_when_they_mean_nothing() {
    let books = synthetic_books();
    let last = overview_of(&books, Period::LastMonth);
    assert_eq!(last.net_text, "6954.26 CAD");
    assert_eq!(last.vs_previous, None);

    let year = overview_of(&books, Period::Last12Months);
    assert_eq!(year.vs_average, None);

    let all = overview_of(&books, Period::AllTime);
    assert_eq!(all.net_text, "10275.12 CAD");
    assert_eq!(all.vs_previous, None);
    assert_eq!(all.vs_average, None);
}

/// The trend is net per month of the last twelve the journal has, and the
/// top lists rank the period's categories largest first, five at most.
#[test]
fn trend_and_top_categories() {
    let books = synthetic_books();
    let o = overview_of(&books, Period::ThisMonth);
    let trend = o.trend.expect("the journal has months");
    assert_eq!(trend.window.labels, ["Sep 2024", "Oct 2024"]);
    assert_eq!(trend.net.values, [695_426.0, 332_086.0]);

    let labels = |rows: &[notedeck_cashflow::views::categories::model::Row]| {
        rows.iter().map(|r| r.label.clone()).collect::<Vec<_>>()
    };
    assert_eq!(o.top_expenses.len(), 5);
    assert_eq!(o.top_expenses[0].label, "housing");
    assert_eq!(o.top_expenses[0].amount_text, "1800.00 CAD");
    assert_eq!(labels(&o.top_income), ["salary", "freelance", "interest"]);
}

/// Routing is its own figure and never income; a zap with no price is
/// counted as unpriced; the zap and the payment are unknown, since they
/// sit on importer defaults; and each layer gets a chip.
#[test]
fn routing_unpriced_unknown_and_layers() {
    let books = layered_books();
    let o = overview_of(&books, Period::ThisMonth);
    assert_eq!(o.routed_text.as_deref(), Some("3.40 CAD"));
    assert_eq!(o.flow.routed, 340);
    assert_eq!(o.unpriced.as_deref(), Some("1 unpriced posting"));
    assert_eq!(o.unknown.as_deref(), Some("2 unknown transactions"));
    assert_eq!(o.layers, ["bank", "cln"]);
    // The unknown payment is an expense all the same.
    assert!(o.top_expenses.iter().any(|r| r.label == "unknown"));
}

/// An Overview harness of `app`, tall enough for every section.
fn overview_harness(app: CashflowApp) -> egui_kittest::Harness<'static, common::CashflowTestState> {
    harness(app, TALL)
}

/// The Overview is the view the app opens on, and shows this month.
#[test]
fn the_app_opens_on_this_months_overview() {
    let harness = overview_harness(synthetic_app());
    assert_eq!(harness.state().app.view(), View::Overview);
    assert!(harness.query_by_label("3320.86 CAD").is_some());
    assert!(harness.query_by_label("+467.30 CAD").is_some());
    assert!(harness.query_by_label("Net cashflow chart").is_some());
}

/// A period chip changes every figure.
#[test]
fn a_period_chip_changes_the_figures() {
    let mut harness = overview_harness(synthetic_app());
    harness.get_by_label("Last month").click();
    harness.run();
    assert!(harness.query_by_label("6954.26 CAD").is_some());
    assert!(harness.query_by_label("3320.86 CAD").is_none());
    assert_eq!(harness.state().app.period(), Period::LastMonth);
}

/// A top category opens the Categories view drilled into it.
#[test]
fn a_top_category_opens_categories_drilled_in() {
    let mut harness = overview_harness(synthetic_app());
    harness.get_by_role_and_label(Role::Button, "food").click();
    harness.run();
    assert_eq!(harness.state().app.view(), View::Categories);
    // Food's own children, under the crumb back up to Expenses.
    assert!(harness.query_by_label("groceries").is_some());
    assert!(harness.query_by_label("restaurant").is_some());
    assert!(harness.query_by_label("Top income").is_none());
}

/// The unknown badge opens the triage inbox, and the missing-segments
/// badge opens Settings.
#[test]
fn badges_link_to_their_views() {
    let mut harness = overview_harness(layered_app());
    harness.get_by_label("2 unknown transactions").click();
    harness.run();
    assert_eq!(harness.state().app.view(), View::Transactions);

    let mut harness = overview_harness(layered_app());
    harness.get_by_label("2 missing segments").click();
    harness.run();
    assert_eq!(harness.state().app.view(), View::Settings);
}

/// A layer chip switches its layer off for the next load, and on again.
#[test]
fn a_layer_chip_switches_its_layer() {
    let mut harness = overview_harness(layered_app());
    assert_eq!(harness.state().app.layers().filter(), LayerFilter::All);
    harness.get_by_label("cln").click();
    harness.run();
    let layers = harness.state().app.layers();
    assert_eq!(layers.filter(), LayerFilter::Except(vec!["cln".to_owned()]));
    assert_eq!(layers.generation(), 1);

    harness.get_by_label("cln").click();
    harness.run();
    assert_eq!(harness.state().app.layers().filter(), LayerFilter::All);
}

/// With no books loaded the view says so.
#[test]
fn no_books_says_so() {
    let harness = harness(CashflowApp::new(), DESKTOP);
    assert!(harness.query_by_label("No journal loaded yet.").is_some());
}

/// The wide layout over the layered journal: every figure, both deltas,
/// routing, every badge and both layer chips.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_overview_desktop() {
    let mut harness = snapshot_harness(layered_app(), TALL);
    harness.snapshot("cashflow_overview_desktop");
}

/// On a phone the tiles stack two to a row and the top lists one under
/// the other.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_overview_mobile() {
    let mut harness = snapshot_harness(layered_app(), MOBILE);
    harness.snapshot("cashflow_overview_mobile");
}
