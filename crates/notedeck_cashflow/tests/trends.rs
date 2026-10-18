//! The Trends view: income vs expenses by month, account trends and net
//! worth, over the synthetic journals in `common`.
//!
//! The model and behavioural tests run under a plain `cargo test`. The
//! snapshots are `#[ignore]`d and run via `scripts/snapshot-test` (lavapipe).

mod common;

use cashflow::Books;
use common::{
    DESKTOP, MOBILE, harness, snapshot_harness, synthetic_app, synthetic_books,
    synthetic_years_app, synthetic_years_books, today,
};
use egui::accesskit::Role;
use egui::pos2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use notedeck_cashflow::View;
use notedeck_cashflow::views::trends::model::{Span, Window, monthly, net_worth, trend, window};

/// A window tall enough for all three charts at once.
const TALL: egui::Vec2 = egui::Vec2::new(1200.0, 1240.0);

/// `span`'s window over `books` on [`common::TODAY`].
fn window_of(books: &Books, span: Span) -> Window {
    window(span, today(), books.query().span()).expect("the journal has months")
}

/// A window never starts before the journal does: twelve months of a
/// two-month journal is two months.
#[test]
fn window_is_cut_to_the_journal() {
    let books = synthetic_books();
    let year = window_of(&books, Span::Year);
    assert_eq!(year.labels, ["Sep 2024", "Oct 2024"]);

    let years = synthetic_years_books();
    assert_eq!(window_of(&years, Span::Year).len(), 12);
    assert_eq!(window_of(&years, Span::Year).labels[0], "Nov 2023");
    assert_eq!(window_of(&years, Span::ThreeYears).len(), 34);
    assert_eq!(window_of(&years, Span::AllTime).labels[0], "Jan 2022");
}

/// Each month's income reads positive, and net is income less expenses:
/// September earned 9300.00 and spent 2345.74, October (through the 17th)
/// earned 6062.34 and spent 2741.48.
#[test]
fn monthly_income_expenses_and_net() {
    let books = synthetic_books();
    let home = books.home().unwrap();
    let m = monthly(&books, home, &window_of(&books, Span::Year));
    assert_eq!(m.income.values, [930_000.0, 606_234.0]);
    assert_eq!(m.expenses.values, [234_574.0, 274_148.0]);
    assert_eq!(m.net.values, [695_426.0, 332_086.0]);
    assert_eq!(m.net.texts[1], "3320.86 CAD");
    assert_eq!(m.income_total, "15362.34 CAD");
    assert!(m.unconverted.is_none());
}

/// A month with a trip spends more than it earns, so its net is negative.
#[test]
fn a_trip_month_nets_negative() {
    let books = synthetic_years_books();
    let home = books.home().unwrap();
    let w = window_of(&books, Span::AllTime);
    let m = monthly(&books, home, &w);
    let july = w.labels.iter().position(|l| l == "Jul 2022").unwrap();
    assert!(m.net.values[july] < 0.0, "{}", m.net.texts[july]);
    let june = july - 1;
    assert!(m.net.values[june] > 0.0, "{}", m.net.texts[june]);
}

/// An income account's trend is flipped to read positive; an expense
/// account's is not.
#[test]
fn trends_read_positive() {
    let books = synthetic_years_books();
    let home = books.home().unwrap();
    let w = window_of(&books, Span::Year);
    let accounts = &books.ledger().journal().accounts;

    let salary = trend(&books, home, accounts.lookup("income:salary").unwrap(), &w);
    assert!(salary.flipped);
    // 2024's salary, every month of it.
    assert_eq!(salary.line.values.last(), Some(&480_000.0));
    assert!(salary.line.values.iter().all(|v| *v > 0.0));

    let rent = trend(
        &books,
        home,
        accounts.lookup("expenses:housing").unwrap(),
        &w,
    );
    assert!(!rent.flipped);
    assert_eq!(rent.line.values[0], 190_000.0);
}

/// Net worth values the bitcoin at each month-end's price, so it moves
/// with the price; "home currency only" leaves it out.
#[test]
fn net_worth_values_bitcoin_at_each_month_end() {
    let books = synthetic_years_books();
    let home = books.home().unwrap();
    let w = window_of(&books, Span::AllTime);
    let with = net_worth(&books, home, &w, false);
    let without = net_worth(&books, home, &w, true);
    assert_eq!(with.others, ["BTC"]);
    assert_eq!(without.others, ["BTC"]);
    assert!(with.unconverted.is_none());

    // Before the first buy (March 2022) the two lines agree.
    assert_eq!(with.line.values[0], without.line.values[0]);
    // At the end of May 2022, 0.05 BTC priced at May 1st's 40000.00.
    let may = 4;
    assert_eq!(with.line.values[may] - without.line.values[may], 200_000.0);
    // At the end of June it is June 1st's 26000.00: the price fell.
    assert_eq!(
        with.line.values[may + 1] - without.line.values[may + 1],
        130_000.0
    );
}

/// Today's net worth is checking plus the bitcoin less what is owed on the
/// card, and the card's balance counts against it.
#[test]
fn net_worth_counts_liabilities() {
    let books = synthetic_books();
    let home = books.home().unwrap();
    let w = window_of(&books, Span::Year);
    let nw = net_worth(&books, home, &w, false);
    assert!(nw.others.is_empty());
    // Everything earned less everything spent, from checking alone.
    assert_eq!(nw.line.values, [695_426.0, 695_426.0 + 332_086.0]);
    assert_eq!(nw.latest, "10275.12 CAD");
}

/// The Trends view of `app`, in a behavioural harness tall enough for every
/// chart.
fn trends_harness(
    app: notedeck_cashflow::CashflowApp,
) -> Harness<'static, common::CashflowTestState> {
    let mut app = app;
    app.set_view(View::Trends);
    harness(app, TALL)
}

/// Type `text` into the account picker.
fn search(harness: &mut Harness<'static, common::CashflowTestState>, text: &str) {
    harness.get_by_role(Role::TextInput).focus();
    harness.run();
    harness.get_by_role(Role::TextInput).type_text(text);
    harness.run();
}

/// Typing in the picker lists matching accounts; clicking one adds its
/// chip and line, and the chip's × removes it.
#[test]
fn picking_an_account_charts_it() {
    let mut harness = trends_harness(synthetic_app());
    assert!(harness.query_by_label("Account trends chart").is_none());

    search(&mut harness, "GROC");
    harness
        .get_by_role_and_label(Role::Button, "expenses:food:groceries")
        .click();
    harness.run();

    assert!(harness.query_by_label("Account trends chart").is_some());
    // The chip names the account and its total over the window.
    assert!(harness.query_by_label("expenses:food:groceries").is_some());
    assert!(harness.query_by_label("478.45 CAD").is_some());

    harness.get_by_role_and_label(Role::Button, "×").click();
    harness.run();
    assert!(harness.query_by_label("Account trends chart").is_none());
}

/// The span chips change how many months the charts cover, and so the
/// income total beside the chart.
#[test]
fn span_chips_change_the_window() {
    let books = synthetic_years_books();
    let home = books.home().unwrap();
    let total = |span| monthly(&books, home, &window_of(&books, span)).income_total;

    let mut harness = trends_harness(synthetic_years_app());
    assert!(harness.query_by_label(&total(Span::Year)).is_some());

    harness.get_by_label("All time").click();
    harness.run();
    assert!(harness.query_by_label(&total(Span::AllTime)).is_some());
    assert!(harness.query_by_label(&total(Span::Year)).is_none());
}

/// Hovering a month of the income chart shows its income, expenses and
/// net.
#[test]
fn hovering_a_month_shows_its_figures() {
    let mut harness = trends_harness(synthetic_app());
    let chart = harness.get_by_label("Income vs expenses chart").rect();
    hover_month(&mut harness, chart, 1, 2);
    assert!(harness.query_by_label("Oct 2024").is_some());
    assert!(harness.query_by_label("6062.34 CAD").is_some());
    assert!(harness.query_by_label("2741.48 CAD").is_some());
    assert!(harness.query_by_label("3320.86 CAD").is_some());
}

/// The net worth chip appears only with other commodities held, and
/// leaving them out changes today's figure.
#[test]
fn home_currency_only_leaves_out_bitcoin() {
    let harness = trends_harness(synthetic_app());
    assert!(harness.query_by_label("Home currency only").is_none());

    let mut harness = trends_harness(synthetic_years_app());
    let with = latest_net_worth(&harness);
    harness.get_by_label("Home currency only").click();
    harness.run();
    let without = latest_net_worth(&harness);
    assert_ne!(with, without);
}

/// The net worth figure beside the heading.
fn latest_net_worth(harness: &Harness<'static, common::CashflowTestState>) -> String {
    let books = synthetic_years_books();
    let home = books.home().unwrap();
    let w = window_of(&books, Span::Year);
    let candidates = [
        net_worth(&books, home, &w, false).latest,
        net_worth(&books, home, &w, true).latest,
    ];
    candidates
        .into_iter()
        .find(|text| harness.query_by_label(text).is_some())
        .expect("one of the two net worth figures is shown")
}

/// Hover the middle of month `month` of `months` in a chart.
///
/// The plot ends 8pt in from the chart's right edge, but starts past y-axis
/// labels of a width the test doesn't know, so this counts back from the
/// right with the labels guessed at 40pt. The guess is off by a fraction of
/// a month for the months near the right, which is where the tests aim.
fn hover_month(
    harness: &mut Harness<'static, common::CashflowTestState>,
    chart: egui::Rect,
    month: usize,
    months: usize,
) {
    let right = chart.right() - 8.0;
    let step = (right - chart.left() - 56.0) / months as f32;
    let x = right - step * ((months - month) as f32 - 0.5);
    harness.hover_at(pos2(x, chart.center().y));
    harness.run();
}

/// With no books loaded the view says so.
#[test]
fn no_books_says_so() {
    let mut app = notedeck_cashflow::CashflowApp::new();
    app.set_view(View::Trends);
    let harness = harness(app, DESKTOP);
    assert!(harness.query_by_label("No journal loaded yet.").is_some());
}

/// The whole view over the multi-year journal, two accounts compared.
fn snapshot_full(harness: &mut Harness<'static, common::CashflowTestState>) {
    search(harness, "groceries");
    harness
        .get_by_role_and_label(Role::Button, "expenses:food:groceries")
        .click();
    harness.run();
    search(harness, "restaurant");
    harness
        .get_by_role_and_label(Role::Button, "expenses:food:restaurant")
        .click();
    harness.run();
}

/// Every chart over the last twelve months, two accounts compared.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_trends_year() {
    let mut app = synthetic_years_app();
    app.set_view(View::Trends);
    let mut harness = snapshot_harness(app, TALL);
    snapshot_full(&mut harness);
    harness.snapshot("cashflow_trends_year");
}

/// Every chart over all time, bitcoin's price moving the net worth line.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_trends_all_time() {
    let mut app = synthetic_years_app();
    app.set_view(View::Trends);
    let mut harness = snapshot_harness(app, TALL);
    harness.get_by_label("All time").click();
    harness.run();
    snapshot_full(&mut harness);
    harness.snapshot("cashflow_trends_all_time");
}

/// The tooltip over a month of the income vs expenses chart.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_trends_month_tooltip() {
    let mut app = synthetic_years_app();
    app.set_view(View::Trends);
    let mut harness = snapshot_harness(app, TALL);
    let chart = harness.get_by_label("Income vs expenses chart").rect();
    hover_month(&mut harness, chart, 8, 12);
    harness.snapshot("cashflow_trends_month_tooltip");
}

/// The net worth tooltip, with its note about prices.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_trends_net_worth_tooltip() {
    let mut app = synthetic_years_app();
    app.set_view(View::Trends);
    let mut harness = snapshot_harness(app, TALL);
    let chart = harness.get_by_label("Net worth chart").rect();
    hover_month(&mut harness, chart, 5, 12);
    harness.snapshot("cashflow_trends_net_worth_tooltip");
}

/// On a phone the charts narrow and the span chips wrap under the heading.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_trends_mobile() {
    let mut app = synthetic_years_app();
    app.set_view(View::Trends);
    let mut harness = snapshot_harness(app, MOBILE);
    harness.snapshot("cashflow_trends_mobile");
}
