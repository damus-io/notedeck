//! A parsed journal: one file's transactions, prices and comments, with the
//! tables their ids point into.

use std::fmt;

use crate::amount::write_styled;
use crate::{
    Accounts, Commodities, CommodityId, Date, Meta, Posting, Span, StrId, StrPool, Txn, TxnId,
};

/// An exact decimal number at its own scale: `mantissa / 10^scale`.
///
/// Prices and costs are kept like this rather than as an [`crate::Amount`]
/// because they are routinely written more finely than their commodity's
/// precision (`P 2024-01-02 BTC 61234.56789 CAD` beside postings in cents),
/// and storing them at the commodity's precision would round.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Decimal {
    /// The digits as an integer, signed.
    pub mantissa: i64,
    /// How many of the digits are after the decimal point.
    pub scale: u8,
}

impl Decimal {
    /// The number written at its own scale beside `commodity`, in that
    /// commodity's style, so that parsing the result gives the same decimal
    /// back.
    pub fn display(self, commodity: CommodityId, commodities: &Commodities) -> DecimalDisplay<'_> {
        DecimalDisplay {
            value: self,
            commodity,
            commodities,
        }
    }
}

/// A [`Decimal`] ready to format, from [`Decimal::display`].
pub struct DecimalDisplay<'a> {
    value: Decimal,
    commodity: CommodityId,
    commodities: &'a Commodities,
}

/// Writes `61234.56789 CAD`: every digit of the scale, in the commodity's
/// style.
impl fmt::Display for DecimalDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_styled(
            f,
            self.value.mantissa,
            self.value.scale,
            self.commodities.name(self.commodity),
            self.commodities.style(self.commodity),
        )
    }
}

/// One `P DATE COMMODITY AMOUNT` directive: on `date`, one unit of
/// `commodity` was worth `value` of `quote`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Price {
    /// The day the price applies from.
    pub date: Date,
    /// The commodity being priced.
    pub commodity: CommodityId,
    /// The commodity the price is in.
    pub quote: CommodityId,
    /// What one unit of `commodity` is worth in `quote`.
    pub value: Decimal,
}

/// Whether a posting's cost is per unit (`@`) or for the whole quantity
/// (`@@`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CostKind {
    /// `@`: the price of one unit.
    PerUnit,
    /// `@@`: the price of the posting's whole quantity.
    Total,
}

/// A posting's `@` or `@@` cost.
///
/// Costs are kept so they can be written back and so a transaction that
/// trades one commodity for another balances, but ledgerdb does no lot
/// accounting with them.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Cost {
    /// The posting the cost is on, as an index into [`Journal::postings`].
    pub posting: u32,
    /// Per unit or total.
    pub kind: CostKind,
    /// The commodity the cost is in.
    pub commodity: CommodityId,
    /// The cost, as written.
    pub value: Decimal,
}

/// A plain `; text` comment inside a transaction.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Comment {
    /// The comment's text after the `;`, trimmed.
    pub text: StrId,
    /// The posting the comment is attached to, as an offset into its
    /// transaction's postings, or `None` when it belongs to the transaction.
    pub posting: Option<u32>,
}

/// A comment line outside any transaction, such as a file's header.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileComment {
    /// The comment's text after the comment character, trimmed.
    pub text: StrId,
    /// How many transactions came before it, so a writer can put it back in
    /// place.
    pub txns_before: u32,
}

/// Everything read from one journal file.
///
/// Rows refer to each other by index and to strings, accounts and
/// commodities by interned id, so the whole journal is a handful of
/// contiguous arrays. A [`Txn`]'s spans index `postings`, `meta` and
/// `comments`; a [`Posting`]'s `txn` indexes `txns`.
#[derive(Debug, Default)]
pub struct Journal {
    /// The account tree every [`crate::AccountId`] refers to.
    pub accounts: Accounts,
    /// The commodity table every [`CommodityId`] refers to.
    pub commodities: Commodities,
    /// The string pool every [`StrId`] refers to.
    pub strings: StrPool,
    /// Transactions, in file order.
    pub txns: Vec<Txn>,
    /// Every transaction's postings, back to back in file order.
    pub postings: Vec<Posting>,
    /// Every transaction's metadata, back to back in file order.
    pub meta: Vec<Meta>,
    /// Every transaction's plain comments, back to back in file order.
    pub comments: Vec<Comment>,
    /// Costs, sorted by the posting they are on.
    pub costs: Vec<Cost>,
    /// `P` directives, in file order.
    pub prices: Vec<Price>,
    /// Comment lines outside transactions, in file order.
    pub file_comments: Vec<FileComment>,
}

impl Journal {
    /// An empty journal.
    pub fn new() -> Self {
        Self::default()
    }

    /// The transaction `id` names.
    pub fn txn(&self, id: TxnId) -> &Txn {
        &self.txns[id.index()]
    }

    /// A transaction's postings.
    pub fn postings_of(&self, txn: &Txn) -> &[Posting] {
        &self.postings[txn.postings.range()]
    }

    /// A transaction's metadata, its own and its postings'.
    pub fn meta_of(&self, txn: &Txn) -> &[Meta] {
        &self.meta[txn.meta.range()]
    }

    /// A transaction's plain comments, its own and its postings'.
    pub fn comments_of(&self, txn: &Txn) -> &[Comment] {
        &self.comments[txn.comments.range()]
    }

    /// The cost on posting `index` (an index into [`Self::postings`]), if it
    /// has one.
    pub fn cost_of(&self, index: u32) -> Option<&Cost> {
        self.costs
            .binary_search_by_key(&index, |cost| cost.posting)
            .ok()
            .map(|at| &self.costs[at])
    }

    /// Give every transaction that has no [`Txn::id`] the id `id_for`
    /// returns, written as a transaction-level `; id:` metadata entry ahead
    /// of its other metadata, and return how many were given one.
    ///
    /// `id_for` sees the journal as it was before any id was added, in file
    /// order, so it can name a transaction by its own written text. The
    /// metadata table is rebuilt once, so this is linear in the journal.
    pub fn fill_missing_ids(&mut self, mut id_for: impl FnMut(&Journal, TxnId) -> String) -> usize {
        let ids: Vec<Option<String>> = self
            .txns
            .iter()
            .enumerate()
            .map(|(index, txn)| txn.id.is_none().then(|| id_for(self, TxnId(index as u32))))
            .collect();
        let filled = ids.iter().flatten().count();
        if filled == 0 {
            return 0;
        }

        let key = self.strings.intern("id");
        let mut meta = Vec::with_capacity(self.meta.len() + filled);
        for (txn, id) in self.txns.iter_mut().zip(ids) {
            let start = meta.len() as u32;
            if let Some(id) = id {
                let value = self.strings.intern(&id);
                txn.id = Some(value);
                meta.push(Meta {
                    key,
                    value,
                    posting: None,
                });
            }
            meta.extend_from_slice(&self.meta[txn.meta.range()]);
            txn.meta = Span {
                start,
                len: meta.len() as u32 - start,
            };
        }
        self.meta = meta;
        filled
    }

    /// The value of the transaction-level metadata entry `key`, if any.
    pub fn txn_meta(&self, txn: &Txn, key: &str) -> Option<&str> {
        let key = self.strings.lookup(key)?;
        self.meta_of(txn)
            .iter()
            .find(|meta| meta.posting.is_none() && meta.key == key)
            .map(|meta| self.strings.get(meta.value))
    }
}
