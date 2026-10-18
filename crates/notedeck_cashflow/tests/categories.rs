//! The Categories view: top spending and earning, and drilling down the
//! chart of accounts, over the synthetic journal in `common`.
//!
//! The model and behavioural tests run under a plain `cargo test`. The
//! snapshots are `#[ignore]`d and run via `scripts/snapshot-test` (lavapipe).

mod common;

use common::{DESKTOP, MOBILE, harness, snapshot_harness, synthetic_app, synthetic_books, today};
use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use notedeck_cashflow::views::categories::model::{Breakdown, Flow, Level, RowKind, breakdown};
use notedeck_cashflow::{CashflowApp, Period, View};

/// The breakdown of `parent` (a full account name) in `flow` over `period`
/// on [`common::TODAY`], eight rows deep.
fn level(parent: &str, flow: Flow, period: Period) -> Breakdown {
    let books = synthetic_books();
    let accounts = &books.ledger().journal().accounts;
    breakdown(Level {
        books: &books,
        home: books.home().expect("CAD is the home commodity"),
        parent: accounts.lookup(parent).expect("the account exists"),
        flow,
        period: period.resolve(today()),
        limit: 8,
    })
}

/// Each row's label, in order.
fn labels(breakdown: &Breakdown) -> Vec<&str> {
    breakdown.rows.iter().map(|r| r.label.as_str()).collect()
}

/// The root ranks the eight largest categories this month and sends the
/// rest to "Other", so the rows sum to the total and the shares to 100%.
#[test]
fn root_ranks_the_top_eight_and_buckets_the_tail() {
    let root = level("expenses", Flow::Spending, Period::ThisMonth);
    assert_eq!(
        labels(&root),
        [
            "housing",
            "food",
            "utilities",
            "travel",
            "transport",
            "fun",
            "shopping",
            "health",
            "Other"
        ]
    );
    // gifts 28.00 + subscriptions 16.99.
    let other = root.rows.last().unwrap();
    assert_eq!(other.kind, RowKind::Other);
    assert_eq!(other.amount, 4499);
    assert_eq!(root.total, 274148);
    assert_eq!(root.rows.iter().map(|r| r.amount).sum::<i128>(), root.total);
    let shares: f64 = root.rows.iter().map(|r| r.share).sum();
    assert!((shares - 1.0).abs() < 1e-9, "shares sum to {shares}");
}

/// A row carries its change from the same days of last month: food was
/// 287.35 over September 1st to 17th and is 336.30 this month.
#[test]
fn rows_compare_with_the_same_days_last_month() {
    let root = level("expenses", Flow::Spending, Period::ThisMonth);
    let food = root.rows.iter().find(|r| r.label == "food").unwrap();
    assert_eq!(food.amount, 33630);
    assert_eq!(food.previous, Some(28735));
    let delta = food.delta_text.as_deref().unwrap();
    assert!(delta.starts_with('+') && delta.contains("48.95"), "{delta}");
    assert!(delta.ends_with("(+17%)"), "{delta}");

    let all_time = level("expenses", Flow::Spending, Period::AllTime);
    assert!(all_time.rows.iter().all(|r| r.delta_text.is_none()));
}

/// One level down, the parent's own postings get a row of their own, so the
/// level still sums to the parent's bar.
#[test]
fn drilled_level_keeps_the_parents_own_postings() {
    let food = level("expenses:food", Flow::Spending, Period::ThisMonth);
    assert_eq!(labels(&food), ["groceries", "restaurant", "food (direct)"]);
    assert_eq!(food.rows[2].kind, RowKind::Direct);
    assert_eq!(food.rows[2].amount, 650);
    assert_eq!(food.total, 33630);
    assert!(matches!(
        food.rows[0].kind,
        RowKind::Account { leaf: true, .. }
    ));
    assert!(matches!(
        food.rows[1].kind,
        RowKind::Account { leaf: false, .. }
    ));
}

/// A leaf account lists its payees, largest first.
#[test]
fn leaf_lists_its_payees() {
    let groceries = level("expenses:food:groceries", Flow::Spending, Period::ThisMonth);
    assert!(groceries.payees);
    assert_eq!(
        labels(&groceries),
        ["Big Box Market", "Corner Grocer", "Farmers Market"]
    );
    assert_eq!(groceries.total, 25150);
    // Farmers Market first appears this month.
    assert_eq!(groceries.rows[2].previous, Some(0));
    assert!(
        groceries.rows[2]
            .delta_text
            .as_deref()
            .unwrap()
            .ends_with("(new)")
    );
}

/// Income postings are negative in the ledger; the Income column shows them
/// as positive earnings.
#[test]
fn income_reads_as_positive() {
    let income = level("income", Flow::Earning, Period::ThisMonth);
    assert_eq!(labels(&income), ["salary", "freelance", "interest"]);
    assert_eq!(income.total, 606234);
    assert!(income.rows.iter().all(|r| r.amount > 0));
}

/// Clicking a category drills into it, a leaf shows its payees, and the
/// breadcrumb walks back to the root.
#[test]
fn clicking_drills_down_and_the_breadcrumb_walks_back() {
    let mut app = synthetic_app();
    app.set_view(View::Categories);
    let mut harness = harness(app, DESKTOP);

    harness.get_by_role_and_label(Role::Button, "food").click();
    harness.run();
    assert!(harness.query_by_label("groceries").is_some());
    assert!(harness.query_by_label("housing").is_none());

    harness
        .get_by_role_and_label(Role::Button, "groceries")
        .click();
    harness.run();
    assert!(harness.query_by_label("Big Box Market").is_some());
    // The crumbs back up: Expenses › food › groceries.
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "food")
            .is_some()
    );

    harness
        .get_by_role_and_label(Role::Button, "Expenses")
        .click();
    harness.run();
    assert!(harness.query_by_label("housing").is_some());
    assert!(harness.query_by_label("Big Box Market").is_none());
    // The other column never moved.
    assert!(harness.query_by_label("salary").is_some());
}

/// Picking a period in the view sets the period every view shares, and the
/// bars follow it.
#[test]
fn period_picker_sets_the_shared_period() {
    let mut app = synthetic_app();
    app.set_view(View::Categories);
    let mut harness = harness(app, DESKTOP);
    // Travel was only booked in October.
    assert!(harness.query_by_label("travel").is_some());

    harness.get_by_label("Last month").click();
    harness.run();

    assert_eq!(harness.state().app.period(), Period::LastMonth);
    assert!(harness.query_by_label("travel").is_none());
    assert!(harness.query_by_label("housing").is_some());
}

/// Hovering a bar shows its amount, its change and what it is measured
/// against.
#[test]
fn hovering_a_bar_shows_its_change() {
    let mut app = synthetic_app();
    app.set_view(View::Categories);
    let mut harness = harness(app, DESKTOP);

    harness.get_by_role_and_label(Role::Button, "food").hover();
    harness.run();

    assert!(harness.query_by_label("expenses:food").is_some());
    assert!(
        harness
            .query_by_label("vs the same days last month")
            .is_some()
    );
    assert!(harness.query_by_label_contains("48.95").is_some());
}

/// With no books loaded the view says so rather than drawing empty columns.
#[test]
fn no_books_says_so() {
    let mut app = CashflowApp::new();
    app.set_view(View::Categories);
    let harness = harness(app, DESKTOP);
    assert!(harness.query_by_label("No journal loaded yet.").is_some());
}

/// The root level of both columns, side by side.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_categories_root() {
    let mut app = synthetic_app();
    app.set_view(View::Categories);
    let mut harness = snapshot_harness(app, DESKTOP);
    harness.snapshot("cashflow_categories_root");
}

/// Expenses drilled into food: its sub-categories, its own postings, and the
/// breadcrumb.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_categories_drilled() {
    let mut app = synthetic_app();
    app.set_view(View::Categories);
    let mut harness = snapshot_harness(app, DESKTOP);
    harness.get_by_role_and_label(Role::Button, "food").click();
    harness.run();
    harness.snapshot("cashflow_categories_drilled");
}

/// Expenses drilled to the groceries leaf, which lists its payees.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_categories_leaf_payees() {
    let mut app = synthetic_app();
    app.set_view(View::Categories);
    let mut harness = snapshot_harness(app, DESKTOP);
    harness.get_by_role_and_label(Role::Button, "food").click();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "groceries")
        .click();
    harness.run();
    harness.snapshot("cashflow_categories_leaf_payees");
}

/// The tooltip over the food bar.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_categories_tooltip() {
    let mut app = synthetic_app();
    app.set_view(View::Categories);
    let mut harness = snapshot_harness(app, DESKTOP);
    harness.get_by_role_and_label(Role::Button, "food").hover();
    harness.run();
    harness.snapshot("cashflow_categories_tooltip");
}

/// On a phone the columns stack.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_categories_mobile() {
    let mut app = synthetic_app();
    app.set_view(View::Categories);
    let mut harness = snapshot_harness(app, MOBILE);
    harness.snapshot("cashflow_categories_mobile");
}
