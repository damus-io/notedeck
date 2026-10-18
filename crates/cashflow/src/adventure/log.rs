//! The XP log: what each month earned in each skill and why, and the
//! corrections that took some of it back.
//!
//! Earned entries are re-derived with the rest of the fold, so they always
//! agree with the levels. An **adjustment** is different: it records that
//! a re-derive lowered a month's XP (a transaction recategorized from
//! income to a transfer, a segment retired), which the earned entries
//! alone would hide, since they only ever show the new figure. Nothing
//! persists the old figure, so an adjustment is found by comparing the
//! fold with the one before it in memory ([`adjustments`]) and is carried
//! from fold to fold from then on. A restart starts with no previous fold,
//! so corrections made while the app was closed lower the levels without
//! an entry saying so.

use std::fmt::Write as _;

use super::figures::Bookkeeping;
use super::{Adventure, Month, SkillId, XpTuning};

/// How many adjustments a fold keeps, newest last, so a long session's log
/// stays bounded.
pub const MAX_ADJUSTMENTS: usize = 200;

/// One change to a skill's XP in one month.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XpEntry {
    /// The month whose XP it is.
    pub month: Month,
    /// The skill.
    pub skill: SkillId,
    /// The XP it adds, or takes away for an adjustment.
    pub delta: i64,
    /// What earned it.
    pub reason: Reason,
}

/// What an [`XpEntry`] was for. Amounts are in the home commodity's
/// smallest units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Earning: the month's income, raised by the Well Rested bonus.
    Income { income: i128, bonus_percent: u32 },
    /// Saving: the closed month's net, raised by the Well Rested bonus.
    Saved { net: i128, bonus_percent: u32 },
    /// Frugality: the closed month's expenses, under the average of the
    /// months before it.
    UnderAverage { expenses: i128, average: i128 },
    /// Stacking: net sats into the BTC asset accounts.
    Stacked { sats: i128 },
    /// Bookkeeping: the vault edits written that month.
    Bookkeeping(Bookkeeping),
    /// A project skill: the project's revenue.
    ProjectRevenue { revenue: i128 },
    /// A re-derive lowered the month's XP from `was` to `now`.
    Adjustment { was: u64, now: u64 },
}

impl Reason {
    /// Whether this is a correction rather than XP earned.
    pub fn is_adjustment(&self) -> bool {
        matches!(self, Reason::Adjustment { .. })
    }

    /// Where the XP came from, in a line: "income CAD 421.00 × 10". `money`
    /// formats an amount in the home commodity.
    pub fn describe(&self, tuning: &XpTuning, money: &dyn Fn(i128) -> String) -> String {
        let bonus = |percent: u32| match percent {
            0 => String::new(),
            p => format!(", +{p}% Well Rested"),
        };
        match *self {
            Reason::Income {
                income,
                bonus_percent,
            } => format!(
                "income {} × {}{}",
                money(income),
                tuning.earning_per_unit,
                bonus(bonus_percent)
            ),
            Reason::Saved { net, bonus_percent } => format!(
                "net {} × {}{}",
                money(net),
                tuning.saving_per_unit,
                bonus(bonus_percent)
            ),
            Reason::UnderAverage { expenses, average } => format!(
                "spent {}, {} under the {}-month average, × {}",
                money(expenses),
                money(average - expenses),
                tuning.frugality_months,
                tuning.frugality_per_unit
            ),
            Reason::Stacked { sats } => {
                format!("{sats} sats stacked ÷ {}", tuning.sats_per_xp)
            }
            Reason::Bookkeeping(counts) => describe_bookkeeping(counts),
            Reason::ProjectRevenue { revenue } => {
                format!("revenue {} × {}", money(revenue), tuning.project_per_unit)
            }
            Reason::Adjustment { was, now } => {
                format!("corrected: the books changed, {was} → {now} XP")
            }
        }
    }
}

/// "3 triaged, 40 by rules, 2 linked, 1 import".
fn describe_bookkeeping(counts: Bookkeeping) -> String {
    let parts = [
        (counts.triaged, "triaged", "triaged"),
        (counts.ruled, "by rules", "by rules"),
        (counts.linked, "linked", "linked"),
        (counts.imports, "import", "imports"),
    ];
    let mut out = String::new();
    for (count, one, many) in parts {
        if count == 0 {
            continue;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        let noun = if count == 1 { one } else { many };
        let _ = write!(out, "{count} {noun}");
    }
    out
}

/// Each month and skill whose XP `next` holds below what `previous` did,
/// as an adjustment entry, oldest month first and in skill order within
/// one. A skill `next` no longer has (a project renamed away) is lowered
/// to nothing.
pub(super) fn adjustments(previous: &Adventure, next: &Adventure) -> Vec<XpEntry> {
    let mut out = Vec::new();
    for before in &previous.months {
        let after = next.months.iter().find(|m| m.month == before.month);
        for (skill, &was) in previous.skills.iter().zip(&before.xp) {
            let now = after.map_or(0, |after| next.xp_in(after, &skill.id));
            if now >= was {
                continue;
            }
            out.push(XpEntry {
                month: before.month,
                skill: skill.id.clone(),
                delta: i64::try_from(now).unwrap_or(i64::MAX)
                    - i64::try_from(was).unwrap_or(i64::MAX),
                reason: Reason::Adjustment { was, now },
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the counts that moved are named, singular or plural.
    #[test]
    fn bookkeeping_names_what_moved() {
        let counts = Bookkeeping {
            triaged: 3,
            ruled: 0,
            linked: 2,
            imports: 1,
        };
        assert_eq!(
            describe_bookkeeping(counts),
            "3 triaged, 2 linked, 1 import"
        );
    }

    /// The tooltip line for Earning.
    #[test]
    fn income_reads_with_its_rate() {
        let money = |qty: i128| format!("CAD {}.{:02}", qty / 100, qty % 100);
        let reason = Reason::Income {
            income: 42_100,
            bonus_percent: 10,
        };
        assert_eq!(
            reason.describe(&XpTuning::default(), &money),
            "income CAD 421.00 × 10, +10% Well Rested"
        );
    }
}
