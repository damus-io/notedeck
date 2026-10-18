//! Quests: targets set from where the books stand, which pay XP when the
//! books meet them.
//!
//! Nothing about a quest is stored. Each one is worked out for every
//! period from the data, the same way on every machine, so completing one
//! is a fact about the books and the vault rather than a click someone
//! made:
//!
//! - **Weekly, triage.** Categorize [`XpTuning::weekly_triage`]
//!   transactions in the week (Monday to Sunday, in the synced zone), or
//!   all of them if fewer were waiting ([`super::backlog`]). Pays
//!   Bookkeeping on the day the count is reached.
//! - **Monthly, beat last month.** End the month with more net than the
//!   month before (or any net at all after a loss). Pays Saving when the
//!   month closes, so the current month's is never yet complete.
//! - **Monthly, import a stale source.** A source not imported for
//!   [`XpTuning::stale_days`] gets a quest for each month it is stale in,
//!   which the next import completes; one gone for
//!   [`XpTuning::forgotten_days`] stops getting them. Pays Bookkeeping.
//! - **Epic, a goal.** Each Headway goal ([`crate::goals`]) is an epic
//!   quest, its progress what [`crate::goals::evaluate`] measures. It pays
//!   [`XpTuning::goal_xp`] when the goal's card moves to a terminal column,
//!   in the month it moved, to the skill its metric levels.
//!
//! A quest's id is its kind, what it asks and its period
//! (`weekly:triage:2024-05-13`, `monthly:import:amex:2024-05`,
//! `epic:headway:cashflow/word-id`), so one completed in the past is the
//! same quest whenever the books are folded. [`generate`] returns the
//! current period's quests for the quest log and every completed one's
//! [`Award`] for the fold.

use std::collections::BTreeMap;
use std::fmt;

use headway::event::BoardView;
use ledgerdb::{CommodityId, Date};
use nostrdb_net::NoteId;

use super::backlog::Backlog;
use super::{Month, SkillId, XpTuning};
use crate::Books;
use crate::goals::progress::Unit;
use crate::goals::{self, Goal, Metric, Scope, Status};
use crate::net::NetFlow;
use crate::projects::Projects;
use crate::zone::Zone;

/// A quest's stable name: its kind, what it asks and its period.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QuestId(pub String);

impl fmt::Display for QuestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How long a quest runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestKind {
    /// Monday to Sunday.
    Weekly,
    /// A calendar month.
    Monthly,
    /// Until a goal's card is done.
    Epic,
}

impl QuestKind {
    /// `weekly`, `monthly` or `epic`.
    pub fn name(self) -> &'static str {
        match self {
            QuestKind::Weekly => "weekly",
            QuestKind::Monthly => "monthly",
            QuestKind::Epic => "epic",
        }
    }
}

/// What a quest's progress and target count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestUnit {
    /// Things: transactions, imports.
    Count,
    /// Smallest units of a commodity.
    Amount(CommodityId),
    /// Percentage points, for a savings-rate goal.
    Percent,
}

/// One quest in the current period.
#[derive(Clone, Debug, PartialEq)]
pub struct Quest {
    /// Its stable id.
    pub id: QuestId,
    /// How long it runs.
    pub kind: QuestKind,
    /// What it asks, in a line.
    pub title: String,
    /// How far along it is, in [`Self::unit`].
    pub progress: f64,
    /// What it asks for, in [`Self::unit`]. For an epic quest on a ceiling
    /// (an expenses goal), progress has to stay under it.
    pub target: f64,
    /// What progress and target count.
    pub unit: QuestUnit,
    /// The XP it pays.
    pub reward_xp: u64,
    /// The skill it pays to.
    pub skill: SkillId,
    /// Whether the books have completed it.
    pub done: bool,
    /// An epic quest's goal status, as [`goals::evaluate`] measures it.
    pub status: Option<Status>,
}

/// A completed quest's XP, for the fold to add in its month.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Award {
    /// The month it completed in.
    pub month: Month,
    /// The skill it pays.
    pub skill: SkillId,
    /// How much.
    pub xp: u64,
    /// The quest.
    pub id: QuestId,
    /// What it asked.
    pub title: String,
}

/// A goal, and when its card moved to the terminal column it is in.
#[derive(Clone, Copy, Debug)]
pub struct EpicGoal<'a> {
    /// The goal.
    pub goal: &'a Goal,
    /// Unix seconds its card was placed in its done column, when it is
    /// done ([`done_at`]).
    pub done_at: Option<u64>,
}

/// When `goal`'s card was placed in the terminal column it is in, if it is
/// done and on `view`.
pub fn done_at(view: &BoardView, goal: &Goal) -> Option<u64> {
    if !goal.done {
        return None;
    }
    view.card(NoteId::new(goal.card)).map(|card| card.placed_at)
}

/// The current quests and every completed quest's award.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quests {
    /// This week's, this month's and the epic quests, in that order.
    pub current: Vec<Quest>,
    /// What every completed quest paid, oldest first by kind.
    pub awards: Vec<Award>,
}

/// What [`generate`] reads, mostly from the fold.
pub(super) struct Sources<'a> {
    pub books: &'a Books,
    pub home: CommodityId,
    pub projects: &'a Projects,
    pub zone: Zone,
    pub tuning: &'a XpTuning,
    pub today: Date,
    pub goals: &'a [EpicGoal<'a>],
    /// The fold's first month.
    pub first: Month,
    /// Each month's flow from `first` through the current month.
    pub flows: &'a [NetFlow],
    /// The triage backlog.
    pub backlog: &'a Backlog,
    /// The skills the fold levels, which an award must name.
    pub skills: &'a [SkillId],
}

/// The quests of `sources`'s books: those running now, and the awards of
/// every one completed so far.
#[profiling::function]
pub(super) fn generate(sources: &Sources<'_>) -> Quests {
    let mut quests = Quests::default();
    triage(sources, &mut quests);
    beat_net(sources, &mut quests);
    stale_imports(sources, &mut quests);
    epics(sources, &mut quests);
    quests
}

/// The day `days` after `date`.
fn plus(date: Date, days: u32) -> Date {
    Date::from_days(date.days() + days)
}

/// The Monday of `date`'s week (1970-01-01, a Thursday, for that first
/// week, since no earlier day is representable).
fn monday_of(date: Date) -> Date {
    Date::from_days(date.days().saturating_sub((date.days() + 3) % 7))
}

/// The weekly triage quests, one per week from the backlog's first.
fn triage(sources: &Sources<'_>, quests: &mut Quests) {
    let Sources {
        backlog,
        today,
        tuning,
        ..
    } = *sources;
    let Some(first) = backlog.first_day() else {
        return;
    };
    let this_week = monday_of(today);
    let mut week = monday_of(first);
    while week <= this_week {
        // The next Monday, also from the epoch's short first week.
        let next = monday_of(plus(week, 7));
        let until = next.min(plus(today, 1));
        let days = backlog.days_in(week, until);
        let triaged: u32 = days.iter().map(|d| d.triaged).sum();
        let left = backlog.left_after(Date::from_days(until.days() - 1));
        let target = tuning.weekly_triage.min(triaged + left);
        let id = QuestId(format!("weekly:triage:{week}"));
        let noun = if target == 1 {
            "transaction"
        } else {
            "transactions"
        };
        let title = format!("Triage {target} unknown {noun}");
        let mut so_far = 0;
        let completed = days.iter().find_map(|d| {
            so_far += d.triaged;
            (target > 0 && so_far >= target).then_some(d.day)
        });
        if let Some(day) = completed {
            quests.awards.push(Award {
                month: Month::of(day),
                skill: SkillId::Bookkeeping,
                xp: tuning.weekly_triage_xp,
                id: id.clone(),
                title: title.clone(),
            });
        }
        if week == this_week && target > 0 {
            quests.current.push(Quest {
                id,
                kind: QuestKind::Weekly,
                title: format!("{title} this week"),
                progress: f64::from(triaged.min(target)),
                target: f64::from(target),
                unit: QuestUnit::Count,
                reward_xp: tuning.weekly_triage_xp,
                skill: SkillId::Bookkeeping,
                done: completed.is_some(),
                status: None,
            });
        }
        week = next;
    }
}

/// The monthly "beat last month" quests, one per month after the first.
fn beat_net(sources: &Sources<'_>, quests: &mut Quests) {
    let Sources {
        flows,
        first,
        home,
        tuning,
        ..
    } = *sources;
    for (i, pair) in flows.windows(2).enumerate() {
        let month = Month(first.0 + i as u32 + 1);
        let last = pair[0].net();
        let net = pair[1].net();
        let target = last.max(0);
        let id = QuestId(format!("monthly:beat-net:{month}"));
        let title = "Beat last month's net".to_owned();
        let closed = i + 2 < flows.len();
        let done = closed && net > target;
        if done {
            quests.awards.push(Award {
                month,
                skill: SkillId::Saving,
                xp: tuning.beat_net_xp,
                id: id.clone(),
                title: title.clone(),
            });
        }
        if !closed {
            quests.current.push(Quest {
                id,
                kind: QuestKind::Monthly,
                title,
                progress: net.max(0) as f64,
                target: target as f64,
                unit: QuestUnit::Amount(home),
                reward_xp: tuning.beat_net_xp,
                skill: SkillId::Saving,
                done,
                status: None,
            });
        }
    }
}

/// The monthly import quests, for each source and month it was stale in.
fn stale_imports(sources: &Sources<'_>, quests: &mut Quests) {
    let Sources {
        books, zone, today, ..
    } = *sources;
    let mut imports: BTreeMap<&str, Vec<Date>> = BTreeMap::new();
    for segment in books.ledger().segments() {
        let day = zone.date(segment.header.imported);
        imports.entry(&segment.header.source).or_default().push(day);
    }
    let current = Month::of(today);
    for (source, days) in &mut imports {
        days.sort_unstable();
        days.dedup();
        let mut month = Month::of(days[0]);
        while month <= current {
            stale_month(sources, source, days, month, quests);
            month = Month(month.0 + 1);
        }
    }
}

/// `source`'s import quest in `month`, if it was stale then: `imports` are
/// its import days, sorted.
fn stale_month(
    sources: &Sources<'_>,
    source: &str,
    imports: &[Date],
    month: Month,
    quests: &mut Quests,
) {
    let tuning = sources.tuning;
    let start = month.start();
    let until = Month(month.0 + 1).start().min(plus(sources.today, 1));
    let mut next = imports.partition_point(|&d| d < start);
    let Some(&before) = next.checked_sub(1).and_then(|at| imports.get(at)) else {
        return;
    };
    let mut last = before;
    let id = QuestId(format!("monthly:import:{source}:{month}"));
    let title = format!("Import {source}");
    loop {
        let stale = plus(last, tuning.stale_days + 1);
        if stale >= until {
            return;
        }
        match imports.get(next).copied().filter(|&d| d < until) {
            Some(day) if day < stale => {
                last = day;
                next += 1;
            }
            Some(_) => {
                quests.awards.push(Award {
                    month,
                    skill: SkillId::Bookkeeping,
                    xp: tuning.stale_import_xp,
                    id: id.clone(),
                    title: title.clone(),
                });
                if month == Month::of(sources.today) {
                    quests.current.push(import_quest(id, title, true, tuning));
                }
                return;
            }
            None => {
                let gap = sources.today.days().saturating_sub(last.days());
                if month == Month::of(sources.today) && gap <= tuning.forgotten_days {
                    let title = format!("{title} (last import {gap} days ago)");
                    quests.current.push(import_quest(id, title, false, tuning));
                }
                return;
            }
        }
    }
}

/// A current import quest.
fn import_quest(id: QuestId, title: String, done: bool, tuning: &XpTuning) -> Quest {
    Quest {
        id,
        kind: QuestKind::Monthly,
        title,
        progress: if done { 1.0 } else { 0.0 },
        target: 1.0,
        unit: QuestUnit::Count,
        reward_xp: tuning.stale_import_xp,
        skill: SkillId::Bookkeeping,
        done,
        status: None,
    }
}

/// The epic quests: each goal whose block reads, measured against the
/// books, and the award of each one done.
fn epics(sources: &Sources<'_>, quests: &mut Quests) {
    let Sources {
        books,
        projects,
        zone,
        today,
        tuning,
        ..
    } = *sources;
    let inputs = goals::progress::Inputs {
        books,
        projects,
        today,
    };
    for epic in sources.goals {
        let goal = epic.goal;
        let Ok(spec) = &goal.spec else {
            continue;
        };
        let skill = goal_skill(spec.metric, spec.scope.as_ref(), sources.skills);
        let id = QuestId(format!("epic:{}", goal.card_ref));
        if goal.done {
            let month = epic.done_at.map_or(Month::of(today), |at| {
                Month::of(zone.date(at)).min(Month::of(today))
            });
            quests.awards.push(Award {
                month,
                skill: skill.clone(),
                xp: tuning.goal_xp,
                id: id.clone(),
                title: goal.title.clone(),
            });
        }
        let Ok(progress) = goals::evaluate(&inputs, spec) else {
            continue;
        };
        quests.current.push(Quest {
            id,
            kind: QuestKind::Epic,
            title: goal.title.clone(),
            progress: progress.current,
            target: progress.target,
            unit: match progress.unit {
                Unit::Amount(commodity) => QuestUnit::Amount(commodity),
                Unit::Percent => QuestUnit::Percent,
            },
            reward_xp: tuning.goal_xp,
            skill,
            done: goal.done,
            status: Some(progress.status),
        });
    }
}

/// The skill a goal pays: its project's, when it is scoped to a project
/// the fold levels, else the one its metric moves.
fn goal_skill(metric: Metric, scope: Option<&Scope>, skills: &[SkillId]) -> SkillId {
    if let Some(Scope::Project(project)) = scope {
        let skill = SkillId::Project(project.clone());
        if skills.contains(&skill) {
            return skill;
        }
    }
    match metric {
        Metric::Income => SkillId::Earning,
        Metric::Expenses => SkillId::Frugality,
        Metric::Net | Metric::SavingsRate | Metric::Balance => SkillId::Saving,
    }
}
