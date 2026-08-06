//! Human-friendly card references — a thin re-export of the shared [`wordid`]
//! crate.
//!
//! The 33-bit BIP-39 encoding is identical across headway cards, notebook
//! nodes, and agentium sessions, so it lives once in the `wordid` crate. Headway
//! cards are keyed by a 32-byte event id, so callers use [`encode`] directly;
//! they prefix the `board#` slug themselves. This module stays so existing
//! `crate::wordid::encode` / `headway::wordid::encode` call sites keep resolving.

pub use wordid::{SEP, encode};
