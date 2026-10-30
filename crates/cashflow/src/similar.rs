//! Transactions like one another: the same payee, or the same amount from
//! another payee, and how the ones with the same payee were categorized.
//!
//! The transaction detail's "Similar transactions" reads an [`Index`] of the
//! books, which groups every transaction once:
//!
//! - **by payee**, normalized with [`unknown::normalize`] (`SHOP #12` and
//!   `Shop 99` are both `shop`), the key worked out once per distinct payee
//!   string, never once per transaction;
//! - **by amount**: its one counter leg's commodity and magnitude. A
//!   transaction with no counter leg, or a split's several, has no amount
//!   to match on.
//!
//! [`Index::build`] walks the books once and runs off the UI thread with the
//! rest of a load. [`Index::like`] then answers for one transaction from its
//! groups: up to [`CAP`] of each kind, newest first, and a [`Tally`] of
//! where every same-payee transaction's counter leg went and what decided
//! it.

use std::collections::HashMap;

use ledgerdb::{AccountId, CommodityId, Journal, StrId, Txn, TxnId, Why};

use crate::transfers::is_own;
use crate::unknown::normalize;
use crate::{Books, DeriveInputs};

/// How many transactions of each kind [`Index::like`] lists.
pub const CAP: usize = 50;

/// The books' transactions grouped by payee and by amount.
#[derive(Clone, Debug, Default)]
pub struct Index {
    /// Each normalized payee's transactions, newest first.
    groups: Vec<Vec<TxnId>>,
    /// Each payee string's group in [`Self::groups`]: `None` for a payee
    /// with no words to normalize to ("12345"), which groups with nothing.
    by_payee: HashMap<StrId, Option<u32>>,
    /// Each counter-leg amount's transactions, newest first.
    by_amount: HashMap<AmountKey, Vec<TxnId>>,
    /// How many times the build normalized a payee: once per distinct
    /// payee string.
    normalized: usize,
}

/// What a transaction's amount is matched on: its one counter leg's
/// commodity and magnitude, whichever way the money went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AmountKey {
    /// The counter leg's commodity.
    pub commodity: CommodityId,
    /// Its quantity's magnitude, in the commodity's smallest unit.
    pub qty_abs: u64,
}

/// The transactions like one, as [`Index::like`] finds them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Similar {
    /// Others with its normalized payee, newest first, at most [`CAP`].
    pub same_payee: Vec<TxnId>,
    /// How many others have its payee, listed or not.
    pub payee_total: usize,
    /// Others of another payee with its amount, newest first, at most
    /// [`CAP`].
    pub same_amount: Vec<TxnId>,
    /// How many others of another payee have its amount, listed or not.
    pub amount_total: usize,
    /// Where every other same-payee transaction's counter legs went, most
    /// first.
    pub accounts: Vec<Tally>,
}

/// How many same-payee transactions have a counter leg on one account, and
/// what put them there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tally {
    /// The account, its full name.
    pub account: String,
    /// How many transactions post to it.
    pub count: usize,
    /// What decided them, by tier.
    pub tiers: TierCounts,
    /// The vault rule that decided the newest of them a rule decided, if
    /// one did and it came from the vault.
    pub rule: Option<String>,
}

/// How many transactions each tier of derive decided.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TierCounts {
    /// A rule.
    pub rules: usize,
    /// An override.
    pub overrides: usize,
    /// A split.
    pub splits: usize,
    /// A transfer link.
    pub links: usize,
    /// Nothing: the importer's default.
    pub unknown: usize,
}

impl TierCounts {
    /// Count one transaction decided by `why`.
    fn add(&mut self, why: Why) {
        match why {
            Why::Rule(_) => self.rules += 1,
            Why::Override(_) => self.overrides += 1,
            Why::Split(_) => self.splits += 1,
            Why::Transfer(_) => self.links += 1,
            Why::Default => self.unknown += 1,
            Why::Untouched => {}
        }
    }

    /// How many a person or a rule decided: every tier but the default.
    pub fn categorized(&self) -> usize {
        self.rules + self.overrides + self.splits + self.links
    }
}

impl Index {
    /// Group every transaction of `books`. Linear in the transactions, plus
    /// one [`normalize`] per distinct payee string and a sort of each
    /// group: run it off the UI thread.
    #[profiling::function]
    pub fn build(books: &Books) -> Self {
        let ledger = books.ledger();
        let journal = ledger.journal();
        let mut index = Index::default();
        // Each normalized key's group, while building.
        let mut by_key: HashMap<String, u32> = HashMap::new();
        for i in 0..ledger.txns().len() {
            let id = TxnId(i as u32);
            let txn = journal.txn(id);
            if let Some(group) = index.group_of_payee(journal, txn.payee, &mut by_key) {
                index.groups[group as usize].push(id);
            }
            if let Some(key) = amount_key(journal, txn) {
                index.by_amount.entry(key).or_default().push(id);
            }
        }
        let newest_first = |rows: &mut Vec<TxnId>| {
            rows.sort_unstable_by_key(|&t| std::cmp::Reverse((journal.txn(t).date, t)));
        };
        index.groups.iter_mut().for_each(newest_first);
        index.by_amount.values_mut().for_each(newest_first);
        index
    }

    /// The group of payee string `payee`, normalizing it the first time it
    /// is seen; `None` for one with no words.
    fn group_of_payee(
        &mut self,
        journal: &Journal,
        payee: StrId,
        by_key: &mut HashMap<String, u32>,
    ) -> Option<u32> {
        if let Some(&group) = self.by_payee.get(&payee) {
            return group;
        }
        self.normalized += 1;
        let key = normalize(journal.strings.get(payee));
        let group = (!key.is_empty()).then(|| {
            let next = self.groups.len() as u32;
            let group = *by_key.entry(key).or_insert(next);
            if group == next {
                self.groups.push(Vec::new());
            }
            group
        });
        self.by_payee.insert(payee, group);
        group
    }

    /// The group of payee string `payee`, if it has words.
    fn group(&self, payee: StrId) -> Option<u32> {
        self.by_payee.get(&payee).copied().flatten()
    }

    /// How many payee strings the build normalized: one per distinct
    /// payee, however many transactions share it.
    pub fn normalized(&self) -> usize {
        self.normalized
    }

    /// Every transaction with `txn`'s normalized payee, `txn` among them,
    /// newest first; empty for a payee with no words.
    pub fn same_payee(&self, books: &Books, txn: TxnId) -> &[TxnId] {
        let payee = books.ledger().journal().txn(txn).payee;
        self.group(payee)
            .map_or(&[], |g| self.groups[g as usize].as_slice())
    }

    /// The transactions like `txn` in `books`, which this index was built
    /// from and `inputs` derived; the tally names the vault rules by id.
    #[profiling::function]
    pub fn like(&self, books: &Books, inputs: &DeriveInputs, txn: TxnId) -> Similar {
        let journal = books.ledger().journal();
        let row = journal.txn(txn);
        let group = self.group(row.payee);
        let mut similar = Similar::default();

        let payee_rows = || {
            self.same_payee(books, txn)
                .iter()
                .copied()
                .filter(|&t| t != txn)
        };
        similar.same_payee = payee_rows().take(CAP).collect();
        similar.payee_total = payee_rows().count();
        similar.accounts = tally(books, inputs, payee_rows());

        let Some(rows) = amount_key(journal, row).and_then(|key| self.by_amount.get(&key)) else {
            return similar;
        };
        // Another payee: another group, or for a payee with no words,
        // another string.
        let other = |t: TxnId| {
            let payee = journal.txn(t).payee;
            match (group, self.group(payee)) {
                (Some(a), Some(b)) => a != b,
                _ => payee != row.payee,
            }
        };
        let amount_rows = || rows.iter().copied().filter(|&t| t != txn && other(t));
        similar.same_amount = amount_rows().take(CAP).collect();
        similar.amount_total = amount_rows().count();
        similar
    }
}

/// `txn`'s amount key: its one counter leg's, a posting off the owner's own
/// accounts; `None` with none or several.
fn amount_key(journal: &Journal, txn: &Txn) -> Option<AmountKey> {
    let mut counter = counter_legs(journal, txn);
    let leg = counter.next()?;
    if counter.next().is_some() {
        return None;
    }
    Some(AmountKey {
        commodity: leg.commodity,
        qty_abs: leg.qty.unsigned_abs(),
    })
}

/// `txn`'s counter legs: its postings off the owner's own accounts.
fn counter_legs<'j>(
    journal: &'j Journal,
    txn: &Txn,
) -> impl Iterator<Item = &'j ledgerdb::Posting> + 'j {
    journal
        .postings_of(txn)
        .iter()
        .filter(|p| !is_own(journal.accounts.full_name(p.account)))
}

/// Where `rows`' counter legs went in `books`, an account counted once per
/// transaction however many of its legs post there, most first, then by
/// name.
fn tally(books: &Books, inputs: &DeriveInputs, rows: impl Iterator<Item = TxnId>) -> Vec<Tally> {
    let journal = books.ledger().journal();
    let mut at: HashMap<AccountId, usize> = HashMap::new();
    let mut tallies: Vec<Tally> = Vec::new();
    for txn in rows {
        let why = books.derived.why(txn);
        let postings = journal.postings_of(journal.txn(txn));
        for (i, p) in postings.iter().enumerate() {
            let name = journal.accounts.full_name(p.account);
            if is_own(name) || postings[..i].iter().any(|q| q.account == p.account) {
                continue;
            }
            let slot = *at.entry(p.account).or_insert_with(|| {
                tallies.push(Tally {
                    account: name.to_owned(),
                    count: 0,
                    tiers: TierCounts::default(),
                    rule: None,
                });
                tallies.len() - 1
            });
            let tally = &mut tallies[slot];
            tally.count += 1;
            tally.tiers.add(why);
            if tally.rule.is_none() {
                tally.rule = inputs.rule_id(why).map(str::to_owned);
            }
        }
    }
    tallies.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.account.cmp(&b.account))
    });
    tallies
}

#[cfg(test)]
mod tests {
    use super::*;

    use ledgerdb::{Cond, Edits, Field, Leg, Override, Rule, Split, TransferLink};

    use crate::edits_cache::Stamp;
    use crate::test_util::books_with;
    use crate::vault_rules::Owner;

    /// Shops under two spellings, a grocer, a refund of a shop's amount in
    /// another commodity, and a card payment's two sides. Record ids are
    /// `t:1`… in order.
    const TEXT: &str = "\
2024-03-01 * SHOP #12
    liabilities:card  -10.00 CAD
    expenses:unknown

2024-03-02 * Shop 99
    liabilities:card  -7.00 CAD
    expenses:unknown

2024-03-03 * SHOP #12
    liabilities:card  -12.00 CAD
    expenses:unknown

2024-03-04 * Corner Grocer
    liabilities:card  -10.00 CAD
    expenses:unknown

2024-03-05 * US Store
    liabilities:card  -10.00 USD
    expenses:unknown

2024-03-06 * SHOP #4
    liabilities:card  -20.00 CAD
    expenses:unknown

2024-03-07 * Card payment
    assets:checking  -500.00 CAD
    expenses:unknown

2024-03-08 * Payment received
    liabilities:card  500.00 CAD
    income:unknown
";

    fn txn(books: &Books, record: &str) -> TxnId {
        books.ledger().find(record).unwrap()
    }

    fn records(books: &Books, rows: &[TxnId]) -> Vec<String> {
        let journal = books.ledger().journal();
        rows.iter()
            .map(|&t| journal.strings.get(journal.txn(t).id.unwrap()).to_owned())
            .collect()
    }

    /// `SHOP #12` and `Shop 99` are one payee; the grocer of the same amount
    /// is another, and the USD charge of the same number isn't the same
    /// amount.
    #[test]
    fn payees_group_normalized_and_amounts_need_the_commodity() {
        let books = books_with(TEXT, &crate::rules::default_edits());
        let inputs = DeriveInputs::from_edits(crate::rules::default_edits());
        let index = Index::build(&books);
        let similar = index.like(&books, &inputs, txn(&books, "t:1"));
        // Newest first, itself left out.
        assert_eq!(records(&books, &similar.same_payee), ["t:6", "t:3", "t:2"]);
        assert_eq!(similar.payee_total, 3);
        assert_eq!(records(&books, &similar.same_amount), ["t:4"]);
        assert_eq!(similar.amount_total, 1);
        // Both sides of the card payment: 500 out, 500 in.
        let payment = index.like(&books, &inputs, txn(&books, "t:7"));
        assert_eq!(records(&books, &payment.same_amount), ["t:8"]);
        assert!(payment.same_payee.is_empty());
    }

    /// The tally counts each same-payee account by the tier that decided
    /// it, names the vault rule, and counts a split once per account.
    #[test]
    fn the_tally_counts_each_tier() {
        let mut edits = crate::rules::default_edits();
        edits.rules.push(Rule {
            account: "expenses:shopping".to_owned(),
            conditions: vec![Cond::Contains {
                field: Field::Payee,
                needle: "Shop 99".to_owned(),
            }],
            layer: None,
        });
        edits.overrides.push(Override {
            record_id: "t:3".to_owned(),
            account: "expenses:shopping".to_owned(),
        });
        edits.splits.push(Split {
            record_id: "t:6".to_owned(),
            legs: vec![
                Leg {
                    account: "expenses:shopping".to_owned(),
                    qty: 1500,
                },
                Leg {
                    account: "expenses:gifts".to_owned(),
                    qty: 500,
                },
            ],
        });
        edits.links.push(TransferLink {
            a: "t:7".to_owned(),
            b: "t:8".to_owned(),
        });
        let books = books_with(TEXT, &edits);
        let mut inputs = DeriveInputs::from_edits(edits);
        inputs.owners = vec![Owner {
            rule: 0,
            pattern: Some(0),
        }];
        inputs.provenance.rules = vec![Stamp {
            id: "quiet-otter".to_owned(),
            created_at: 1,
            agentium: None,
        }];
        let index = Index::build(&books);

        let similar = index.like(&books, &inputs, txn(&books, "t:1"));
        let shopping = TierCounts {
            rules: 1,
            overrides: 1,
            splits: 1,
            ..TierCounts::default()
        };
        assert_eq!(
            similar.accounts,
            [
                Tally {
                    account: "expenses:shopping".to_owned(),
                    count: 3,
                    tiers: shopping,
                    rule: Some("quiet-otter".to_owned()),
                },
                Tally {
                    account: "expenses:gifts".to_owned(),
                    count: 1,
                    tiers: TierCounts {
                        splits: 1,
                        ..TierCounts::default()
                    },
                    rule: None,
                },
            ]
        );
        assert_eq!(similar.accounts[0].tiers.categorized(), 3);
        // From the shop with the rule, the first one is unknown.
        let from_rule = index.like(&books, &inputs, txn(&books, "t:2"));
        let unknown = from_rule
            .accounts
            .iter()
            .find(|t| t.account == "expenses:unknown")
            .unwrap();
        assert_eq!(unknown.tiers.unknown, 1);
        // A split has no one counter leg to match amounts on.
        let split = index.like(&books, &inputs, txn(&books, "t:6"));
        assert!(split.same_amount.is_empty());
        // A linked transfer's counter leg is the transfers account.
        let linked = index.like(&books, &inputs, txn(&books, "t:7"));
        assert_eq!(records(&books, &linked.same_amount), ["t:8"]);
    }

    /// Each list stops at [`CAP`], newest first, the totals counting every
    /// one; a payee string is normalized once however many share it.
    #[test]
    fn lists_are_capped_and_payees_normalized_once() {
        let mut text = String::new();
        for i in 0..120u32 {
            let day = 1 + i % 28;
            let month = 1 + i / 28;
            let payee = ["Fuel Stop", "Fuel Stop #2", "Bakery"][i as usize % 3];
            text.push_str(&format!(
                "2024-{month:02}-{day:02} * {payee}\n    liabilities:card  -5.00 CAD\n    expenses:unknown\n\n"
            ));
        }
        let edits = crate::rules::default_edits();
        let books = books_with(&text, &edits);
        let inputs = DeriveInputs::from_edits(edits);
        let index = Index::build(&books);
        // Three payee strings, three normalizes, for 120 transactions.
        assert_eq!(index.normalized(), 3);

        // The newest fuel stop.
        let similar = index.like(&books, &inputs, txn(&books, "t:119"));
        // Both fuel stop spellings are one payee: 80, less itself.
        assert_eq!(similar.payee_total, 79);
        assert_eq!(similar.same_payee.len(), CAP);
        let journal = books.ledger().journal();
        let dates: Vec<_> = similar
            .same_payee
            .iter()
            .map(|&t| journal.txn(t).date)
            .collect();
        assert!(dates.windows(2).all(|w| w[0] >= w[1]), "newest first");
        // The bakery's 40 share the amount from another payee.
        assert_eq!(similar.amount_total, 40);
        assert_eq!(similar.same_amount.len(), 40);
        assert_eq!(similar.accounts[0].count, 79);
    }

    /// A payee with no words groups with nothing.
    #[test]
    fn a_wordless_payee_has_no_same_payee() {
        let text = "\
2024-03-01 * 12345
    liabilities:card  -1.00 CAD
    expenses:unknown

2024-03-02 * 12345
    liabilities:card  -2.00 CAD
    expenses:unknown
";
        let edits: Edits = crate::rules::default_edits();
        let books = books_with(text, &edits);
        let inputs = DeriveInputs::from_edits(edits);
        let index = Index::build(&books);
        let similar = index.like(&books, &inputs, txn(&books, "t:1"));
        assert!(similar.same_payee.is_empty());
        assert!(index.same_payee(&books, txn(&books, "t:1")).is_empty());
    }
}
