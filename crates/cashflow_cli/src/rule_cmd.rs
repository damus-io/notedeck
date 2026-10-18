//! `cashflow rule`: the vault's categorization rules
//! ([`cashflow::vault_rules`]).
//!
//! Every command reads the rules from the [`Session`]'s vault and the raw
//! ledger from the journal, so `ls` can count what each rule decides and a
//! write can say what it moves before (with `--dry-run`, instead of) writing.
//! A write is one note per rule touched, published to the relay.

use std::collections::BTreeMap;
use std::path::Path;

use cashflow::vault_edits::VaultEdits;
use cashflow::vault_rules::{
    AmountCond, Compiled, Decision, MetaNeedle, Moved, Owner, Place as RulePlace, RuleNote,
    VaultRules, moves, new_rule_id,
};
use cashflow::{CashflowDir, rules};
use ledgerdb::{Ledger, TxnId, Why, derive};
use serde_json::{Value, json};

use crate::args::{NewRule, Place, RuleCommand, Write};
use crate::books_cmd::raw_ledger;
use crate::vault_cmd::{Session, session_ref};

/// How many moved rows a write's summary lists.
const SAMPLE: usize = 5;

/// Run one `rule` subcommand.
pub async fn run(
    dir: &CashflowDir,
    session: &mut Session,
    cmd: &RuleCommand,
) -> Result<(), String> {
    let raw = raw_ledger(dir, session)?;
    match cmd {
        RuleCommand::Ls { by, json } => ls(session, &raw, by.as_deref(), *json),
        RuleCommand::Test { record_id, json } => test(session, &raw, record_id, *json),
        RuleCommand::Add { rule, place, write } => add(session, &raw, rule, place, *write).await,
        RuleCommand::Mv { rule, place, write } => mv(session, &raw, rule, place, *write).await,
        RuleCommand::Rm { rule, write } => rm(session, &raw, rule, *write).await,
        RuleCommand::Import { path, write } => import(session, &raw, path, *write).await,
    }
}

/// `compiled` with the vault's overrides, splits and transfer links added.
/// They win over every rule, so counting what a rule decides, or what a
/// rule change moves, has to leave out the transactions they decide.
fn with_edits(mut compiled: Compiled, per_txn: &VaultEdits) -> Compiled {
    per_txn.apply(&mut compiled.edits);
    compiled
}

/// `place` as the vault's placement.
fn rule_place(place: &Place) -> RulePlace<'_> {
    match place {
        Place::First => RulePlace::First,
        Place::Last => RulePlace::Last,
        Place::Before(rule) => RulePlace::Before(rule),
        Place::After(rule) => RulePlace::After(rule),
    }
}

/// How many transactions each live rule decides, by its index in
/// [`VaultRules::live`].
fn match_counts(raw: &Ledger, compiled: &Compiled, rules: usize) -> Vec<u64> {
    let derived = derive(raw, &compiled.edits);
    let mut counts = vec![0u64; rules];
    for i in 0..raw.txns().len() {
        if let Some(owner) = compiled.owner(derived.why(TxnId(i as u32))) {
            counts[owner.rule as usize] += 1;
        }
    }
    counts
}

/// A rule as JSON.
fn rule_json(id: &str, rule: &RuleNote) -> Value {
    json!({
        "id": id,
        "rank": rule.rank,
        "account": rule.account,
        "match": rule.patterns,
        "meta": rule.meta.iter().map(|m| json!({"key": m.key, "needle": m.needle})).collect::<Vec<_>>(),
        "amount": rule.amount.map(|a| json!({"min": a.min, "max": a.max, "commodity": a.commodity})),
        "source": rule.source,
        "layer": rule.layer,
        "agentium": rule.agentium,
    })
}

/// `rule ls`.
fn ls(session: &Session, raw: &Ledger, by: Option<&str>, json: bool) -> Result<(), String> {
    let txn = session.txn()?;
    let rules = VaultRules::load(&session.ndb, &txn, session.vault.author());
    let per_txn = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    let live: Vec<_> = rules.live().collect();
    let counts = match_counts(raw, &with_edits(rules.compile(), &per_txn), live.len());
    let shown = live
        .iter()
        .zip(&counts)
        .filter(|(rule, _)| by.is_none_or(|by| rule.body.agentium == Some(by)));
    if json {
        let rows: Vec<Value> = shown
            .map(|(rule, &matches)| {
                let mut row = rule_json(rule.id, &rule.body);
                row["matches"] = matches.into();
                row
            })
            .collect();
        println!("{}", Value::Array(rows));
        return Ok(());
    }
    for (rule, matches) in shown {
        let by = rule
            .body
            .agentium
            .map_or(String::new(), |r| format!("  by {r}"));
        println!(
            "{:<24}{:>8}  {:<32}  {}{by}",
            rule.id,
            matches,
            rule.body.account,
            rule.body.conditions()
        );
    }
    Ok(())
}

/// `rule test <record-id>`: which tier decides it, and for a rule, which
/// one and on which pattern.
fn test(session: &Session, raw: &Ledger, record_id: &str, json: bool) -> Result<(), String> {
    let txn = session.txn()?;
    let rules = VaultRules::load(&session.ndb, &txn, session.vault.author());
    let per_txn = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    let live: Vec<_> = rules.live().collect();
    let compiled = with_edits(rules.compile(), &per_txn);
    let derived = derive(raw, &compiled.edits);
    let id = raw
        .find(record_id)
        .ok_or_else(|| format!("no transaction with id '{record_id}' in the journal"))?;
    let why = derived.why(id);
    let journal = raw.journal();
    let found = journal.txn(id);
    let payee = journal.strings.get(found.payee);
    let decided = Decision::of(raw, &compiled, id, why);
    let owner = compiled.owner(why);
    let rule = owner.map(|o| live[o.rule as usize]);
    let pattern = owner.and_then(|Owner { rule, pattern }| {
        Some(&live[rule as usize].body.patterns[pattern? as usize])
    });
    let tier = match why {
        Why::Rule(_) => "rule",
        Why::Default => "default",
        Why::Override(_) => "override",
        Why::Split(_) => "split",
        Why::Transfer(_) => "transfer",
        Why::Untouched => "untouched",
    };
    if json {
        let out = json!({
            "record_id": record_id,
            "date": found.date.to_string(),
            "payee": payee,
            "decided_by": tier,
            "account": decided.as_ref().map(|d| &d.account),
            "rule": rule.map(|r| rule_json(r.id, &r.body)),
            "pattern": pattern,
        });
        println!("{out}");
        return Ok(());
    }
    println!("{record_id}  {}  {payee}", found.date);
    match (why, rule, decided) {
        (Why::Rule(_), Some(rule), Some(decided)) => {
            println!("  rule {} sends it to {}", rule.id, decided.account);
            println!("  conditions: {}", rule.body.conditions());
            if let Some(pattern) = pattern {
                println!("  matched on payee pattern '{pattern}'");
            }
        }
        (Why::Default, _, Some(decided)) => {
            println!("  no rule matches: it stays at {}", decided.account)
        }
        (Why::Untouched, ..) => println!(
            "  it has no counter leg (no posting, or more than one, to a default account), so no rule applies"
        ),
        _ => println!("  a {tier} decides it, ahead of every rule"),
    }
    Ok(())
}

/// The live rules after a change, as `(id, rule)` in the order derive
/// applies them.
struct Plan<'r, 'a> {
    rules: Vec<PlannedRule<'r, 'a>>,
}

/// One rule in a [`Plan`].
struct PlannedRule<'r, 'a> {
    id: &'r str,
    rule: &'r RuleNote<'a>,
}

impl<'r, 'a> Plan<'r, 'a> {
    /// The vault's live rules, as they stand.
    fn current(rules: &'r VaultRules<'a>) -> Self {
        Plan {
            rules: rules
                .live()
                .map(|r| PlannedRule {
                    id: r.id,
                    rule: &r.body,
                })
                .collect(),
        }
    }

    /// Sort by rank then id, as [`VaultRules`] does.
    fn sorted(mut self) -> Self {
        self.rules
            .sort_by(|a, b| (a.rule.rank, a.id).cmp(&(b.rule.rank, b.id)));
        self
    }

    fn compile(&self) -> Compiled {
        Compiled::of(self.rules.iter().map(|r| r.rule))
    }

    fn id(&self, index: u32) -> &'r str {
        self.rules[index as usize].id
    }
}

/// What a change does to the books: every row it moves. `per_txn`'s rows
/// never move.
fn plan_moves(raw: &Ledger, per_txn: &VaultEdits, before: &Plan, after: &Plan) -> Vec<Moved> {
    moves(
        raw,
        &with_edits(before.compile(), per_txn),
        &with_edits(after.compile(), per_txn),
        |i| before.id(i),
        |i| after.id(i),
    )
}

/// Print what `moved` moves; `extra` is the command's own JSON fields.
fn summarize(
    raw: &Ledger,
    moved: &[Moved],
    before: &Plan,
    after: &Plan,
    write: Write,
    extra: Value,
) {
    // (from, to) -> rows, and the rows each existing rule loses.
    let mut flows: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    let mut taken: BTreeMap<&str, u64> = BTreeMap::new();
    for row in moved {
        *flows
            .entry((&row.before.account, &row.after.account))
            .or_default() += 1;
        if let Some(rule) = row.before.rule {
            let still_here = after.rules.iter().any(|r| r.id == before.id(rule));
            // A rule that is still there lost the row to another; a removed
            // one didn't have it taken.
            if still_here {
                *taken.entry(before.id(rule)).or_default() += 1;
            }
        }
    }
    let journal = raw.journal();
    let sample = moved.iter().take(SAMPLE).map(|row| {
        let txn = journal.txn(row.txn);
        let id = txn.id.map_or("", |id| journal.strings.get(id));
        (txn, id, row)
    });

    if write.json {
        let mut out = json!({
            "dry_run": write.dry_run,
            "moved": moved.len(),
            "moves": flows.iter().map(|((from, to), rows)| json!({"from": from, "to": to, "rows": rows})).collect::<Vec<_>>(),
            "taken": taken.iter().map(|(rule, rows)| json!({"rule": rule, "rows": rows})).collect::<Vec<_>>(),
            "sample": sample.map(|(txn, id, row)| json!({
                "date": txn.date.to_string(),
                "id": id,
                "payee": journal.strings.get(txn.payee),
                "from": row.before.account,
                "to": row.after.account,
            })).collect::<Vec<_>>(),
        });
        if let (Value::Object(out), Value::Object(extra)) = (&mut out, extra) {
            out.extend(extra);
        }
        println!("{out}");
        return;
    }
    let verb = if write.dry_run { "would move" } else { "moved" };
    println!("{verb} {} transactions", moved.len());
    for ((from, to), rows) in &flows {
        println!("{rows:>10}  {from} -> {to}");
    }
    if !taken.is_empty() {
        let from: Vec<String> = taken
            .iter()
            .map(|(rule, rows)| format!("{rule} {rows}"))
            .collect();
        let total: u64 = taken.values().sum();
        println!("taken from other rules: {total} ({})", from.join(", "));
    }
    let mut sample = sample.peekable();
    if sample.peek().is_some() {
        println!("sample:");
    }
    for (txn, id, row) in sample {
        println!(
            "  {}  {id}  {}  {} -> {}",
            txn.date,
            journal.strings.get(txn.payee),
            row.before.account,
            row.after.account
        );
    }
    if write.dry_run {
        println!("dry run: nothing written");
    }
}

/// `rule add`.
async fn add(
    session: &mut Session,
    raw: &Ledger,
    new: &NewRule,
    place: &Place,
    write: Write,
) -> Result<(), String> {
    let txn = session.txn()?;
    let rules = VaultRules::load(&session.ndb, &txn, session.vault.author());
    let per_txn = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    let agentium = session_ref();
    let rank = rules
        .ranks_at(rule_place(place), 1, None)
        .map_err(|e| e.to_string())?
        .remove(0);
    let note = RuleNote {
        patterns: new.patterns.iter().map(|p| p.as_str().into()).collect(),
        meta: new
            .meta
            .iter()
            .map(|m| MetaNeedle {
                key: &m.key,
                needle: m.needle.as_str().into(),
            })
            .collect(),
        amount: new.amount.as_ref().map(|a| AmountCond {
            min: a.min,
            max: a.max,
            commodity: a.commodity.as_deref(),
        }),
        source: new.source.as_deref(),
        layer: new.layer.as_deref(),
        agentium: agentium.as_deref(),
        ..RuleNote::new(&new.account, &rank)
    };
    note.check().map_err(|e| e.to_string())?;
    if !note.has_conditions() {
        return Err(
            "a rule needs a condition (--match, --meta, --amount or --source): one without would take every transaction"
                .to_owned(),
        );
    }
    if let Some(id) = rules.duplicate_of(&note) {
        return Err(format!("rule {id} already does this"));
    }
    let id = new_rule_id(&rules);

    let before = Plan::current(&rules);
    let mut after = Plan::current(&rules);
    after.rules.push(PlannedRule {
        id: &id,
        rule: &note,
    });
    let after = after.sorted();
    let moved = plan_moves(raw, &per_txn, &before, &after);
    let extra = json!({"added": id, "rule": rule_json(&id, &note)});
    if !write.dry_run {
        let written = session
            .vault
            .write(&session.ndb, &id, &note, 0)
            .map_err(|e| e.to_string())?;
        session.publish(&[written.frame]).await?;
    }
    summarize(raw, &moved, &before, &after, write, extra);
    if !write.dry_run && !write.json {
        println!("added rule {id}{}", session.offline_note());
    }
    Ok(())
}

/// `rule mv`.
async fn mv(
    session: &mut Session,
    raw: &Ledger,
    selector: &str,
    place: &Place,
    write: Write,
) -> Result<(), String> {
    let txn = session.txn()?;
    let rules = VaultRules::load(&session.ndb, &txn, session.vault.author());
    let per_txn = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    let target = rules.find(selector).map_err(|e| e.to_string())?;
    let rank = rules
        .ranks_at(rule_place(place), 1, Some(target.id))
        .map_err(|e| e.to_string())?
        .remove(0);
    let moved_note = RuleNote {
        rank: &rank,
        ..target.body.clone()
    };
    let before = Plan::current(&rules);
    let mut after = Plan::current(&rules);
    for planned in &mut after.rules {
        if planned.id == target.id {
            planned.rule = &moved_note;
        }
    }
    let after = after.sorted();
    let moved = plan_moves(raw, &per_txn, &before, &after);
    if !write.dry_run {
        let written = session
            .vault
            .write(&session.ndb, target.id, &moved_note, target.created_at)
            .map_err(|e| e.to_string())?;
        session.publish(&[written.frame]).await?;
    }
    summarize(
        raw,
        &moved,
        &before,
        &after,
        write,
        json!({"rule": target.id}),
    );
    if !write.dry_run && !write.json {
        println!("moved rule {}{}", target.id, session.offline_note());
    }
    Ok(())
}

/// `rule rm`: rewrite the rule as a tombstone.
async fn rm(
    session: &mut Session,
    raw: &Ledger,
    selector: &str,
    write: Write,
) -> Result<(), String> {
    let txn = session.txn()?;
    let rules = VaultRules::load(&session.ndb, &txn, session.vault.author());
    let per_txn = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    let target = rules.find(selector).map_err(|e| e.to_string())?;
    let tombstone = RuleNote {
        deleted: true,
        ..target.body.clone()
    };
    let before = Plan::current(&rules);
    let mut after = Plan::current(&rules);
    after.rules.retain(|r| r.id != target.id);
    let moved = plan_moves(raw, &per_txn, &before, &after);
    if !write.dry_run {
        let written = session
            .vault
            .write(&session.ndb, target.id, &tombstone, target.created_at)
            .map_err(|e| e.to_string())?;
        session.publish(&[written.frame]).await?;
    }
    summarize(
        raw,
        &moved,
        &before,
        &after,
        write,
        json!({"removed": target.id}),
    );
    if !write.dry_run && !write.json {
        println!("removed rule {}{}", target.id, session.offline_note());
    }
    Ok(())
}

/// `rule import <file>`: append the file's rules in file order, skipping
/// the ones the vault already has.
async fn import(
    session: &mut Session,
    raw: &Ledger,
    path: &Path,
    write: Write,
) -> Result<(), String> {
    let file = rules::read(path).map_err(|e| e.to_string())?;
    let base = file.base_edits();
    let builtin = rules::default_edits();
    if base.defaults != builtin.defaults || base.transfers != builtin.transfers {
        eprintln!(
            "cashflow: {} sets defaults, unknown or transfers; the vault keeps only the built-in ones ({}; transfers to {}), so its rules apply to those",
            path.display(),
            builtin.defaults.join(", "),
            builtin.transfers
        );
    }
    let txn = session.txn()?;
    let rules = VaultRules::load(&session.ndb, &txn, session.vault.author());
    let per_txn = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    let fresh: Vec<_> = file
        .rules
        .iter()
        .filter(|r| rules.duplicate_of(&r.note("")).is_none())
        .collect();
    let skipped = file.rules.len() - fresh.len();
    let ranks = rules
        .ranks_at(RulePlace::Last, fresh.len(), None)
        .map_err(|e| e.to_string())?;
    let agentium = session_ref();
    let mut ids: Vec<String> = Vec::with_capacity(fresh.len());
    let mut notes: Vec<RuleNote> = Vec::with_capacity(fresh.len());
    for (index, (rule, rank)) in fresh.iter().zip(&ranks).enumerate() {
        let note = RuleNote {
            agentium: agentium.as_deref(),
            ..rule.note(rank)
        };
        note.check().map_err(|e| {
            format!(
                "{}: rule {} ({}): {e}",
                path.display(),
                index + 1,
                note.account
            )
        })?;
        notes.push(note);
        // Each id is new to the vault and to the ones minted before it.
        let id = loop {
            let id = new_rule_id(&rules);
            if !ids.contains(&id) {
                break id;
            }
        };
        ids.push(id);
    }

    let before = Plan::current(&rules);
    let mut after = Plan::current(&rules);
    after.rules.extend(
        ids.iter()
            .zip(&notes)
            .map(|(id, rule)| PlannedRule { id, rule }),
    );
    let after = after.sorted();
    let moved = plan_moves(raw, &per_txn, &before, &after);
    if !write.dry_run {
        let mut frames = Vec::with_capacity(notes.len());
        for (id, note) in ids.iter().zip(&notes) {
            let written = session
                .vault
                .write(&session.ndb, id, note, 0)
                .map_err(|e| e.to_string())?;
            frames.push(written.frame);
        }
        session.publish(&frames).await?;
    }
    let extra = json!({"imported": notes.len(), "skipped": skipped, "ids": ids});
    summarize(raw, &moved, &before, &after, write, extra);
    if !write.json {
        let verb = if write.dry_run {
            "would import"
        } else {
            "imported"
        };
        let offline = if write.dry_run {
            ""
        } else {
            session.offline_note()
        };
        println!(
            "{verb} {} rules from {} ({skipped} already in the vault){offline}",
            notes.len(),
            path.display()
        );
    }
    Ok(())
}
