//! A generic, in-memory, plain-old-data ledger engine.
//!
//! ledgerdb reads and holds journals in a subset of the ledger-cli format.
//! Its core types are small and `Copy`: dates are day counts, amounts are
//! integers in a commodity's smallest unit, and accounts, commodities and
//! strings are interned behind dense ids, so a [`Posting`] is 24 bytes with
//! no heap data and a ledger is a few contiguous arrays.
//!
//! It knows nothing about any particular data source; see the README.

mod account;
mod amount;
mod commodity;
mod date;
mod strpool;
mod txn;

pub use account::{AccountError, AccountId, Accounts};
pub use amount::{Amount, AmountDisplay, AmountError, AmountText};
pub use commodity::{Commodities, CommodityId, MAX_PRECISION, Side, Style};
pub use date::{Date, DateError, Ymd};
pub use strpool::{StrId, StrPool};
pub use txn::{Meta, Posting, PostingFlags, Span, Txn, TxnFlag, TxnId};
