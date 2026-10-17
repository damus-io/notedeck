//! The commands that read the categorized books: `export` and `report`.
//!
//! Each loads [`Books`] once (the journal, derived with `--rules`, indexed in
//! `--in`'s commodity) and prints from it. What didn't load or apply goes to
//! stderr, so stdout stays clean for a pipe.

use std::io::Write;

use cashflow::journal;
use cashflow::{Books, CashflowDir, rules};
use ledgerdb::{
    AccountId, Amount, Commodities, CommodityId, Date, DateRange, Filter, Journal, Unconverted,
    WriteOptions,
};

use crate::args::{BooksArgs, Range, Report};

/// Load the books `args` names, reporting on stderr what didn't load or
/// apply.
fn load(dir: &CashflowDir, args: &BooksArgs) -> Result<Books, String> {
    let edits = match &args.rules {
        Some(path) => rules::load(path).map_err(|e| e.to_string())?,
        None => rules::default_edits(),
    };
    let books = Books::load(
        &journal::store(dir),
        args.filter.clone(),
        &edits,
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
pub fn export(dir: &CashflowDir, args: &BooksArgs, range: Range) -> Result<(), String> {
    let books = load(dir, args)?;
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
pub fn report(dir: &CashflowDir, args: &BooksArgs, report: &Report) -> Result<(), String> {
    let books = load(dir, args)?;
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
    }
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
fn sorted(journal: &Journal, ids: &[AccountId]) -> Vec<AccountId> {
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
    let range = match (range, query.span()) {
        (
            Range {
                begin: None,
                end: None,
            },
            Some(span),
        ) => span,
        (range, _) => date_range(range),
    };
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
fn warn_unconverted(missed: &[Unconverted], into: Option<CommodityId>, journal: &Journal) {
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
fn format_total(qty: i128, commodity: CommodityId, commodities: &Commodities) -> String {
    match i64::try_from(qty) {
        Ok(qty) => Amount { qty, commodity }.display(commodities).to_string(),
        Err(_) => format!("{qty} units of {}", commodities.name(commodity)),
    }
}
