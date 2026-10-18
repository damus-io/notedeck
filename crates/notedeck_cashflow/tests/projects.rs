//! The Projects view: revenue per project, its delta, sparkline, mix and
//! net, the unattributed row, and a project's detail, over a synthetic
//! journal with three projects.
//!
//! The model and behavioural tests run under a plain `cargo test`. The
//! snapshots are `#[ignore]`d and run via `scripts/snapshot-test` (lavapipe).

mod common;

use cashflow::Books;
use cashflow::projects::{ProjectRoot, Projects};
use common::{DESKTOP, MOBILE, books_from, harness, snapshot_harness, today};
use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use notedeck_cashflow::views::projects::model::{Inputs, Report, projects};
use notedeck_cashflow::{CashflowApp, Period, View};

/// A window tall enough for the list or a detail.
const TALL: egui::Vec2 = egui::Vec2::new(1200.0, 1000.0);

/// June through October 2024, each month: Damus earns zaps, App Store and
/// (from August) purple, and merch once in October; Notedeck, a project
/// tagged inside Damus's subtree, earns zaps and pays for hosting; a node
/// earns routing fees; salary and interest belong to no project; rent is
/// an expense no project tags. Every amount is made up.
fn journal() -> String {
    let mut text = String::new();
    let mut id = 0;
    let mut txn = |date: String, payee: &str, account: &str, amount: String| {
        id += 1;
        text.push_str(&format!(
            "{date} * {payee}\n  ; id: proj:{id}\n  {account}  {amount} CAD\n  assets:checking\n\n"
        ));
    };
    for month in 6u32..=10 {
        let day = |d: u32| format!("2024-{month:02}-{d:02}");
        txn(day(1), "Landlord", "expenses:rent", "1500.00".into());
        txn(day(2), "Employer", "income:salary", "-4000.00".into());
        txn(
            day(5),
            "Zappers",
            "income:damus:zaps",
            format!("-{}.00", 100 + month),
        );
        txn(
            day(10),
            "App Store",
            "income:damus:appstore",
            "-300.00".into(),
        );
        if month >= 8 {
            txn(day(11), "Purple", "income:damus:purple", "-40.00".into());
        }
        if month == 10 {
            txn(day(12), "Merch shop", "income:damus:merch", "-25.00".into());
        }
        txn(
            day(6),
            "Zappers",
            "income:damus:notedeck:zaps",
            format!("-{}.00", 20 * month),
        );
        txn(
            day(7),
            "Host Co",
            "expenses:notedeck:hosting",
            "50.00".into(),
        );
        txn(day(8), "Peers", "routed:in", "-3.00".into());
        txn(day(8), "Peers", "routed:out", "1.00".into());
        txn(day(9), "Bank", "income:interest", "-1.50".into());
    }
    text
}

/// The chart's attribution for [`journal`].
fn attribution() -> Projects {
    let root = |account: &str, project: &str| ProjectRoot {
        account: account.to_owned(),
        project: project.to_owned(),
    };
    Projects::new([
        root("income:damus", "damus"),
        root("income:damus:notedeck", "notedeck"),
        root("expenses:notedeck", "notedeck"),
        root("routed", "node"),
    ])
}

fn books() -> Books {
    books_from(&journal())
}

/// The report on [`books`] over `period` with `projects`.
fn report_of(books: &Books, projects_: &Projects, period: Period) -> Report {
    projects(Inputs {
        books,
        home: books.home().expect("books in CAD"),
        projects: projects_,
        period,
        today: today(),
    })
}

/// A fresh app on [`common::TODAY`] reporting on [`books`] with
/// [`attribution`].
fn app() -> CashflowApp {
    let mut app = CashflowApp::new();
    app.set_today(today());
    app.set_books(books());
    app.set_projects(attribution());
    app.set_view(View::Projects);
    app
}

/// This month (October 1st–17th): Damus earned 475.00 (26.00 more than
/// September's first seventeen days), Notedeck 200.00 less 50.00 hosting,
/// the node 2.00 of routing, and 4001.50 went to no project. Projects are
/// by revenue, the unattributed row last.
#[test]
fn revenue_per_project_this_month() {
    let books = books();
    let r = report_of(&books, &attribution(), Period::ThisMonth);
    assert!(r.attributed);
    let rows: Vec<_> = r
        .rows
        .iter()
        .map(|row| (row.label.as_str(), row.revenue_text.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("damus", "475.00 CAD"),
            ("notedeck", "200.00 CAD"),
            ("node", "2.00 CAD"),
            ("Unattributed", "4001.50 CAD"),
        ]
    );
    assert_eq!(r.total_text, "4678.50 CAD");

    let damus = r.row(Some("damus")).unwrap();
    assert_eq!(damus.vs_previous.as_ref().unwrap().text, "+26.00 CAD");
    assert_eq!(damus.share_text, "10%");
    assert_eq!(damus.net, None, "damus tags no expense account");

    let notedeck = r.row(Some("notedeck")).unwrap();
    let net = notedeck.net.as_ref().expect("notedeck tags hosting");
    assert_eq!(net.expenses_text, "50.00 CAD");
    assert_eq!(net.net_text, "150.00 CAD");
    assert_eq!(notedeck.vs_previous.as_ref().unwrap().text, "+20.00 CAD");

    let node = r.row(Some("node")).unwrap();
    assert_eq!(node.vs_previous.as_ref().unwrap().amount, 0);
}

/// Damus's sources are its own accounts, named below `income:damus`, with
/// Notedeck's zaps taken out; its mix names the four that earned.
#[test]
fn sources_and_mix() {
    let books = books();
    let r = report_of(&books, &attribution(), Period::ThisMonth);
    let damus = r.row(Some("damus")).unwrap();
    let sources: Vec<_> = damus
        .sources
        .iter()
        .map(|s| (s.label.as_str(), s.amount_text.as_str()))
        .collect();
    assert_eq!(
        sources,
        [
            ("appstore", "300.00 CAD"),
            ("zaps", "110.00 CAD"),
            ("purple", "40.00 CAD"),
            ("merch", "25.00 CAD"),
        ]
    );
    assert_eq!(damus.sources[0].detail, "income:damus:appstore");
    assert_eq!(
        damus.sources[3].delta_text.as_deref(),
        Some("+25.00 CAD (new)")
    );
    let mix: Vec<_> = damus
        .mix
        .iter()
        .map(|m| (m.label.as_str(), m.share_text.as_str()))
        .collect();
    assert_eq!(
        mix,
        [
            ("appstore", "63%"),
            ("zaps", "23%"),
            ("purple", "8%"),
            ("merch", "5%"),
        ]
    );

    let notedeck = r.row(Some("notedeck")).unwrap();
    assert_eq!(notedeck.sources[0].label, "zaps");
    assert_eq!(notedeck.sources[0].detail, "income:damus:notedeck:zaps");

    let node = r.row(Some("node")).unwrap();
    let routing: Vec<_> = node.sources.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(routing, ["routing"]);

    let unattributed = r.row(None).unwrap();
    let labels: Vec<_> = unattributed
        .sources
        .iter()
        .map(|s| s.label.as_str())
        .collect();
    assert_eq!(labels, ["salary", "interest"]);
}

/// Each project's revenue per month over the months the journal has
/// (June through October), its expenses beside it when it has any.
#[test]
fn revenue_per_month() {
    let books = books();
    let r = report_of(&books, &attribution(), Period::ThisMonth);
    let window = r.window.as_ref().expect("the journal has months");
    assert_eq!(window.labels.first().map(String::as_str), Some("Jun 2024"));
    assert_eq!(window.len(), 5);

    let damus = r.row(Some("damus")).unwrap();
    assert_eq!(
        damus.revenue_line.values,
        [40_600.0, 40_700.0, 44_800.0, 44_900.0, 47_500.0]
    );
    assert_eq!(damus.spark.len(), 5);
    assert!(damus.expenses_line.is_none());

    let notedeck = r.row(Some("notedeck")).unwrap();
    assert_eq!(
        notedeck.expenses_line.as_ref().unwrap().values,
        [5_000.0; 5]
    );
}

/// The period moves every figure: all time totals every month.
#[test]
fn all_time_has_no_delta() {
    let books = books();
    let r = report_of(&books, &attribution(), Period::AllTime);
    let damus = r.row(Some("damus")).unwrap();
    assert_eq!(damus.revenue_text, "2185.00 CAD");
    assert_eq!(damus.vs_previous, None);
}

/// With no account in a project, every bit of income is unattributed and
/// the report says nothing is attributed.
#[test]
fn nothing_attributed() {
    let books = books();
    let r = report_of(&books, &Projects::default(), Period::ThisMonth);
    assert!(!r.attributed);
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0].label, "Unattributed");
    // Routing no project claims is unattributed revenue too, as a source
    // of its own.
    assert_eq!(r.rows[0].revenue_text, "4678.50 CAD");
    assert!(r.rows[0].sources.iter().any(|s| s.label == "routing"));
}

/// A tile opens its project's detail, and the back link returns.
#[test]
fn a_tile_opens_its_detail() {
    let mut harness = harness(app(), TALL);
    assert!(harness.query_by_label("475.00 CAD").is_some());
    assert!(harness.query_by_label("Project revenue chart").is_none());

    harness.get_by_role_and_label(Role::Button, "damus").click();
    harness.run();
    assert!(harness.query_by_label("Project revenue chart").is_some());
    assert!(harness.query_by_label("appstore").is_some());
    assert!(harness.query_by_label("4001.50 CAD").is_none());

    harness.get_by_label("← All projects").click();
    harness.run();
    assert!(harness.query_by_label("Project revenue chart").is_none());
    assert!(harness.query_by_label("4001.50 CAD").is_some());
}

/// A source in a detail opens the Categories view drilled into it.
#[test]
fn a_source_opens_categories() {
    let mut harness = harness(app(), TALL);
    harness.get_by_role_and_label(Role::Button, "damus").click();
    harness.run();
    harness.get_by_role_and_label(Role::Button, "zaps").click();
    harness.run();
    assert_eq!(harness.state().app.view(), View::Categories);
}

/// The unattributed tile links to the triage inbox.
#[test]
fn unattributed_links_to_triage() {
    let mut harness = harness(app(), TALL);
    harness
        .get_by_label("Give it a project in the triage inbox →")
        .click();
    harness.run();
    assert_eq!(harness.state().app.view(), View::Transactions);
}

/// Replacing the attribution recomputes the view.
#[test]
fn new_attribution_recomputes() {
    let mut harness = harness(app(), TALL);
    assert!(harness.query_by_label("475.00 CAD").is_some());
    harness.state_mut().app.set_projects(Projects::default());
    harness.run();
    assert!(harness.query_by_label("475.00 CAD").is_none());
    // The total and the one row, everything unattributed.
    assert_eq!(harness.query_all_by_label("4678.50 CAD").count(), 2);
}

/// With no books loaded the view says so.
#[test]
fn no_books_says_so() {
    let mut app = CashflowApp::new();
    app.set_view(View::Projects);
    let harness = harness(app, DESKTOP);
    assert!(harness.query_by_label("No journal loaded yet.").is_some());
}

/// The list, wide: a tile per project with its sparkline and mix, the
/// unattributed tile last with its triage link.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_projects_list() {
    let mut harness = snapshot_harness(app(), TALL);
    harness.snapshot("cashflow_projects_list");
}

/// Notedeck's detail: revenue and hosting per month, and its source.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_projects_detail() {
    let mut harness = snapshot_harness(app(), TALL);
    harness
        .get_by_role_and_label(Role::Button, "notedeck")
        .click();
    harness.run();
    harness.snapshot("cashflow_projects_detail");
}

/// The list on a phone: tiles wrap their figures under the name.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_projects_mobile() {
    let mut harness = snapshot_harness(app(), MOBILE);
    harness.snapshot("cashflow_projects_mobile");
}
