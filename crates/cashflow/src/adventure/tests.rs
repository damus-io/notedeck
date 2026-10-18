//! The fold over small journals: each skill, the streak and its bonus,
//! determinism, and the adjustment a correction logs.

use ledgerdb::{Date, Edits, Override};

use super::*;
use crate::edits_cache::Stamp;
use crate::goals::Goal;
use crate::projects::ProjectRoot;
use crate::test_util::{books, books_with};
use crate::vault_rules::Owner;

/// Today in the tests: May 2024 is the current month, January to April
/// are closed.
pub(super) const TODAY: &str = "2024-05-15";

/// Unix seconds at noon UTC on `date`.
pub(super) fn secs(date: &str) -> u64 {
    u64::from(Date::parse(date).unwrap().days()) * 86_400 + 43_200
}

/// What a fold reads besides the books.
pub(super) struct World {
    pub derive: DeriveInputs,
    pub projects: Projects,
    pub tuning: XpTuning,
}

impl World {
    pub fn new() -> Self {
        World {
            derive: DeriveInputs::from_edits(Edits::default()),
            projects: Projects::default(),
            tuning: XpTuning::default(),
        }
    }

    pub fn fold(&self, books: &Books, previous: Option<&Adventure>) -> Adventure {
        self.fold_with(books, &[], previous)
    }

    /// [`Self::fold`], with `goals` as the epic quests.
    pub fn fold_with(
        &self,
        books: &Books,
        goals: &[Goal],
        previous: Option<&Adventure>,
    ) -> Adventure {
        let inputs = Inputs {
            books,
            home: books.home().unwrap(),
            derive: &self.derive,
            projects: &self.projects,
            zone: Zone::Fixed(0),
            tuning: &self.tuning,
            today: Date::parse(TODAY).unwrap(),
            goals,
        };
        fold(&inputs, previous)
    }
}

/// A month's income, as one salary transaction on its second day.
pub(super) fn salary(month: &str, amount: &str) -> String {
    format!("{month}-02 * Acme\n  income:salary  -{amount} CAD\n  assets:checking\n\n")
}

/// A month's spending, as one grocery transaction on its third day.
pub(super) fn groceries(month: &str, amount: &str) -> String {
    format!("{month}-03 * Grocer\n  expenses:food  {amount} CAD\n  assets:checking\n\n")
}

pub(super) fn xp(adventure: &Adventure, month: &str, id: SkillId) -> u64 {
    let date = Date::parse(&format!("{month}-01")).unwrap();
    let fold = adventure
        .months
        .iter()
        .find(|m| m.month == Month::of(date))
        .unwrap();
    adventure.xp_in(fold, &id)
}

/// Income earns Earning at 10 XP a dollar, a closed month's net earns
/// Saving at 20, and the levels follow the curve.
#[test]
fn income_and_net_earn_xp() {
    let books = books(&(salary("2024-01", "1000.00") + &groceries("2024-01", "400.00")));
    let adventure = World::new().fold(&books, None);
    assert_eq!(xp(&adventure, "2024-01", SkillId::Earning), 10_000);
    assert_eq!(xp(&adventure, "2024-01", SkillId::Saving), 12_000);
    let earning = adventure.skill(&SkillId::Earning).unwrap();
    assert_eq!(earning.xp, 10_000);
    assert_eq!(earning.level, level_for_xp(10_000));
    assert_eq!(earning.xp_this_month, 0);
    assert_eq!(adventure.months.len(), 5);
    assert_eq!(
        adventure.total_level,
        adventure
            .skills
            .iter()
            .map(|s| u32::from(s.level))
            .sum::<u32>()
    );
    let names: Vec<&str> = adventure.skills.iter().map(|s| s.id.name()).collect();
    assert_eq!(
        names,
        ["Earning", "Saving", "Frugality", "Stacking", "Bookkeeping"]
    );
}

/// A month that spends more than it earns pays no Saving, ends the streak
/// and shows the debuff, and costs no XP already earned.
#[test]
fn a_negative_month_earns_nothing_and_breaks_the_streak() {
    let text = salary("2024-01", "1000.00")
        + &salary("2024-02", "1000.00")
        + &salary("2024-03", "100.00")
        + &groceries("2024-03", "500.00");
    let adventure = World::new().fold(&books(&text), None);
    assert_eq!(xp(&adventure, "2024-03", SkillId::Saving), 0);
    assert_eq!(xp(&adventure, "2024-02", SkillId::Saving), 21_000);
    assert_eq!(adventure.months[1].streak, 2);
    assert_eq!(adventure.months[2].streak, 0);
    // April earned nothing either: not positive, so no streak.
    assert_eq!(adventure.streak, Streak::default());
    assert_eq!(adventure.buffs.well_rested, None);
    // The debuff is the last closed month's, and April's net was zero.
    assert_eq!(adventure.buffs.debuff, None);

    let april_loss = text + &groceries("2024-04", "50.00");
    let adventure = World::new().fold(&books(&april_loss), None);
    let debuff = adventure.buffs.debuff.unwrap();
    assert_eq!(debuff.month.to_string(), "2024-04");
    assert_eq!(debuff.net, -5_000);
    assert!(adventure.log.iter().all(|e| e.delta > 0));
}

/// Each positive month behind a month adds 5% to its Earning and Saving.
#[test]
fn the_streak_raises_earning_and_saving() {
    let text: String = ["2024-01", "2024-02", "2024-03", "2024-04", "2024-05"]
        .iter()
        .map(|m| salary(m, "1000.00"))
        .collect();
    let adventure = World::new().fold(&books(&text), None);
    let bonuses: Vec<u32> = adventure.months.iter().map(|m| m.bonus_percent).collect();
    assert_eq!(bonuses, [0, 5, 10, 15, 20]);
    assert_eq!(xp(&adventure, "2024-04", SkillId::Earning), 11_500);
    assert_eq!(xp(&adventure, "2024-04", SkillId::Saving), 23_000);
    // May is in progress: it earns Earning with its bonus, Saving not yet.
    assert_eq!(xp(&adventure, "2024-05", SkillId::Earning), 12_000);
    assert_eq!(xp(&adventure, "2024-05", SkillId::Saving), 0);
    assert_eq!(adventure.streak.months, 4);
    assert_eq!(adventure.streak.since.unwrap().to_string(), "2024-01");
    assert_eq!(
        adventure.buffs.well_rested,
        Some(WellRested { percent: 20 })
    );
    assert_eq!(
        adventure.skill(&SkillId::Earning).unwrap().xp_this_month,
        12_000
    );
}

/// Expenses under the average of the months before pay Frugality; the
/// first month has nothing to compare with.
#[test]
fn spending_under_the_average_earns_frugality() {
    let text = groceries("2024-01", "600.00")
        + &groceries("2024-02", "400.00")
        + &groceries("2024-03", "200.00");
    let adventure = World::new().fold(&books(&text), None);
    assert_eq!(xp(&adventure, "2024-01", SkillId::Frugality), 0);
    assert_eq!(xp(&adventure, "2024-02", SkillId::Frugality), 2_000);
    // Under (600 + 400) / 2 by 300.
    assert_eq!(xp(&adventure, "2024-03", SkillId::Frugality), 3_000);
    // April spent nothing: under 400 by 400.
    assert_eq!(xp(&adventure, "2024-04", SkillId::Frugality), 4_000);
}

/// Net sats into BTC asset accounts stack; moving between own wallets
/// nets out.
#[test]
fn sats_into_assets_stack() {
    let text = "2024-01-05 * Buy\n  assets:btc:hot  0.01000000 BTC @ 60000.00 CAD\n  assets:checking  -600.00 CAD\n\n\
                2024-01-06 * Move\n  assets:btc:cold  0.00500000 BTC\n  assets:btc:hot  -0.00500000 BTC\n\n";
    let adventure = World::new().fold(&books(text), None);
    assert_eq!(adventure.months[0].sats, 1_000_000);
    assert_eq!(xp(&adventure, "2024-01", SkillId::Stacking), 1_000);
}

/// A project with an income account gets a skill from its revenue, and an
/// account carved out to another project counts only there.
#[test]
fn project_skills_follow_attribution() {
    let text = "2024-01-02 * Purple\n  income:damus:purple  -300.00 CAD\n  assets:checking\n\n\
                2024-01-03 * Tip\n  income:damus:tips  -50.00 CAD\n  assets:checking\n\n";
    let mut world = World::new();
    let root = |account: &str, project: &str| ProjectRoot {
        account: account.to_owned(),
        project: project.to_owned(),
    };
    world.projects = Projects::new([
        root("income:damus", "damus"),
        root("income:damus:tips", "personal"),
    ]);
    let adventure = world.fold(&books(text), None);
    let names: Vec<&str> = adventure.skills[SkillId::FIXED.len()..]
        .iter()
        .map(|s| s.id.name())
        .collect();
    assert_eq!(names, ["damus", "personal"]);
    let damus = SkillId::Project("damus".to_owned());
    let personal = SkillId::Project("personal".to_owned());
    assert_eq!(xp(&adventure, "2024-01", damus), 3_000);
    assert_eq!(xp(&adventure, "2024-01", personal), 500);
}

/// Bookkeeping counts the notes behind the books in the month each was
/// written: an override triages, a rule's transactions count up to its
/// cap.
#[test]
fn bookkeeping_counts_edits_by_when_they_were_written() {
    let text = "2024-01-02 * Corner Grocer\n  liabilities:card  -4.50 CAD\n  expenses:unknown\n\n\
                2024-01-03 * Corner Grocer\n  liabilities:card  -5.50 CAD\n  expenses:unknown\n\n\
                2024-01-04 * Grocer Two\n  liabilities:card  -6.50 CAD\n  expenses:unknown\n\n\
                2024-01-05 * Airline\n  liabilities:card  -100.00 CAD\n  expenses:unknown\n\n";
    let mut edits = crate::rules::parse(
        "[[rule]]\naccount = \"expenses:food\"\nmatch = [\"grocer\"]\n",
        std::path::Path::new("r.toml"),
    )
    .unwrap();
    edits.overrides.push(Override {
        record_id: "t:4".to_owned(),
        account: "expenses:travel".to_owned(),
    });
    let mut world = World::new();
    world.tuning.rule_cap = 2;
    world.derive = DeriveInputs::from_edits(edits.clone());
    world.derive.owners = vec![Owner {
        rule: 0,
        pattern: Some(0),
    }];
    world.derive.provenance.rules = vec![Stamp {
        id: "r1".to_owned(),
        created_at: secs("2024-02-10"),
    }];
    world.derive.provenance.overrides = vec![secs("2024-03-10")];
    let adventure = world.fold(&books_with(text, &edits), None);
    let counts = |month: &str| {
        let at = adventure
            .months
            .iter()
            .position(|m| m.month.to_string() == month)
            .unwrap();
        match adventure
            .log
            .iter()
            .find(|e| e.month == adventure.months[at].month && e.skill == SkillId::Bookkeeping)
        {
            Some(XpEntry {
                reason: Reason::Bookkeeping(counts),
                ..
            }) => *counts,
            _ => Bookkeeping::default(),
        }
    };
    assert_eq!(counts("2024-02").ruled, 2, "three matches, capped at two");
    assert_eq!(counts("2024-03").triaged, 1);
    assert_eq!(xp(&adventure, "2024-02", SkillId::Bookkeeping), 10);
    // March's override cleared the last one waiting: that week's triage
    // quest pays too.
    assert_eq!(
        xp(&adventure, "2024-03", SkillId::Bookkeeping),
        10 + world.tuning.weekly_triage_xp
    );
    // The segment was imported in 1970, before the books' months.
    assert_eq!(xp(&adventure, "2024-01", SkillId::Bookkeeping), 0);
}

/// The same books fold the same, every time.
#[test]
fn the_fold_is_deterministic() {
    let text = salary("2024-01", "1000.00")
        + &groceries("2024-02", "300.00")
        + &salary("2024-03", "2500.00");
    let world = World::new();
    let books = books(&text);
    assert_eq!(world.fold(&books, None), world.fold(&books, None));
}

/// Recategorizing income as a transfer lowers the month's Earning, and the
/// fold that sees it logs the drop as an adjustment, which later folds
/// keep without logging it again.
#[test]
fn a_correction_is_logged_as_an_adjustment() {
    // February's income was imported to the default counter account.
    let text = salary("2024-01", "1000.00")
        + "2024-02-02 * Acme\n  income:unknown  -500.00 CAD\n  assets:checking\n\n";
    let world = World::new();
    let before = world.fold(&books_with(&text, &crate::rules::default_edits()), None);
    assert_eq!(before.adjustments().count(), 0);

    let edits = Edits {
        overrides: vec![Override {
            record_id: "t:2".to_owned(),
            account: "transfers".to_owned(),
        }],
        ..crate::rules::default_edits()
    };
    let corrected = books_with(&text, &edits);
    let after = world.fold(&corrected, Some(&before));
    let adjustments: Vec<&XpEntry> = after.adjustments().collect();
    let earning = adjustments
        .iter()
        .find(|e| e.skill == SkillId::Earning)
        .unwrap();
    assert_eq!(earning.month.to_string(), "2024-02");
    assert_eq!(earning.delta, -5_250);
    assert_eq!(earning.reason, Reason::Adjustment { was: 5_250, now: 0 });
    // Saving fell with it.
    assert!(adjustments.iter().any(|e| e.skill == SkillId::Saving));
    assert!(
        after.skill(&SkillId::Earning).unwrap().xp < before.skill(&SkillId::Earning).unwrap().xp
    );

    let again = world.fold(&corrected, Some(&after));
    assert_eq!(again.adjustments().count(), adjustments.len());
    let fresh = world.fold(&corrected, None);
    assert_eq!(
        fresh.adjustments().count(),
        0,
        "a restart has no previous fold"
    );
}

/// A journal with nothing in it still has every fixed skill, at level 1.
#[test]
fn an_empty_journal_is_level_one() {
    let adventure = World::new().fold(&books(&groceries("2024-05", "0.00")), None);
    assert!(adventure.skills.iter().all(|s| s.level == 1));
    assert_eq!(adventure.total_level, 5);
}
