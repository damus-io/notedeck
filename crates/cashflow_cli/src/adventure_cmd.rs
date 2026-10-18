//! The Adventure's commands, each a view of one fold of the books (see
//! [`cashflow::adventure`]):
//!
//! - `cashflow skills`: one row per skill, then the total level and the
//!   streak;
//! - `cashflow quests`: this week's and month's quests and the epic ones
//!   (the Headway goals), with their progress and reward;
//! - `cashflow achievements`: every achievement, unlocked or not, with its
//!   points and unlock date.
//!
//! Each folds the same way, goals included, so the XP a completed goal
//! pays shows the same in all three.

use std::time::{SystemTime, UNIX_EPOCH};

use cashflow::CashflowDir;
use cashflow::adventure::achievements::{self, Unlocked};
use cashflow::adventure::{self, Adventure, Inputs, Quest, QuestUnit, Reason, XpEntry, XpTuning};
use cashflow::chart::Chart;
use cashflow::goals::Goal;
use cashflow::projects::Projects;
use cashflow::settings::{Setting, Settings};
use cashflow::zone::Zone;
use ledgerdb::{Commodities, CommodityId};
use serde_json::{Value, json};

use crate::args::{AdventureArgs, Run};
use crate::books_cmd::{LoadedBooks, format_total, loaded_books};
use crate::goals_cmd::GoalBoards;
use crate::vault_cmd::Session;

/// One fold of the books, with what it was folded from.
struct Folded {
    loaded: LoadedBooks,
    home: CommodityId,
    tuning: XpTuning,
    adventure: Adventure,
}

impl Folded {
    /// The commodity table, for formatting amounts.
    fn commodities(&self) -> &Commodities {
        &self.loaded.books.ledger().journal().commodities
    }

    /// `qty` in the home commodity, formatted.
    fn money(&self, qty: i128) -> String {
        format_total(qty, self.home, self.commodities())
    }
}

/// Load the books and the goals `args` names, and fold them.
async fn fold(dir: &CashflowDir, run: &Run, args: &AdventureArgs) -> Result<Folded, String> {
    let session = Session::open_if_logged_in(dir, run).await?;
    let loaded = loaded_books(dir, session.as_ref(), &args.books)?;
    let books = &loaded.books;
    let home = books.home().ok_or_else(|| {
        format!(
            "the ledger has no {} amounts or prices to count XP in (try --in)",
            args.books.home
        )
    })?;
    let (projects, zone) = match &session {
        Some(session) => {
            let txn = session.txn()?;
            let author = session.vault.author();
            let settings = Settings::load(&session.ndb, &txn, author);
            let zone = Zone::parse(settings.effective(Setting::Timezone)).unwrap_or(Zone::Local);
            (Chart::load(&session.ndb, &txn, author).projects(), zone)
        }
        None => {
            eprintln!(
                "cashflow: no signing key, so no projects or Bookkeeping: run `cashflow login`"
            );
            (Projects::default(), Zone::Local)
        }
    };
    let goals: Vec<Goal> = match GoalBoards::load(dir, &args.goals) {
        Ok(boards) => boards.goals(),
        Err(err) => {
            eprintln!("cashflow: no goals, so no epic quests: {err}");
            Vec::new()
        }
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let tuning = XpTuning::default();
    let adventure = adventure::fold(
        &Inputs {
            books,
            home,
            derive: &loaded.inputs,
            projects: &projects,
            zone,
            tuning: &tuning,
            today: zone.date(now),
            goals: &goals,
        },
        None,
    );
    Ok(Folded {
        loaded,
        home,
        tuning,
        adventure,
    })
}

/// `cashflow skills`: the skills table, or with `--json` the whole fold.
pub async fn skills(dir: &CashflowDir, run: &Run, args: &AdventureArgs) -> Result<(), String> {
    let folded = fold(dir, run, args).await?;
    if args.json {
        println!("{}", adventure_json(&folded));
        return Ok(());
    }
    print_table(&folded.adventure);
    Ok(())
}

/// `cashflow quests`: the current quests, then what completed ones paid.
pub async fn quests(dir: &CashflowDir, run: &Run, args: &AdventureArgs) -> Result<(), String> {
    let folded = fold(dir, run, args).await?;
    let adventure = &folded.adventure;
    if args.json {
        let rows: Vec<Value> = adventure
            .quests
            .iter()
            .map(|q| quest_json(&folded, q))
            .collect();
        let done = Completed::of(adventure);
        println!(
            "{}",
            json!({
                "quests": rows,
                "completed": { "count": done.count, "xp": done.xp },
            })
        );
        return Ok(());
    }
    if adventure.quests.is_empty() {
        println!("no quests running");
    }
    let width = adventure
        .quests
        .iter()
        .map(|q| q.title.chars().count())
        .max()
        .unwrap_or(0);
    for quest in &adventure.quests {
        let mark = if quest.done { "✓" } else { " " };
        let status = quest
            .status
            .map_or_else(String::new, |s| format!("  [{}]", s.name()));
        println!(
            "{mark} {:<7}  {:<width$}  {} / {}  +{} {}{status}",
            quest.kind.name(),
            quest.title,
            measure(&folded, quest, quest.progress),
            measure(&folded, quest, quest.target),
            thousands(quest.reward_xp),
            quest.skill.name(),
        );
    }
    let done = Completed::of(adventure);
    println!(
        "completed: {} quest(s), {} XP",
        done.count,
        thousands(done.xp)
    );
    Ok(())
}

/// The quests the books completed, all periods, and the XP they paid.
struct Completed {
    count: usize,
    xp: u64,
}

impl Completed {
    fn of(adventure: &Adventure) -> Self {
        let mut done = Completed { count: 0, xp: 0 };
        for entry in &adventure.log {
            if matches!(entry.reason, Reason::Quest { .. }) {
                done.count += 1;
                done.xp += entry.delta.unsigned_abs();
            }
        }
        done
    }
}

/// A quest's progress or target, in its unit.
fn measure(folded: &Folded, quest: &Quest, value: f64) -> String {
    match quest.unit {
        QuestUnit::Count => format!("{value:.0}"),
        QuestUnit::Percent => format!("{value:.1}%"),
        QuestUnit::Amount(commodity) => {
            format_total(value.round() as i128, commodity, folded.commodities())
        }
    }
}

/// A quest as JSON.
fn quest_json(folded: &Folded, quest: &Quest) -> Value {
    json!({
        "id": quest.id.to_string(),
        "kind": quest.kind.name(),
        "title": quest.title,
        "progress": quest.progress,
        "target": quest.target,
        "progress_text": measure(folded, quest, quest.progress),
        "target_text": measure(folded, quest, quest.target),
        "reward_xp": quest.reward_xp,
        "skill": quest.skill.name(),
        "done": quest.done,
        "status": quest.status.map(|s| s.name()),
    })
}

/// `cashflow achievements`: every achievement, the unlocked ones first
/// marked and dated, then the points.
pub async fn achievements(
    dir: &CashflowDir,
    run: &Run,
    args: &AdventureArgs,
) -> Result<(), String> {
    let folded = fold(dir, run, args).await?;
    let unlocked = achievements::evaluate(&folded.adventure);
    let on = |id: &str| unlocked.iter().find(|u| u.id == id);
    let total: u32 = achievements::ALL.iter().map(|a| a.points).sum();
    let points = achievements::points(&unlocked);
    if args.json {
        let rows: Vec<Value> = achievements::ALL
            .iter()
            .map(|a| {
                json!({
                    "id": a.id,
                    "name": a.name,
                    "description": a.description,
                    "points": a.points,
                    "icon": a.icon,
                    "unlocked": on(a.id).map(|u: &Unlocked| u.date.to_string()),
                })
            })
            .collect();
        println!(
            "{}",
            json!({ "achievements": rows, "points": points, "total_points": total })
        );
        return Ok(());
    }
    let width = achievements::ALL
        .iter()
        .map(|a| a.name.chars().count())
        .max()
        .unwrap_or(0);
    for a in achievements::ALL {
        let (mark, date) = match on(a.id) {
            Some(u) => ("✓", u.date.to_string()),
            None => (" ", String::new()),
        };
        let line = format!(
            "{mark} {} {:<width$}  {:>3}  {:<48}  {date}",
            a.icon, a.name, a.points, a.description
        );
        println!("{}", line.trim_end());
    }
    println!(
        "points: {points} / {total} ({} of {} unlocked)",
        unlocked.len(),
        achievements::ALL.len()
    );
    Ok(())
}

/// The skills table, the total level and the streak line.
fn print_table(adventure: &Adventure) {
    let width = adventure
        .skills
        .iter()
        .map(|s| s.id.name().chars().count())
        .max()
        .unwrap_or(0)
        .max("skill".len());
    println!(
        "{:<width$}  {:>5}  {:>12}  {:>12}  {:>10}",
        "skill", "level", "xp", "to next", "this month"
    );
    for skill in &adventure.skills {
        let next = skill
            .xp_to_next_level()
            .map_or_else(|| "max".to_owned(), thousands);
        println!(
            "{:<width$}  {:>5}  {:>12}  {:>12}  {:>10}",
            skill.id.name(),
            skill.level,
            thousands(skill.xp),
            next,
            thousands(skill.xp_this_month),
        );
    }
    println!("{:<width$}  {:>5}", "total", adventure.total_level);
    let streak = &adventure.streak;
    match (streak.since, adventure.buffs.well_rested) {
        (Some(since), Some(rested)) => println!(
            "streak: {} positive month(s) since {since}, Well Rested +{}%",
            streak.months, rested.percent
        ),
        _ => println!("streak: none"),
    }
    if let Some(debuff) = adventure.buffs.debuff {
        println!("debuff: {} spent more than it earned", debuff.month);
    }
}

/// `n` with a comma every three digits: `13,034,431`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The fold as JSON: skills, total level, streak, buffs and the XP log
/// with each entry's reason in words.
fn adventure_json(folded: &Folded) -> Value {
    let adventure = &folded.adventure;
    let tuning = &folded.tuning;
    let money = |qty: i128| folded.money(qty);
    let skills: Vec<Value> = adventure
        .skills
        .iter()
        .map(|s| {
            json!({
                "skill": s.id.name(),
                "project": matches!(s.id, adventure::SkillId::Project(_)),
                "level": s.level,
                "xp": s.xp,
                "xp_to_next_level": s.xp_to_next_level(),
                "xp_this_month": s.xp_this_month,
            })
        })
        .collect();
    let entry = |e: &XpEntry| {
        json!({
            "month": e.month.to_string(),
            "skill": e.skill.name(),
            "delta": e.delta,
            "adjustment": e.reason.is_adjustment(),
            "reason": e.reason.describe(tuning, &money),
        })
    };
    json!({
        "skills": skills,
        "total_level": adventure.total_level,
        "streak": {
            "months": adventure.streak.months,
            "since": adventure.streak.since.map(|m| m.to_string()),
        },
        "well_rested_percent": adventure.buffs.well_rested.map(|w| w.percent),
        "debuff": adventure.buffs.debuff.map(|d| json!({
            "month": d.month.to_string(),
            "net": money(d.net),
        })),
        "log": adventure.log.iter().map(entry).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::thousands;

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(13_034_431), "13,034,431");
    }
}
