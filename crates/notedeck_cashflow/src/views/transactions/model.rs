//! What the Transactions view shows, worked out from the books and their
//! triage: the register's matching rows and the text of the ones on screen,
//! the inbox's payee groups and the transfer suggestions.
//!
//! Everything here is pure and runs only when what it depends on changes:
//! the register's match list when the books or the filter do, a row's text
//! when it scrolls into view, the inbox's text when a new triage arrives.
//! The view draws from what these return.

use cashflow::Books;
use cashflow::chart::Labels;
use cashflow::transfers::OWN_ROOTS;
use cashflow::triage::{Decided, Triage};
use ledgerdb::{AccountId, DateRange, Filter, Journal, TxnId, Why};

use crate::money::money;

/// What the register lists: the shared period's days and these.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegisterFilter {
    /// Payee text, matched ignoring ASCII case; empty for any.
    pub payee: String,
    /// Only transactions with a posting in this account's subtree.
    pub account: Option<String>,
    /// Only transactions from segments with this `; source:`.
    pub source: Option<String>,
    /// Only what nothing has categorized yet.
    pub unknown_only: bool,
}

/// The transactions in `range` that `filter` matches, newest first.
#[profiling::function]
pub fn matching(books: &Books, range: DateRange, filter: &RegisterFilter) -> Vec<TxnId> {
    let ledger = books.ledger();
    let account = match &filter.account {
        Some(name) => match ledger.journal().accounts.lookup(name) {
            Some(account) => Some(account),
            None => return Vec::new(),
        },
        None => None,
    };
    let payee = (!filter.payee.is_empty()).then_some(filter.payee.as_str());
    let query = books.query();
    let mut found: Vec<TxnId> = query
        .register(Filter {
            account,
            range: Some(range),
            payee,
            ..Filter::default()
        })
        .map(|found| found.id)
        .filter(|&id| {
            filter
                .source
                .as_deref()
                .is_none_or(|s| ledger.segment_of(id).header.source == s)
        })
        .filter(|&id| !filter.unknown_only || books.derived.why(id) == Why::Default)
        .collect();
    found.reverse();
    found
}

/// One register row's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowText {
    /// `2024-10-16`.
    pub date: String,
    /// The payee.
    pub payee: String,
    /// The counter account as shown: icon and display name, else its full
    /// name; "Split (3)" for several.
    pub account: String,
    /// The counter leg's amount, in its own commodity.
    pub amount: String,
    /// The same in the home commodity, empty when it has no rate.
    pub home: String,
    /// The hover: the counter account's full name, and what decided it.
    pub why: String,
    /// Whether nothing has categorized it yet.
    pub unknown: bool,
    /// Whether money came in (the counter leg is negative).
    pub incoming: bool,
}

/// Whether `account` is one of the owner's own ([`OWN_ROOTS`]).
fn is_own(journal: &Journal, account: AccountId) -> bool {
    let root = journal.accounts.full_name(account).split(':').next();
    root.is_some_and(|root| OWN_ROOTS.contains(&root))
}

/// The text of register row `txn`.
pub fn row_text(books: &Books, triage: &Triage, labels: &Labels, txn: TxnId) -> RowText {
    let journal = books.ledger().journal();
    let found = journal.txn(txn);
    let legs = journal.postings_of(found);
    let decided = triage.explain(books, txn);
    let mut row = RowText {
        date: found.date.to_string(),
        payee: journal.strings.get(found.payee).to_owned(),
        account: String::new(),
        amount: String::new(),
        home: String::new(),
        why: decided_text(decided),
        unknown: decided == Decided::Unknown,
        incoming: false,
    };
    // The counter legs: the ones off the owner's own accounts, else (a
    // move between two of them) the last leg.
    let counters = || legs.iter().filter(|p| !is_own(journal, p.account));
    let Some(first) = counters().next().or(legs.last()) else {
        return row;
    };
    let full = journal.accounts.full_name(first.account);
    let count = counters().count();
    let qty: i128 = if count > 1 {
        row.account = format!("Split ({count})");
        counters()
            .filter(|p| p.commodity == first.commodity)
            .map(|p| i128::from(p.qty))
            .sum()
    } else {
        row.account = shown(labels, full);
        i128::from(first.qty)
    };
    row.why = format!("{full}\n{}", row.why);
    row.incoming = qty < 0;
    row.amount = money(qty, first.commodity, &journal.commodities);
    row.home = match books.home() {
        Some(home) if home == first.commodity => row.amount.clone(),
        Some(home) => i64::try_from(qty)
            .ok()
            .and_then(|qty| {
                let amount = ledgerdb::Amount {
                    qty,
                    commodity: first.commodity,
                };
                books.prices.convert(amount, home, found.date)
            })
            .map_or(String::new(), |converted| {
                money(i128::from(converted.qty), home, &journal.commodities)
            }),
        None => String::new(),
    };
    row
}

/// An account as a list shows it: its icon and display name from the
/// chart, else its full name.
pub fn shown(labels: &Labels, account: &str) -> String {
    let label = labels.label(account);
    let name = label.name.unwrap_or(account);
    match label.icon {
        Some(icon) => format!("{icon} {name}"),
        None => name.to_owned(),
    }
}

/// What decided a transaction, as its hover says it.
pub fn decided_text(decided: Decided<'_>) -> String {
    match decided {
        Decided::AsImported => "As imported: it has no counter leg".to_owned(),
        Decided::Unknown => "Uncategorized: still on the importer's default".to_owned(),
        Decided::Rule { id: Some(id) } => format!("Categorized by rule {id}"),
        Decided::Rule { id: None } => "Categorized by a rules-file rule".to_owned(),
        Decided::Transfer { other: Some(other) } => format!("A transfer, linked with {other}"),
        Decided::Transfer { other: None } => "A transfer".to_owned(),
        Decided::Override => "Categorized by an override on this transaction".to_owned(),
        Decided::Split => "Split by hand".to_owned(),
    }
}

/// How many rows of a group the inbox lists before "… N more".
pub const GROUP_PREVIEW: usize = 6;

/// One payee group of the inbox, as drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupView {
    /// The group's index in [`Triage::groups`].
    pub index: usize,
    /// The payee, or "(no payee)".
    pub title: String,
    /// "3 transactions · amex · 1,234.00 CAD on expenses:unknown".
    pub detail: String,
    /// The rule "categorize all" would write, when the rows share a
    /// pattern.
    pub rule: Option<RuleDraft>,
    /// The first [`GROUP_PREVIEW`] rows.
    pub rows: Vec<GroupRow>,
    /// How many rows that leaves out.
    pub more: usize,
    /// "… and 12 more", when [`Self::more`] isn't 0.
    pub more_text: Option<String>,
}

/// The match of a rule from a group: its payee pattern, and the source to
/// limit it to when every row came from one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleDraft {
    /// The payee substring.
    pub pattern: String,
    /// The one source the rows came from.
    pub source: Option<String>,
    /// "payee contains “sq *coffee shop” in amex".
    pub text: String,
}

/// One transaction of a [`GroupView`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupRow {
    /// The transaction.
    pub txn: TxnId,
    /// Its record id, which an override names; `None` for one without.
    pub record: Option<String>,
    /// `2024-03-04  SQ *COFFEE SHOP #12  -4.50 CAD`.
    pub text: String,
}

/// The inbox's groups, as drawn.
#[profiling::function]
pub fn group_views(books: &Books, triage: &Triage) -> Vec<GroupView> {
    let journal = books.ledger().journal();
    let home = books.home();
    triage
        .groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let n = group.rows.len();
            let s = if n == 1 { "" } else { "s" };
            let total = home.map_or(String::new(), |home| {
                format!(" · {}", money(group.total, home, &journal.commodities))
            });
            let rule = group.pattern.as_ref().map(|pattern| {
                let source = match group.sources.as_slice() {
                    [one] => Some(one.clone()),
                    _ => None,
                };
                let text = match &source {
                    Some(source) => format!("payee contains “{pattern}” in {source}"),
                    None => format!("payee contains “{pattern}”"),
                };
                RuleDraft {
                    pattern: pattern.clone(),
                    source,
                    text,
                }
            });
            GroupView {
                index,
                title: if group.key.is_empty() {
                    "(no payee)".to_owned()
                } else {
                    group.key.clone()
                },
                detail: format!(
                    "{n} transaction{s} · {}{total} on {}",
                    group.sources.join(", "),
                    group.account
                ),
                rule,
                rows: group
                    .rows
                    .iter()
                    .take(GROUP_PREVIEW)
                    .map(|&txn| group_row(journal, txn))
                    .collect(),
                more: n.saturating_sub(GROUP_PREVIEW),
                more_text: (n > GROUP_PREVIEW).then(|| format!("… and {} more", n - GROUP_PREVIEW)),
            }
        })
        .collect()
}

/// One [`GroupRow`].
fn group_row(journal: &Journal, txn: TxnId) -> GroupRow {
    let found = journal.txn(txn);
    let amount = journal
        .postings_of(found)
        .iter()
        .find(|p| is_own(journal, p.account))
        .map(|p| money(i128::from(p.qty), p.commodity, &journal.commodities))
        .unwrap_or_default();
    GroupRow {
        txn,
        record: found.id.map(|id| journal.strings.get(id).to_owned()),
        text: format!(
            "{}  {}  {amount}",
            found.date,
            journal.strings.get(found.payee)
        ),
    }
}

/// One transfer suggestion, as drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuggestionView {
    /// The suggestion's index in [`Triage::suggestions`].
    pub index: usize,
    /// The side money left: its record id.
    pub a: String,
    /// The side it arrived on.
    pub b: String,
    /// `2024-03-01  Card payment  assets:checking  -500.00 CAD`.
    pub out_text: String,
    /// The same for the side it arrived on.
    pub in_text: String,
    /// "2 days apart".
    pub days_text: String,
}

/// The transfer suggestions, as drawn.
pub fn suggestion_views(books: &Books, triage: &Triage) -> Vec<SuggestionView> {
    let ledger = books.ledger();
    let journal = ledger.journal();
    let side = |record: &str| match ledger.find(record) {
        Some(txn) => {
            let found = journal.txn(txn);
            let own = journal
                .postings_of(found)
                .iter()
                .find(|p| is_own(journal, p.account));
            let leg = own.map_or(String::new(), |p| {
                format!(
                    "{}  {}",
                    journal.accounts.full_name(p.account),
                    money(i128::from(p.qty), p.commodity, &journal.commodities)
                )
            });
            format!(
                "{}  {}  {leg}",
                found.date,
                journal.strings.get(found.payee)
            )
        }
        None => record.to_owned(),
    };
    triage
        .suggestions
        .iter()
        .enumerate()
        .map(|(index, s)| SuggestionView {
            index,
            a: s.link.a.clone(),
            b: s.link.b.clone(),
            out_text: side(&s.link.a),
            in_text: side(&s.link.b),
            days_text: match s.days {
                0 => "same day".to_owned(),
                1 => "1 day apart".to_owned(),
                n => format!("{n} days apart"),
            },
        })
        .collect()
}

/// Every account in the books, by full name, sorted: what the account
/// picker searches.
pub fn account_names(books: &Books) -> Vec<String> {
    let accounts = &books.ledger().journal().accounts;
    let mut names: Vec<String> = accounts
        .ids()
        .map(|id| accounts.full_name(id).to_owned())
        .collect();
    names.sort_unstable();
    names
}

/// Every segment source in the books, sorted: the register's source
/// filter.
pub fn sources(books: &Books) -> Vec<String> {
    let mut sources: Vec<String> = books
        .ledger()
        .segments()
        .iter()
        .map(|s| s.header.source.clone())
        .collect();
    sources.sort_unstable();
    sources.dedup();
    sources
}
