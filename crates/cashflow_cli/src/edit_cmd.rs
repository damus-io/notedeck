//! The per-transaction edit commands: `categorize`, `uncategorize`,
//! `split`, `transfer` and `edits` ([`cashflow::vault_edits`]), and the
//! triage queue, `unknown` ([`cashflow::unknown`]).
//!
//! A write derives the raw ledger with the vault's edits before and after
//! the change, so it can print the transaction's legs both ways and refuse
//! an edit derive would reject (a split that doesn't sum, a link that
//! doesn't net) before anything is stored. `--dry-run` stops there.

use cashflow::transfers;
use cashflow::unknown::{self, Scope};
use cashflow::vault::{Record, Written};
use cashflow::vault_edits::{
    EditNote, LinkBody, LinkNote, OverrideBody, OverrideNote, SplitBody, SplitLeg, SplitNote,
    VaultEdits, link_id,
};
use cashflow::{Books, CashflowDir, journal};
use ledgerdb::{
    AmountText, Derived, Edits, LayerFilter, Ledger, Override, Split, TransferLink, TxnId, Why,
    derive,
};
use serde_json::{Value, json};

use crate::args::{LegArg, TransferCommand, UnknownArgs, Write};
use crate::books_cmd::{format_total, raw_ledger, vault_edits};
use crate::vault_cmd::{Session, session_ref};

/// The raw transaction with record id `record`, or an error.
fn find(raw: &Ledger, record: &str) -> Result<TxnId, String> {
    raw.find(record)
        .ok_or_else(|| format!("no transaction with id '{record}' in the journal"))
}

/// The name derive's report and `rule test` give each tier.
fn tier(why: Why) -> &'static str {
    match why {
        Why::Untouched => "untouched",
        Why::Default => "default",
        Why::Rule(_) => "rule",
        Why::Transfer(_) => "transfer",
        Why::Override(_) => "override",
        Why::Split(_) => "split",
    }
}

/// One posting as shown: its account and amount.
struct Leg {
    account: String,
    amount: String,
}

/// How a derive left one transaction: which tier decided it and the legs
/// it decided, leaving out the source's own legs, which no edit moves.
struct Categorized {
    tier: &'static str,
    legs: Vec<Leg>,
}

impl Categorized {
    /// Transaction `txn` in `derived`, whose raw form is in `raw` and whose
    /// default accounts are `edits.defaults`.
    fn of(raw: &Ledger, derived: &Derived, edits: &Edits, txn: TxnId) -> Self {
        let raw_journal = raw.journal();
        let own: Vec<&str> = raw_journal
            .postings_of(raw_journal.txn(txn))
            .iter()
            .map(|p| raw_journal.accounts.full_name(p.account))
            .filter(|name| !edits.defaults.iter().any(|d| d == name))
            .collect();
        let journal = derived.ledger().journal();
        let legs = journal
            .postings_of(journal.txn(txn))
            .iter()
            .filter(|p| !own.contains(&journal.accounts.full_name(p.account)))
            .map(|p| Leg {
                account: journal.accounts.full_name(p.account).to_owned(),
                amount: p.amount().display(&journal.commodities).to_string(),
            })
            .collect();
        Categorized {
            tier: tier(derived.why(txn)),
            legs,
        }
    }

    fn json(&self) -> Value {
        json!({
            "decided_by": self.tier,
            "legs": self.legs.iter().map(|l| json!({"account": l.account, "amount": l.amount})).collect::<Vec<_>>(),
        })
    }

    /// `tier  account amount, account amount`.
    fn line(&self) -> String {
        let legs: Vec<String> = self
            .legs
            .iter()
            .map(|l| format!("{} {}", l.account, l.amount))
            .collect();
        format!("{:<9} {}", self.tier, legs.join(", "))
    }
}

/// One transaction a change touches, before and after.
struct Change {
    record: String,
    date: String,
    payee: String,
    before: Categorized,
    after: Categorized,
}

impl Change {
    fn json(&self) -> Value {
        json!({
            "record_id": self.record,
            "date": self.date,
            "payee": self.payee,
            "before": self.before.json(),
            "after": self.after.json(),
        })
    }

    fn print(&self) {
        println!("{}  {}  {}", self.record, self.date, self.payee);
        println!("  before  {}", self.before.line());
        println!("  after   {}", self.after.line());
    }
}

/// A change to the vault's edits, planned: the edits as they stand and as
/// they would be.
struct Planned<'r> {
    raw: &'r Ledger,
    before: Edits,
    after: Edits,
}

/// The derives of a [`Planned`] change.
struct Derives {
    before: Derived,
    after: Derived,
}

impl Planned<'_> {
    fn derive(&self) -> Derives {
        Derives {
            before: derive(self.raw, &self.before),
            after: derive(self.raw, &self.after),
        }
    }

    /// Transaction `record` before and after.
    fn change(&self, derives: &Derives, record: &str) -> Result<Change, String> {
        let txn = find(self.raw, record)?;
        let journal = self.raw.journal();
        let found = journal.txn(txn);
        Ok(Change {
            record: record.to_owned(),
            date: found.date.to_string(),
            payee: journal.strings.get(found.payee).to_owned(),
            before: Categorized::of(self.raw, &derives.before, &self.before, txn),
            after: Categorized::of(self.raw, &derives.after, &self.after, txn),
        })
    }
}

/// Fail unless derive applied the edit to `record` after the change: it is
/// decided by `want`. Otherwise say what derive rejected about it.
fn require_applied(
    raw: &Ledger,
    derived: &Derived,
    record: &str,
    want: fn(Why) -> bool,
    what: &str,
) -> Result<(), String> {
    let txn = find(raw, record)?;
    if want(derived.why(txn)) {
        return Ok(());
    }
    let problems: Vec<String> = derived
        .report()
        .rejected
        .iter()
        .map(|r| format!("{:?}", r.problem))
        .collect();
    let why = match derived.why(txn) {
        Why::Untouched => {
            "it has no counter leg (no posting, or more than one, to a default account)".to_owned()
        }
        other if problems.is_empty() => format!("a {} decides it", tier(other)),
        _ => problems.join("; "),
    };
    Err(format!("the {what} on {record} wouldn't apply: {why}"))
}

/// Fail when `record` is in a live transfer link: an override or split
/// would shadow the link and leave its other side half-netted.
fn refuse_linked(edits: &VaultEdits, record: &str) -> Result<(), String> {
    match edits.links_of(record).next() {
        Some(link) => {
            let other = link.body.body.map_or("", |b| b.other(record));
            Err(format!(
                "{record} is linked as a transfer with {other}: `cashflow transfer unlink {record} {other}` first"
            ))
        }
        None => Ok(()),
    }
}

/// The `d` tag a record of type `T` with id `id` is stored at.
fn d_of<'a, T: Record<'a>>(id: &str) -> String {
    format!("{}:{id}", T::PREFIX)
}

/// Write each of `writes` to the vault and publish them together.
async fn store(session: &mut Session, writes: Vec<Pending<'_>>) -> Result<(), String> {
    let mut frames = Vec::with_capacity(writes.len());
    for pending in writes {
        let written = pending.write(session)?;
        frames.push(written.frame);
    }
    session.publish(&frames).await
}

/// One vault write a command makes.
enum Pending<'a> {
    Override {
        id: &'a str,
        note: OverrideNote<'a>,
        after: u64,
    },
    Split {
        id: &'a str,
        note: SplitNote<'a>,
        after: u64,
    },
    Link {
        id: String,
        note: LinkNote<'a>,
        after: u64,
    },
}

impl Pending<'_> {
    fn write(&self, session: &Session) -> Result<Written, String> {
        let ndb = &session.ndb;
        let written = match self {
            Pending::Override { id, note, after } => session.vault.write(ndb, id, note, *after),
            Pending::Split { id, note, after } => session.vault.write(ndb, id, note, *after),
            Pending::Link { id, note, after } => session.vault.write(ndb, id, note, *after),
        };
        written.map_err(|e| e.to_string())
    }
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

fn legs_json(legs: &[SplitLeg]) -> Value {
    legs.iter()
        .map(|l| json!({"account": l.account, "qty": l.qty}))
        .collect()
}

/// Print a single-transaction write's outcome.
fn report(
    session: &Session,
    write: Write,
    changes: &[Change],
    d: &str,
    replaced: Value,
    verb: &str,
) {
    if write.json {
        let out = json!({
            "dry_run": write.dry_run,
            "d": d,
            "replaced": replaced,
            "changes": changes.iter().map(Change::json).collect::<Vec<_>>(),
        });
        println!("{out}");
        return;
    }
    for change in changes {
        change.print();
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

/// Drop every override and split of `record` from `edits`.
fn without_categorization(edits: &mut Edits, record: &str) {
    edits.overrides.retain(|o| o.record_id != record);
    edits.splits.retain(|s| s.record_id != record);
}

/// The clears that go with replacing `record`'s categorization: its live
/// override (unless `keep_override`) and live split (unless `keep_split`).
fn clears<'a>(
    stored: &VaultEdits<'a>,
    record: &'a str,
    agentium: Option<&'a str>,
    keep: Keep,
) -> Vec<Pending<'a>> {
    let mut out = Vec::new();
    if let Some(o) = stored.override_of(record)
        && o.body.body.is_some()
        && !keep.override_note
    {
        out.push(Pending::Override {
            id: record,
            note: EditNote::cleared(agentium),
            after: o.created_at,
        });
    }
    if let Some(s) = stored.split_of(record)
        && s.body.body.is_some()
        && !keep.split_note
    {
        out.push(Pending::Split {
            id: record,
            note: EditNote::cleared(agentium),
            after: s.created_at,
        });
    }
    out
}

/// Which of a record's notes [`clears`] leaves alone, because the command
/// rewrites it anyway.
#[derive(Clone, Copy, Default)]
struct Keep {
    override_note: bool,
    split_note: bool,
}

/// `categorize <record-id> <account>`.
pub async fn categorize(
    dir: &CashflowDir,
    session: &mut Session,
    record: &str,
    account: &str,
    write: Write,
) -> Result<(), String> {
    let raw = raw_ledger(dir)?;
    find(&raw, record)?;
    let before = vault_edits(session)?;
    let txn = session.txn()?;
    let stored = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    refuse_linked(&stored, record)?;
    let agentium = session_ref();
    let note = OverrideNote::new(OverrideBody { account }, agentium.as_deref());
    note.check().map_err(|e| e.to_string())?;

    let mut after = before.clone();
    without_categorization(&mut after, record);
    after.overrides.push(Override {
        record_id: record.to_owned(),
        account: account.to_owned(),
    });
    let plan = Planned {
        raw: &raw,
        before,
        after,
    };
    let derives = plan.derive();
    require_applied(
        &raw,
        &derives.after,
        record,
        |w| matches!(w, Why::Override(_)),
        "override",
    )?;
    let change = plan.change(&derives, record)?;
    // What the write replaces: the override it rewrites, else the split it
    // clears.
    let replaced = override_json(&stored, record)
        .or_else(|| split_json(&stored, record))
        .unwrap_or(Value::Null);
    if !write.dry_run {
        let mut writes = clears(
            &stored,
            record,
            agentium.as_deref(),
            Keep {
                override_note: true,
                split_note: false,
            },
        );
        writes.push(Pending::Override {
            id: record,
            note,
            after: stored.override_of(record).map_or(0, |o| o.created_at),
        });
        store(session, writes).await?;
    }
    report(
        session,
        write,
        &[change],
        &d_of::<OverrideNote>(record),
        replaced,
        "wrote",
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
    let raw = raw_ledger(dir)?;
    find(&raw, record)?;
    let before = vault_edits(session)?;
    let txn = session.txn()?;
    let stored = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    let agentium = session_ref();
    let writes = clears(&stored, record, agentium.as_deref(), Keep::default());
    if writes.is_empty() {
        return Err(format!("{record} has no override or split to clear"));
    }
    let replaced = override_json(&stored, record)
        .or_else(|| split_json(&stored, record))
        .unwrap_or(Value::Null);
    let d = replaced["d"].as_str().unwrap_or_default().to_owned();
    let mut after = before.clone();
    without_categorization(&mut after, record);
    let plan = Planned {
        raw: &raw,
        before,
        after,
    };
    let change = plan.change(&plan.derive(), record)?;
    if !write.dry_run {
        store(session, writes).await?;
    }
    report(session, write, &[change], &d, Value::Null, "cleared");
    Ok(())
}

/// The counter leg of `txn` in `raw`: its one posting to a default.
fn counter_leg<'l>(raw: &'l Ledger, edits: &Edits, txn: TxnId) -> Option<&'l ledgerdb::Posting> {
    let journal = raw.journal();
    let mut on_default = journal.postings_of(journal.txn(txn)).iter().filter(|p| {
        edits
            .defaults
            .iter()
            .any(|d| d == journal.accounts.full_name(p.account))
    });
    let counter = on_default.next()?;
    on_default.next().is_none().then_some(counter)
}

/// `legs` in the counter leg's smallest units and sign, the remainder leg
/// filled in.
fn split_legs<'l>(
    raw: &Ledger,
    counter: &ledgerdb::Posting,
    legs: &'l [LegArg],
) -> Result<Vec<SplitLeg<'l>>, String> {
    let commodities = &raw.journal().commodities;
    let commodity = commodities.name(counter.commodity);
    let precision = commodities.precision(counter.commodity);
    let sign = if counter.qty < 0 { -1 } else { 1 };
    let mut out = Vec::with_capacity(legs.len());
    let mut rest_at = None;
    for leg in legs {
        let Some(text) = &leg.amount else {
            rest_at = Some(out.len());
            out.push(SplitLeg {
                account: &leg.account,
                qty: 0,
            });
            continue;
        };
        let amount = AmountText::parse(text).map_err(|e| format!("{}={text}: {e}", leg.account))?;
        if !amount.commodity.is_empty() && amount.commodity != commodity {
            return Err(format!(
                "{}={text}: the transaction's counter leg is in {commodity}",
                leg.account
            ));
        }
        let qty = amount
            .qty_at(precision)
            .map_err(|e| format!("{}={text}: {e}", leg.account))?;
        out.push(SplitLeg {
            account: &leg.account,
            qty: qty * sign,
        });
    }
    let sum: i128 = out.iter().map(|l| i128::from(l.qty)).sum();
    let want = i128::from(counter.qty);
    let shown = |qty: i128| {
        i64::try_from(qty).map_or(qty.to_string(), |qty| {
            ledgerdb::Amount {
                qty,
                commodity: counter.commodity,
            }
            .display(commodities)
            .to_string()
        })
    };
    match rest_at {
        Some(at) => {
            let rest =
                i64::try_from(want - sum).map_err(|_| "the legs are too large".to_owned())?;
            out[at].qty = rest;
        }
        None if sum != want => {
            return Err(format!(
                "the legs add up to {}, but the counter leg is {}",
                shown(sum),
                shown(want)
            ));
        }
        None => {}
    }
    Ok(out)
}

/// `split <record-id> <account>[=<amount>]...`.
pub async fn split(
    dir: &CashflowDir,
    session: &mut Session,
    record: &str,
    legs: &[LegArg],
    write: Write,
) -> Result<(), String> {
    let raw = raw_ledger(dir)?;
    let id = find(&raw, record)?;
    let before = vault_edits(session)?;
    let counter = counter_leg(&raw, &before, id).ok_or_else(|| {
        format!("{record} has no counter leg to split (no posting, or more than one, to a default account)")
    })?;
    let txn = session.txn()?;
    let stored = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    refuse_linked(&stored, record)?;
    let agentium = session_ref();
    let legs = split_legs(&raw, counter, legs)?;
    let note = SplitNote::new(SplitBody { legs: legs.clone() }, agentium.as_deref());
    note.check().map_err(|e| e.to_string())?;

    let mut after = before.clone();
    without_categorization(&mut after, record);
    after.splits.push(Split {
        record_id: record.to_owned(),
        legs: legs
            .iter()
            .map(|l| ledgerdb::Leg {
                account: l.account.to_owned(),
                qty: l.qty,
            })
            .collect(),
    });
    let plan = Planned {
        raw: &raw,
        before,
        after,
    };
    let derives = plan.derive();
    require_applied(
        &raw,
        &derives.after,
        record,
        |w| matches!(w, Why::Split(_)),
        "split",
    )?;
    let change = plan.change(&derives, record)?;
    // What the write replaces: the split it rewrites, else the override it
    // clears.
    let replaced = split_json(&stored, record)
        .or_else(|| override_json(&stored, record))
        .unwrap_or(Value::Null);
    if !write.dry_run {
        let mut writes = clears(
            &stored,
            record,
            agentium.as_deref(),
            Keep {
                override_note: false,
                split_note: true,
            },
        );
        writes.push(Pending::Split {
            id: record,
            note,
            after: stored.split_of(record).map_or(0, |s| s.created_at),
        });
        store(session, writes).await?;
    }
    report(
        session,
        write,
        &[change],
        &d_of::<SplitNote>(record),
        replaced,
        "wrote",
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

/// Fail unless `record` can join a new link: it exists, and no override,
/// split or other link decides it.
fn linkable(raw: &Ledger, stored: &VaultEdits, record: &str) -> Result<(), String> {
    find(raw, record)?;
    if stored
        .override_of(record)
        .is_some_and(|o| o.body.body.is_some())
        || stored
            .split_of(record)
            .is_some_and(|s| s.body.body.is_some())
    {
        return Err(format!(
            "{record} has an override or split: `cashflow uncategorize {record}` first"
        ));
    }
    refuse_linked(stored, record)
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
    let raw = raw_ledger(dir)?;
    let before = vault_edits(session)?;
    let txn = session.txn()?;
    let stored = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    linkable(&raw, &stored, a)?;
    linkable(&raw, &stored, b)?;
    let agentium = session_ref();
    let note = LinkNote::new(LinkBody { a, b }, agentium.as_deref());

    let mut after = before.clone();
    after.links.push(TransferLink {
        a: a.to_owned(),
        b: b.to_owned(),
    });
    let plan = Planned {
        raw: &raw,
        before,
        after,
    };
    let derives = plan.derive();
    for side in [a, b] {
        require_applied(
            &raw,
            &derives.after,
            side,
            |w| matches!(w, Why::Transfer(_)),
            "link",
        )?;
    }
    let changes = [plan.change(&derives, a)?, plan.change(&derives, b)?];
    if !write.dry_run {
        let after = stored.link(&id).map_or(0, |l| l.created_at);
        store(
            session,
            vec![Pending::Link {
                id: id.clone(),
                note,
                after,
            }],
        )
        .await?;
    }
    report(
        session,
        write,
        &changes,
        &d_of::<LinkNote>(&id),
        Value::Null,
        "wrote",
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
    let raw = raw_ledger(dir)?;
    let before = vault_edits(session)?;
    let txn = session.txn()?;
    let stored = VaultEdits::load(&session.ndb, &txn, session.vault.author());
    let Some(existing) = stored.link(&id).filter(|l| l.body.body.is_some()) else {
        return Err(format!("{a} and {b} aren't linked"));
    };
    let mut after = before.clone();
    after
        .links
        .retain(|l| link_id(&l.a, &l.b).is_ok_and(|other| other != id));
    let plan = Planned {
        raw: &raw,
        before,
        after,
    };
    let derives = plan.derive();
    let mut changes = Vec::with_capacity(2);
    for side in [a, b] {
        if raw.find(side).is_some() {
            changes.push(plan.change(&derives, side)?);
        }
    }
    if !write.dry_run {
        let agentium = session_ref();
        store(
            session,
            vec![Pending::Link {
                id: id.clone(),
                note: EditNote::cleared(agentium.as_deref()),
                after: existing.created_at,
            }],
        )
        .await?;
    }
    report(
        session,
        write,
        &changes,
        &d_of::<LinkNote>(&id),
        Value::Null,
        "cleared",
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
    let raw = raw_ledger(dir)?;
    let edits = vault_edits(session)?;
    let pairs = transfers::suggest(&raw, &edits);
    let writing = accept_all && !write.dry_run;
    if writing {
        let agentium = session_ref();
        let txn = session.txn()?;
        let stored = VaultEdits::load(&session.ndb, &txn, session.vault.author());
        let mut writes = Vec::with_capacity(pairs.len());
        for pair in &pairs {
            let id = link_id(&pair.link.a, &pair.link.b).map_err(|e| e.to_string())?;
            let after = stored.link(&id).map_or(0, |l| l.created_at);
            writes.push(Pending::Link {
                id,
                note: LinkNote::new(
                    LinkBody {
                        a: &pair.link.a,
                        b: &pair.link.b,
                    },
                    agentium.as_deref(),
                ),
                after,
            });
        }
        store(session, writes).await?;
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
    let books = Books::load(&journal::store(dir), LayerFilter::All, &edits, &args.home)
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
