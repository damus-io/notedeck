//! The Goals view: a tile per goal with its status, progress and pace, a
//! goal's detail chart, a click on a goal opening its Headway card, the
//! Overview's strip, and the goals found on a real board in the host's
//! nostrdb.
//!
//! Every goal is made up, over [`common::synthetic_years_books`] on
//! [`common::TODAY`]. The model and behavioural tests run under a plain
//! `cargo test`. The snapshots are `#[ignore]`d and run via
//! `scripts/snapshot-test` (lavapipe).

mod common;

use cashflow::goals::{Goal, GoalErrorKind, Status, parse_goal};
use cashflow::projects::Projects;
use common::{
    DESKTOP, JournalFixture, MOBILE, account, harness, loaded_harness, snapshot_harness,
    step_until, synthetic_years_app, synthetic_years_books, today,
};
use egui_kittest::kittest::Queryable;
use headway::boards::{Identity, Roster};
use headway::store::{self, BoardAction, NewCard, NoPublish, Signer};
use headway::teams::RootRegistry;
use notedeck_cashflow::views::goals::model::{Inputs, Report, report};
use notedeck_cashflow::{CashflowApp, View};

/// A window tall enough for every tile, or the detail.
const TALL: egui::Vec2 = egui::Vec2::new(1200.0, 1100.0);

/// A goal on a made-up `cashflow` card `word`, in `column`, whose block is
/// `body`.
fn goal(word: &str, title: &str, column: &str, body: &str) -> Goal {
    let desc = format!("Why this matters.\n\n```cashflow-goal\n{body}```\n");
    Goal {
        card_ref: format!("headway:cashflow/{word}"),
        board: "cashflow".to_owned(),
        owner: [1; 32],
        card: [word.len() as u8; 32],
        title: title.to_owned(),
        column: column.to_owned(),
        done: false,
        spec: parse_goal(&desc).expect("a goal block"),
    }
}

/// One goal in each state, then one whose block doesn't read and one the
/// books can't measure.
fn goals() -> Vec<Goal> {
    vec![
        goal(
            "save-the-year",
            "Save 30k this year",
            "In Progress",
            "metric: net\ntarget: 30000 CAD\nper: year\n",
        ),
        goal(
            "earn-more-monthly",
            "Earn 9.5k a month",
            "In Progress",
            "metric: income\ntarget: 9500 CAD\n",
        ),
        goal(
            "keep-spending-down",
            "Spend under 3k a month",
            "Todo",
            "metric: expenses\ntarget: 3000 CAD\n",
        ),
        goal(
            "cushion-in-checking",
            "Keep 5k in checking",
            "Done",
            "metric: balance\naccount: assets:checking\ntarget: 5000 CAD\nper: total\n",
        ),
        goal(
            "typo-in-block",
            "Half-written goal",
            "Backlog",
            "metric: net\n",
        ),
        goal(
            "no-such-account",
            "Grow the side gig",
            "Backlog",
            "metric: income\naccount: income:sidegig\ntarget: 500 CAD\n",
        ),
    ]
}

/// [`goals`] measured against the synthetic books on [`common::TODAY`].
fn measured() -> Report {
    let books = synthetic_years_books();
    let goals = goals();
    report(Inputs {
        books: &books,
        projects: &Projects::default(),
        today: today(),
        goals: &goals,
    })
}

/// A fresh app on the synthetic books with [`goals`], showing Goals.
fn app() -> CashflowApp {
    let mut app = synthetic_years_app();
    app.set_goals(goals());
    app.set_view(View::Goals);
    app
}

/// Each goal lands in the state its numbers put it in, and the two that
/// can't be measured say why.
#[test]
fn each_goal_is_measured() {
    let r = measured();
    let statuses: Vec<_> = r
        .rows
        .iter()
        .map(|row| {
            (
                row.title.as_str(),
                row.outcome
                    .as_ref()
                    .map(|m| m.status)
                    .map_err(String::as_str),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        [
            ("Save 30k this year", Ok(Status::OnTrack)),
            ("Earn 9.5k a month", Ok(Status::AtRisk)),
            ("Spend under 3k a month", Ok(Status::Behind)),
            ("Keep 5k in checking", Ok(Status::Achieved)),
            (
                "Half-written goal",
                Err("goal block, line 3: missing `target`")
            ),
            (
                "Grow the side gig",
                Err("no account `income:sidegig` in the books")
            ),
        ]
    );
    assert_eq!(
        r.summary.text,
        "2 to fix, 1 behind, 1 at risk, 1 on track, 1 achieved"
    );
}

/// The year's savings goal charts January to December: the year so far
/// solid, the projection dashed on from October, joined to October's
/// value, the target as a level and the year end marked.
#[test]
fn a_years_chart_runs_to_its_end() {
    let r = measured();
    let save = r.row("headway:cashflow/save-the-year").expect("the goal");
    let chart = &save.outcome.as_ref().expect("measured").chart;
    assert_eq!(chart.caption, "Year to date");
    assert_eq!(chart.labels.first().map(String::as_str), Some("Jan 2024"));
    assert_eq!(chart.labels.last().map(String::as_str), Some("Dec 2024"));
    assert_eq!(chart.actual.len(), 10, "January through October");
    assert_eq!(
        chart.projection_start, 9,
        "the projection starts at October"
    );
    assert_eq!(chart.projection.len(), 3, "October, November, December");
    assert_eq!(chart.join, chart.actual.last().copied());
    assert_eq!(chart.target, 3_000_000.0, "30000 CAD in cents");
    assert_eq!(chart.deadline, Some(11));
    assert_eq!(chart.deadline_label, "year end");
    assert_eq!(chart.axis_text(3_000_000.0), "30k");
}

/// The tiles show each goal's numbers, and a click on a goal's title asks
/// the host to open its card.
#[test]
fn a_goals_title_opens_its_card() {
    let mut harness = harness(app(), TALL);
    harness.get_by_label("Earn 9.5k a month");
    harness.get_by_label("at risk");
    assert_eq!(harness.query_all_by_label("needs fixing").count(), 2);
    harness.get_by_label("no account `income:sidegig` in the books");
    assert!(harness.state().opened.is_empty());

    harness.get_by_label("Save 30k this year").click();
    harness.run();
    assert_eq!(harness.state().opened, ["headway:cashflow/save-the-year"]);
}

/// "Chart →" opens a goal's detail, whose title opens the card too, and
/// "← All goals" goes back.
#[test]
fn chart_opens_the_detail() {
    let mut harness = harness(app(), TALL);
    harness
        .query_all_by_label("Chart →")
        .next()
        .expect("a tile")
        .click();
    harness.run();
    harness.get_by_label("Goal progress chart");
    harness.get_by_label("Year to date");

    harness.get_by_label("Save 30k this year").click();
    harness.run();
    assert_eq!(harness.state().opened, ["headway:cashflow/save-the-year"]);

    harness.get_by_label("← All goals").click();
    harness.run();
    assert!(harness.query_by_label("Goal progress chart").is_none());
    assert_eq!(harness.query_all_by_label("Chart →").count(), 4);
}

/// The Overview counts the goals in a strip that links to the Goals view,
/// and has no strip without goals.
#[test]
fn overview_strip_links_to_goals() {
    let without = harness(synthetic_years_app(), DESKTOP);
    assert!(without.query_by_label_contains("on track").is_none());

    let mut app = synthetic_years_app();
    app.set_goals(goals());
    let mut harness = harness(app, DESKTOP);
    harness
        .get_by_label("2 to fix, 1 behind, 1 at risk, 1 on track, 1 achieved")
        .click();
    harness.run();
    assert_eq!(harness.state().app.view(), View::Goals);
}

/// With no goals, the view says how to make one, naming the boards.
#[test]
fn no_goals_says_how_to_make_one() {
    let mut app = synthetic_years_app();
    app.set_view(View::Goals);
    let harness = harness(app, DESKTOP);
    harness.get_by_label("No goals yet.");
    harness.get_by_label("personal");
}

/// An app that loads its own books finds the goals on the account's own
/// board in the host's nostrdb, and a goal card added later appears
/// without a restart.
#[test]
fn finds_goals_on_the_accounts_board_live() {
    let fixture = JournalFixture::synthetic();
    let mut harness = loaded_harness(fixture.app(), DESKTOP, false);
    let ndb = harness.state_mut().notedeck.app_context().ndb.clone();
    let me = account();
    let secret = me.secret_key.secret_bytes();
    let mut registry = RootRegistry::default();

    let landed = |kind: u32| {
        let filter = nostrdb::Filter::new().kinds([u64::from(kind)]).build();
        ndb.subscribe(&[filter]).expect("subscribe")
    };
    let wait = |sub| {
        common::block_on(ndb.wait_for_all_notes_within(sub, 1, common::INGEST_TIMEOUT))
            .expect("the note landed");
    };
    let board = landed(headway::event::KIND_BOARD);
    store::seed_board(
        &ndb,
        &me.pubkey,
        &secret,
        "personal",
        "Personal",
        &mut NoPublish,
    );
    wait(board);

    let mut add = |title: &str, body: &str| {
        let roster = Roster::load(&ndb, Identity::own(me.pubkey), &mut registry);
        let view = headway::boards::load_board(&ndb, &roster, "personal").expect("the board");
        let channel = roster.channel("personal");
        let card = landed(headway::event::KIND_ISSUE);
        let declined = store::apply(
            &ndb,
            "personal",
            &view,
            &me.pubkey,
            &Signer::new(&secret, channel.as_ref()),
            BoardAction::AddCard(NewCard {
                col: 0,
                title: title.to_owned(),
                description: format!("```cashflow-goal\n{body}```\n"),
                labels: vec![cashflow::goals::GOAL_LABEL.to_owned()],
                ..NewCard::default()
            }),
            &mut NoPublish,
        );
        assert!(declined.is_none(), "{declined:?}");
        wait(card);
    };

    add("Earn 9.5k a month", "metric: income\ntarget: 9500 CAD\n");
    step_until(&mut harness, "the goal is found", |s| {
        s.app.goals().len() == 1
    });
    let found = &harness.state().app.goals()[0];
    assert_eq!(found.title, "Earn 9.5k a month");
    assert!(
        found.card_ref.starts_with("headway:personal/"),
        "{}",
        found.card_ref
    );

    add("Broken", "metric: net\n");
    step_until(&mut harness, "the second goal is found", |s| {
        s.app.goals().len() == 2
    });
    let broken = harness
        .state()
        .app
        .goals()
        .iter()
        .find(|g| g.title == "Broken");
    let kind = broken.and_then(|g| g.spec.as_ref().err()).map(|e| &e.kind);
    assert_eq!(kind, Some(&GoalErrorKind::Missing("target")));
}

/// Every state's tile and the two that can't be measured.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_goals_list() {
    let mut harness = snapshot_harness(app(), TALL);
    harness.snapshot("cashflow_goals_list");
}

/// A year's savings goal: the year so far, the projection, the target and
/// the year end.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_goals_detail() {
    let mut harness = snapshot_harness(app(), DESKTOP);
    harness
        .query_all_by_label("Chart →")
        .next()
        .expect("a tile")
        .click();
    harness.run();
    harness.snapshot("cashflow_goals_detail");
}

/// The tiles on a phone.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_goals_mobile() {
    let mut harness = snapshot_harness(app(), MOBILE);
    harness.snapshot("cashflow_goals_mobile");
}
