//! The Adventure view's figures: the books folded into skills, quests and
//! achievements ([`cashflow::adventure`]), and every number on the view
//! written out with where it came from, so drawing a frame formats nothing.
//!
//! [`report`] runs the fold once. It is called only when the books, the
//! goals, the time zone or today change, and it hands the fold back in
//! [`Report::adventure`] so the next one can log what a correction took
//! away ([`cashflow::adventure::log`]).

use cashflow::adventure::achievements;
use cashflow::adventure::seen::Seen;
use cashflow::adventure::{
    self, Adventure, MAX_LEVEL, QuestKind, QuestUnit, Reason, SkillId, XpEntry, XpTuning,
    xp_for_level,
};
use cashflow::goals::{Goal, Status};
use cashflow::projects::Projects;
use cashflow::zone::Zone;
use cashflow::{Books, DeriveInputs};
use ledgerdb::{Commodities, CommodityId, Date};

use crate::money::money;
use crate::ui::hud::{Grouped, XpProgress};
use crate::views::goals::model::day_label;
use crate::views::trends::model::month_label;

/// The most XP log rows the view lists, newest first.
pub const LOG_ROWS: usize = 40;

/// What [`report`] reads.
#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    /// The derived books.
    pub books: &'a Books,
    /// The vault edits the books were derived with, for Bookkeeping.
    pub derive: &'a DeriveInputs,
    /// The chart's project attribution, for the project skills.
    pub projects: &'a Projects,
    /// The zone a vault note's `created_at` is dated in.
    pub zone: Zone,
    /// The day the fold is as of.
    pub today: Date,
    /// The goals on the Headway boards: the epic quests.
    pub goals: &'a [Goal],
    /// The XP rates.
    pub tuning: &'a XpTuning,
}

/// The fold, and everything the view draws from it.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// The fold itself, for the next fold to compare with.
    pub adventure: Adventure,
    /// One row per skill, in the fold's order: the fixed skills, then one
    /// per project.
    pub skills: Vec<SkillRow>,
    /// The character sheet.
    pub sheet: Sheet,
    /// Every achievement, the unlocked ones first.
    pub achievements: Vec<AchievementRow>,
    /// The points of the unlocked achievements.
    pub points: u32,
    /// The points of every achievement.
    pub total_points: u32,
    /// How many achievements are unlocked.
    pub unlocked: usize,
    /// Each skill's level and every achievement unlocked: what the toasts
    /// compare with the seen note ([`cashflow::adventure::seen`]).
    pub standing: Seen,
    /// This week's, this month's and the epic quests, in that order.
    pub quests: Vec<QuestRow>,
    /// The recent XP log: the adjustments newest first, then what was
    /// earned, newest month first, at most [`LOG_ROWS`] in all.
    pub log: Vec<LogRow>,
}

/// One skill as the view shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillRow {
    /// The skill's name: a fixed skill's, or the project's.
    pub name: String,
    /// Its glyph.
    pub icon: &'static str,
    /// Where it stands, for the tile and the XP bar.
    pub progress: XpProgress,
    /// How to start it, while it has no XP yet: the locked tile's hint.
    pub unlock: Option<&'static str>,
    /// What the current month has earned, `+4,210 XP`.
    pub gain_text: String,
    /// Where the current month's XP came from, a line per entry:
    /// `income 421.00 CAD × 10`.
    pub gain_reasons: Vec<String>,
}

/// The character: total level, streak, buffs, and this month's gains.
#[derive(Debug, Clone, PartialEq)]
pub struct Sheet {
    /// The sum of the skills' levels.
    pub total_level: u32,
    /// `Sum of 6 skill levels`.
    pub total_level_detail: String,
    /// `4 months` or `None`.
    pub streak_text: String,
    /// How the streak is counted, and since when.
    pub streak_detail: String,
    /// The buff and the debuff on the character, in that order.
    pub buffs: Vec<BuffRow>,
    /// The skills that earned XP this month, by [`Report::skills`] index,
    /// the most first.
    pub gains: Vec<usize>,
    /// The skill nearest its next level, by [`Report::skills`] index: the
    /// one the sheet's XP bar shows.
    pub closest: Option<usize>,
}

/// A buff or a debuff on the character sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct BuffRow {
    /// Whether it helps (a streak's bonus) or is only a mark (a month
    /// that lost money).
    pub good: bool,
    /// Its glyph.
    pub icon: &'static str,
    /// `Well Rested`, `Overspent`.
    pub name: &'static str,
    /// What it does: `+20% Earning and Saving XP`.
    pub text: String,
    /// Where it came from.
    pub detail: String,
}

/// One achievement as the view lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct AchievementRow {
    /// Its glyph.
    pub icon: &'static str,
    /// Its name.
    pub name: &'static str,
    /// What reaching it takes: a locked one's hint.
    pub description: &'static str,
    /// What it is worth.
    pub points: u32,
    /// The day it was first reached, `Mar 31, 2024`; `None` while locked.
    pub unlocked: Option<String>,
}

/// One quest as the quest log shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestRow {
    /// Weekly, monthly or epic.
    pub kind: QuestKind,
    /// What it asks.
    pub title: String,
    /// How far along it is, `0..=1` of the target.
    pub fraction: f32,
    /// `3 / 10`, `1,200.00 CAD / 5,000.00 CAD`.
    pub progress_text: String,
    /// The reward, `+150 XP Saving`.
    pub reward_text: String,
    /// Whether the books have completed it.
    pub done: bool,
    /// An epic quest's goal status.
    pub status: Option<Status>,
    /// An epic quest's Headway card, as a full `headway:<board>/<word-id>`
    /// ref: what a click on its title opens.
    pub card_ref: Option<String>,
}

/// One XP log entry, written out.
#[derive(Debug, Clone, PartialEq)]
pub struct LogRow {
    /// `Oct 2024`.
    pub month: String,
    /// The skill's name.
    pub skill: String,
    /// `+4,210`, or `-120` for an adjustment.
    pub delta: String,
    /// What earned it, in a line.
    pub reason: String,
    /// Whether it is a correction rather than XP earned.
    pub adjustment: bool,
}

/// Fold the books in `inputs` into an adventure, given the last fold made
/// here (`previous`) so its adjustments carry on, and write it out. `None`
/// when the books hold nothing in their home commodity to count XP in.
#[profiling::function]
pub fn report(inputs: Inputs<'_>, previous: Option<&Adventure>) -> Option<Report> {
    let home = inputs.books.home()?;
    let adventure = adventure::fold(
        &adventure::Inputs {
            books: inputs.books,
            home,
            derive: inputs.derive,
            projects: inputs.projects,
            zone: inputs.zone,
            tuning: inputs.tuning,
            today: inputs.today,
            goals: inputs.goals,
        },
        previous,
    );
    let units = Units {
        home,
        commodities: &inputs.books.ledger().journal().commodities,
        tuning: inputs.tuning,
    };
    let skills: Vec<SkillRow> = adventure
        .skills
        .iter()
        .map(|skill| skill_row(&adventure, skill, units))
        .collect();
    let sheet = sheet(&adventure, &skills, units);
    let unlocked = achievements::evaluate(&adventure);
    let mut achievement_rows: Vec<AchievementRow> = achievements::ALL
        .iter()
        .map(|a| AchievementRow {
            icon: a.icon,
            name: a.name,
            description: a.description,
            points: a.points,
            unlocked: unlocked
                .iter()
                .find(|u| u.id == a.id)
                .map(|u| day_label(u.date)),
        })
        .collect();
    // Stable: each half keeps the definitions' order.
    achievement_rows.sort_by_key(|row| row.unlocked.is_none());
    let quests = adventure
        .quests
        .iter()
        .map(|quest| QuestRow {
            kind: quest.kind,
            title: quest.title.clone(),
            fraction: fraction(quest.progress, quest.target),
            progress_text: format!(
                "{} / {}",
                units.measure(quest.unit, quest.progress),
                units.measure(quest.unit, quest.target)
            ),
            reward_text: format!("+{} XP {}", Grouped(quest.reward_xp), quest.skill.name()),
            done: quest.done,
            status: quest.status,
            card_ref: quest.id.0.strip_prefix("epic:").map(str::to_owned),
        })
        .collect();
    let log = log_rows(&adventure, units);
    let standing = Seen::of(&adventure, &unlocked);
    Some(Report {
        standing,
        skills,
        sheet,
        points: achievements::points(&unlocked),
        total_points: achievements::ALL.iter().map(|a| a.points).sum(),
        unlocked: unlocked.len(),
        achievements: achievement_rows,
        quests,
        log,
        adventure,
    })
}

/// How amounts and reasons are written.
#[derive(Clone, Copy)]
struct Units<'a> {
    home: CommodityId,
    commodities: &'a Commodities,
    tuning: &'a XpTuning,
}

impl Units<'_> {
    /// `qty` of the home commodity: `421.00 CAD`.
    fn money(self, qty: i128) -> String {
        money(qty, self.home, self.commodities)
    }

    /// A quest's progress or target, in its unit.
    fn measure(self, unit: QuestUnit, value: f64) -> String {
        match unit {
            QuestUnit::Count => format!("{value:.0}"),
            QuestUnit::Percent => format!("{value:.1}%"),
            QuestUnit::Amount(id) => money(value.round() as i128, id, self.commodities),
        }
    }

    /// Where an entry's XP came from, in a line.
    fn reason(self, reason: &Reason) -> String {
        reason.describe(self.tuning, &|qty| self.money(qty))
    }
}

/// `skill`'s row.
fn skill_row(adventure: &Adventure, skill: &adventure::Skill, units: Units<'_>) -> SkillRow {
    let current = adventure.months.last().map(|m| m.month);
    let gain_reasons = adventure
        .log
        .iter()
        .filter(|e| Some(e.month) == current && e.skill == skill.id)
        .filter(|e| !e.reason.is_adjustment())
        .map(|e| units.reason(&e.reason))
        .collect();
    let level_xp = xp_for_level(skill.level);
    let next_level_xp = if skill.level >= MAX_LEVEL {
        level_xp
    } else {
        xp_for_level(skill.level + 1)
    };
    SkillRow {
        name: skill.id.name().to_owned(),
        icon: icon(&skill.id),
        progress: XpProgress {
            level: skill.level,
            xp: skill.xp,
            level_xp,
            next_level_xp,
            month_xp: skill.xp_this_month,
            rested_xp: 0,
        },
        unlock: (skill.xp == 0).then(|| unlock_hint(&skill.id)),
        gain_text: format!("+{} XP", Grouped(skill.xp_this_month)),
        gain_reasons,
    }
}

/// A skill's glyph.
fn icon(id: &SkillId) -> &'static str {
    match id {
        SkillId::Earning => "💰",
        SkillId::Saving => "🐷",
        SkillId::Frugality => "✂",
        SkillId::Stacking => "🪙",
        SkillId::Bookkeeping => "📒",
        SkillId::Project(_) => "🛠",
    }
}

/// What starts a skill that has no XP yet.
fn unlock_hint(id: &SkillId) -> &'static str {
    match id {
        SkillId::Earning => "Record some income.",
        SkillId::Saving => "Close a month with more coming in than going out.",
        SkillId::Frugality => "Spend less in a month than the months before it averaged.",
        SkillId::Stacking => "Move bitcoin into an assets account.",
        SkillId::Bookkeeping => "Categorize a transaction or import a statement.",
        SkillId::Project(_) => "Bring in revenue for this project.",
    }
}

/// The character sheet of `adventure`, whose skills are written out in
/// `skills`.
fn sheet(adventure: &Adventure, skills: &[SkillRow], units: Units<'_>) -> Sheet {
    let streak = adventure.streak;
    let streak_text = match streak.months {
        0 => "None".to_owned(),
        1 => "1 month".to_owned(),
        n => format!("{n} months"),
    };
    let streak_detail = match streak.since {
        Some(since) => format!(
            "Every closed month since {} brought in more than it spent.",
            month_label(since.0)
        ),
        None => "A closed month that brings in more than it spends starts one.".to_owned(),
    };
    let mut buffs = Vec::new();
    if let Some(rested) = adventure.buffs.well_rested {
        buffs.push(BuffRow {
            good: true,
            icon: "🔥",
            name: "Well Rested",
            text: format!("+{}% Earning and Saving XP", rested.percent),
            detail: format!(
                "+{}% per month of the streak, up to +{}%.",
                units.tuning.streak_bonus_percent, units.tuning.streak_cap_percent
            ),
        });
    }
    if let Some(debuff) = adventure.buffs.debuff {
        buffs.push(BuffRow {
            good: false,
            icon: "🥀",
            name: "Overspent",
            text: format!(
                "{} spent {} more than it earned",
                month_label(debuff.month.0),
                units.money(-debuff.net)
            ),
            detail: "Cosmetic: it costs no XP, and the next positive month clears it.".to_owned(),
        });
    }
    let mut gains: Vec<usize> = (0..skills.len())
        .filter(|&s| skills[s].progress.month_xp > 0)
        .collect();
    gains.sort_by_key(|&s| std::cmp::Reverse(skills[s].progress.month_xp));
    let closest = (0..skills.len())
        .filter(|&s| skills[s].progress.xp > 0 && !skills[s].progress.is_max())
        .max_by(|&a, &b| {
            let (a, b) = (&skills[a].progress, &skills[b].progress);
            a.fraction().total_cmp(&b.fraction())
        });
    Sheet {
        total_level: adventure.total_level,
        total_level_detail: format!("The sum of {} skill levels.", skills.len()),
        streak_text,
        streak_detail,
        buffs,
        gains,
        closest,
    }
}

/// The recent log: adjustments newest first, then earned XP newest month
/// first, at most [`LOG_ROWS`].
fn log_rows(adventure: &Adventure, units: Units<'_>) -> Vec<LogRow> {
    let row = |e: &XpEntry| LogRow {
        month: month_label(e.month.0),
        skill: e.skill.name().to_owned(),
        delta: if e.delta < 0 {
            format!("-{}", Grouped(e.delta.unsigned_abs()))
        } else {
            format!("+{}", Grouped(e.delta.unsigned_abs()))
        },
        reason: units.reason(&e.reason),
        adjustment: e.reason.is_adjustment(),
    };
    let adjustments = adventure
        .log
        .iter()
        .rev()
        .filter(|e| e.reason.is_adjustment());
    // Earned entries are oldest month first, in skill order within one:
    // walk the months back, keeping each month's skill order.
    let mut earned: Vec<&XpEntry> = adventure
        .log
        .iter()
        .filter(|e| !e.reason.is_adjustment())
        .collect();
    earned.sort_by_key(|e| std::cmp::Reverse(e.month));
    adjustments.chain(earned).take(LOG_ROWS).map(row).collect()
}

/// `progress` as a share of `target`, `0..=1`.
fn fraction(progress: f64, target: f64) -> f32 {
    if target <= 0.0 {
        return if progress >= target { 1.0 } else { 0.0 };
    }
    (progress / target).clamp(0.0, 1.0) as f32
}
