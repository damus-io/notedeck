//! A split as a person writes it, made into the legs derive applies.
//!
//! A split replaces a transaction's counter leg with several postings, and
//! its legs are stored in the counter leg's smallest units and with its sign
//! ([`ledgerdb::Leg`]). People write them as amounts: `expenses:food=12.50`,
//! a positive spend however the bank signs it, with one leg left blank to
//! take what the others don't. [`legs`] turns those [`LegDraft`]s into legs,
//! the one copy of the rules the CLI's `split` and the app's split editor
//! both use, and [`triage::Action::Split`](crate::triage::Action::Split)
//! writes them.

use ledgerdb::{Amount, AmountError, AmountText, Edits, Ledger, Leg, Posting, TxnId};

/// One leg of a split as written: where it goes, and how much.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegDraft {
    /// The account.
    pub account: String,
    /// The amount as written (`12.50`, `-3`, `12.50 CAD`), in the counter
    /// leg's commodity and direction; `None` takes what the other legs
    /// leave.
    pub amount: Option<String>,
}

/// Why [`legs`] can't make a split.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SplitError {
    /// No counter leg to replace.
    #[error("{0} has no counter leg to split (no posting, or more than one, to a default account)")]
    NoCounterLeg(String),
    /// An amount doesn't parse, or has more decimals than the commodity.
    #[error("{account}={text}: {error}")]
    Amount {
        /// The leg's account.
        account: String,
        /// The amount as written.
        text: String,
        /// What is wrong with it.
        error: AmountError,
    },
    /// An amount names another commodity than the counter leg's.
    #[error("{account}={text}: the transaction's counter leg is in {commodity}")]
    Commodity {
        /// The leg's account.
        account: String,
        /// The amount as written.
        text: String,
        /// The counter leg's commodity.
        commodity: String,
    },
    /// More than one leg left blank: only one can take the remainder.
    #[error("only one leg can go without an amount")]
    TwoRemainders,
    /// The remainder doesn't fit an amount.
    #[error("the legs are too large")]
    TooLarge,
    /// No leg is blank, and they don't add up to the counter leg.
    #[error("the legs add up to {sum}, but the counter leg is {want}")]
    Sum {
        /// What the legs add up to, as shown.
        sum: String,
        /// The counter leg, as shown.
        want: String,
    },
}

/// The counter leg of `txn` in `raw`: its one posting to a default account
/// of `edits`, which a split replaces.
pub fn counter_leg<'l>(raw: &'l Ledger, edits: &Edits, txn: TxnId) -> Option<&'l Posting> {
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

/// `drafts` as the legs of a split of transaction `txn` (record id `record`)
/// in `raw`, whose default accounts are `edits.defaults`: in the counter
/// leg's smallest units and sign, the blank leg filled in with the
/// remainder. With no blank leg they must add up to the counter leg
/// exactly.
pub fn legs(
    raw: &Ledger,
    edits: &Edits,
    txn: TxnId,
    record: &str,
    drafts: &[LegDraft],
) -> Result<Vec<Leg>, SplitError> {
    let counter =
        counter_leg(raw, edits, txn).ok_or_else(|| SplitError::NoCounterLeg(record.to_owned()))?;
    let commodities = &raw.journal().commodities;
    let commodity = commodities.name(counter.commodity);
    let precision = commodities.precision(counter.commodity);
    let sign = if counter.qty < 0 { -1 } else { 1 };
    let mut out = Vec::with_capacity(drafts.len());
    let mut rest_at = None;
    for draft in drafts {
        let Some(text) = &draft.amount else {
            if rest_at.is_some() {
                return Err(SplitError::TwoRemainders);
            }
            rest_at = Some(out.len());
            out.push(Leg {
                account: draft.account.clone(),
                qty: 0,
            });
            continue;
        };
        let bad = |error| SplitError::Amount {
            account: draft.account.clone(),
            text: text.clone(),
            error,
        };
        let amount = AmountText::parse(text).map_err(bad)?;
        if !amount.commodity.is_empty() && amount.commodity != commodity {
            return Err(SplitError::Commodity {
                account: draft.account.clone(),
                text: text.clone(),
                commodity: commodity.to_owned(),
            });
        }
        let qty = amount.qty_at(precision).map_err(bad)?;
        out.push(Leg {
            account: draft.account.clone(),
            qty: qty * sign,
        });
    }
    let sum: i128 = out.iter().map(|l| i128::from(l.qty)).sum();
    let want = i128::from(counter.qty);
    match rest_at {
        Some(at) => out[at].qty = i64::try_from(want - sum).map_err(|_| SplitError::TooLarge)?,
        None if sum != want => {
            let shown = |qty: i128| {
                i64::try_from(qty).map_or(qty.to_string(), |qty| {
                    Amount {
                        qty,
                        commodity: counter.commodity,
                    }
                    .display(commodities)
                    .to_string()
                })
            };
            return Err(SplitError::Sum {
                sum: shown(sum),
                want: shown(want),
            });
        }
        None => {}
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use ledgerdb::{JournalStore, LayerFilter, SegmentHeader};

    /// A 100.00 CAD grocery run on the card, unknown, and a payment with no
    /// default leg.
    const TEXT: &str = "\
2024-03-01 * Grocer
    ; id: card:1
    liabilities:card  -100.00 CAD
    expenses:unknown

2024-03-02 * Move
    ; id: bank:1
    assets:checking  -5.00 CAD
    assets:savings
";

    struct Fixture {
        raw: Ledger,
        edits: Edits,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = JournalStore::new(dir.path());
        let header = SegmentHeader {
            source: "t".to_owned(),
            imported: 1,
            layer: "t".to_owned(),
        };
        store
            .write_segment(&header, &ledgerdb::Journal::parse(TEXT).journal)
            .unwrap();
        Fixture {
            raw: Ledger::load(&store, LayerFilter::All).unwrap().ledger,
            edits: Edits {
                defaults: vec!["expenses:unknown".to_owned()],
                ..Edits::default()
            },
            _dir: dir,
        }
    }

    impl Fixture {
        fn legs(
            &self,
            record: &str,
            drafts: &[(&str, Option<&str>)],
        ) -> Result<Vec<Leg>, SplitError> {
            let drafts: Vec<LegDraft> = drafts
                .iter()
                .map(|(account, amount)| LegDraft {
                    account: (*account).to_owned(),
                    amount: amount.map(str::to_owned),
                })
                .collect();
            let txn = self.raw.find(record).unwrap();
            legs(&self.raw, &self.edits, txn, record, &drafts)
        }
    }

    fn leg(account: &str, qty: i64) -> Leg {
        Leg {
            account: account.to_owned(),
            qty,
        }
    }

    /// The blank leg takes the remainder, in the counter leg's sign.
    #[test]
    fn the_blank_leg_takes_the_rest() {
        let f = fixture();
        let got = f.legs(
            "card:1",
            &[("expenses:food", Some("60")), ("expenses:home", None)],
        );
        assert_eq!(
            got.unwrap(),
            [leg("expenses:food", 6000), leg("expenses:home", 4000)]
        );
    }

    /// With every amount given they must add up exactly.
    #[test]
    fn full_legs_must_sum_exactly() {
        let f = fixture();
        let exact = f.legs(
            "card:1",
            &[
                ("expenses:food", Some("60.00 CAD")),
                ("expenses:home", Some("40")),
            ],
        );
        assert_eq!(
            exact.unwrap(),
            [leg("expenses:food", 6000), leg("expenses:home", 4000)]
        );
        let over = f
            .legs(
                "card:1",
                &[("expenses:food", Some("60")), ("expenses:home", Some("41"))],
            )
            .unwrap_err();
        assert_eq!(
            over.to_string(),
            "the legs add up to 101.00 CAD, but the counter leg is 100.00 CAD"
        );
    }

    /// More decimals than CAD has, another commodity, two blanks and a
    /// transaction with no counter leg are refused.
    #[test]
    fn bad_legs_are_refused() {
        let f = fixture();
        let precise = f.legs("card:1", &[("expenses:food", Some("0.001")), ("a:b", None)]);
        assert!(matches!(precise, Err(SplitError::Amount { .. })));
        assert!(
            precise
                .unwrap_err()
                .to_string()
                .starts_with("expenses:food=0.001: ")
        );
        let other = f.legs(
            "card:1",
            &[("expenses:food", Some("60 USD")), ("a:b", None)],
        );
        assert_eq!(
            other.unwrap_err().to_string(),
            "expenses:food=60 USD: the transaction's counter leg is in CAD"
        );
        let two = f.legs("card:1", &[("a:b", None), ("c:d", None)]);
        assert_eq!(two, Err(SplitError::TwoRemainders));
        let none = f.legs("bank:1", &[("a:b", None)]);
        assert_eq!(none, Err(SplitError::NoCounterLeg("bank:1".to_owned())));
    }
}
