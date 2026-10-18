//! The Projects view: revenue per project, its delta, sparkline, mix and
//! net, the unattributed row, and a project's detail, over a synthetic
//! journal with three projects; and the work on the Headway boards two of
//! them link, from hand-made board progress.
//!
//! The model and behavioural tests run under a plain `cargo test`. The
//! snapshots are `#[ignore]`d and run via `scripts/snapshot-test` (lavapipe).

mod common;

use cashflow::Books;
use cashflow::board_progress::BoardProgress;
use cashflow::projects::{ProjectRoot, Projects};
use common::{DESKTOP, MOBILE, books_from, harness, snapshot_harness, today};
use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use ledgerdb::Date;
use notedeck_cashflow::boards::ProjectLink;
use notedeck_cashflow::views::projects::model::{
    BoardInputs, BoardRow, Inputs, Report, boards, projects,
};
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

/// Noon UTC on `date`, in unix seconds: the same day in any zone within
/// eleven hours of UTC, so the machine's zone can't move a card a day.
fn noon(date: &str) -> u64 {
    u64::from(Date::parse(date).unwrap().days()) * 86_400 + 43_200
}

/// Damus links its iOS board and an Android board that isn't in the
/// cache; Notedeck links its own; the node links nothing.
fn links() -> Vec<ProjectLink> {
    let link = |project: &str, boards: &[&str]| ProjectLink {
        project: project.to_owned(),
        boards: boards.iter().map(|b| (*b).to_owned()).collect(),
    };
    vec![
        link("damus", &["damus-ios", "damus-android"]),
        link("notedeck", &["notedeck"]),
        link("unknown-project", &["elsewhere"]),
    ]
}

/// The linked boards: damus-ios finished three cards this October, two in
/// September, one in June and one in 2023, before the journal starts;
/// notedeck four in August and one this month. Every card is made up.
fn progress() -> Vec<BoardProgress> {
    let board = |slug: &str, in_progress, in_review, done: &[&str]| BoardProgress {
        board: slug.to_owned(),
        owner: [7; 32],
        in_progress,
        in_review,
        completed: done.iter().map(|d| noon(d)).collect(),
    };
    vec![
        board(
            "damus-ios",
            2,
            1,
            &[
                "2023-11-20",
                "2024-06-14",
                "2024-09-02",
                "2024-09-30",
                "2024-10-03",
                "2024-10-05",
                "2024-10-10",
            ],
        ),
        board(
            "notedeck",
            1,
            0,
            &[
                "2024-08-01",
                "2024-08-09",
                "2024-08-20",
                "2024-08-31",
                "2024-10-16",
            ],
        ),
        board("elsewhere", 5, 5, &["2024-10-01"]),
    ]
}

/// [`app`] with [`links`] and [`progress`].
fn app_with_boards() -> CashflowApp {
    let mut app = app();
    app.set_project_links(links());
    app.set_board_progress(progress());
    app
}

/// The board figures for [`report_of`] over `period`.
fn boards_of(report: &Report, period: Period) -> Vec<Option<BoardRow>> {
    boards(BoardInputs {
        report,
        links: &links(),
        progress: &progress(),
        period,
        today: today(),
    })
}

/// This month, Damus's boards finished three cards and Notedeck's one, as
/// the rows' sparklines count per month from June; a linked board not in
/// the cache is named; the node and the unattributed row link nothing, and
/// a linked project with no revenue row is dropped.
#[test]
fn board_progress_beside_revenue() {
    let books = books();
    let r = report_of(&books, &attribution(), Period::ThisMonth);
    let rows = boards_of(&r, Period::ThisMonth);
    assert_eq!(rows.len(), r.rows.len());
    let of = |project: Option<&str>| rows[r.position(project).unwrap()].as_ref();

    let damus = of(Some("damus")).expect("damus links boards");
    assert_eq!(damus.boards_text, "damus-ios, damus-android");
    assert_eq!((damus.done, damus.in_progress, damus.in_review), (3, 2, 1));
    assert_eq!(damus.stats_text, "3 done · 2 in progress · 1 in review");
    assert_eq!(damus.spark, [1.0, 0.0, 0.0, 2.0, 3.0]);
    assert_eq!(
        damus.missing_text.as_deref(),
        Some("not found: damus-android")
    );
    assert_eq!(damus.lines.len(), 1);
    assert_eq!(damus.lines[0].board, "damus-ios");

    let notedeck = of(Some("notedeck")).expect("notedeck links its board");
    assert_eq!(notedeck.stats_text, "1 done · 1 in progress · 0 in review");
    assert_eq!(notedeck.spark, [0.0, 0.0, 4.0, 0.0, 1.0]);
    assert_eq!(notedeck.missing_text, None);

    assert!(of(Some("node")).is_none(), "the node links no board");
    assert!(of(None).is_none());

    // All time counts every card, the one before the journal too.
    let all = boards_of(&r, Period::AllTime);
    let damus = all[r.position(Some("damus")).unwrap()].as_ref().unwrap();
    assert_eq!(damus.done, 7);
    let last = boards_of(&r, Period::LastMonth);
    let damus = last[r.position(Some("damus")).unwrap()].as_ref().unwrap();
    assert_eq!(damus.done, 2);
}

/// The tiles of linked projects carry their board line; one with no
/// linked board shows none, and an app with no links shows none at all.
#[test]
fn only_linked_projects_show_board_progress() {
    let linked = harness(app_with_boards(), TALL);
    assert_eq!(linked.query_all_by_label("Headway").count(), 2);
    assert!(
        linked
            .query_by_label("3 done · 2 in progress · 1 in review")
            .is_some()
    );
    assert!(linked.query_by_label("not found: damus-android").is_some());

    let unlinked = harness(app(), TALL);
    assert_eq!(unlinked.query_all_by_label("Headway").count(), 0);
}

/// A project's detail shows its boards' work, a line per board when two
/// or more fold; new progress redraws it.
#[test]
fn detail_lists_each_board() {
    let mut harness = harness(app_with_boards(), TALL);
    harness.get_by_role_and_label(Role::Button, "damus").click();
    harness.run();
    assert!(harness.query_by_label("Work on Headway").is_some());
    assert!(harness.query_by_label("damus-ios, damus-android").is_some());
    // Only damus-ios folded, so no line of its own.
    assert!(harness.query_by_label("damus-ios").is_none());

    let mut fewer = progress();
    fewer[0].in_progress = 9;
    harness.state_mut().app.set_board_progress(fewer);
    harness.run();
    assert!(
        harness
            .query_by_label("3 done · 9 in progress · 1 in review")
            .is_some()
    );
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

/// The list with Damus and Notedeck linked to boards: each tile's board
/// line and its finished-cards sparkline under the revenue one.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_projects_boards() {
    let mut harness = snapshot_harness(app_with_boards(), TALL);
    harness.snapshot("cashflow_projects_boards");
}

/// Damus's detail with its boards' work, the Android board not found.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_projects_boards_detail() {
    let mut harness = snapshot_harness(app_with_boards(), TALL);
    harness.get_by_role_and_label(Role::Button, "damus").click();
    harness.run();
    harness.snapshot("cashflow_projects_boards_detail");
}

/// The boards list on a phone: the board line wraps inside its tile.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_projects_boards_mobile() {
    let mut harness = snapshot_harness(app_with_boards(), MOBILE);
    harness.snapshot("cashflow_projects_boards_mobile");
}
