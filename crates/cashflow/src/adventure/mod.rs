//! The Adventure's core: skills levelled by the books, on the OSRS curve.
//!
//! [`fold`] reads the derived books and the vault edits behind them month
//! by month and works out each skill's XP, with no clock, no randomness and
//! nothing stored, so every machine holding the same journal and notes
//! shows the same levels. The skills, per the design on
//! headway:cashflow/road-valve-game (rates in [`XpTuning`]):
//!
//! - **Earning**: income, as [`crate::net`] totals it for the Overview.
//! - **Saving**: a closed month's net. A month that spent more than it
//!   earned pays nothing: performance never costs XP.
//! - **Frugality**: a closed month's expenses under the average of the
//!   months before it.
//! - **Stacking**: net sats into the BTC asset accounts.
//! - **Bookkeeping**: the vault edits, in the month each note was written
//!   (its `created_at`, dated in the synced zone): transactions triaged by
//!   an override or split, categorized by a rule (capped per rule),
//!   confirmed as transfers, and each source imported that month.
//! - **One skill per project** with an income account in the chart
//!   ([`crate::projects`]): that project's revenue.
//!
//! Saving and Frugality are paid when a month closes, since a month in
//! progress hasn't decided either yet; the other skills count the current
//! month to date. A run of closed months with positive net is a
//! **streak**, and a month starts **Well Rested** by
//! [`XpTuning::streak_bonus_percent`] per month of the streak behind it,
//! up to [`XpTuning::streak_cap_percent`], raising its Earning and Saving.
//! A month that wasn't positive ends the streak; a negative one shows as a
//! cosmetic debuff and costs nothing else.
//!
//! Each month's XP per skill is kept ([`Adventure::months`]) so the
//! achievements can read the history, and the [`log`] says what earned it.
//! A correction that lowers a past month's XP is logged as an adjustment
//! when the fold before it is passed in (see [`log`] for what a restart
//! loses).

pub mod curve;
mod figures;
pub mod log;
pub mod tuning;

use std::fmt;

use ledgerdb::{CommodityId, Date};

use crate::net::NetFlow;
use crate::projects::Projects;
use crate::zone::Zone;
use crate::{Books, DeriveInputs};

pub use curve::{MAX_LEVEL, level_for_xp, xp_for_level, xp_to_next_level};
pub use figures::{ASSETS, BTC, Bookkeeping};
pub use log::{Reason, XpEntry};
pub use tuning::XpTuning;

use figures::{Figures, Sources};

/// Where each fixed skill sits in [`SkillId::FIXED`], and so in
/// [`Adventure::skills`] and every [`MonthFold::xp`].
const EARNING: usize = 0;
const SAVING: usize = 1;
const FRUGALITY: usize = 2;
const STACKING: usize = 3;
const BOOKKEEPING: usize = 4;
use log::MAX_ADJUSTMENTS;
use tuning::xp_of;

/// A calendar month, as months since year 0 ([`Date::month_index`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Month(pub u32);

impl Month {
    /// The month `date` is in.
    pub fn of(date: Date) -> Month {
        Month(date.month_index())
    }

    /// The month's first day.
    pub fn start(self) -> Date {
        Date::first_of_month(self.0)
    }
}

/// `2024-03`.
impl fmt::Display for Month {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.0 / 12, self.0 % 12 + 1)
    }
}

/// A skill.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SkillId {
    /// Income.
    Earning,
    /// Positive net.
    Saving,
    /// Spending under the recent average.
    Frugality,
    /// Bitcoin stacked.
    Stacking,
    /// Keeping the books.
    Bookkeeping,
    /// A project's revenue, by the project's name in the chart of
    /// accounts.
    Project(String),
}

impl SkillId {
    /// The skills every adventure has, in the order they are listed.
    pub const FIXED: [SkillId; 5] = [
        SkillId::Earning,
        SkillId::Saving,
        SkillId::Frugality,
        SkillId::Stacking,
        SkillId::Bookkeeping,
    ];

    /// The skill's name: a fixed skill's, or the project's.
    pub fn name(&self) -> &str {
        match self {
            SkillId::Earning => "Earning",
            SkillId::Saving => "Saving",
            SkillId::Frugality => "Frugality",
            SkillId::Stacking => "Stacking",
            SkillId::Bookkeeping => "Bookkeeping",
            SkillId::Project(name) => name,
        }
    }
}

/// One skill's standing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// Which skill.
    pub id: SkillId,
    /// Every month's XP, summed.
    pub xp: u64,
    /// [`level_for_xp`] of [`Self::xp`].
    pub level: u8,
    /// What the current month has earned so far.
    pub xp_this_month: u64,
}

impl Skill {
    /// The XP still needed for the next level, or `None` at
    /// [`MAX_LEVEL`].
    pub fn xp_to_next_level(&self) -> Option<u64> {
        xp_to_next_level(self.xp)
    }
}

/// One month of the fold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonthFold {
    /// Which month.
    pub month: Month,
    /// Whether it has ended: before the month `today` is in.
    pub closed: bool,
    /// Its income, expenses and net.
    pub flow: NetFlow,
    /// Net sats into the BTC asset accounts.
    pub sats: i128,
    /// The Well Rested bonus it started with, in percent.
    pub bonus_percent: u32,
    /// The streak of positive closed months ending with it (0 when it
    /// wasn't positive); for the current month, the streak behind it.
    pub streak: u32,
    /// The XP it earned in each of [`Adventure::skills`], in that order.
    pub xp: Vec<u64>,
}

/// The run of positive-net closed months up to the current one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Streak {
    /// How many months long it is.
    pub months: u32,
    /// The first month of it, when it is running.
    pub since: Option<Month>,
}

/// The Well Rested bonus the current month has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WellRested {
    /// The bonus to Earning and Saving, in percent.
    pub percent: u32,
}

/// The last closed month spent more than it earned: cosmetic only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Debuff {
    /// The month.
    pub month: Month,
    /// Its net, negative, in home units.
    pub net: i128,
}

/// What is on the character now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Buffs {
    /// The streak's bonus, while one runs.
    pub well_rested: Option<WellRested>,
    /// A negative last month.
    pub debuff: Option<Debuff>,
}

/// Every skill, levelled from the books.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adventure {
    /// The fixed skills in [`SkillId::FIXED`] order, then one per project,
    /// by name.
    pub skills: Vec<Skill>,
    /// The sum of the levels.
    pub total_level: u32,
    /// The positive-net run behind the current month.
    pub streak: Streak,
    /// The current month's buff and debuff.
    pub buffs: Buffs,
    /// Every month from the journal's first through the current one.
    pub months: Vec<MonthFold>,
    /// XP earned, month by month and in skill order within one, then the
    /// adjustments, in the order the folds found them.
    pub log: Vec<XpEntry>,
}

impl Adventure {
    /// The skill `id`, if this adventure has it.
    pub fn skill(&self, id: &SkillId) -> Option<&Skill> {
        self.skills.iter().find(|s| &s.id == id)
    }

    /// What `month` earned in skill `id`: nothing for a skill this
    /// adventure doesn't have.
    pub fn xp_in(&self, month: &MonthFold, id: &SkillId) -> u64 {
        self.skills
            .iter()
            .position(|s| &s.id == id)
            .and_then(|at| month.xp.get(at).copied())
            .unwrap_or(0)
    }

    /// The adjustments in [`Self::log`].
    pub fn adjustments(&self) -> impl Iterator<Item = &XpEntry> {
        self.log.iter().filter(|e| e.reason.is_adjustment())
    }
}

/// What [`fold`] reads.
pub struct Inputs<'a> {
    /// The derived books.
    pub books: &'a Books,
    /// The commodity money is counted in.
    pub home: CommodityId,
    /// The vault edits the books were derived with, for Bookkeeping.
    pub derive: &'a DeriveInputs,
    /// The chart's project attribution, for the project skills.
    pub projects: &'a Projects,
    /// The zone a note's `created_at` is dated in: the synced
    /// [`crate::settings::Setting::Timezone`].
    pub zone: Zone,
    /// The XP rates.
    pub tuning: &'a XpTuning,
    /// Today: its month is the current one, and the months before it are
    /// closed.
    pub today: Date,
}

/// Level every skill from `inputs`, month by month. With `previous`, the
/// fold last made on this machine, a month whose XP went down since is
/// logged as an adjustment, and the adjustments `previous` already logged
/// are kept.
#[profiling::function]
pub fn fold(inputs: &Inputs<'_>, previous: Option<&Adventure>) -> Adventure {
    let tuning = inputs.tuning;
    let current = Month::of(inputs.today);
    let first = inputs
        .books
        .query()
        .span()
        .map_or(current, |span| Month::of(span.start).min(current));
    let len = (current.0 - first.0 + 1) as usize;
    let sources = Sources {
        books: inputs.books,
        home: inputs.home,
        derive: inputs.derive,
        projects: inputs.projects,
        zone: inputs.zone,
        rule_cap: tuning.rule_cap,
    };
    let figures = Figures::gather(&sources, first, len);
    let precision = inputs
        .books
        .ledger()
        .journal()
        .commodities
        .precision(inputs.home);

    let mut ids: Vec<SkillId> = SkillId::FIXED.to_vec();
    ids.extend(
        figures
            .projects
            .iter()
            .map(|p| SkillId::Project(p.project.clone())),
    );
    let mut months = Vec::with_capacity(len);
    let mut log = Vec::new();
    let mut streak = Streak::default();
    let mut debuff = None;
    for i in 0..len {
        let month = Month(first.0 + i as u32);
        let closed = month < current;
        let flow = figures.flows[i];
        let bonus_percent = tuning.bonus_percent(streak.months);
        let mut earned = Earned {
            ids: &ids,
            month,
            xp: vec![0; ids.len()],
            log: &mut log,
        };

        earned.add(
            EARNING,
            xp_of(
                flow.income,
                precision,
                tuning.earning_per_unit,
                bonus_percent,
            ),
            Reason::Income {
                income: flow.income,
                bonus_percent,
            },
        );
        if closed {
            earned.add(
                SAVING,
                xp_of(flow.net(), precision, tuning.saving_per_unit, bonus_percent),
                Reason::Saved {
                    net: flow.net(),
                    bonus_percent,
                },
            );
            let window = i.saturating_sub(tuning.frugality_months as usize)..i;
            if !window.is_empty() {
                let average = figures.flows[window.clone()]
                    .iter()
                    .map(|f| f.expenses)
                    .sum::<i128>()
                    / window.len() as i128;
                earned.add(
                    FRUGALITY,
                    xp_of(
                        average - flow.expenses,
                        precision,
                        tuning.frugality_per_unit,
                        0,
                    ),
                    Reason::UnderAverage {
                        expenses: flow.expenses,
                        average,
                    },
                );
            }
        }
        let sats = figures.sats[i];
        earned.add(
            STACKING,
            u64::try_from(sats.max(0) / i128::from(tuning.sats_per_xp.max(1))).unwrap_or(u64::MAX),
            Reason::Stacked { sats },
        );
        let counts = figures.bookkeeping[i];
        earned.add(
            BOOKKEEPING,
            bookkeeping_xp(counts, tuning),
            Reason::Bookkeeping(counts),
        );
        for (p, project) in figures.projects.iter().enumerate() {
            let revenue = project.months[i];
            earned.add(
                SkillId::FIXED.len() + p,
                xp_of(revenue, precision, tuning.project_per_unit, 0),
                Reason::ProjectRevenue { revenue },
            );
        }

        if closed {
            if flow.net() > 0 {
                streak.since.get_or_insert(month);
                streak.months += 1;
            } else {
                streak = Streak::default();
            }
            debuff = (flow.net() < 0).then_some(Debuff {
                month,
                net: flow.net(),
            });
        }
        months.push(MonthFold {
            month,
            closed,
            flow,
            sats,
            bonus_percent,
            streak: streak.months,
            xp: earned.xp,
        });
    }

    let skills: Vec<Skill> = ids
        .into_iter()
        .enumerate()
        .map(|(s, id)| {
            let xp = months.iter().map(|m| m.xp[s]).sum();
            Skill {
                id,
                xp,
                level: level_for_xp(xp),
                xp_this_month: months.last().map_or(0, |m| m.xp[s]),
            }
        })
        .collect();
    let percent = tuning.bonus_percent(streak.months);
    let mut adventure = Adventure {
        total_level: skills.iter().map(|s| u32::from(s.level)).sum(),
        skills,
        streak,
        buffs: Buffs {
            well_rested: (percent > 0).then_some(WellRested { percent }),
            debuff,
        },
        months,
        log,
    };
    if let Some(previous) = previous {
        let mut kept: Vec<XpEntry> = previous.adjustments().cloned().collect();
        kept.extend(log::adjustments(previous, &adventure));
        let drop = kept.len().saturating_sub(MAX_ADJUSTMENTS);
        adventure.log.extend(kept.into_iter().skip(drop));
    }
    adventure
}

/// A month's Bookkeeping XP from its counts.
fn bookkeeping_xp(counts: Bookkeeping, tuning: &XpTuning) -> u64 {
    u64::from(counts.triaged) * tuning.triage_xp
        + u64::from(counts.ruled) * tuning.rule_xp
        + u64::from(counts.linked) * tuning.link_xp
        + u64::from(counts.imports) * tuning.import_xp
}

/// One month's XP as it is added up, logging each skill that earned any.
struct Earned<'a> {
    ids: &'a [SkillId],
    month: Month,
    xp: Vec<u64>,
    log: &'a mut Vec<XpEntry>,
}

impl Earned<'_> {
    /// Credit `xp` to the skill at `at`, for `reason`, if it is any.
    fn add(&mut self, at: usize, xp: u64, reason: Reason) {
        if xp == 0 {
            return;
        }
        self.xp[at] += xp;
        self.log.push(XpEntry {
            month: self.month,
            skill: self.ids[at].clone(),
            delta: i64::try_from(xp).unwrap_or(i64::MAX),
            reason,
        });
    }
}

#[cfg(test)]
mod tests;
