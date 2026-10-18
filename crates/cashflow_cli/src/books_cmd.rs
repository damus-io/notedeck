//! The commands that read the categorized books: `export` and `report`.
//!
//! Each loads [`Books`] once (the journal less the segments the vault
//! retires, derived with the vault's rules or `--rules`, indexed in `--in`'s
//! commodity) and prints from it. What didn't load or apply, and the
//! segments the vault knows of that aren't here yet, go to stderr, so stdout
//! stays clean for a pipe.

use std::collections::HashSet;
use std::io::Write;

use cashflow::chart::Chart;
use cashflow::{Books, CashflowDir, DeriveInputs, rules};
use cashflow::{journal, manifest};
use ledgerdb::{
    AccountId, Amount, Bucket, Commodities, CommodityId, Date, DateRange, Edits, Filter, Journal,
    LayerFilter, Ledger, SegmentHash, Unconverted, WriteOptions,
};

use crate::args::{BooksArgs, Range, Report, Run};
use crate::vault_cmd::Session;

/// Every segment in the journal but the retired ones, uncategorized: what
/// edits are tried on.
pub fn raw_ledger(dir: &CashflowDir, session: &Session) -> Result<Ledger, String> {
    let loaded = Ledger::load_without(&journal::store(dir), LayerFilter::All, retired(session)?)
        .map_err(|e| e.to_string())?;
    Ok(loaded.ledger)
}

/// The segments the vault's manifests retire, which every load leaves out.
pub fn retired(session: &Session) -> Result<HashSet<SegmentHash>, String> {
    let txn = session.txn()?;
    let manifests = manifest::load(&session.ndb, &txn, session.vault.author());
    Ok(manifest::retired(&manifests))
}

/// Warn on stderr when the vault knows of live segments this journal
/// lacks, so a total is never silently partial. Only stats files.
fn warn_missing(dir: &CashflowDir, session: &Session) -> Result<(), String> {
    let store = journal::store(dir);
    let txn = session.txn()?;
    let manifests = manifest::load(&session.ndb, &txn, session.vault.author());
    let missing = manifests
        .iter()
        .filter(|m| !m.body.retired)
        .filter_map(manifest::hash_of)
        .filter(|&hash| !store.contains(hash))
        .count();
    if missing > 0 {
        eprintln!(
            "cashflow: {missing} segment(s) in the vault aren't in this journal yet, so these totals leave them out (see `cashflow journal status`)"
        );
    }
    Ok(())
}

/// The vault folded once for this run: its edits (rules compiled, then
/// overrides, splits and transfer links, then account aliases) and the
/// segments it retires.
pub fn derive_inputs(session: &Session) -> Result<DeriveInputs, String> {
    let txn = session.txn()?;
    Ok(DeriveInputs::load(
        &session.ndb,
        &txn,
        session.vault.author(),
    ))
}

/// The vault's edits, owned: [`derive_inputs`]' edits.
pub fn vault_edits(session: &Session) -> Result<Edits, String> {
    Ok(derive_inputs(session)?.edits)
}

/// The edits the books derive with and the segments they leave out:
/// `--rules`' file alone, else the vault's rules and per-transaction
/// edits, else (no stored key) none. Logged in, the vault's retired
/// segments are left out even under `--rules`.
fn inputs(session: Option<&Session>, args: &BooksArgs) -> Result<DeriveInputs, String> {
    let Some(session) = session else {
        let edits = match &args.rules {
            Some(path) => rules::load(path).map_err(|e| e.to_string())?,
            None => {
                eprintln!(
                    "cashflow: no signing key, so no vault rules: categorizing as imported (run `cashflow login`, or pass --rules)"
                );
                rules::default_edits()
            }
        };
        return Ok(DeriveInputs {
            edits,
            owners: Vec::new(),
            retired: HashSet::new(),
        });
    };
    let mut inputs = derive_inputs(session)?;
    if let Some(path) = &args.rules {
        inputs.edits = rules::load(path).map_err(|e| e.to_string())?;
        inputs.owners.clear();
    }
    Ok(inputs)
}

/// Load the books `args` names, reporting on stderr what didn't load or
/// apply. Logged in, the vault's retired segments are left out even under
/// `--rules`.
async fn load(dir: &CashflowDir, run: &Run, args: &BooksArgs) -> Result<Books, String> {
    let session = Session::open_if_logged_in(dir, run).await?;
    books_with(dir, session.as_ref(), args)
}

/// [`load`] with the vault `session` already open (`None` when logged
/// out), for a command that reads the vault for more than the books.
pub fn books_with(
    dir: &CashflowDir,
    session: Option<&Session>,
    args: &BooksArgs,
) -> Result<Books, String> {
    let inputs = inputs(session, args)?;
    if let Some(session) = session {
        warn_missing(dir, session)?;
    }
    let books = Books::load(
        &journal::store(dir),
        args.filter.clone(),
        &inputs.retired,
        &inputs.edits,
        &args.home,
    )
    .map_err(|e| e.to_string())?;

    let report = &books.report;
    for err in &report.unreadable {
        eprintln!("cashflow: skipped {err}");
    }
    for err in &report.unmergeable {
        eprintln!("cashflow: skipped a segment: {err}");
    }
    for damaged in &report.damaged {
        eprintln!(
            "cashflow: segment {} has {} lines that don't parse; loaded without them",
            damaged.hash,
            damaged.errors.len()
        );
    }
    for rejected in &books.derived.report().rejected {
        eprintln!(
            "cashflow: {:?} not applied: {:?}",
            rejected.edit, rejected.problem
        );
    }
    Ok(books)
}

/// `cashflow export`: the derived ledger as canonical ledger-cli text.
pub async fn export(
    dir: &CashflowDir,
    run: &Run,
    args: &BooksArgs,
    range: Range,
) -> Result<(), String> {
    let books = load(dir, run, args).await?;
    let journal = books.ledger().journal();
    let mut out = String::new();
    let written = if range == Range::default() {
        journal.write_ledger(&mut out, WriteOptions::default())
    } else {
        write_range(&books, journal, range, &mut out)
    };
    written.map_err(|e| e.to_string())?;
    std::io::stdout()
        .lock()
        .write_all(out.as_bytes())
        .map_err(|e| e.to_string())
}

/// The prices before `range`'s end, then the transactions in it.
fn write_range(
    books: &Books,
    journal: &Journal,
    range: Range,
    out: &mut String,
) -> std::fmt::Result {
    journal.write_prices(range.end, out)?;
    let filter = Filter {
        range: Some(date_range(range)),
        ..Filter::default()
    };
    let mut txns = books.query().register(filter).map(|t| t.id).peekable();
    if !out.is_empty() && txns.peek().is_some() {
        out.push('\n');
    }
    journal.write_txns(txns, out, WriteOptions::default())
}

/// `range` with open ends filled in: from the epoch, to the last day.
fn date_range(range: Range) -> DateRange {
    DateRange::new(
        range.begin.unwrap_or(Date::EPOCH),
        range.end.unwrap_or(Date::from_days(u32::MAX)),
    )
}

/// `cashflow report ...`.
pub async fn report(
    dir: &CashflowDir,
    run: &Run,
    args: &BooksArgs,
    report: &Report,
) -> Result<(), String> {
    let session = Session::open_if_logged_in(dir, run).await?;
    let books = books_with(dir, session.as_ref(), args)?;
    let home = || {
        books.home().ok_or_else(|| {
            format!(
                "the ledger has no {} amounts or prices to convert into (try --in or --native)",
                args.home
            )
        })
    };
    match report {
        Report::Bal {
            account,
            depth,
            end,
            native,
        } => {
            let into = if *native { None } else { Some(home()?) };
            bal(&books, account.as_deref(), *depth, *end, into)
        }
        Report::Reg {
            account,
            range,
            payee,
        } => reg(&books, account.as_deref(), *range, payee.as_deref()),
        Report::Top {
            parent,
            depth,
            range,
            limit,
        } => top(&books, parent.as_deref(), *depth, *range, *limit, home()?),
        Report::Projects { parent, range } => {
            let into = home()?;
            match &session {
                Some(session) => {
                    let txn = session.txn()?;
                    let chart = Chart::load(&session.ndb, &txn, session.vault.author());
                    projects(&books, &chart, parent.as_deref(), *range, into)
                }
                None => {
                    eprintln!(
                        "cashflow: no signing key, so no projects: everything is unattributed (run `cashflow login`)"
                    );
                    projects(&books, &Chart::empty(), parent.as_deref(), *range, into)
                }
            }
        }
    }
}

/// `range`, or the ledger's whole span when neither end is given.
fn range_or_span(books: &Books, range: Range) -> DateRange {
    match (range, books.query().span()) {
        (
            Range {
                begin: None,
                end: None,
            },
            Some(span),
        ) => span,
        (range, _) => date_range(range),
    }
}

/// `report projects`: `parent`'s subtree over `range`, summed by project.
fn projects(
    books: &Books,
    chart: &Chart,
    parent: Option<&str>,
    range: Range,
    into: CommodityId,
) -> Result<(), String> {
    let journal = books.ledger().journal();
    let parent = account(journal, parent.unwrap_or("income"))?;
    let query = books.query();
    let range = range_or_span(books, range);
    let totals = chart.by_project(&journal.accounts, parent, |account| {
        query.flows(account, range, Bucket::Year, into).total()
    });
    // The parent's subtree covers every project's, so its misses are all.
    let missed = query.flows(parent, range, Bucket::Year, into).unconverted;
    let all: i128 = totals.iter().map(|t| t.amount).sum();
    for total in &totals {
        let share = if all == 0 {
            0.0
        } else {
            total.amount as f64 / all as f64 * 100.0
        };
        println!(
            "{:>24}  {share:>5.1}%  {}",
            format_total(total.amount, into, &journal.commodities),
            total.project.unwrap_or("(unattributed)")
        );
    }
    println!(
        "{:>24}  {:>6}  total",
        format_total(all, into, &journal.commodities),
        ""
    );
    warn_unconverted(std::slice::from_ref(&missed), Some(into), journal);
    Ok(())
}

/// The account named `name`, or an error saying the ledger has none.
fn account(journal: &Journal, name: &str) -> Result<AccountId, String> {
    journal
        .accounts
        .lookup(name)
        .ok_or_else(|| format!("no account named '{name}' in the ledger"))
}

/// `report bal`: each account from `from` down to `depth`, depth first and
/// by name.
fn bal(
    books: &Books,
    from: Option<&str>,
    depth: Option<u16>,
    end: Option<Date>,
    into: Option<CommodityId>,
) -> Result<(), String> {
    let journal = books.ledger().journal();
    let accounts = &journal.accounts;
    let mut stack: Vec<AccountId> = match from {
        Some(name) => vec![account(journal, name)?],
        None => sorted(journal, accounts.roots()),
    };
    let base = stack.first().map_or(1, |&a| accounts.depth(a));
    let max_depth = depth.unwrap_or(base + 1);
    let at = match end {
        Some(end) => Date::from_days(end.days().saturating_sub(1)),
        None => Date::from_days(u32::MAX),
    };
    let query = books.query();
    // The top accounts' misses: their subtrees cover every printed account.
    let mut missed: Vec<Unconverted> = Vec::new();
    let tops = stack.clone();
    // Reverse so popping visits in name order.
    stack.reverse();
    while let Some(id) = stack.pop() {
        let amounts = query.balance(id, at, into);
        for total in amounts.totals.iter().filter(|t| t.qty != 0) {
            println!(
                "{:>24}  {}",
                format_total(total.qty, total.commodity, &journal.commodities),
                accounts.full_name(id)
            );
        }
        if tops.contains(&id) {
            missed.push(amounts.unconverted);
        }
        if accounts.depth(id) < max_depth {
            let mut children = sorted(journal, accounts.children(id));
            children.reverse();
            stack.extend(children);
        }
    }
    warn_unconverted(&missed, into, journal);
    Ok(())
}

/// `ids` sorted by full name.
pub fn sorted(journal: &Journal, ids: &[AccountId]) -> Vec<AccountId> {
    let mut ids = ids.to_vec();
    ids.sort_by(|&a, &b| {
        journal
            .accounts
            .full_name(a)
            .cmp(journal.accounts.full_name(b))
    });
    ids
}

/// `report reg`: every posting under `under`, in date order.
fn reg(
    books: &Books,
    under: Option<&str>,
    range: Range,
    payee: Option<&str>,
) -> Result<(), String> {
    let journal = books.ledger().journal();
    let accounts = &journal.accounts;
    let under = under.map(|name| account(journal, name)).transpose()?;
    let filter = Filter {
        account: under,
        range: Some(date_range(range)),
        payee,
        ..Filter::default()
    };
    let mut out = std::io::stdout().lock();
    for found in books.query().register(filter) {
        let txn_payee = journal.strings.get(found.txn.payee);
        for p in journal.postings_of(found.txn) {
            if under.is_some_and(|a| p.account != a && !accounts.is_ancestor(a, p.account)) {
                continue;
            }
            let amount = p.amount().display(&journal.commodities);
            writeln!(
                out,
                "{}  {txn_payee}  {}  {amount}",
                p.date,
                accounts.full_name(p.account)
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// `report top`: the largest accounts at `depth` under `parent`.
fn top(
    books: &Books,
    parent: Option<&str>,
    depth: Option<u16>,
    range: Range,
    limit: usize,
    into: CommodityId,
) -> Result<(), String> {
    let journal = books.ledger().journal();
    let parent = account(journal, parent.unwrap_or("expenses"))?;
    let depth = depth.unwrap_or(journal.accounts.depth(parent) + 1);
    let query = books.query();
    let range = range_or_span(books, range);
    let top = query.top_n(parent, depth, range, limit, into);
    for ranked in &top.ranked {
        println!(
            "{:>24}  {:>5.1}%  {}",
            format_total(ranked.amount, into, &journal.commodities),
            ranked.share * 100.0,
            journal.accounts.full_name(ranked.account)
        );
    }
    println!(
        "{:>24}  {:>6}  total",
        format_total(top.total, into, &journal.commodities),
        ""
    );
    warn_unconverted(std::slice::from_ref(&top.unconverted), Some(into), journal);
    Ok(())
}

/// Say on stderr how many postings had no rate into `into`, and in which
/// commodities.
pub fn warn_unconverted(missed: &[Unconverted], into: Option<CommodityId>, journal: &Journal) {
    let Some(into) = into else {
        return;
    };
    let count: u64 = missed.iter().map(|u| u.count).sum();
    if count == 0 {
        return;
    }
    let mut from: Vec<&str> = missed
        .iter()
        .flat_map(|u| &u.native)
        .map(|t| journal.commodities.name(t.commodity))
        .collect();
    from.sort_unstable();
    from.dedup();
    eprintln!(
        "cashflow: {count} postings ({}) had no rate into {} and are left out",
        from.join(", "),
        journal.commodities.name(into)
    );
}

/// A sum of smallest units of `commodity`, in its style; a sum too large for
/// an [`Amount`] prints as a bare count of units.
pub fn format_total(qty: i128, commodity: CommodityId, commodities: &Commodities) -> String {
    match i64::try_from(qty) {
        Ok(qty) => Amount { qty, commodity }.display(commodities).to_string(),
        Err(_) => format!("{qty} units of {}", commodities.name(commodity)),
    }
}
