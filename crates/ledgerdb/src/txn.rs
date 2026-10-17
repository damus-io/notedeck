//! Transactions and their postings, as plain old data.

use std::ops::Range;

use crate::{AccountId, Amount, CommodityId, Date, StrId};

/// The index of a transaction in its ledger's transaction table.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TxnId(pub u32);

impl TxnId {
    /// The transaction's position in its table.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Per-posting flags, a bit set in a `u16`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PostingFlags(pub u16);

impl PostingFlags {
    /// No flags.
    pub const NONE: Self = Self(0);
    /// The posting is marked cleared (`* account`).
    pub const CLEARED: Self = Self(1 << 0);
    /// The posting is marked pending (`! account`).
    pub const PENDING: Self = Self(1 << 1);
    /// A virtual posting that need not balance: `(account)`.
    pub const VIRTUAL: Self = Self(1 << 2);
    /// A virtual posting that must balance with other bracketed postings:
    /// `[account]`.
    pub const BALANCED_VIRTUAL: Self = Self(1 << 3);
    /// The amount was left out and filled in to balance the transaction, so a
    /// writer can leave it out again.
    pub const ELIDED: Self = Self(1 << 4);

    /// Whether every flag in `other` is set.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// These flags with `other`'s set as well.
    pub fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// One leg of a transaction: an amount of one commodity moved into or out of
/// one account.
///
/// Twenty-four bytes, `Copy`, with no heap data, so a ledger's postings sit
/// in one contiguous array and a fold is a linear scan. The date is copied
/// from the transaction so date-bounded scans never touch the transaction
/// table.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Posting {
    /// The transaction's date.
    pub date: Date,
    /// The account the amount moves into (positive) or out of (negative).
    pub account: AccountId,
    /// The commodity of `qty`.
    pub commodity: CommodityId,
    /// Cleared, pending, virtual and elided bits.
    pub flags: PostingFlags,
    /// The transaction this posting belongs to.
    pub txn: TxnId,
    /// The quantity in the commodity's smallest unit.
    pub qty: i64,
}

impl Posting {
    /// The posting's quantity and commodity as an [`Amount`].
    pub fn amount(self) -> Amount {
        Amount {
            qty: self.qty,
            commodity: self.commodity,
        }
    }
}

/// A transaction's clearing state, from the mark after its date.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum TxnFlag {
    /// No mark.
    #[default]
    Uncleared,
    /// `*`: cleared.
    Cleared,
    /// `!`: pending.
    Pending,
}

/// A contiguous run of rows in one of a ledger's arrays: a transaction's
/// postings, or its metadata.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    /// The first row.
    pub start: u32,
    /// How many rows.
    pub len: u32,
}

impl Span {
    /// The rows as an index range.
    pub fn range(self) -> Range<usize> {
        let start = self.start as usize;
        start..start + self.len as usize
    }
}

/// One `; key: value` metadata entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Meta {
    /// The key, interned.
    pub key: StrId,
    /// The value, interned; empty for a bare `; key:` tag.
    pub value: StrId,
    /// The posting the entry is attached to, as an offset into its
    /// transaction's postings, or `None` when it belongs to the transaction.
    pub posting: Option<u32>,
}

/// One transaction: its header and where its postings and metadata are.
///
/// Every string is a [`StrId`] into the ledger's string pool and every list a
/// [`Span`] into one of its arrays, so a `Txn` owns nothing on the heap.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Txn {
    /// The transaction's date.
    pub date: Date,
    /// The clearing mark after the date.
    pub flag: TxnFlag,
    /// The `(code)` after the mark, if any.
    pub code: Option<StrId>,
    /// The payee or description.
    pub payee: StrId,
    /// The transaction's postings, in the order written.
    pub postings: Span,
    /// The transaction's metadata, its own and its postings', in the order
    /// written.
    pub meta: Span,
    /// The transaction's plain comments, its own and its postings', in the
    /// order written.
    pub comments: Span,
    /// The source's record id, from its `; id:` metadata, which identifies
    /// the same record across re-imports.
    pub id: Option<StrId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posting_stays_24_bytes() {
        assert_eq!(std::mem::size_of::<Posting>(), 24);
        assert_eq!(std::mem::align_of::<Posting>(), 8);
    }

    #[test]
    fn flags_combine() {
        let flags = PostingFlags::CLEARED.with(PostingFlags::ELIDED);
        assert!(flags.contains(PostingFlags::CLEARED));
        assert!(flags.contains(PostingFlags::ELIDED));
        assert!(!flags.contains(PostingFlags::VIRTUAL));
        assert!(flags.contains(PostingFlags::NONE));
    }

    #[test]
    fn span_range() {
        let span = Span { start: 4, len: 3 };
        assert_eq!(span.range(), 4..7);
        assert_eq!(Span::default().range(), 0..0);
    }
}
