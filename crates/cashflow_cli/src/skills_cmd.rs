//! `cashflow skills`: the Adventure's skills levelled from the books (see
//! [`cashflow::adventure`]), one row each, then the total level and the
//! streak.

use std::time::{SystemTime, UNIX_EPOCH};

use cashflow::CashflowDir;
use cashflow::adventure::{self, Adventure, Inputs, XpEntry, XpTuning};
use cashflow::chart::Chart;
use cashflow::projects::Projects;
use cashflow::settings::{Setting, Settings};
use cashflow::zone::Zone;
use ledgerdb::{Commodities, CommodityId};
use serde_json::{Value, json};

use crate::args::{BooksArgs, Run};
use crate::books_cmd::{format_total, loaded_books};
use crate::vault_cmd::Session;

/// `cashflow skills`: fold the books and print the skills table, or with
/// `json` the whole fold.
pub async fn skills(
    dir: &CashflowDir,
    run: &Run,
    args: &BooksArgs,
    json: bool,
) -> Result<(), String> {
    let session = Session::open_if_logged_in(dir, run).await?;
    let loaded = loaded_books(dir, session.as_ref(), args)?;
    let books = &loaded.books;
    let home = books.home().ok_or_else(|| {
        format!(
            "the ledger has no {} amounts or prices to count XP in (try --in)",
            args.home
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
        },
        None,
    );

    let commodities = &books.ledger().journal().commodities;
    if json {
        println!("{}", adventure_json(&adventure, &tuning, home, commodities));
        return Ok(());
    }
    print_table(&adventure);
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
fn adventure_json(
    adventure: &Adventure,
    tuning: &XpTuning,
    home: CommodityId,
    commodities: &Commodities,
) -> Value {
    let money = |qty: i128| format_total(qty, home, commodities);
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
