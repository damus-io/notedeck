//! The per-transaction edit commands: `categorize`, `uncategorize`,
//! `split`, `transfer` and `edits` ([`cashflow::vault_edits`]), and the
//! triage queue, `unknown` ([`cashflow::unknown`]).
//!
//! A write is planned first ([`cashflow::plan::plan`]): the raw ledger
//! derived with the vault's edits before and after the change, so it can
//! print the transaction's legs both ways and refuse an edit derive would
//! reject (a split that doesn't sum, a link that doesn't net) before
//! anything is stored. `--dry-run` stops there. The plan's refusals and
//! the write itself are [`cashflow::triage`]'s, the copy of the rules the
//! app uses too.

use cashflow::plan::{self, Categorized, Move, Plan};
use cashflow::split::{self, LegDraft};
use cashflow::transfers;
use cashflow::triage::{self, Action, WriteError};
use cashflow::unknown::{self, Scope};
use cashflow::vault::Record;
use cashflow::vault_edits::{LinkNote, OverrideNote, SplitLeg, SplitNote, VaultEdits, link_id};
use cashflow::{Books, CashflowDir, DeriveInputs, journal};
use ledgerdb::{Edits, LayerFilter, Ledger};
use serde_json::{Value, json};

use crate::args::{TransferCommand, UnknownArgs, Write};
use crate::books_cmd::{derive_inputs, format_total, raw_ledger, retired, vault_edits};
use crate::vault_cmd::{Session, session_ref};

/// How a transaction is decided, as JSON.
fn categorized_json(categorized: &Categorized) -> Value {
    json!({
        "decided_by": plan::tier(categorized.why),
        "legs": categorized.legs.iter().map(|l| json!({"account": l.account, "amount": l.amount})).collect::<Vec<_>>(),
    })
}

/// `tier  account amount, account amount`.
fn categorized_line(categorized: &Categorized) -> String {
    let legs: Vec<String> = categorized
        .legs
        .iter()
        .map(|l| format!("{} {}", l.account, l.amount))
        .collect();
    format!("{:<9} {}", plan::tier(categorized.why), legs.join(", "))
}

/// One transaction a write reaches, before and after, as JSON.
fn move_json(raw: &Ledger, moved: &Move) -> Value {
    let journal = raw.journal();
    let txn = journal.txn(moved.txn);
    json!({
        "record_id": moved.record,
        "date": txn.date.to_string(),
        "payee": journal.strings.get(txn.payee),
        "before": categorized_json(&moved.before),
        "after": categorized_json(&moved.after),
    })
}

/// Print one transaction a write reaches, before and after.
fn print_move(raw: &Ledger, moved: &Move) {
    let journal = raw.journal();
    let txn = journal.txn(moved.txn);
    let record = moved.record.as_deref().unwrap_or("");
    println!("{record}  {}  {}", txn.date, journal.strings.get(txn.payee));
    println!("  before  {}", categorized_line(&moved.before));
    println!("  after   {}", categorized_line(&moved.after));
}

/// What `action` would do to the raw ledger, derived with the vault's
/// edits, or the refusal it meets ([`plan::plan`]).
fn planned(raw: &Ledger, inputs: &DeriveInputs, action: &Action) -> Result<Plan, String> {
    plan::plan(raw, inputs, action).map_err(refusal)
}

/// `err` as the CLI says it: a refusal names the command that gets past it.
fn refusal(err: WriteError) -> String {
    match err {
        WriteError::Linked { record, other } => format!(
            "{record} is linked as a transfer with {other}: `cashflow transfer unlink {record} {other}` first"
        ),
        WriteError::Categorized(record) => {
            format!("{record} has an override or split: `cashflow uncategorize {record}` first")
        }
        err => err.to_string(),
    }
}

/// The `d` tag a record of type `T` with id `id` is stored at.
fn d_of<'a, T: Record<'a>>(id: &str) -> String {
    format!("{}:{id}", T::PREFIX)
}

/// Write each of `actions` through [`triage::apply`], as this session, and
/// publish them together.
async fn commit(session: &mut Session, actions: &[Action]) -> Result<(), String> {
    let agentium = session_ref();
    let mut frames = Vec::with_capacity(actions.len());
    for action in actions {
        let applied = triage::apply(&session.vault, &session.ndb, action, agentium.as_deref())
            .map_err(refusal)?;
        frames.extend(applied.written.into_iter().map(|w| w.frame));
    }
    session.publish(&frames).await
}

/// `record`'s live override, as JSON, if it has one.
fn override_json(edits: &VaultEdits, record: &str) -> Option<Value> {
    let body = edits.override_of(record)?.body.body.as_ref()?;
    Some(json!({"kind": "override", "d": d_of::<OverrideNote>(record), "account": body.account}))
}

/// `record`'s live split, as JSON, if it has one.
fn split_json(edits: &VaultEdits, record: &str) -> Option<Value> {
    let body = edits.split_of(record)?.body.body.as_ref()?;
    Some(json!({"kind": "split", "d": d_of::<SplitNote>(record), "legs": legs_json(&body.legs)}))
}

/// The note a write to one record rewrites: the other of its override and
/// split, if live, is cleared.
#[derive(Clone, Copy)]
enum Rewrites {
    Override,
    Split,
}

/// What a write to `record` replaces, as JSON: the note it rewrites, else
/// the one it clears, else null. Read in a transaction of its own, since
/// [`triage::apply`] needs the thread's only one.
fn replaced(session: &Session, record: &str, rewrites: Rewrites) -> Result<Value, String> {
    let txn = session.txn()?;
    let stored = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    let override_note = || override_json(&stored, record);
    let split_note = || split_json(&stored, record);
    let found = match rewrites {
        Rewrites::Override => override_note().or_else(split_note),
        Rewrites::Split => split_note().or_else(override_note),
    };
    Ok(found.unwrap_or(Value::Null))
}

fn legs_json(legs: &[SplitLeg]) -> Value {
    legs.iter()
        .map(|l| json!({"account": l.account, "qty": l.qty}))
        .collect()
}

/// What a single-transaction write reached, to print.
struct Outcome<'a> {
    /// The raw ledger the plan was made over.
    raw: &'a Ledger,
    /// The plan.
    plan: &'a Plan,
    /// The `d` tag written or cleared.
    d: &'a str,
    /// The note it replaced, as JSON, or null.
    replaced: Value,
    /// What it did: "wrote" or "cleared".
    verb: &'a str,
}

/// Print a single-transaction write's outcome.
fn report(session: &Session, write: Write, outcome: Outcome<'_>) {
    let Outcome {
        raw,
        plan,
        d,
        replaced,
        verb,
    } = outcome;
    if write.json {
        let out = json!({
            "dry_run": write.dry_run,
            "d": d,
            "replaced": replaced,
            "changes": plan.moves.iter().map(|m| move_json(raw, m)).collect::<Vec<_>>(),
        });
        println!("{out}");
        return;
    }
    for moved in &plan.moves {
        print_move(raw, moved);
    }
    let replaced = match replaced.get("kind").and_then(Value::as_str) {
        Some(kind) => format!(
            " (replacing the {kind} at {})",
            replaced["d"].as_str().unwrap_or("")
        ),
        None => String::new(),
    };
    if write.dry_run {
        println!("dry run: would {verb} {d}{replaced}; nothing written");
    } else {
        println!("{verb} {d}{replaced}{}", session.offline_note());
    }
}

/// `categorize <record-id> <account>`.
pub async fn categorize(
    dir: &CashflowDir,
    session: &mut Session,
    record: &str,
    account: &str,
    write: Write,
) -> Result<(), String> {
    let raw = raw_ledger(dir, session)?;
    let action = Action::Override {
        record: record.to_owned(),
        account: account.to_owned(),
    };
    let plan = planned(&raw, &derive_inputs(session)?, &action)?;
    let replaced = replaced(session, record, Rewrites::Override)?;
    if !write.dry_run {
        commit(session, &[action]).await?;
    }
    let d = d_of::<OverrideNote>(record);
    report(
        session,
        write,
        Outcome {
            raw: &raw,
            plan: &plan,
            d: &d,
            replaced,
            verb: "wrote",
        },
    );
    Ok(())
}

/// `uncategorize <record-id>`: clear its override and split.
pub async fn uncategorize(
    dir: &CashflowDir,
    session: &mut Session,
    record: &str,
    write: Write,
) -> Result<(), String> {
    let raw = raw_ledger(dir, session)?;
    let action = Action::Uncategorize {
        record: record.to_owned(),
    };
    let plan = planned(&raw, &derive_inputs(session)?, &action)?;
    let replaced = replaced(session, record, Rewrites::Override)?;
    let d = replaced["d"].as_str().unwrap_or_default().to_owned();
    if !write.dry_run {
        commit(session, &[action]).await?;
    }
    report(
        session,
        write,
        Outcome {
            raw: &raw,
            plan: &plan,
            d: &d,
            replaced: Value::Null,
            verb: "cleared",
        },
    );
    Ok(())
}

/// `split <record-id> <account>[=<amount>]...`.
pub async fn split(
    dir: &CashflowDir,
    session: &mut Session,
    record: &str,
    legs: &[LegDraft],
    write: Write,
) -> Result<(), String> {
    let raw = raw_ledger(dir, session)?;
    let id = raw
        .find(record)
        .ok_or_else(|| WriteError::NoRecord(record.to_owned()).to_string())?;
    let inputs = derive_inputs(session)?;
    let legs = split::legs(&raw, &inputs.edits, id, record, legs).map_err(|e| e.to_string())?;
    let action = Action::Split {
        record: record.to_owned(),
        legs,
    };
    let plan = planned(&raw, &inputs, &action)?;
    let replaced = replaced(session, record, Rewrites::Split)?;
    if !write.dry_run {
        commit(session, &[action]).await?;
    }
    let d = d_of::<SplitNote>(record);
    report(
        session,
        write,
        Outcome {
            raw: &raw,
            plan: &plan,
            d: &d,
            replaced,
            verb: "wrote",
        },
    );
    Ok(())
}

/// Run one `transfer` subcommand.
pub async fn transfer(
    dir: &CashflowDir,
    session: &mut Session,
    cmd: &TransferCommand,
) -> Result<(), String> {
    match cmd {
        TransferCommand::Link { a, b, write } => link(dir, session, a, b, *write).await,
        TransferCommand::Unlink { a, b, write } => unlink(dir, session, a, b, *write).await,
        TransferCommand::Suggest { accept_all, write } => {
            suggest(dir, session, *accept_all, *write).await
        }
    }
}

/// `transfer link <a> <b>`.
async fn link(
    dir: &CashflowDir,
    session: &mut Session,
    a: &str,
    b: &str,
    write: Write,
) -> Result<(), String> {
    let id = link_id(a, b).map_err(|e| e.to_string())?;
    let raw = raw_ledger(dir, session)?;
    let action = Action::Link {
        a: a.to_owned(),
        b: b.to_owned(),
    };
    let plan = planned(&raw, &derive_inputs(session)?, &action)?;
    if !write.dry_run {
        commit(session, &[action]).await?;
    }
    let d = d_of::<LinkNote>(&id);
    report(
        session,
        write,
        Outcome {
            raw: &raw,
            plan: &plan,
            d: &d,
            replaced: Value::Null,
            verb: "wrote",
        },
    );
    Ok(())
}

/// `transfer unlink <a> <b>`.
async fn unlink(
    dir: &CashflowDir,
    session: &mut Session,
    a: &str,
    b: &str,
    write: Write,
) -> Result<(), String> {
    let id = link_id(a, b).map_err(|e| e.to_string())?;
    let raw = raw_ledger(dir, session)?;
    let action = Action::Unlink {
        a: a.to_owned(),
        b: b.to_owned(),
    };
    let plan = planned(&raw, &derive_inputs(session)?, &action)?;
    if !write.dry_run {
        commit(session, &[action]).await?;
    }
    let d = d_of::<LinkNote>(&id);
    report(
        session,
        write,
        Outcome {
            raw: &raw,
            plan: &plan,
            d: &d,
            replaced: Value::Null,
            verb: "cleared",
        },
    );
    Ok(())
}

/// One side of a suggested transfer as shown: its own leg, the one not on
/// a default account.
fn side_json(raw: &Ledger, edits: &Edits, record: &str) -> Value {
    let Some(txn) = raw.find(record) else {
        return json!({"record_id": record});
    };
    let journal = raw.journal();
    let found = journal.txn(txn);
    let own = journal.postings_of(found).iter().find(|p| {
        !edits
            .defaults
            .iter()
            .any(|d| d == journal.accounts.full_name(p.account))
    });
    json!({
        "record_id": record,
        "date": found.date.to_string(),
        "payee": journal.strings.get(found.payee),
        "account": own.map(|p| journal.accounts.full_name(p.account)),
        "amount": own.map(|p| p.amount().display(&journal.commodities).to_string()),
    })
}

/// `transfer suggest [--accept-all [--dry-run]]`.
async fn suggest(
    dir: &CashflowDir,
    session: &mut Session,
    accept_all: bool,
    write: Write,
) -> Result<(), String> {
    let raw = raw_ledger(dir, session)?;
    let edits = vault_edits(session)?;
    let pairs = transfers::suggest(&raw, &edits);
    let writing = accept_all && !write.dry_run;
    if writing {
        let links: Vec<Action> = pairs
            .iter()
            .map(|pair| Action::Link {
                a: pair.link.a.clone(),
                b: pair.link.b.clone(),
            })
            .collect();
        commit(session, &links).await?;
    }
    if write.json {
        let out = json!({
            "dry_run": write.dry_run,
            "linked": if writing { pairs.len() } else { 0 },
            "pairs": pairs.iter().map(|p| json!({
                "d": d_of::<LinkNote>(&link_id(&p.link.a, &p.link.b).unwrap_or_default()),
                "days": p.days,
                "a": side_json(&raw, &edits, &p.link.a),
                "b": side_json(&raw, &edits, &p.link.b),
            })).collect::<Vec<_>>(),
        });
        println!("{out}");
        return Ok(());
    }
    for pair in &pairs {
        let line = |record: &str| {
            let side = side_json(&raw, &edits, record);
            format!(
                "{}  {}  {}  {}",
                side["date"].as_str().unwrap_or(""),
                record,
                side["account"].as_str().unwrap_or(""),
                side["amount"].as_str().unwrap_or("")
            )
        };
        println!(
            "{}\n  -> {}  ({} days)",
            line(&pair.link.a),
            line(&pair.link.b),
            pair.days
        );
    }
    let verb = match (accept_all, write.dry_run) {
        (false, _) => "suggested",
        (true, true) => "would link",
        (true, false) => "linked",
    };
    let offline = if writing { session.offline_note() } else { "" };
    println!("{verb} {} transfers{offline}", pairs.len());
    if accept_all && write.dry_run {
        println!("dry run: nothing written");
    }
    Ok(())
}

/// `edits [--by <ref>]`: every live per-transaction edit.
pub fn ls(session: &Session, by: Option<&str>, json: bool) -> Result<(), String> {
    let txn = session.txn()?;
    let stored = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    let keep = |agentium: Option<&str>| by.is_none_or(|by| agentium == Some(by));
    let mut rows: Vec<Value> = Vec::new();
    for o in &stored.overrides {
        if let Some(body) = &o.body.body
            && keep(o.body.agentium)
        {
            rows.push(
                json!({"kind": "override", "d": d_of::<OverrideNote>(o.id), "record_id": o.id,
                "account": body.account, "agentium": o.body.agentium, "created_at": o.created_at}),
            );
        }
    }
    for s in &stored.splits {
        if let Some(body) = &s.body.body
            && keep(s.body.agentium)
        {
            rows.push(json!({"kind": "split", "d": d_of::<SplitNote>(s.id), "record_id": s.id,
                "legs": legs_json(&body.legs), "agentium": s.body.agentium, "created_at": s.created_at}));
        }
    }
    for l in &stored.links {
        if let Some(body) = &l.body.body
            && keep(l.body.agentium)
        {
            rows.push(
                json!({"kind": "transfer", "d": d_of::<LinkNote>(l.id), "a": body.a, "b": body.b,
                "agentium": l.body.agentium, "created_at": l.created_at}),
            );
        }
    }
    if json {
        println!("{}", Value::Array(rows));
        return Ok(());
    }
    for row in &rows {
        let what = match row["kind"].as_str() {
            Some("override") => row["account"].as_str().unwrap_or("").to_owned(),
            Some("split") => row["legs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|l| format!("{}={}", l["account"].as_str().unwrap_or(""), l["qty"]))
                .collect::<Vec<_>>()
                .join(" "),
            _ => format!(
                "{} <-> {}",
                row["a"].as_str().unwrap_or(""),
                row["b"].as_str().unwrap_or("")
            ),
        };
        let by = row["agentium"]
            .as_str()
            .map_or(String::new(), |r| format!("  by {r}"));
        println!(
            "{:<9} {:<40}  {what}{by}",
            row["kind"].as_str().unwrap_or(""),
            row["d"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

/// `s` quoted for a POSIX shell when it needs it.
fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/=@%+,".contains(c));
    if plain {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `unknown`: the triage queue.
pub async fn unknown(
    dir: &CashflowDir,
    session: &Session,
    args: &UnknownArgs,
) -> Result<(), String> {
    let edits = vault_edits(session)?;
    let books = Books::load(
        &journal::store(dir),
        LayerFilter::All,
        &retired(session)?,
        &edits,
        &args.home,
    )
    .map_err(|e| e.to_string())?;
    let groups = unknown::groups(
        &books,
        &edits,
        Scope {
            source: args.source.as_deref(),
        },
    );
    let journal = books.ledger().journal();
    let rows: usize = groups.iter().map(|g| g.rows.len()).sum();
    let total = |qty: i128| match books.home() {
        Some(home) => format_total(qty, home, &journal.commodities),
        None => "-".to_owned(),
    };
    let rule_add = |group: &unknown::PayeeGroup| -> Option<Vec<String>> {
        let pattern = group.pattern.as_ref()?;
        let mut argv: Vec<String> = ["cashflow", "rule", "add", "<account>", "--match"]
            .map(str::to_owned)
            .into();
        argv.push(pattern.clone());
        if let [source] = group.sources.as_slice() {
            argv.extend(["--source".to_owned(), source.clone()]);
        }
        Some(argv)
    };
    let shown = groups.iter().take(args.limit);
    if args.json {
        let out = json!({
            "rows": rows,
            "groups_total": groups.len(),
            "home": books.home().map(|h| journal.commodities.name(h)),
            "groups": shown.map(|g| json!({
                "payee": g.key,
                "account": g.account,
                "rows": g.rows.len(),
                "total": total(g.total),
                "total_qty": g.total.to_string(),
                "unconverted": g.unconverted,
                "sources": g.sources,
                "pattern": g.pattern,
                "record_ids": g.rows.iter().filter_map(|&id| journal.txn(id).id.map(|s| journal.strings.get(s))).collect::<Vec<_>>(),
                "rule_add": rule_add(g),
            })).collect::<Vec<_>>(),
        });
        println!("{out}");
        return Ok(());
    }
    println!(
        "{rows} unknown transactions in {} groups{}",
        groups.len(),
        if groups.len() > args.limit {
            format!("; the {} biggest:", args.limit)
        } else {
            String::new()
        }
    );
    for group in shown {
        let unconverted = if group.unconverted > 0 {
            format!("  ({} unpriced)", group.unconverted)
        } else {
            String::new()
        };
        let payee = if group.key.is_empty() {
            "(no payee words)"
        } else {
            &group.key
        };
        println!(
            "{:>20}  {:>6}  {:<18}  {payee}{unconverted}",
            total(group.total),
            group.rows.len(),
            group.account
        );
        if let Some(argv) = rule_add(group) {
            let line: Vec<String> = argv
                .iter()
                .map(|a| match a.as_str() {
                    "<account>" => a.clone(),
                    _ => shell_quote(a),
                })
                .collect();
            println!("{:>30}{}", "", line.join(" "));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_quotes_only_when_needed() {
        assert_eq!(shell_quote("amex"), "amex");
        assert_eq!(shell_quote("corner grocer"), "'corner grocer'");
        assert_eq!(shell_quote("joe's"), r"'joe'\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
