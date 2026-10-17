//! One arena for every string a ledger holds.
//!
//! Payees, codes, metadata keys and values and record ids are interned into
//! a single `String` buffer and addressed by a [`StrId`], so a transaction
//! holds four-byte ids instead of owning heap strings, and each distinct
//! string is stored once however many transactions repeat it.

use std::hash::BuildHasher;

use hashbrown::{DefaultHashBuilder, HashTable};

/// The id of a string interned in a [`StrPool`].
///
/// Ids are dense and assigned in the order strings are first interned, so
/// interning the same strings in the same order always yields the same ids.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StrId(u32);

impl StrId {
    /// The id's position in its pool, `0` for the first string interned.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Where one interned string sits in the pool's buffer.
#[derive(Copy, Clone, Debug)]
struct StrSpan {
    start: u32,
    len: u32,
}

/// An append-only string interner backed by one contiguous buffer.
#[derive(Debug, Default)]
pub struct StrPool {
    /// Every interned string, back to back.
    buf: String,
    /// The span of each string in `buf`, indexed by [`StrId`].
    spans: Vec<StrSpan>,
    /// Ids keyed by the hash of their string, for deduplication.
    index: HashTable<StrId>,
    hasher: DefaultHashBuilder,
}

impl StrPool {
    /// An empty pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// The id of `s`, interning it first if the pool has not seen it.
    ///
    /// # Panics
    ///
    /// If the pool would exceed 4 GiB of text or `u32::MAX` strings.
    pub fn intern(&mut self, s: &str) -> StrId {
        let hash = self.hasher.hash_one(s);
        let Self {
            buf,
            spans,
            index,
            hasher,
        } = self;
        if let Some(id) = index.find(hash, |id| span_str(buf, spans[id.index()]) == s) {
            return *id;
        }

        let id = StrId(u32::try_from(spans.len()).expect("StrPool holds at most u32::MAX strings"));
        let start = u32::try_from(buf.len()).expect("StrPool holds at most 4 GiB of text");
        let len = u32::try_from(s.len()).expect("an interned string is under 4 GiB");
        start
            .checked_add(len)
            .expect("StrPool holds at most 4 GiB of text");
        buf.push_str(s);
        spans.push(StrSpan { start, len });
        index.insert_unique(hash, id, |id| {
            hasher.hash_one(span_str(buf, spans[id.index()]))
        });
        id
    }

    /// The id of `s` if it has been interned, without interning it.
    pub fn lookup(&self, s: &str) -> Option<StrId> {
        let hash = self.hasher.hash_one(s);
        self.index.find(hash, |id| self.get(*id) == s).copied()
    }

    /// The string `id` names.
    ///
    /// # Panics
    ///
    /// If `id` came from a different pool and is out of range for this one.
    pub fn get(&self, id: StrId) -> &str {
        span_str(&self.buf, self.spans[id.index()])
    }

    /// How many distinct strings the pool holds.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Whether the pool holds no strings.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Every id in the pool, in the order the strings were first interned.
    pub fn ids(&self) -> impl Iterator<Item = StrId> + use<> {
        (0..self.spans.len() as u32).map(StrId)
    }

    /// The total bytes of interned text, not counting bookkeeping.
    pub fn text_bytes(&self) -> usize {
        self.buf.len()
    }
}

/// The text of `span` in `buf`.
fn span_str(buf: &str, span: StrSpan) -> &str {
    let start = span.start as usize;
    &buf[start..start + span.len as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_dedupes_and_round_trips() {
        let mut pool = StrPool::new();
        let a = pool.intern("coffee");
        let b = pool.intern("tea");
        let empty = pool.intern("");
        assert_eq!(pool.intern("coffee"), a);
        assert_ne!(a, b);
        assert_eq!(pool.get(a), "coffee");
        assert_eq!(pool.get(b), "tea");
        assert_eq!(pool.get(empty), "");
        assert_eq!(pool.len(), 3);
        assert_eq!(pool.text_bytes(), "coffeetea".len());
        assert_eq!(pool.lookup("tea"), Some(b));
        assert_eq!(pool.lookup("water"), None);
    }

    #[test]
    fn ids_follow_first_interning_order() {
        let mut pool = StrPool::new();
        let ids: Vec<StrId> = ["x", "y", "x", "z"]
            .iter()
            .map(|s| pool.intern(s))
            .collect();
        let indices: Vec<usize> = ids.iter().map(|id| id.index()).collect();
        assert_eq!(indices, [0, 1, 0, 2]);
    }

    #[test]
    fn survives_many_strings_and_rehashing() {
        let mut pool = StrPool::new();
        let ids: Vec<StrId> = (0..10_000).map(|i| pool.intern(&format!("s{i}"))).collect();
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(pool.get(*id), format!("s{i}"));
            assert_eq!(pool.lookup(&format!("s{i}")), Some(*id));
        }
    }
}
