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
//!
//! An on-chain move between a bitcoind wallet and the CLN node's wallet is
//! recorded twice the same way, by the `btc` and the `cln` importer, and
//! both records carry the transaction's `raw-txid`. That id is proof the two
//! are one move, so [`suggest`] pairs those first, by txid and with no date
//! window, before it pairs the rest by amount and date. Only records with a
//! leg on one of the owner's own accounts pair by txid: the `payroll`
//! importer writes `raw-txid` on a payout too, but a payout is spending, not
//! a move between own accounts, so it is never one side of a transfer.

use std::collections::{BTreeMap, HashMap, HashSet};

use ledgerdb::{AccountId, CommodityId, Date, Edits, Journal, Ledger, Posting, TransferLink, Txn};

/// The metadata key both on-chain importers keep a record's transaction id
/// under.
pub const TXID_KEY: &str = "raw-txid";

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
/// First by txid: own-account records (those with a posting to an account
/// under [`OWN_ROOTS`]) of two different sources (the `<source>:` prefix of
/// their ids) that carry one [`TXID_KEY`] are one on-chain move, and pair
/// when each has exactly one counter leg (a posting to one of
/// [`Edits::defaults`], the leg derive moves) and exactly one pair of those
/// legs is in one commodity and nets to zero. Their other legs, a fee or a
/// third party's output, don't matter, and neither do their dates. A txid
/// whose records don't net that way suggests nothing: a send that paid the
/// other wallet and someone else in one transaction needs a split first.
/// Every own-account record of a txid two sources hold is then left out of
/// the amount pass, paired or not, so it isn't paired with a stranger
/// instead. A record with no own-account leg, such as a payroll payout that
/// names the send that paid it, takes no part in the txid pass at all: it
/// pairs with nothing and holds no other record back.
///
/// Then by amount: two transactions pair when each is an importer's raw record (one leg to
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

    let mut found = Vec::new();
    let by_txid = pair_by_txid(journal, &is_default, &is_own, &decided, &mut found);

    // Every candidate side, grouped by what its pair must match.
    let mut groups: BTreeMap<Group, Sides> = BTreeMap::new();
    for txn in raw.txns() {
        let Some(id) = txn.id.map(|id| journal.strings.get(id)) else {
            continue;
        };
        if decided.contains(id) || by_txid.contains(id) {
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

    for sides in groups.into_values() {
        sides.pair(&mut found);
    }
    found.sort_by(|x, y| (x.date, &x.suggestion.link.a).cmp(&(y.date, &y.suggestion.link.a)));
    found.into_iter().map(|f| f.suggestion).collect()
}

/// Pair the own-account records of each txid two sources hold, as
/// [`suggest`] says, pushing the pairs to `found`, and return the ids of
/// every own-account record of those txids, paired or not.
fn pair_by_txid<'a>(
    journal: &'a Journal,
    is_default: &HashSet<AccountId>,
    is_own: &impl Fn(AccountId) -> bool,
    decided: &HashSet<&str>,
    found: &mut Vec<Found>,
) -> HashSet<&'a str> {
    let mut by_txid: HashMap<&str, Vec<Record>> = HashMap::new();
    for txn in &journal.txns {
        let Some(id) = txn.id.map(|id| journal.strings.get(id)) else {
            continue;
        };
        let Some(txid) = journal.txn_meta(txn, TXID_KEY) else {
            continue;
        };
        if !journal.postings_of(txn).iter().any(|p| is_own(p.account)) {
            continue;
        }
        let counter = counter_leg(journal, is_default, txn).filter(|_| !decided.contains(id));
        by_txid.entry(txid).or_default().push(Record {
            id,
            source: id.split_once(':').map_or(id, |(source, _)| source),
            date: txn.date,
            counter: counter.copied(),
        });
    }

    let mut held = HashSet::new();
    for records in by_txid.values() {
        let first = records[0].source;
        if records.iter().all(|r| r.source == first) {
            continue;
        }
        held.extend(records.iter().map(|r| r.id));
        if let Some(suggestion) = netting_pair(records) {
            found.push(suggestion);
        }
    }
    held
}

/// The one pair of `records`, from two sources, whose counter legs net to
/// zero in one commodity, as a suggestion; `None` when there is no such
/// pair or more than one.
fn netting_pair(records: &[Record]) -> Option<Found> {
    let mut pair = None;
    for (at, x) in records.iter().enumerate() {
        for y in &records[at + 1..] {
            let (Some(xc), Some(yc)) = (x.counter, y.counter) else {
                continue;
            };
            let nets = x.source != y.source
                && xc.commodity == yc.commodity
                && xc.qty != 0
                && i128::from(xc.qty) + i128::from(yc.qty) == 0;
            if !nets {
                continue;
            }
            if pair.is_some() {
                return None;
            }
            // The side whose counter leg is positive is the one money left.
            pair = Some(if xc.qty > 0 { (x, y) } else { (y, x) });
        }
    }
    let (out, into) = pair?;
    Some(Found {
        date: out.date,
        suggestion: Suggestion {
            link: TransferLink {
                a: out.id.to_owned(),
                b: into.id.to_owned(),
            },
            days: out.date.days().abs_diff(into.date.days()),
        },
    })
}

/// `txn`'s one posting to a default account, or `None` when it has none or
/// several: the leg derive moves when a link names it.
fn counter_leg<'a>(
    journal: &'a Journal,
    is_default: &HashSet<AccountId>,
    txn: &Txn,
) -> Option<&'a Posting> {
    let mut legs = journal
        .postings_of(txn)
        .iter()
        .filter(|posting| is_default.contains(&posting.account));
    let leg = legs.next()?;
    legs.next().is_none().then_some(leg)
}

/// A record that carries a txid.
struct Record<'a> {
    id: &'a str,
    /// The `<source>:` prefix of its id.
    source: &'a str,
    date: Date,
    /// Its counter leg, or `None` when it has no single one or an edit
    /// already decided it, so it can't be paired.
    counter: Option<Posting>,
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

    /// An on-chain record `id` of txid `txid`, its postings `legs` in msat.
    fn chain(id: &str, date: &str, txid: &str, legs: &[(&str, i64)]) -> String {
        let mut text = format!("{date} * x\n  ; id: {id}\n  ; raw-txid: {txid}\n");
        for (account, msat) in legs {
            text.push_str(&format!("  {account}  {msat} msat\n"));
        }
        text + "\n"
    }

    /// The CLN wallet paying a bitcoind wallet: bookkeeper's grouped
    /// withdrawal against the btc importer's receipt.
    fn cln_to_btc(txid: &str, date: &str, msat: i64, fee: i64) -> String {
        let cln = chain(
            &format!("cln:tx:{txid}"),
            date,
            txid,
            &[
                ("assets:cln:wallet", -(msat + fee)),
                ("expenses:fees:onchain", fee),
                ("expenses:unknown", msat),
            ],
        );
        let btc = chain(
            &format!("btc:{txid}:0"),
            date,
            txid,
            &[("assets:btc:hot", msat), ("income:unknown", -msat)],
        );
        cln + &btc
    }

    /// A raw record with no txid, `msat` into `own` against the default by
    /// sign, which the amount pass would pair with a txid's record.
    fn stranger(id: &str, date: &str, own: &str, msat: i64) -> String {
        let counter = if msat < 0 {
            "expenses:unknown"
        } else {
            "income:unknown"
        };
        format!("{date} * x\n  ; id: {id}\n  {own}  {msat} msat\n  {counter}\n\n")
    }

    /// Both directions pair by txid, each side's fee on its own account and
    /// however far apart they are dated; a link of each nets in derive, and
    /// neither record is also offered to a stranger by amount.
    #[test]
    fn on_chain_moves_pair_by_txid_both_ways() {
        let text = [
            cln_to_btc("aa", "2026-08-01", 1_000_000, 1_000),
            // The bitcoind wallet paying the CLN wallet, logged ten days on.
            chain(
                "btc:bb",
                "2026-08-02",
                "bb",
                &[
                    ("assets:btc:hot", -2_000_500),
                    ("expenses:fees:btc", 500),
                    ("expenses:unknown", 2_000_000),
                ],
            ),
            chain(
                "cln:tx:bb",
                "2026-08-12",
                "bb",
                &[
                    ("assets:cln:wallet", 2_000_000),
                    ("income:unknown", -2_000_000),
                ],
            ),
            // Would pair with cln:tx:bb by amount and date.
            stranger("bank:1", "2026-08-12", "assets:chequing", -2_000_000),
        ]
        .concat();
        let raw = &ledger(&text).ledger;
        let edits = crate::rules::default_edits();
        let found = suggest(raw, &edits);
        assert_eq!(
            found,
            [
                pair("cln:tx:aa", "btc:aa:0", 0),
                pair("btc:bb", "cln:tx:bb", 10)
            ]
        );

        let mut linked = edits.clone();
        linked.links.extend(found.into_iter().map(|s| s.link));
        assert!(suggest(raw, &linked).is_empty());
        let derived = derive(raw, &linked);
        assert!(derived.report().rejected.is_empty());
        let journal = derived.ledger().journal();
        let transfers = journal.accounts.lookup("transfers").unwrap();
        let legs: Vec<i64> = derived
            .ledger()
            .postings_to(transfers)
            .map(|p| p.qty)
            .collect();
        assert_eq!(legs.len(), 4);
        assert_eq!(legs.iter().sum::<i64>(), 0);
    }

    /// A send that paid the CLN wallet and a third party in one transaction
    /// doesn't net, so it suggests nothing, by txid or by amount.
    #[test]
    fn a_third_party_output_suggests_nothing() {
        let text = [
            chain(
                "btc:cc",
                "2026-08-01",
                "cc",
                &[
                    ("assets:btc:hot", -3_000_500),
                    ("expenses:fees:btc", 500),
                    ("expenses:unknown", 3_000_000),
                ],
            ),
            chain(
                "cln:tx:cc",
                "2026-08-01",
                "cc",
                &[
                    ("assets:cln:wallet", 1_000_000),
                    ("income:unknown", -1_000_000),
                ],
            ),
            stranger("bank:1", "2026-08-01", "assets:chequing", -1_000_000),
        ]
        .concat();
        let raw = &ledger(&text).ledger;
        assert!(suggest(raw, &crate::rules::default_edits()).is_empty());
    }

    /// Of several receipts of one txid only the one that nets pairs; when two
    /// would, none does, and they aren't summed to make one.
    #[test]
    fn one_receipt_of_several_must_net() {
        let one = [
            cln_to_btc("dd", "2026-08-01", 1_000_000, 1_000),
            chain(
                "btc:dd:1",
                "2026-08-01",
                "dd",
                &[("assets:btc:cold", 500_000), ("income:unknown", -500_000)],
            ),
        ]
        .concat();
        let raw = &ledger(&one).ledger;
        let edits = crate::rules::default_edits();
        assert_eq!(suggest(raw, &edits), [pair("cln:tx:dd", "btc:dd:0", 0)]);

        let two = [
            cln_to_btc("ee", "2026-08-01", 1_000_000, 1_000),
            chain(
                "btc:ee:1",
                "2026-08-01",
                "ee",
                &[
                    ("assets:btc:cold", 1_000_000),
                    ("income:unknown", -1_000_000),
                ],
            ),
        ]
        .concat();
        assert!(suggest(&ledger(&two).ledger, &edits).is_empty());

        let split = [
            chain(
                "cln:tx:ff",
                "2026-08-01",
                "ff",
                &[
                    ("assets:cln:wallet", -1_501_000),
                    ("expenses:fees:onchain", 1_000),
                    ("expenses:unknown", 1_500_000),
                ],
            ),
            chain(
                "btc:ff:0",
                "2026-08-01",
                "ff",
                &[
                    ("assets:btc:hot", 1_000_000),
                    ("income:unknown", -1_000_000),
                ],
            ),
            chain(
                "btc:ff:1",
                "2026-08-01",
                "ff",
                &[("assets:btc:hot", 500_000), ("income:unknown", -500_000)],
            ),
        ]
        .concat();
        assert!(suggest(&ledger(&split).ledger, &edits).is_empty());
    }

    /// Records of one source that share a txid aren't paired by it.
    #[test]
    fn one_sources_records_of_a_txid_dont_pair() {
        let text = [
            chain(
                "cln:chain:1",
                "2026-08-01",
                "gg",
                &[
                    ("assets:cln:wallet", -1_000_000),
                    ("expenses:unknown", 1_000_000),
                ],
            ),
            chain(
                "cln:chain:2",
                "2026-08-20",
                "gg",
                &[
                    ("assets:cln:other", 1_000_000),
                    ("income:unknown", -1_000_000),
                ],
            ),
        ]
        .concat();
        let raw = &ledger(&text).ledger;
        assert!(suggest(raw, &crate::rules::default_edits()).is_empty());
    }

    /// A payroll payout: the btc importer's send `btc:<txid>`, and the
    /// payroll importer's record of the same txid moving the paid amount
    /// from `expenses:unknown` to `expenses:payroll`.
    fn payroll_send(txid: &str, date: &str, msat: i64, fee: i64) -> String {
        let send = chain(
            &format!("btc:{txid}"),
            date,
            txid,
            &[
                ("assets:btc:hot", -(msat + fee)),
                ("expenses:fees:btc", fee),
                ("expenses:unknown", msat),
            ],
        );
        let payout = chain(
            &format!("payroll:{txid}"),
            date,
            txid,
            &[("expenses:payroll", msat), ("expenses:unknown", -msat)],
        );
        send + &payout
    }

    /// A payroll record nets against its send's counter leg, but has no
    /// own-account leg, so the two aren't a transfer.
    #[test]
    fn a_payroll_payout_doesnt_pair_with_its_send() {
        let raw = &ledger(&payroll_send("jj", "2026-08-01", 1_000_000, 500)).ledger;
        assert!(suggest(raw, &crate::rules::default_edits()).is_empty());
    }

    /// A send that paid the CLN wallet, which a payroll record also names,
    /// still pairs with the CLN receipt by txid.
    #[test]
    fn a_payroll_record_doesnt_stop_a_txid_pair() {
        let text = [
            payroll_send("kk", "2026-08-01", 1_000_000, 500),
            chain(
                "cln:tx:kk",
                "2026-08-03",
                "kk",
                &[
                    ("assets:cln:wallet", 1_000_000),
                    ("income:unknown", -1_000_000),
                ],
            ),
        ]
        .concat();
        let raw = &ledger(&text).ledger;
        assert_eq!(
            suggest(raw, &crate::rules::default_edits()),
            [pair("btc:kk", "cln:tx:kk", 2)]
        );
    }

    /// A txid one of whose records an edit already decided suggests
    /// nothing, and its other record isn't offered to a stranger by amount.
    #[test]
    fn a_decided_txid_record_is_skipped() {
        let text = [
            cln_to_btc("hh", "2026-08-01", 1_000_000, 1_000),
            stranger("bank:1", "2026-08-01", "assets:chequing", -1_000_000),
        ]
        .concat();
        let raw = &ledger(&text).ledger;
        let mut edits = crate::rules::default_edits();
        assert_eq!(suggest(raw, &edits), [pair("cln:tx:hh", "btc:hh:0", 0)]);

        edits.overrides.push(ledgerdb::Override {
            record_id: "cln:tx:hh".to_owned(),
            account: "expenses:gifts".to_owned(),
        });
        assert!(suggest(raw, &edits).is_empty());
    }
}
