//! Achievements: milestones the books reach once, each with points and the
//! day it was first reached.
//!
//! The definitions are data ([`ALL`]), generic to anybody's books: no
//! merchant, project or account of anyone's is named. [`evaluate`] reads
//! them off the fold's month-by-month history, so an achievement's unlock
//! date is when its condition first held historically, not when this
//! machine first noticed, and every machine dates it the same.
//!
//! The fold keeps its figures by month, so a condition on a month's figures
//! is met at the end of the first month that reaches it (as of today for
//! the current month). Inbox zero is the exception: it is dated by the day
//! ([`Adventure::inbox_zero`]).
//!
//! Since the history is derived, a correction to the books can move an
//! unlock date or take an achievement back, the way it can lower XP.

use ledgerdb::Date;

use super::{Adventure, MonthFold, Reason, level_for_xp};

/// One achievement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Achievement {
    /// Its stable id.
    pub id: &'static str,
    /// Its name.
    pub name: &'static str,
    /// What reaching it takes, in a line.
    pub description: &'static str,
    /// What it is worth.
    pub points: u32,
    /// An emoji for it.
    pub icon: &'static str,
    /// When it is reached.
    pub condition: Condition,
}

/// What reaching an achievement takes. Amounts are in whole units of the
/// home commodity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    /// A source imported.
    FirstImport,
    /// No transaction left to categorize, once any had been waiting.
    InboxZero,
    /// A month's income at least this much.
    IncomeMonth { units: i128 },
    /// This many positive-net closed months in a row.
    Streak { months: u32 },
    /// Revenue into at least this many income accounts in one month.
    IncomeSources { count: u32 },
    /// At least this many sats stacked, net, since the books began.
    Stacked { sats: i128 },
    /// A project's revenue in one month at least this much.
    ProjectMonth { units: i128 },
    /// Any skill at this level.
    SkillLevel { level: u8 },
    /// The total level at least this.
    TotalLevel { level: u32 },
    /// At least this many quests completed.
    Quests { count: u32 },
}

/// Every achievement, in the order the view lists them.
pub const ALL: &[Achievement] = &[
    Achievement {
        id: "first-import",
        name: "First Haul",
        description: "Import a statement",
        points: 10,
        icon: "📥",
        condition: Condition::FirstImport,
    },
    Achievement {
        id: "inbox-zero",
        name: "Inbox Zero",
        description: "Categorize every waiting transaction",
        points: 25,
        icon: "📭",
        condition: Condition::InboxZero,
    },
    Achievement {
        id: "income-1k",
        name: "Four Figures",
        description: "Bring in 1,000 in a month",
        points: 10,
        icon: "💵",
        condition: Condition::IncomeMonth { units: 1_000 },
    },
    Achievement {
        id: "income-10k",
        name: "Five Figures",
        description: "Bring in 10,000 in a month",
        points: 25,
        icon: "💰",
        condition: Condition::IncomeMonth { units: 10_000 },
    },
    Achievement {
        id: "income-100k",
        name: "Six Figures",
        description: "Bring in 100,000 in a month",
        points: 50,
        icon: "🏦",
        condition: Condition::IncomeMonth { units: 100_000 },
    },
    Achievement {
        id: "streak-3",
        name: "Hat Trick",
        description: "Three positive months in a row",
        points: 10,
        icon: "🔥",
        condition: Condition::Streak { months: 3 },
    },
    Achievement {
        id: "streak-6",
        name: "Half a Year in the Green",
        description: "Six positive months in a row",
        points: 25,
        icon: "🌿",
        condition: Condition::Streak { months: 6 },
    },
    Achievement {
        id: "streak-12",
        name: "A Year in the Green",
        description: "Twelve positive months in a row",
        points: 50,
        icon: "🌳",
        condition: Condition::Streak { months: 12 },
    },
    Achievement {
        id: "sources-3",
        name: "Diversified",
        description: "Revenue from three income accounts in one month",
        points: 25,
        icon: "🧺",
        condition: Condition::IncomeSources { count: 3 },
    },
    Achievement {
        id: "btc-1",
        name: "Whole Coin",
        description: "Stack 1 BTC",
        points: 50,
        icon: "🪙",
        condition: Condition::Stacked { sats: 100_000_000 },
    },
    Achievement {
        id: "project-1k",
        name: "It Pays",
        description: "A project brings in 1,000 in a month",
        points: 25,
        icon: "🚀",
        condition: Condition::ProjectMonth { units: 1_000 },
    },
    Achievement {
        id: "skill-50",
        name: "Journeyman",
        description: "Any skill to level 50",
        points: 25,
        icon: "⚒️",
        condition: Condition::SkillLevel { level: 50 },
    },
    Achievement {
        id: "skill-99",
        name: "Mastery",
        description: "Any skill to level 99",
        points: 100,
        icon: "👑",
        condition: Condition::SkillLevel { level: 99 },
    },
    Achievement {
        id: "total-100",
        name: "Centurion",
        description: "Total level 100",
        points: 25,
        icon: "🛡️",
        condition: Condition::TotalLevel { level: 100 },
    },
    Achievement {
        id: "quest-1",
        name: "Errand Runner",
        description: "Complete a quest",
        points: 10,
        icon: "📜",
        condition: Condition::Quests { count: 1 },
    },
    Achievement {
        id: "quest-50",
        name: "Seasoned",
        description: "Complete 50 quests",
        points: 50,
        icon: "🗺️",
        condition: Condition::Quests { count: 50 },
    },
];

/// The achievement `id`, if there is one.
pub fn get(id: &str) -> Option<&'static Achievement> {
    ALL.iter().find(|a| a.id == id)
}

/// An achievement reached, and the day it first was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unlocked {
    /// Its [`Achievement::id`].
    pub id: &'static str,
    /// The day its condition first held.
    pub date: Date,
}

/// The achievements in [`ALL`] that `adventure`'s history reaches, in
/// [`ALL`]'s order.
#[profiling::function]
pub fn evaluate(adventure: &Adventure) -> Vec<Unlocked> {
    let history = History::of(adventure);
    ALL.iter()
        .filter_map(|a| {
            Some(Unlocked {
                id: a.id,
                date: history.first_met(a.condition)?,
            })
        })
        .collect()
}

/// The sum of the points of `unlocked`.
pub fn points(unlocked: &[Unlocked]) -> u32 {
    unlocked
        .iter()
        .filter_map(|u| get(u.id))
        .map(|a| a.points)
        .sum()
}

/// What the conditions read, worked out once per evaluation: the running
/// totals a condition on "so far" compares with, month by month.
struct History<'a> {
    adventure: &'a Adventure,
    /// One smallest unit's worth of a whole home unit.
    scale: i128,
    /// Sats stacked through each month.
    stacked: Vec<i128>,
    /// The highest skill level at each month's end.
    best_level: Vec<u8>,
    /// The total level at each month's end.
    total_level: Vec<u32>,
    /// Quests completed through each month.
    quests: Vec<u32>,
}

impl<'a> History<'a> {
    fn of(adventure: &'a Adventure) -> Self {
        let months = &adventure.months;
        let mut xp = vec![0u64; adventure.skills.len()];
        let mut stacked = Vec::with_capacity(months.len());
        let mut best_level = Vec::with_capacity(months.len());
        let mut total_level = Vec::with_capacity(months.len());
        let mut sats = 0;
        for month in months {
            sats += month.sats;
            stacked.push(sats);
            for (total, earned) in xp.iter_mut().zip(&month.xp) {
                *total += earned;
            }
            let levels = xp.iter().map(|&x| level_for_xp(x));
            best_level.push(levels.clone().max().unwrap_or(1));
            total_level.push(levels.map(u32::from).sum());
        }
        let mut quests = vec![0u32; months.len()];
        let first = months.first().map_or(0, |m| m.month.0);
        for entry in &adventure.log {
            if !matches!(entry.reason, Reason::Quest { .. }) {
                continue;
            }
            if let Some(count) = quests.get_mut(entry.month.0.wrapping_sub(first) as usize) {
                *count += 1;
            }
        }
        let mut done = 0;
        for count in &mut quests {
            done += *count;
            *count = done;
        }
        History {
            adventure,
            scale: 10i128.pow(u32::from(adventure.precision)),
            stacked,
            best_level,
            total_level,
            quests,
        }
    }

    /// The day `condition` first held.
    fn first_met(&self, condition: Condition) -> Option<Date> {
        let months = &self.adventure.months;
        let at = match condition {
            Condition::InboxZero => return self.adventure.inbox_zero,
            Condition::FirstImport => months.iter().position(|m| m.bookkeeping.imports > 0),
            Condition::IncomeMonth { units } => months
                .iter()
                .position(|m| m.flow.income >= units * self.scale),
            Condition::Streak { months: run } => months.iter().position(|m| m.streak >= run),
            Condition::IncomeSources { count } => {
                months.iter().position(|m| m.income_sources >= count)
            }
            Condition::Stacked { sats } => self.stacked.iter().position(|&s| s >= sats),
            Condition::ProjectMonth { units } => self.first_project_month(units * self.scale),
            Condition::SkillLevel { level } => self.best_level.iter().position(|&l| l >= level),
            Condition::TotalLevel { level } => self.total_level.iter().position(|&l| l >= level),
            Condition::Quests { count } => self.quests.iter().position(|&q| q >= count),
        }?;
        Some(self.met_on(&months[at]))
    }

    /// The first month any project's revenue reached `qty`.
    fn first_project_month(&self, qty: i128) -> Option<usize> {
        let month = self
            .adventure
            .log
            .iter()
            .filter(|e| matches!(e.reason, Reason::ProjectRevenue { revenue } if revenue >= qty))
            .map(|e| e.month)
            .min()?;
        self.adventure.months.iter().position(|m| m.month == month)
    }

    /// The day a condition on `month`'s figures is met: its last day, or
    /// today for the month in progress.
    fn met_on(&self, month: &MonthFold) -> Date {
        if !month.closed {
            return self.adventure.as_of;
        }
        let next = super::Month(month.month.0 + 1).start();
        Date::from_days(next.days() - 1)
    }
}
