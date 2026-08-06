//! Human-friendly node references — a thin re-export of the shared [`wordid`]
//! crate.
//!
//! The 33-bit BIP-39 encoding is identical across notebook nodes, headway
//! cards, and agentium sessions, so it lives once in the `wordid` crate.
//! Notebook nodes are keyed by a 32-byte event id, so callers use [`encode`]
//! directly; the CLI prefixes the canvas id and an `@` sigil, giving ids like
//! `notebook@maple-river-canyon`. The systems are kept apart not by the words
//! but by that render-layer sigil (headway uses `#`, the notebook `@`), so a
//! reference is never mistaken for the other system's. This module stays so
//! existing `crate::wordid::encode` call sites keep resolving.

pub use wordid::{SEP, encode};
