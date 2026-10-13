//! Storage stats: how many bytes each kind's notes take in nostrdb, tallied
//! in the dashboard's single fold from each note's stored size.
//!
//! nostrdb keeps every version of a replaceable note, not just the newest, so
//! for replaceable and addressable kinds the tally also counts distinct
//! addresses `(kind, pubkey, d)`: notes per address is how many versions pile
//! up behind each one.

use std::hash::{Hash, Hasher};

use nostrdb::Note;
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};

/// All-time totals for one kind's stored notes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct KindStorage {
    /// Notes of this kind.
    pub count: u64,
    /// Their summed stored size (`Note::size`), in bytes.
    pub bytes: u64,
    /// Distinct replaceable addresses among them; 0 for regular kinds.
    pub addresses: u64,
}

impl KindStorage {
    /// Mean stored size of one note.
    pub fn avg_bytes(&self) -> u64 {
        self.bytes.checked_div(self.count).unwrap_or(0)
    }

    /// Mean versions stored per replaceable address, if the kind has any.
    pub fn versions_per_address(&self) -> Option<f64> {
        (self.addresses > 0).then(|| self.count as f64 / self.addresses as f64)
    }
}

/// One replaceable address: kind, author and a hash of the `d` tag (empty
/// for plain replaceable kinds). A hash rather than the string keeps the set
/// allocation-free per note; a collision would undercount one address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct AddressKey {
    kind: u32,
    pubkey: [u8; 32],
    d: u64,
}

/// One row of the storage card: a kind and its totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StorageRow {
    pub kind: u32,
    pub storage: KindStorage,
}

/// NIP-01 replaceable (0, 3, 10000–19999) or addressable (30000–39999):
/// kinds where a newer note supersedes the older ones at its address.
fn is_replaceable(kind: u32) -> bool {
    kind == 0 || kind == 3 || (10_000..20_000).contains(&kind) || (30_000..40_000).contains(&kind)
}

fn d_tag<'a>(note: &Note<'a>) -> Option<&'a str> {
    note.tags()
        .into_iter()
        .find(|tag| tag.count() >= 2 && tag.get_str(0) == Some("d"))
        .and_then(|tag| tag.get_str(1))
}

/// Add one note to the per-kind tally, recording its address in `addresses`
/// when its kind is replaceable.
#[inline(always)]
pub(crate) fn bump(
    kinds: &mut FxHashMap<u32, KindStorage>,
    addresses: &mut FxHashSet<AddressKey>,
    note: &Note<'_>,
) {
    let kind = note.kind();
    let entry = kinds.entry(kind).or_default();
    entry.count += 1;
    entry.bytes += note.size() as u64;

    if !is_replaceable(kind) {
        return;
    }

    let mut hasher = FxHasher::default();
    if (30_000..40_000).contains(&kind) {
        d_tag(note).unwrap_or("").hash(&mut hasher);
    }
    let key = AddressKey {
        kind,
        pubkey: *note.pubkey(),
        d: hasher.finish(),
    };
    if addresses.insert(key) {
        entry.addresses += 1;
    }
}

/// Refill `rows` with `kinds`, biggest first, ties broken by kind.
pub(crate) fn rank_into(kinds: &FxHashMap<u32, KindStorage>, rows: &mut Vec<StorageRow>) {
    rows.clear();
    rows.extend(kinds.iter().map(|(kind, storage)| StorageRow {
        kind: *kind,
        storage: *storage,
    }));
    rows.sort_unstable_by(|a, b| {
        b.storage
            .bytes
            .cmp(&a.storage.bytes)
            .then_with(|| a.kind.cmp(&b.kind))
    });
}

/// Every kind's totals summed into one.
pub(crate) fn total(kinds: &FxHashMap<u32, KindStorage>) -> KindStorage {
    kinds
        .values()
        .fold(KindStorage::default(), |acc, k| KindStorage {
            count: acc.count + k.count,
            bytes: acc.bytes + k.bytes,
            addresses: acc.addresses + k.addresses,
        })
}

/// A name for the kinds worth naming: Dave's own, and the common nostr ones.
pub(crate) fn kind_name(kind: u32) -> Option<&'static str> {
    Some(match kind {
        0 => "profile",
        1 => "text note",
        3 => "contacts",
        4 => "DM",
        5 => "deletion",
        6 => "repost",
        7 => "reaction",
        1059 => "gift wrap",
        1080 => "PNS envelope",
        1988 => "Dave conversation",
        1989 => "Dave source data",
        9734 => "zap request",
        9735 => "zap",
        10002 => "relay list",
        30023 => "long-form",
        31988 => "Dave session state",
        31989 => "Dave commands",
        31990 => "Dave inspector details",
        31991 => "Dave run configs",
        _ => return None,
    })
}

/// Bytes for humans, base 2: `512 B`, `12.1 KiB`, `118 MiB`, `11.6 GiB`.
pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 100.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_picks_the_unit() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(12_345), "12.1 KiB");
        assert_eq!(format_bytes(123_456_789), "118 MiB");
        assert_eq!(format_bytes(12 * 1024 * 1024 * 1024), "12.0 GiB");
    }

    #[test]
    fn replaceable_ranges() {
        for kind in [0, 3, 10_002, 31_988, 39_999] {
            assert!(is_replaceable(kind), "{kind}");
        }
        for kind in [1, 7, 1988, 1080, 20_000, 40_000] {
            assert!(!is_replaceable(kind), "{kind}");
        }
    }

    #[test]
    fn rank_is_biggest_first() {
        let mut kinds = FxHashMap::default();
        let of = |bytes| KindStorage {
            count: 1,
            bytes,
            addresses: 0,
        };
        kinds.insert(1, of(10));
        kinds.insert(1988, of(500));
        kinds.insert(7, of(10));
        let mut rows = Vec::new();
        rank_into(&kinds, &mut rows);
        let order: Vec<u32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(order, vec![1988, 1, 7]);
    }
}
