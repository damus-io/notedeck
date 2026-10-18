//! The triage queue: transactions nothing has categorized yet, grouped so
//! one rule fixes the most of them.
//!
//! A transaction is **unknown** when derive left its counter leg on the
//! importer's default ([`Why::Default`]): no override, split, link or rule
//! decided it. [`groups`] gathers those by normalized payee and by the
//! default they sit on (`expenses:unknown` and `income:unknown` rows of the
//! same payee want different rules), and ranks the groups by their total in
//! the home commodity, biggest first, since that is where one rule moves
//! the most money. Each group carries a payee substring every row in it
//! contains, the `--match` of the rule that would take them all.
//!
//! The CLI's `cashflow unknown` prints these; the dashboard's triage inbox
//! is meant to read the same list.

use std::collections::HashMap;

use ledgerdb::{Edits, Journal, TxnId, Why};

use crate::Books;

/// How short a payee substring may be and still be suggested as a rule's
/// `--match`: anything shorter matches far more than the group.
pub const MIN_PATTERN: usize = 3;

/// Unknown transactions that share a normalized payee and a default
/// account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayeeGroup {
    /// The payee, lower-cased, digits and punctuation dropped: what the
    /// rows were grouped by. Empty for rows with no wordy payee.
    pub key: String,
    /// The default account the rows' counter legs are on.
    pub account: String,
    /// A substring (lower case) of every row's payee, at least
    /// [`MIN_PATTERN`] long, if there is one: the suggested rule's `--match`.
    pub pattern: Option<String>,
    /// The segment sources the rows came from, sorted.
    pub sources: Vec<String>,
    /// The rows, in ledger order.
    pub rows: Vec<TxnId>,
    /// The counter legs' magnitudes summed in the home commodity's
    /// smallest units, over the rows that converted.
    pub total: i128,
    /// How many rows had no rate into the home commodity (or there is
    /// none), and so aren't in [`Self::total`].
    pub unconverted: u64,
}

/// Which unknown transactions [`groups`] looks at.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scope<'s> {
    /// Only rows from segments with this `; source:`.
    pub source: Option<&'s str>,
}

/// The unknown transactions in `books`, derived with `edits`, grouped and
/// ranked: by [`PayeeGroup::total`], then row count, both descending, then
/// key and account.
#[profiling::function]
pub fn groups(books: &Books, edits: &Edits, scope: Scope) -> Vec<PayeeGroup> {
    let ledger = books.ledger();
    let journal = ledger.journal();
    let home = books.home();
    let mut by_key: HashMap<GroupKey, PayeeGroup> = HashMap::new();
    for i in 0..ledger.txns().len() {
        let id = TxnId(i as u32);
        if books.derived.why(id) != Why::Default {
            continue;
        }
        let source = &ledger.segment_of(id).header.source;
        if scope.source.is_some_and(|s| s != source) {
            continue;
        }
        let txn = journal.txn(id);
        let Some(counter) = journal
            .postings_of(txn)
            .iter()
            .find(|p| is_default(journal, edits, p.account))
        else {
            continue;
        };
        let payee = journal.strings.get(txn.payee);
        let group = by_key
            .entry(GroupKey {
                payee: normalize(payee),
                account: counter.account,
            })
            .or_insert_with_key(|key| PayeeGroup {
                key: key.payee.clone(),
                account: journal.accounts.full_name(key.account).to_owned(),
                pattern: None,
                sources: Vec::new(),
                rows: Vec::new(),
                total: 0,
                unconverted: 0,
            });
        group.rows.push(id);
        if !group.sources.iter().any(|s| s == source) {
            group.sources.push(source.clone());
        }
        let converted =
            home.and_then(|home| books.prices.convert(counter.amount(), home, txn.date));
        match converted {
            Some(amount) => group.total += i128::from(amount.qty).abs(),
            None => group.unconverted += 1,
        }
    }

    let mut groups: Vec<PayeeGroup> = by_key.into_values().collect();
    for group in &mut groups {
        group.sources.sort();
        group.pattern = common_pattern(group.rows.iter().map(|&id| {
            let txn = journal.txn(id);
            journal.strings.get(txn.payee)
        }));
    }
    groups.sort_by(|a, b| {
        b.total
            .cmp(&a.total)
            .then(b.rows.len().cmp(&a.rows.len()))
            .then_with(|| (&a.key, &a.account).cmp(&(&b.key, &b.account)))
    });
    groups
}

/// How many transactions in `books` are unknown: the size of the triage
/// queue [`groups`] ranks, without grouping it.
#[profiling::function]
pub fn count(books: &Books) -> usize {
    (0..books.ledger().txns().len())
        .filter(|&i| books.derived.why(TxnId(i as u32)) == Why::Default)
        .count()
}

/// What a group's rows share.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct GroupKey {
    payee: String,
    account: ledgerdb::AccountId,
}

/// Whether `account` is one of `edits`' default counter accounts.
fn is_default(journal: &Journal, edits: &Edits, account: ledgerdb::AccountId) -> bool {
    let name = journal.accounts.full_name(account);
    edits.defaults.iter().any(|d| d == name)
}

/// `payee` lower-cased, split into words at anything not alphanumeric,
/// with the words holding a digit (store numbers, dates, references)
/// dropped: `SQ *COFFEE SHOP #123` and `Sq *Coffee Shop 9` are both
/// `sq coffee shop`.
pub fn normalize(payee: &str) -> String {
    let mut key = String::with_capacity(payee.len());
    for word in payee
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !w.chars().any(|c| c.is_ascii_digit()))
    {
        if !key.is_empty() {
            key.push(' ');
        }
        key.extend(word.chars().flat_map(char::to_lowercase));
    }
    key
}

/// A payee substring every one of `payees` contains, ignoring case: their
/// longest common prefix cut back to a whole word, else the longest word
/// of the first that all contain, if either is at least [`MIN_PATTERN`]
/// long.
pub fn common_pattern<'p>(payees: impl Iterator<Item = &'p str> + Clone) -> Option<String> {
    let mut lower = payees.clone().map(str::to_lowercase);
    let first = lower.next()?;
    let mut prefix = first.as_str();
    for payee in lower {
        let shared = prefix
            .char_indices()
            .zip(payee.chars())
            .find(|((_, a), b)| a != b)
            .map_or(prefix.len().min(payee.len()), |((at, _), _)| at);
        prefix = &prefix[..shared];
    }
    // Cut a part-word off the end, unless the prefix is all of the first.
    if prefix.len() < first.len()
        && !first[prefix.len()..].starts_with(|c: char| !c.is_alphanumeric())
    {
        prefix = prefix.trim_end_matches(char::is_alphanumeric);
    }
    let prefix = prefix.trim_end_matches(|c: char| !c.is_alphanumeric());
    if prefix.chars().count() >= MIN_PATTERN {
        return Some(prefix.to_owned());
    }
    let mut words: Vec<&str> = first
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= MIN_PATTERN && !w.chars().any(|c| c.is_ascii_digit()))
        .collect();
    words.sort_by_key(|w| std::cmp::Reverse(w.len()));
    words
        .into_iter()
        .find(|w| payees.clone().all(|p| p.to_lowercase().contains(w)))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    use ledgerdb::{Journal, JournalStore, LayerFilter, SegmentHeader};

    #[test]
    fn normalize_drops_numbers_and_punctuation() {
        assert_eq!(normalize("SQ *COFFEE SHOP #123"), "sq coffee shop");
        assert_eq!(normalize("Sq *Coffee Shop 9"), "sq coffee shop");
        assert_eq!(normalize("12345"), "");
        assert_eq!(normalize("Café  Olé"), "café olé");
    }

    #[test]
    fn the_pattern_is_in_every_payee() {
        let pattern = |payees: &[&str]| common_pattern(payees.iter().copied());
        assert_eq!(
            pattern(&["SQ *COFFEE SHOP #123", "SQ *COFFEE SHOP #77"]).as_deref(),
            Some("sq *coffee shop")
        );
        // A shared prefix that ends mid-word is cut back to the word.
        assert_eq!(
            pattern(&["Corner Market 1", "Corner Mart"]).as_deref(),
            Some("corner")
        );
        assert_eq!(pattern(&["Bakery"]).as_deref(), Some("bakery"));
        // No useful prefix: the longest word they all contain.
        assert_eq!(
            pattern(&["1 UBER TRIP", "22 UBER EATS"]).as_deref(),
            Some("uber")
        );
        assert_eq!(pattern(&["ab 1", "cd 2"]), None);
        assert_eq!(pattern(&[]), None);
    }

    /// Unknown rows group by payee and default, rank by their total in the
    /// home commodity, and leave out rows something categorized.
    #[test]
    fn groups_rank_by_total_and_skip_categorized_rows() {
        let text = "\
2024-01-02 * Corner Grocer #1
    ; id: t:1
    liabilities:card  -45.50 CAD
    expenses:unknown

2024-01-03 * CORNER GROCER #2
    ; id: t:2
    liabilities:card  -4.50 CAD
    expenses:unknown

2024-01-04 * Airline
    ; id: t:3
    liabilities:card  -300.00 CAD
    expenses:unknown

2024-01-05 * Corner Grocer refund
    ; id: t:4
    liabilities:card  10.00 CAD
    income:unknown

2024-01-06 * Bakery
    ; id: t:5
    liabilities:card  -3.00 CAD
    expenses:unknown

2024-01-07 * Lightning
    ; id: t:6
    assets:ln  1000 msat
    income:unknown
";
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path());
        let header = SegmentHeader {
            source: "card".to_owned(),
            imported: 1,
            layer: "t".to_owned(),
        };
        store
            .write_segment(&header, &Journal::parse(text).journal)
            .unwrap();
        let mut edits = crate::rules::default_edits();
        edits.overrides.push(ledgerdb::Override {
            record_id: "t:5".to_owned(),
            account: "expenses:food".to_owned(),
        });
        let books =
            Books::load(&store, LayerFilter::All, &Default::default(), &edits, "CAD").unwrap();

        let found = groups(&books, &edits, Scope::default());
        let summary: Vec<(&str, &str, usize, i128, u64)> = found
            .iter()
            .map(|g| {
                (
                    g.key.as_str(),
                    g.account.as_str(),
                    g.rows.len(),
                    g.total,
                    g.unconverted,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("airline", "expenses:unknown", 1, 30000, 0),
                ("corner grocer", "expenses:unknown", 2, 5000, 0),
                ("corner grocer refund", "income:unknown", 1, 1000, 0),
                ("lightning", "income:unknown", 1, 0, 1),
            ]
        );
        // The count is every grouped row: five, the overridden one left out.
        assert_eq!(
            count(&books),
            found.iter().map(|g| g.rows.len()).sum::<usize>()
        );
        assert_eq!(count(&books), 5);
        assert_eq!(found[1].pattern.as_deref(), Some("corner grocer"));
        assert_eq!(found[1].sources, ["card"]);

        let none = groups(
            &books,
            &edits,
            Scope {
                source: Some("bank"),
            },
        );
        assert!(none.is_empty());
    }
}
