//! The chart-of-accounts commands: `account ls`, `account set`, `account
//! alias` and `account unalias` ([`cashflow::chart`]).
//!
//! `ls` reads the derived books and the vault's account metadata together.
//! `alias` and `unalias` derive the journal with the vault's edits before
//! and after the change, so they print every account that moves (and how
//! many postings with it) before anything is stored; `--dry-run` stops
//! there.

use std::collections::BTreeMap;

use cashflow::CashflowDir;
use cashflow::chart::{
    self, AccountMeta, AliasBody, AliasNote, Chart, Inherited, Resolved, account_of, record_id,
};
use ledgerdb::{AccountId, Alias, Date, Derived, EditRef, Edits, derive};
use serde_json::{Value, json};

use crate::args::{BooksArgs, MetaArgs, Run, Write};
use crate::books_cmd::{books_with, format_total, raw_ledger, sorted, vault_edits};
use crate::vault_cmd::{Session, session_ref};

/// What `account ls` lists: where it starts, how deep, and whether hidden
/// and empty accounts are included.
pub struct LsArgs<'a> {
    pub books: &'a BooksArgs,
    pub account: Option<&'a str>,
    pub depth: Option<u16>,
    pub all: bool,
    pub json: bool,
}

/// One account `account ls` prints.
struct Row<'a> {
    account: &'a str,
    depth: u16,
    balance: String,
    meta: Resolved<'a>,
}

impl Row<'_> {
    /// Whether the account sets its project itself rather than inheriting it.
    fn starts_project(&self) -> bool {
        self.meta
            .project
            .is_some_and(|p| p.from == record_id(self.account))
    }

    fn json(&self) -> Value {
        fn field<T: Into<Value>>(f: Option<Inherited<'_, T>>) -> Value {
            f.map_or(
                Value::Null,
                |f| json!({"value": f.value.into(), "from": account_of(f.from)}),
            )
        }
        let m = &self.meta;
        json!({
            "account": self.account,
            "depth": self.depth,
            "balance": self.balance,
            "name": m.name,
            "icon": field(m.icon),
            "color": field(m.color),
            "project": field(m.project),
            "hidden": field(m.hidden),
            "type": field(m.kind.map(|k| Inherited { value: k.value.as_str(), from: k.from })),
        })
    }

    /// `balance  account  icon name  [project]  (hidden)`.
    fn print(&self) {
        let mut line = format!("{:>24}  {}", self.balance, self.account);
        let icon = self.meta.icon.map(|i| i.value);
        let decor: Vec<&str> = [icon, self.meta.name].into_iter().flatten().collect();
        if !decor.is_empty() {
            line.push_str("  ");
            line.push_str(&decor.join(" "));
        }
        if let Some(project) = self.meta.project.filter(|_| self.starts_project()) {
            line.push_str(&format!("  [{}]", project.value));
        }
        if self.meta.is_hidden() {
            line.push_str("  (hidden)");
        }
        println!("{line}");
    }
}

/// Whether `account` or anything under it has a posting.
fn has_postings(ledger: &ledgerdb::Ledger, account: AccountId) -> bool {
    !ledger.account_postings(account).is_empty()
        || ledger
            .journal()
            .accounts
            .children(account)
            .iter()
            .any(|&child| has_postings(ledger, child))
}

/// `account ls`: the tree with balances and metadata, then the metadata of
/// accounts the ledger doesn't hold, then the aliases.
pub async fn ls(dir: &CashflowDir, run: &Run, args: LsArgs<'_>) -> Result<(), String> {
    let session = Session::open_if_logged_in(dir, run).await?;
    let books = books_with(dir, session.as_ref(), args.books)?;
    let txn = session.as_ref().map(Session::txn).transpose()?;
    let chart = match (&session, &txn) {
        (Some(session), Some(txn)) => Chart::load(&session.ndb, txn, session.vault.author()),
        _ => Chart::empty(),
    };

    let ledger = books.ledger();
    let journal = ledger.journal();
    let accounts = &journal.accounts;
    let mut stack: Vec<AccountId> = match args.account {
        Some(name) => vec![
            accounts
                .lookup(name)
                .ok_or_else(|| format!("no account named '{name}' in the ledger"))?,
        ],
        None => sorted(journal, accounts.roots()),
    };
    stack.reverse();
    let into = books.home();
    let query = books.query();
    let mut rows = Vec::new();
    while let Some(id) = stack.pop() {
        let name = accounts.full_name(id);
        let meta = chart.resolve(name);
        let descend = args.depth.is_none_or(|max| accounts.depth(id) < max);
        if !args.all && !has_postings(ledger, id) {
            continue;
        }
        if descend {
            let mut children = sorted(journal, accounts.children(id));
            children.reverse();
            stack.extend(children);
        }
        // A hidden account's children are still visited: one may show
        // itself again.
        if !args.all && meta.is_hidden() {
            continue;
        }
        let totals: Vec<String> = query
            .balance(id, Date::from_days(u32::MAX), into)
            .totals
            .iter()
            .map(|t| format_total(t.qty, t.commodity, &journal.commodities))
            .collect();
        rows.push(Row {
            account: name,
            depth: accounts.depth(id),
            balance: totals.join(", "),
            meta,
        });
    }
    let unused: Vec<_> = chart
        .accounts
        .iter()
        .filter(|s| !s.body.is_empty() && accounts.lookup(&account_of(s.id)).is_none())
        .collect();
    let aliases: Vec<_> = chart
        .aliases
        .iter()
        .filter_map(|s| Some((account_of(s.id), s.body.body?.to, s.body.agentium)))
        .collect();

    if args.json {
        let out = json!({
            "accounts": rows.iter().map(Row::json).collect::<Vec<_>>(),
            "not_in_ledger": unused.iter().map(|s| json!({"account": account_of(s.id), "meta": meta_json(&s.body)})).collect::<Vec<_>>(),
            "aliases": aliases.iter().map(|(from, to, agentium)| json!({"from": from, "to": to, "agentium": agentium})).collect::<Vec<_>>(),
        });
        println!("{out}");
        return Ok(());
    }
    for row in &rows {
        row.print();
    }
    if !unused.is_empty() {
        println!("\nmetadata for accounts not in the ledger:");
        for stored in &unused {
            println!("  {}  {}", account_of(stored.id), fields_line(&stored.body));
        }
    }
    if !aliases.is_empty() {
        println!("\naliases:");
        for (from, to, _) in &aliases {
            println!("  {from} -> {to}");
        }
    }
    Ok(())
}

/// The fields `meta` sets, as JSON.
fn meta_json(meta: &AccountMeta) -> Value {
    json!({
        "name": meta.name,
        "icon": meta.icon,
        "color": meta.color,
        "project": meta.project,
        "hidden": meta.hidden,
        "type": meta.kind.map(|k| k.as_str()),
        "agentium": meta.agentium,
    })
}

/// The fields `meta` sets, as `field=value` words.
fn fields_line(meta: &AccountMeta) -> String {
    let hidden = meta.hidden.map(|h| if h { "yes" } else { "no" });
    let fields = [
        ("name", meta.name),
        ("icon", meta.icon),
        ("color", meta.color),
        ("project", meta.project),
        ("hidden", hidden),
        ("type", meta.kind.map(|k| k.as_str())),
    ];
    let words: Vec<String> = fields
        .iter()
        .filter_map(|(field, value)| value.map(|v| format!("{field}={v}")))
        .collect();
    if words.is_empty() {
        "(no metadata)".to_owned()
    } else {
        words.join(" ")
    }
}

/// `account set <account> ...`: merge the given fields into the account's
/// metadata note.
pub async fn set(
    session: &mut Session,
    account: &str,
    set: &MetaArgs,
    write: Write,
) -> Result<(), String> {
    let txn = session.txn()?;
    let chart = Chart::load(&session.ndb, &txn, session.vault.author());
    let stored = chart.meta(account);
    let before = stored.map(|s| s.body.clone()).unwrap_or_default();
    let agentium = session_ref();
    let mut after = before.clone();
    if let Some(name) = &set.name {
        after.name = Some(name);
    }
    if let Some(icon) = &set.icon {
        after.icon = Some(icon);
    }
    if let Some(color) = &set.color {
        after.color = Some(color);
    }
    if let Some(project) = &set.project {
        after.project = Some(project);
    }
    if set.hidden.is_some() {
        after.hidden = set.hidden;
    }
    if set.kind.is_some() {
        after.kind = set.kind;
    }
    for &field in &set.clear {
        after.clear(field);
    }
    after.agentium = agentium.as_deref();
    after.check(account).map_err(|e| e.to_string())?;

    let id = record_id(account);
    let d = format!("acct:{id}");
    if !write.dry_run {
        let written = session
            .vault
            .write(
                &session.ndb,
                &id,
                &after,
                stored.map_or(0, |s| s.created_at),
            )
            .map_err(|e| e.to_string())?;
        session.publish(&[written.frame]).await?;
    }
    if write.json {
        let out = json!({
            "dry_run": write.dry_run,
            "d": d,
            "before": meta_json(&before),
            "after": meta_json(&after),
        });
        println!("{out}");
        return Ok(());
    }
    println!("{account}");
    println!("  before  {}", fields_line(&before));
    println!("  after   {}", fields_line(&after));
    if write.dry_run {
        println!("dry run: would set {d}; nothing written");
    } else {
        println!("set {d}{}", session.offline_note());
    }
    Ok(())
}

/// Every account a change renames, with the postings that move with it.
struct Move {
    from: String,
    to: String,
    postings: usize,
}

/// The accounts whose postings sit somewhere else in `after` than in
/// `before`, two derives of the same raw ledger that differ only in their
/// aliases, so their postings line up one for one.
fn moves(before: &Derived, after: &Derived) -> Vec<Move> {
    let old = before.ledger().journal();
    let new = after.ledger().journal();
    let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (a, b) in old.postings.iter().zip(&new.postings) {
        let from = old.accounts.full_name(a.account);
        let to = new.accounts.full_name(b.account);
        if from != to {
            *counts.entry((from, to)).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .map(|((from, to), postings)| Move {
            from: from.to_owned(),
            to: to.to_owned(),
            postings,
        })
        .collect()
}

/// `account alias <from> <to>` (`to` set) or `account unalias <from>`
/// (`to` `None`).
pub async fn alias(
    dir: &CashflowDir,
    session: &mut Session,
    from: &str,
    to: Option<&str>,
    write: Write,
) -> Result<(), String> {
    if let Some(to) = to {
        chart::check_alias(from, to).map_err(|e| e.to_string())?;
    }
    // Each of these opens its own transaction, and nostrdb allows one per
    // thread, so they run before the chart's.
    let raw = raw_ledger(dir, session)?;
    let before: Edits = vault_edits(session)?;
    let txn = session.txn()?;
    let chart = Chart::load(&session.ndb, &txn, session.vault.author());
    let stored = chart.alias(from);
    let live = stored.and_then(|s| s.body.body);
    if to.is_none() && live.is_none() {
        return Err(format!("there is no alias of {from} to clear"));
    }
    let mut after = before.clone();
    after.aliases.retain(|a| a.from != from);
    if let Some(to) = to {
        after.aliases.push(Alias {
            from: from.to_owned(),
            to: to.to_owned(),
        });
    }
    let derived_before = derive(&raw, &before);
    let derived_after = derive(&raw, &after);
    if to.is_some() {
        let this = EditRef::Alias(after.aliases.len() as u32 - 1);
        if let Some(rejected) = derived_after
            .report()
            .rejected
            .iter()
            .find(|r| r.edit == this)
        {
            return Err(format!("the alias wouldn't apply: {:?}", rejected.problem));
        }
    }
    let moved = moves(&derived_before, &derived_after);

    let id = record_id(from);
    let d = format!("alias:{id}");
    if !write.dry_run {
        let agentium = session_ref();
        let note = match to {
            Some(to) => AliasNote::new(AliasBody { to }, agentium.as_deref()),
            None => AliasNote::cleared(agentium.as_deref()),
        };
        let written = session
            .vault
            .write(&session.ndb, &id, &note, stored.map_or(0, |s| s.created_at))
            .map_err(|e| e.to_string())?;
        session.publish(&[written.frame]).await?;
    }

    let verb = if to.is_some() { "alias" } else { "clear" };
    if write.json {
        let out = json!({
            "dry_run": write.dry_run,
            "d": d,
            "replaced": live.map(|b| b.to),
            "to": to,
            "moves": moved.iter().map(|m| json!({"from": m.from, "to": m.to, "postings": m.postings})).collect::<Vec<_>>(),
        });
        println!("{out}");
        return Ok(());
    }
    for m in &moved {
        println!("  {} -> {}  ({} postings)", m.from, m.to, m.postings);
    }
    if moved.is_empty() {
        println!("  no posting moves (nothing under {from} in the journal yet)");
    }
    let replaced = match (live, to) {
        (Some(old), Some(_)) => format!(" (replacing -> {})", old.to),
        _ => String::new(),
    };
    if write.dry_run {
        println!("dry run: would {verb} {d}{replaced}; nothing written");
    } else {
        let done = if to.is_some() { "aliased" } else { "cleared" };
        println!("{done} {d}{replaced}{}", session.offline_note());
    }
    Ok(())
}
