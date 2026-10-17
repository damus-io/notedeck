//! Finding moves between the owner's own accounts that two sources each
//! recorded, so the dashboard can offer to link them.
//!
//! Paying the Amex card from chequing shows up twice: as money out of
//! chequing in the RBC download and as a payment in the Amex export. Each
//! importer posts its side's counter leg to a structural default
//! (`expenses:unknown`, `income:unknown`), so until the two are linked the
//! move counts once as an expense and once as income. A
//! [`TransferLink`] nets them out (see [`ledgerdb::derive`]); [`suggest`]
//! proposes the links, and nothing here applies one.

use std::collections::{BTreeMap, HashSet};

use ledgerdb::{AccountId, CommodityId, Date, Edits, Ledger, TransferLink};

/// How many days apart the two sides of a transfer may be dated. A payment
/// leaves chequing a day or two before the card posts it.
pub const WINDOW_DAYS: u32 = 5;

/// The top-level accounts that are the owner's own: money moving between
/// two accounts under these is a transfer, not income or spending.
pub const OWN_ROOTS: &[&str] = &["assets", "liabilities"];

/// A pair of transactions that look like one transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The link to add to confirm it: `a` is the side money left, `b` the
    /// side it arrived on.
    pub link: TransferLink,
    /// How many days apart the two sides are dated.
    pub days: u32,
}

/// Transfers in the `raw` ledger, as importers wrote it, that `edits` don't
/// already account for, in date order.
///
/// Two transactions pair when each is an importer's raw record (one leg to
/// an account under [`OWN_ROOTS`], one counter leg to one of
/// [`Edits::defaults`]), their own legs are on different accounts and in
/// the same commodity with equal and opposite quantities, and they are
/// dated at most [`WINDOW_DAYS`] apart. A transaction an override, split or
/// link already names is left out, as is one without a record id, which no
/// link could name. Each transaction is in at most one suggestion: the
/// outgoing sides are taken in date order, and each pairs with the nearest
/// unpaired incoming side.
///
/// The derived ledger can't be used here: derive has already moved the
/// counter legs a rule matched. A link outranks a rule, so a move a rule
/// already categorized is still suggested.
#[profiling::function]
pub fn suggest(raw: &Ledger, edits: &Edits) -> Vec<Suggestion> {
    let journal = raw.journal();
    let accounts = &journal.accounts;
    let is_default: HashSet<AccountId> = edits
        .defaults
        .iter()
        .filter_map(|name| accounts.lookup(name))
        .collect();
    let decided: HashSet<&str> = edits
        .overrides
        .iter()
        .map(|o| o.record_id.as_str())
        .chain(edits.splits.iter().map(|s| s.record_id.as_str()))
        .chain(
            edits
                .links
                .iter()
                .flat_map(|l| [l.a.as_str(), l.b.as_str()]),
        )
        .collect();
    let is_own = |account: AccountId| {
        let root = accounts.full_name(account).split(':').next();
        root.is_some_and(|root| OWN_ROOTS.contains(&root))
    };

    // Every candidate side, grouped by what its pair must match.
    let mut groups: BTreeMap<Group, Sides> = BTreeMap::new();
    for txn in raw.txns() {
        let Some(id) = txn.id.map(|id| journal.strings.get(id)) else {
            continue;
        };
        if decided.contains(id) {
            continue;
        }
        let [first, second] = journal.postings_of(txn) else {
            continue;
        };
        let (own, counter) = match (
            is_default.contains(&first.account),
            is_default.contains(&second.account),
        ) {
            (false, true) => (first, second),
            (true, false) => (second, first),
            _ => continue,
        };
        if !is_own(own.account) || own.qty == 0 || counter.commodity != own.commodity {
            continue;
        }
        let side = Side {
            date: txn.date,
            id,
            account: own.account,
        };
        let sides = groups
            .entry(Group {
                commodity: own.commodity,
                magnitude: own.qty.unsigned_abs(),
            })
            .or_default();
        if own.qty < 0 {
            sides.out.push(side);
        } else {
            sides.into.push(side);
        }
    }

    let mut found = Vec::new();
    for sides in groups.into_values() {
        sides.pair(&mut found);
    }
    found.sort_by(|x, y| (x.date, &x.suggestion.link.a).cmp(&(y.date, &y.suggestion.link.a)));
    found.into_iter().map(|f| f.suggestion).collect()
}

/// What both sides of a transfer share.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Group {
    commodity: CommodityId,
    magnitude: u64,
}

/// The candidate sides of one [`Group`].
#[derive(Default)]
struct Sides<'a> {
    /// Money leaving an own account.
    out: Vec<Side<'a>>,
    /// Money arriving in one.
    into: Vec<Side<'a>>,
}

/// One transaction that could be one side of a transfer.
#[derive(Copy, Clone, Debug)]
struct Side<'a> {
    date: Date,
    /// Its record id.
    id: &'a str,
    /// The own account its leg is on.
    account: AccountId,
}

/// A suggestion and the date of its outgoing side, to sort by.
struct Found {
    date: Date,
    suggestion: Suggestion,
}

impl Sides<'_> {
    /// Pair each outgoing side, in date order, with the nearest unpaired
    /// incoming side on another account, and push the pairs to `found`.
    /// Ties go to the earlier incoming side; sides are ordered by date then
    /// record id, so the pairing doesn't depend on load order.
    fn pair(mut self, found: &mut Vec<Found>) {
        let order = |x: &Side, y: &Side| (x.date, x.id).cmp(&(y.date, y.id));
        self.out.sort_by(order);
        self.into.sort_by(order);
        let mut taken = vec![false; self.into.len()];
        for out in &self.out {
            let nearest = self
                .into
                .iter()
                .enumerate()
                .filter(|(at, side)| !taken[*at] && side.account != out.account)
                .map(|(at, side)| (out.date.days().abs_diff(side.date.days()), at))
                .filter(|(days, _)| *days <= WINDOW_DAYS)
                .min();
            let Some((days, at)) = nearest else {
                continue;
            };
            taken[at] = true;
            found.push(Found {
                date: out.date,
                suggestion: Suggestion {
                    link: TransferLink {
                        a: out.id.to_owned(),
                        b: self.into[at].id.to_owned(),
                    },
                    days,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ledgerdb::{Journal, JournalStore, LayerFilter, SegmentHeader, derive};

    /// A raw ledger and the store it was loaded from.
    struct Raw {
        ledger: Ledger,
        /// Held so the store outlives the test.
        _tmp: tempfile::TempDir,
    }

    /// A raw ledger of `text`, one segment.
    fn ledger(text: &str) -> Raw {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path());
        let parsed = Journal::parse(text);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let header = SegmentHeader {
            source: "t".to_owned(),
            imported: 1,
            layer: "t".to_owned(),
        };
        store.write_segment(&header, &parsed.journal).unwrap();
        let ledger = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        Raw { ledger, _tmp: tmp }
    }

    /// A raw two-leg transaction: `qty` cents into `own`, the counter leg
    /// to `expenses:unknown` or `income:unknown` by sign.
    fn txn(id: &str, date: &str, own: &str, qty: &str) -> String {
        let counter = if qty.starts_with('-') {
            "expenses:unknown"
        } else {
            "income:unknown"
        };
        format!("{date} * x\n  ; id: {id}\n  {own}  {qty} CAD\n  {counter}\n\n")
    }

    /// The suggestion linking `a` to `b`, dated `days` apart.
    fn pair(a: &str, b: &str, days: u32) -> Suggestion {
        Suggestion {
            link: TransferLink {
                a: a.to_owned(),
                b: b.to_owned(),
            },
            days,
        }
    }

    #[test]
    fn a_card_payment_from_chequing_pairs() {
        let text = [
            // Chequing pays the card; the card sees it two days later.
            txn("bank:1", "2026-08-05", "assets:chequing", "-1500.00"),
            txn("card:1", "2026-08-07", "liabilities:card", "1500.00"),
            // Chequing to savings, same day.
            txn("bank:2", "2026-08-03", "assets:chequing", "-500.00"),
            txn("save:1", "2026-08-03", "assets:savings", "500.00"),
            // A purchase and its refund on one card: not a transfer.
            txn("card:2", "2026-08-10", "liabilities:card", "-20.00"),
            txn("card:3", "2026-08-11", "liabilities:card", "20.00"),
            // Equal and opposite, but too far apart.
            txn("bank:3", "2026-09-01", "assets:chequing", "-70.00"),
            txn("card:4", "2026-09-07", "liabilities:card", "70.00"),
        ]
        .concat();
        let raw = &ledger(&text).ledger;
        let edits = crate::rules::default_edits();
        assert_eq!(
            suggest(raw, &edits),
            [pair("bank:2", "save:1", 0), pair("bank:1", "card:1", 2)]
        );

        // Once linked, derive nets it out and it is no longer suggested.
        let mut linked = edits.clone();
        linked.links.push(suggest(raw, &edits)[0].link.clone());
        assert_eq!(suggest(raw, &linked), [pair("bank:1", "card:1", 2)]);
        let derived = derive(raw, &linked);
        assert!(derived.report().rejected.is_empty());
        let transfers = derived
            .ledger()
            .journal()
            .accounts
            .lookup("transfers")
            .unwrap();
        let net: i64 = derived.ledger().postings_to(transfers).map(|p| p.qty).sum();
        assert_eq!(net, 0);
    }

    #[test]
    fn each_side_pairs_once_with_the_nearest() {
        let text = [
            txn("bank:1", "2026-08-01", "assets:chequing", "-100.00"),
            txn("bank:2", "2026-08-04", "assets:chequing", "-100.00"),
            txn("card:1", "2026-08-05", "liabilities:card", "100.00"),
            txn("card:2", "2026-08-02", "liabilities:card", "100.00"),
            txn("card:3", "2026-08-03", "liabilities:card", "100.00"),
        ]
        .concat();
        let raw = &ledger(&text).ledger;
        assert_eq!(
            suggest(raw, &crate::rules::default_edits()),
            [pair("bank:1", "card:2", 1), pair("bank:2", "card:3", 1)]
        );
    }

    #[test]
    fn decided_and_categorized_transactions_are_left_out() {
        let text = [
            txn("bank:1", "2026-08-01", "assets:chequing", "-100.00"),
            txn("card:1", "2026-08-01", "liabilities:card", "100.00"),
            // Hand-entered: no counter leg to a default.
            "2026-08-01 * x\n  ; id: hand:1\n  assets:savings  100.00 CAD\n  income:salary\n\n"
                .to_owned(),
            // Not an own account.
            txn("other:1", "2026-08-01", "equity:opening", "100.00"),
        ]
        .concat();
        let raw = &ledger(&text).ledger;
        let mut edits = crate::rules::default_edits();
        assert_eq!(suggest(raw, &edits), [pair("bank:1", "card:1", 0)]);

        edits.overrides.push(ledgerdb::Override {
            record_id: "card:1".to_owned(),
            account: "expenses:fees".to_owned(),
        });
        assert!(suggest(raw, &edits).is_empty());
    }
}
