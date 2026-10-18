//! The Adventure view: the skills grid, the character sheet, the quest log
//! with its epic quests opening their Headway cards, the achievements and
//! the XP log, folded from [`common::synthetic_years_books`] on
//! [`common::TODAY`] with a made-up `Freelance` project and two made-up
//! goals.
//!
//! The model and behavioural tests run under a plain `cargo test`. The
//! snapshots are `#[ignore]`d and run via `scripts/snapshot-test`
//! (lavapipe).

mod common;

use cashflow::DeriveInputs;
use cashflow::adventure::{QuestKind, XpTuning};
use cashflow::goals::{Goal, parse_goal};
use cashflow::projects::{ProjectRoot, Projects};
use cashflow::rules::default_edits;
use cashflow::zone::Zone;
use common::{
    MOBILE, harness, snapshot_harness, synthetic_years_app, synthetic_years_books, today,
};
use egui_kittest::kittest::Queryable;
use notedeck_cashflow::views::adventure::model::{Inputs, LOG_ROWS, Report, report};
use notedeck_cashflow::{CashflowApp, View};

/// A window wide enough for the two-column layout and tall enough for most
/// of it.
const WIDE: egui::Vec2 = egui::Vec2::new(1200.0, 1100.0);

/// The made-up project: the freelance income.
fn projects() -> Projects {
    Projects::new([ProjectRoot {
        account: "income:freelance".to_owned(),
        project: "Freelance".to_owned(),
    }])
}

/// A goal on the made-up `cashflow` card `word`, done on `done_on` if it
/// is.
fn goal(word: &str, title: &str, body: &str, done_on: Option<&str>) -> Goal {
    let desc = format!("```cashflow-goal\n{body}```\n");
    let done_at = done_on.map(|day| {
        let date = ledgerdb::Date::parse(day).expect("a date");
        u64::from(date.days()) * 86_400 + 43_200
    });
    Goal {
        card_ref: format!("headway:cashflow/{word}"),
        board: "cashflow".to_owned(),
        owner: [1; 32],
        card: [word.len() as u8; 32],
        title: title.to_owned(),
        column: if done_on.is_some() {
            "Done"
        } else {
            "In Progress"
        }
        .to_owned(),
        done: done_on.is_some(),
        done_at,
        spec: parse_goal(&desc).expect("a goal block"),
        posted: Vec::new(),
    }
}

/// One goal still running and one done in the summer.
fn goals() -> Vec<Goal> {
    vec![
        goal(
            "save-the-year",
            "Save 30k this year",
            "metric: net\ntarget: 30000 CAD\nper: year\n",
            None,
        ),
        goal(
            "earn-five-k",
            "Earn 5k a month",
            "metric: income\ntarget: 5000 CAD\nper: month\n",
            Some("2024-07-15"),
        ),
    ]
}

/// The synthetic books folded with [`projects`] and [`goals`].
fn folded() -> Report {
    let books = synthetic_years_books();
    let derive = DeriveInputs::from_edits(default_edits());
    let goals = goals();
    report(
        Inputs {
            books: &books,
            derive: &derive,
            projects: &projects(),
            zone: Zone::Fixed(0),
            today: today(),
            goals: &goals,
            tuning: &XpTuning::default(),
        },
        None,
    )
    .expect("the books count in CAD")
}

/// A fresh app on the synthetic books with [`projects`] and [`goals`],
/// showing the Adventure.
fn app() -> CashflowApp {
    let mut app = synthetic_years_app();
    app.set_projects(projects());
    app.set_goals(goals());
    app.set_view(View::Adventure);
    app
}

/// The fixed skills come first, then the project; a skill nothing has
/// started (Bookkeeping, with no vault edits) is locked with a hint.
#[test]
fn skills_are_the_fixed_ones_then_the_projects() {
    let r = folded();
    let names: Vec<&str> = r.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Earning",
            "Saving",
            "Frugality",
            "Stacking",
            "Bookkeeping",
            "Freelance"
        ]
    );
    let bookkeeping = &r.skills[4];
    assert!(bookkeeping.unlock.is_some(), "no vault edits, so no XP");
    let earning = &r.skills[0];
    assert!(earning.unlock.is_none());
    assert!(earning.progress.level > 1);
    assert_eq!(
        r.sheet.total_level,
        r.skills
            .iter()
            .map(|s| u32::from(s.progress.level))
            .sum::<u32>()
    );
}

/// This month's gains are listed most first, and each says where its XP
/// came from: Earning's names the income and its rate.
#[test]
fn gains_say_where_their_xp_came_from() {
    let r = folded();
    assert!(!r.sheet.gains.is_empty());
    let gains: Vec<u64> = r
        .sheet
        .gains
        .iter()
        .map(|&at| r.skills[at].progress.month_xp)
        .collect();
    assert!(gains.windows(2).all(|w| w[0] >= w[1]), "{gains:?}");
    let earning = &r.skills[0];
    assert!(earning.gain_text.starts_with('+') && earning.gain_text.ends_with(" XP"));
    assert!(
        earning
            .gain_reasons
            .iter()
            .any(|line| line.starts_with("income ") && line.contains("CAD × 10")),
        "{:?}",
        earning.gain_reasons
    );
}

/// The quest log runs weekly, then monthly, then epic; the goals are the
/// epic quests, each linking its card, and the done one is complete.
#[test]
fn goals_are_epic_quests_that_link_their_cards() {
    let r = folded();
    let kinds: Vec<QuestKind> = r.quests.iter().map(|q| q.kind).collect();
    let mut sorted = kinds.clone();
    sorted.sort_by_key(|k| match k {
        QuestKind::Weekly => 0,
        QuestKind::Monthly => 1,
        QuestKind::Epic => 2,
    });
    assert_eq!(kinds, sorted);
    let epics: Vec<_> = r
        .quests
        .iter()
        .filter(|q| q.kind == QuestKind::Epic)
        .collect();
    assert_eq!(epics.len(), 2);
    assert_eq!(
        epics[0].card_ref.as_deref(),
        Some("headway:cashflow/save-the-year")
    );
    assert!(!epics[0].done);
    assert!(epics[1].done);
    assert!(
        r.quests
            .iter()
            .filter(|q| q.kind != QuestKind::Epic)
            .all(|q| q.card_ref.is_none())
    );
    // The done goal paid in July, the month its card moved.
    assert!(
        r.log
            .iter()
            .any(|e| e.month == "Jul 2024" && e.reason == "quest: Earn 5k a month"),
        "{:?}",
        r.log
    );
}

/// The achievements list the unlocked first, dated, then the locked ones,
/// and the points add up.
#[test]
fn achievements_list_unlocked_first() {
    let r = folded();
    assert!(r.unlocked > 0);
    let first_locked = r
        .achievements
        .iter()
        .position(|a| a.unlocked.is_none())
        .unwrap_or(r.achievements.len());
    assert_eq!(first_locked, r.unlocked);
    assert!(
        r.achievements[first_locked..]
            .iter()
            .all(|a| a.unlocked.is_none())
    );
    let points: u32 = r.achievements[..first_locked]
        .iter()
        .map(|a| a.points)
        .sum();
    assert_eq!(points, r.points);
    assert!(r.points < r.total_points);
}

/// The log is newest month first and bounded.
#[test]
fn the_log_is_newest_first_and_bounded() {
    let r = folded();
    assert!(!r.log.is_empty() && r.log.len() <= LOG_ROWS);
    assert_eq!(r.log[0].month, "Oct 2024");
    assert!(r.log.iter().all(|e| !e.adjustment));
}

/// A second fold that lost XP logs the correction at the top.
#[test]
fn a_correction_is_logged_first() {
    let books = synthetic_years_books();
    let derive = DeriveInputs::from_edits(default_edits());
    let tuning = XpTuning::default();
    let inputs = |projects| Inputs {
        books: &books,
        derive: &derive,
        projects,
        zone: Zone::Fixed(0),
        today: today(),
        goals: &[],
        tuning: &tuning,
    };
    let with = projects();
    let before = report(inputs(&with), None).unwrap();
    // The project goes away, and its XP with it.
    let after = report(inputs(&Projects::default()), Some(&before.adventure)).unwrap();
    assert!(after.log[0].adjustment, "{:?}", after.log[0]);
    assert_eq!(after.log[0].skill, "Freelance");
    assert!(after.log[0].delta.starts_with('-'));
}

/// The view draws every panel and the skills by name.
#[test]
fn the_view_draws_every_panel() {
    let mut harness = harness(app(), WIDE);
    harness.run();
    for name in ["Skills", "Character", "Quests", "Achievements", "XP log"] {
        assert!(harness.query_by_label(name).is_some(), "no {name} panel");
    }
    for skill in ["Earning", "Freelance", "Bookkeeping"] {
        assert!(
            harness.query_all_by_label(skill).next().is_some(),
            "{skill}"
        );
    }
}

/// Clicking an epic quest's title asks to open its Headway card.
#[test]
fn clicking_an_epic_quest_opens_its_card() {
    let mut harness = harness(app(), WIDE);
    harness.run();
    harness.get_by_label("Save 30k this year").click();
    harness.run();
    assert_eq!(harness.state().opened, ["headway:cashflow/save-the-year"]);
}

/// The wide layout: skills and the character on the left, quests,
/// achievements and the log on the right.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_adventure_wide() {
    let mut harness = snapshot_harness(app(), WIDE);
    harness.snapshot("cashflow_adventure_wide");
}

/// On a phone the panels stack.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_adventure_mobile() {
    let mut harness = snapshot_harness(app(), MOBILE);
    harness.snapshot("cashflow_adventure_mobile");
}
