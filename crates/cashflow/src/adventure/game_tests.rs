//! Quests and achievements over small journals: each quest's target,
//! completion and award, goals as epic quests, and each achievement's
//! condition and unlock date.

use ledgerdb::{Date, Edits, Override};

use super::achievements::{self, Unlocked};
use super::tests::{TODAY, World, groceries, salary, secs, xp};
use super::*;
use crate::goals::{Goal, parse_goal};
use crate::projects::ProjectRoot;
use crate::rules::default_edits;
use crate::test_util::{TestSegment, books, books_of};

/// The ids of the quests the fold paid XP for, in log order.
fn awarded(adventure: &Adventure) -> Vec<String> {
    adventure
        .log
        .iter()
        .filter_map(|e| match &e.reason {
            Reason::Quest { id, .. } => Some(id.to_string()),
            _ => None,
        })
        .collect()
}

/// The current quest `id`, if one runs.
fn quest<'a>(adventure: &'a Adventure, id: &str) -> Option<&'a Quest> {
    adventure.quests.iter().find(|q| q.id.0 == id)
}

fn date(text: &str) -> Date {
    Date::parse(text).unwrap()
}

/// Books and the world their edits were written in.
struct Triaged {
    books: Books,
    world: World,
}

/// Three card charges on the importer's default, imported on Tuesday
/// 2024-05-07, and a world whose overrides categorize the first
/// `written.len()` of them on those days.
fn triage_world(written: &[&str]) -> Triaged {
    let text = "2024-05-01 * Shop A\n  liabilities:card  -1.00 CAD\n  expenses:unknown\n\n\
                2024-05-02 * Shop B\n  liabilities:card  -2.00 CAD\n  expenses:unknown\n\n\
                2024-05-03 * Shop C\n  liabilities:card  -3.00 CAD\n  expenses:unknown\n\n";
    let mut edits = default_edits();
    for n in 1..=written.len() {
        edits.overrides.push(Override {
            record_id: format!("card:{n}"),
            account: "expenses:food".to_owned(),
        });
    }
    let books = books_of(
        &[TestSegment {
            source: "card",
            imported: secs("2024-05-07"),
            text,
        }],
        &edits,
    );
    let mut world = World::new();
    world.derive = DeriveInputs::from_edits(edits);
    world.derive.provenance.overrides = written.iter().map(|d| secs(d)).collect();
    Triaged { books, world }
}

/// Triaging all three the week they arrived completes that week's quest,
/// and leaves nothing for this week's.
#[test]
fn a_week_that_clears_the_backlog_completes_its_quest() {
    let Triaged { books, world } = triage_world(&["2024-05-08", "2024-05-08", "2024-05-09"]);
    let adventure = world.fold(&books, None);
    assert_eq!(awarded(&adventure), ["weekly:triage:2024-05-06"]);
    assert_eq!(
        xp(&adventure, "2024-05", SkillId::Bookkeeping),
        3 * world.tuning.triage_xp + world.tuning.import_xp + world.tuning.weekly_triage_xp
    );
    assert!(quest(&adventure, "weekly:triage:2024-05-13").is_none());
    assert_eq!(adventure.inbox_zero, Some(date("2024-05-09")));
}

/// A week that leaves one waiting misses; the next week's target is what
/// is left, and triaging it completes that one.
#[test]
fn the_target_is_what_was_waiting() {
    let Triaged { books, world } = triage_world(&["2024-05-08", "2024-05-08", "2024-05-14"]);
    let adventure = world.fold(&books, None);
    assert_eq!(awarded(&adventure), ["weekly:triage:2024-05-13"]);
    let this_week = quest(&adventure, "weekly:triage:2024-05-13").unwrap();
    assert_eq!(this_week.kind, QuestKind::Weekly);
    assert_eq!((this_week.progress, this_week.target), (1.0, 1.0));
    assert!(this_week.done);
    assert_eq!(adventure.inbox_zero, Some(date("2024-05-14")));

    let Triaged { books, world } = triage_world(&["2024-05-08", "2024-05-08"]);
    let adventure = world.fold(&books, None);
    assert!(awarded(&adventure).is_empty());
    let open = quest(&adventure, "weekly:triage:2024-05-13").unwrap();
    assert_eq!((open.progress, open.target), (0.0, 1.0));
    assert!(!open.done);
    assert_eq!(adventure.inbox_zero, None, "one is still waiting");
}

/// The weekly target is capped at the tuning's.
#[test]
fn the_weekly_target_caps() {
    let Triaged { books, mut world } = triage_world(&["2024-05-08", "2024-05-08", "2024-05-08"]);
    world.tuning.weekly_triage = 2;
    let adventure = world.fold(&books, None);
    assert_eq!(awarded(&adventure), ["weekly:triage:2024-05-06"]);
}

/// A closed month whose net beats the month before's pays Saving; the
/// month in progress has a quest but no award yet, and after a loss its
/// target is any net at all.
#[test]
fn beating_last_months_net_pays_saving() {
    let text = salary("2024-01", "1000.00")
        + &groceries("2024-01", "400.00")
        + &salary("2024-02", "1000.00")
        + &groceries("2024-02", "200.00")
        + &salary("2024-03", "1000.00")
        + &groceries("2024-03", "500.00")
        + &groceries("2024-04", "100.00")
        + &salary("2024-05", "300.00");
    let world = World::new();
    let adventure = world.fold(&books(&text), None);
    assert_eq!(awarded(&adventure), ["monthly:beat-net:2024-02"]);
    // CAD 800 at 20 a dollar with February's 5% Well Rested, and the quest.
    assert_eq!(
        xp(&adventure, "2024-02", SkillId::Saving),
        16_800 + world.tuning.beat_net_xp
    );
    let may = quest(&adventure, "monthly:beat-net:2024-05").unwrap();
    assert_eq!((may.progress, may.target), (30_000.0, 0.0));
    assert!(!may.done, "decided when the month closes");
    assert_eq!(may.skill, SkillId::Saving);
}

/// A source imports on 01-05, 01-20 and 03-10. It goes stale on 02-20,
/// so February's quest is missed and March's import completes March's;
/// it goes stale again on 04-10, and May's quest is open.
#[test]
fn a_stale_source_gets_an_import_quest() {
    let texts = [
        "2024-01-04 * A\n  expenses:food  1.00 CAD\n  assets:checking\n\n",
        "2024-01-19 * B\n  expenses:food  2.00 CAD\n  assets:checking\n\n",
        "2024-03-09 * C\n  expenses:food  3.00 CAD\n  assets:checking\n\n",
    ];
    let days = ["2024-01-05", "2024-01-20", "2024-03-10"];
    let segments: Vec<TestSegment<'_>> = days
        .iter()
        .zip(texts)
        .map(|(day, text)| TestSegment {
            source: "card",
            imported: secs(day),
            text,
        })
        .collect();
    let books = books_of(&segments, &Edits::default());
    let mut world = World::new();
    let adventure = world.fold(&books, None);
    assert_eq!(awarded(&adventure), ["monthly:import:card:2024-03"]);
    let may = quest(&adventure, "monthly:import:card:2024-05").unwrap();
    assert_eq!(may.title, "Import card (last import 66 days ago)");
    assert!(!may.done);
    assert_eq!(may.skill, SkillId::Bookkeeping);

    world.tuning.forgotten_days = 60;
    let adventure = world.fold(&books, None);
    assert!(quest(&adventure, "monthly:import:card:2024-05").is_none());
}

/// A goal card from a `cashflow-goal` block.
fn goal(block: &str, done: bool) -> Goal {
    Goal {
        card_ref: "headway:cashflow/test-goal-card".to_owned(),
        board: "cashflow".to_owned(),
        owner: [0; 32],
        card: [1; 32],
        title: "A goal".to_owned(),
        column: if done { "Done" } else { "Todo" }.to_owned(),
        done,
        spec: parse_goal(&format!("```cashflow-goal\n{block}\n```")).unwrap(),
        posted: Vec::new(),
    }
}

/// A goal is an epic quest mirroring its status, and pays its skill in
/// the month its card moved to done.
#[test]
fn a_goal_is_an_epic_quest() {
    let text: String = ["2024-01", "2024-02", "2024-03", "2024-04", "2024-05"]
        .iter()
        .map(|m| salary(m, "1000.00"))
        .collect();
    let books = books(&text);
    let world = World::new();
    let done = goal("metric: income\ntarget: 1000 CAD\nper: month", true);
    let epic = [EpicGoal {
        goal: &done,
        done_at: Some(secs("2024-03-04")),
    }];
    let adventure = world.fold_with(&books, &epic, None);
    assert_eq!(
        awarded(&adventure),
        ["epic:headway:cashflow/test-goal-card"]
    );
    let entry = adventure
        .log
        .iter()
        .find(|e| matches!(e.reason, Reason::Quest { .. }))
        .unwrap();
    assert_eq!(entry.month.to_string(), "2024-03");
    assert_eq!(entry.skill, SkillId::Earning);
    let quest = quest(&adventure, "epic:headway:cashflow/test-goal-card").unwrap();
    assert_eq!(quest.kind, QuestKind::Epic);
    assert_eq!(quest.status, Some(crate::goals::Status::Achieved));
    assert_eq!((quest.progress, quest.target), (100_000.0, 100_000.0));
    assert!(quest.done);

    let open = goal("metric: net\ntarget: 5000 CAD\nper: month", false);
    let epic = [EpicGoal {
        goal: &open,
        done_at: None,
    }];
    let adventure = world.fold_with(&books, &epic, None);
    assert!(awarded(&adventure).is_empty());
    let quest = &adventure.quests[adventure.quests.len() - 1];
    assert!(!quest.done);
    assert_eq!(quest.skill, SkillId::Saving);
    assert_ne!(quest.status, Some(crate::goals::Status::Achieved));
}

/// A goal scoped to a project pays that project's skill.
#[test]
fn a_project_goal_pays_its_project() {
    let text = "2024-01-02 * Sale\n  income:damus  -300.00 CAD\n  assets:checking\n\n";
    let mut world = World::new();
    world.projects = Projects::new([ProjectRoot {
        account: "income:damus".to_owned(),
        project: "damus".to_owned(),
    }]);
    let done = goal(
        "metric: income\nproject: damus\ntarget: 100 CAD\nper: total",
        true,
    );
    let epic = [EpicGoal {
        goal: &done,
        done_at: Some(secs("2024-01-20")),
    }];
    let adventure = world.fold_with(&books(text), &epic, None);
    let damus = SkillId::Project("damus".to_owned());
    assert_eq!(
        xp(&adventure, "2024-01", damus),
        3_000 + world.tuning.goal_xp
    );
}

/// Quest XP is part of the fold, so folding again finds nothing to
/// adjust.
#[test]
fn quest_xp_is_stable_across_folds() {
    let Triaged { books, world } = triage_world(&["2024-05-08", "2024-05-08", "2024-05-09"]);
    let first = world.fold(&books, None);
    let again = world.fold(&books, Some(&first));
    assert_eq!(again.adjustments().count(), 0);
    assert_eq!(first, world.fold(&books, None));
}

/// When `id` unlocked, if it did.
fn unlocked_on(unlocked: &[Unlocked], id: &str) -> Option<Date> {
    unlocked.iter().find(|u| u.id == id).map(|u| u.date)
}

/// Each achievement unlocks at the end of the month that first met it.
#[test]
fn achievements_date_from_history() {
    let text = salary("2024-01", "11000.00")
        + &salary("2024-02", "1000.00")
        + &salary("2024-03", "2000.00")
        + "2024-04-02 * A\n  income:a  -10.00 CAD\n  assets:checking\n\n\
           2024-04-02 * B\n  income:b  -10.00 CAD\n  assets:checking\n\n\
           2024-04-02 * C\n  income:c  -10.00 CAD\n  assets:checking\n\n";
    let adventure = World::new().fold(&books(&text), None);
    let unlocked = achievements::evaluate(&adventure);
    let on = |id| unlocked_on(&unlocked, id);
    assert_eq!(on("income-1k"), Some(date("2024-01-31")));
    assert_eq!(on("income-10k"), Some(date("2024-01-31")));
    assert_eq!(on("income-100k"), None);
    assert_eq!(on("skill-50"), Some(date("2024-01-31")));
    assert_eq!(on("streak-3"), Some(date("2024-03-31")));
    assert_eq!(on("streak-6"), None);
    assert_eq!(
        on("quest-1"),
        Some(date("2024-03-31")),
        "March beat February"
    );
    assert_eq!(on("sources-3"), Some(date("2024-04-30")));
    // Nothing ever waited, so the inbox was empty from the first import.
    assert_eq!(on("inbox-zero"), Some(Date::EPOCH));
    assert_eq!(
        on("first-import"),
        None,
        "imported in 1970, before the books"
    );
    let points: u32 = unlocked
        .iter()
        .map(|u| achievements::get(u.id).unwrap().points)
        .sum();
    assert_eq!(achievements::points(&unlocked), points);
}

/// A month in progress unlocks as of today; stacking counts the running
/// total; a project's first big month counts.
#[test]
fn achievements_in_progress_stacking_and_projects() {
    let text = "2024-02-05 * Buy\n  assets:btc  0.60000000 BTC @ 60000.00 CAD\n  assets:checking  -36000.00 CAD\n\n\
                2024-03-05 * Buy\n  assets:btc  0.50000000 BTC @ 60000.00 CAD\n  assets:checking  -30000.00 CAD\n\n\
                2024-05-02 * Sale\n  income:damus  -1500.00 CAD\n  assets:checking\n\n";
    let mut world = World::new();
    world.projects = Projects::new([ProjectRoot {
        account: "income:damus".to_owned(),
        project: "damus".to_owned(),
    }]);
    let segments = [TestSegment {
        source: "bank",
        imported: secs("2024-02-10"),
        text,
    }];
    let adventure = world.fold(&books_of(&segments, &Edits::default()), None);
    let unlocked = achievements::evaluate(&adventure);
    let on = |id| unlocked_on(&unlocked, id);
    assert_eq!(on("btc-1"), Some(date("2024-03-31")));
    assert_eq!(on("income-1k"), Some(date(TODAY)));
    assert_eq!(on("project-1k"), Some(date(TODAY)));
    assert_eq!(on("first-import"), Some(date("2024-02-29")));
}
