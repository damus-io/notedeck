//! Which of a batch's transactions a ledger already holds, so an importer
//! that re-reads overlapping data writes only what is new or changed.
//!
//! A batch transaction is matched to the ledger's by record id (`; id:`),
//! the same identity [`Ledger`] dedups on, and the two are compared by
//! value: dates, flags, strings, account and commodity names, quantities,
//! costs, metadata and comments. Nothing is compared by interned id or by
//! written bytes, since both differ between a ledger and a batch parsed with
//! tables of its own: the ledger rescales a commodity to the finest
//! precision any segment gave it, so `4.50 CAD` in a batch is `4.500 CAD` in
//! a ledger that has seen a three-place CAD amount, and the two are the same
//! row.
//!
//! A `P` directive has no record id; its date and pair are its identity, and
//! it is compared with the rate the ledger uses for them.

use crate::{Decimal, Journal, Ledger, PostingFlags, StrId, Txn, TxnId};

/// How a batch transaction compares with what a ledger holds.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Novelty {
    /// The ledger has no transaction with its record id, or it has none.
    New,
    /// The ledger has its record id with different contents, such as a
    /// pending row that has since posted.
    Changed,
    /// The ledger already holds it exactly.
    Unchanged,
}

impl Ledger {
    /// How transaction `txn` of `batch` compares with the live transaction
    /// carrying its record id. A transaction without an id is always
    /// [`Novelty::New`], since nothing could replace it.
    ///
    /// Only the layers this ledger includes are looked in, so load it with
    /// [`crate::LayerFilter::All`] to ask whether a row is anywhere in the
    /// journal: record ids dedup across every layer.
    pub fn novelty(&self, batch: &Journal, txn: TxnId) -> Novelty {
        let Some(id) = batch.txn(txn).id else {
            return Novelty::New;
        };
        let Some(held) = self.find(batch.strings.get(id)) else {
            return Novelty::New;
        };
        if self.journal().same_txn(held, batch, txn) {
            Novelty::Unchanged
        } else {
            Novelty::Changed
        }
    }

    /// How price `index` of `batch` (an index into [`Journal::prices`])
    /// compares with the rate the ledger uses for its date and pair: the
    /// latest import's, since a `P` directive's date and pair are its
    /// identity. [`Novelty::Changed`] is a revised rate for that date.
    pub fn price_novelty(&self, batch: &Journal, index: usize) -> Novelty {
        let price = &batch.prices[index];
        let journal = self.journal();
        let name = |id| batch.commodities.name(id);
        let (Some(commodity), Some(quote)) = (
            journal.commodities.get(name(price.commodity)),
            journal.commodities.get(name(price.quote)),
        ) else {
            return Novelty::New;
        };
        let prices = self.prices();
        let start = prices.partition_point(|p| p.date < price.date);
        let end = prices.partition_point(|p| p.date <= price.date);
        let held = prices[start..end]
            .iter()
            .rev()
            .find(|p| p.commodity == commodity && p.quote == quote);
        match held {
            None => Novelty::New,
            Some(held) if same_value(held.value, price.value) => Novelty::Unchanged,
            Some(_) => Novelty::Changed,
        }
    }
}

impl Journal {
    /// Whether transaction `txn` here and `other_txn` in `other` hold the
    /// same contents: date, flag, code, payee, postings (account, commodity,
    /// quantity, flags and cost), metadata and comments, in order.
    ///
    /// The journals may have different tables: names are compared rather
    /// than ids, and quantities at a common precision, so the same row read
    /// at two precisions is equal. A posting whose amount was left out
    /// (elided) is compared by account and flags only, as it was written:
    /// its amount was inferred from the other postings, which are compared,
    /// and only how each journal's precision rounded it can differ.
    pub fn same_txn(&self, txn: TxnId, other: &Journal, other_txn: TxnId) -> bool {
        let (a, b) = (self.txn(txn), other.txn(other_txn));
        let same_str = |x: StrId, y: StrId| self.strings.get(x) == other.strings.get(y);
        let same_code = match (a.code, b.code) {
            (Some(x), Some(y)) => same_str(x, y),
            (x, y) => x.is_none() && y.is_none(),
        };
        if a.date != b.date || a.flag != b.flag || !same_code || !same_str(a.payee, b.payee) {
            return false;
        }

        let (metas, other_metas) = (self.meta_of(a), other.meta_of(b));
        let same_meta = metas.len() == other_metas.len()
            && metas.iter().zip(other_metas).all(|(x, y)| {
                x.posting == y.posting && same_str(x.key, y.key) && same_str(x.value, y.value)
            });
        let (comments, other_comments) = (self.comments_of(a), other.comments_of(b));
        let same_comments = comments.len() == other_comments.len()
            && comments
                .iter()
                .zip(other_comments)
                .all(|(x, y)| x.posting == y.posting && same_str(x.text, y.text));
        if !same_meta || !same_comments {
            return false;
        }

        let same_commodity = |x, y| self.commodities.name(x) == other.commodities.name(y);
        let (ours, theirs) = (self.written_postings(a), other.written_postings(b));
        ours.clone().count() == theirs.clone().count()
            && ours.zip(theirs).all(|(at, other_at)| {
                let (x, y) = (
                    &self.postings[at as usize],
                    &other.postings[other_at as usize],
                );
                if x.flags != y.flags
                    || self.accounts.full_name(x.account) != other.accounts.full_name(y.account)
                {
                    return false;
                }
                // An elided amount was inferred from the others, so when
                // they match it can differ only in how each journal's
                // precision rounded it.
                if x.flags.contains(PostingFlags::ELIDED) {
                    return true;
                }
                let same_cost = match (self.cost_of(at), other.cost_of(other_at)) {
                    (Some(x), Some(y)) => {
                        x.kind == y.kind
                            && same_commodity(x.commodity, y.commodity)
                            && same_value(x.value, y.value)
                    }
                    (x, y) => x.is_none() && y.is_none(),
                };
                let qty = |p: &crate::Posting, journal: &Journal| Decimal {
                    mantissa: p.qty,
                    scale: journal.commodities.precision(p.commodity),
                };
                same_cost
                    && same_commodity(x.commodity, y.commodity)
                    && same_value(qty(x, self), qty(y, other))
            })
    }

    /// The postings of `txn` as the writer prints them without explicit
    /// amounts, as indices into [`Self::postings`]: an elided posting the
    /// parser filled with several commodities is one posting per commodity
    /// here, and only its first is kept.
    fn written_postings(&self, txn: &Txn) -> impl Iterator<Item = u32> + Clone + '_ {
        let postings = self.postings_of(txn);
        let start = txn.postings.start;
        (0..postings.len())
            .filter(move |&at| {
                let elided = |p: &crate::Posting| p.flags.contains(PostingFlags::ELIDED);
                let p = &postings[at];
                !(at > 0
                    && elided(p)
                    && elided(&postings[at - 1])
                    && postings[at - 1].account == p.account)
            })
            .map(move |at| start + at as u32)
    }
}

/// Whether two decimals are the same number, whatever their scales:
/// `4.50` equals `4.500`. Trailing zeros are stripped from both rather than
/// one widened to the other's scale, which could overflow.
fn same_value(a: Decimal, b: Decimal) -> bool {
    let trimmed = |mut d: Decimal| {
        while d.scale > 0 && d.mantissa % 10 == 0 {
            d.mantissa /= 10;
            d.scale -= 1;
        }
        d
    };
    trimmed(a) == trimmed(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(mantissa: i64, scale: u8) -> Decimal {
        Decimal { mantissa, scale }
    }

    #[test]
    fn decimals_compare_by_value_across_scales() {
        assert!(same_value(dec(450, 2), dec(4500, 3)));
        assert!(same_value(dec(-1, 0), dec(-1000, 3)));
        assert!(same_value(dec(0, 0), dec(0, 9)));
        assert!(!same_value(dec(451, 2), dec(4500, 3)));
        assert!(!same_value(dec(45, 1), dec(45, 2)));
        // Scales an i128 could not widen between.
        assert!(!same_value(dec(i64::MAX, 0), dec(1, 200)));
    }
}
