//! The nostr kinds cashflow stores its vault notes under.
//!
//! cashflow owns the addressable range **30700–30709**. Before it was taken,
//! `git grep -nw '307[0-9][0-9]'` found no other user in the workspace; the
//! neighbours are headway (30619–30626), the app-data kind 30078, notebook
//! (31606–31610) and agentium (31988–31991).
//!
//! Every kind here is parameterized-replaceable: one note per record, newest
//! revision per `d` tag wins. A record's `d` is always `<prefix>:<id>`, never
//! a bare id, because nostrdb stores a 64-hex tag value as a 32-byte id and a
//! string lookup then silently misses it (see [`crate::vault::Record::PREFIX`]).
//!
//! These notes never reach a relay in the clear: each is the rumor inside a
//! kind-1081 SNS envelope of the account's vault (see [`crate::vault`]).
//!
//! [`SETTING`], [`RULE`], [`OVERRIDE`], [`SPLIT`] and [`TRANSFER`] have
//! writers so far. The rest are allocated here,
//! with the card that builds each, so two lanes never pick the same number.

use std::ops::RangeInclusive;

/// The range cashflow owns.
pub const RANGE: RangeInclusive<u32> = 30700..=30709;

/// One app setting per note, `d = set:<name>`, its value the tag
/// `["value", …]` (see [`crate::settings`]).
pub const SETTING: u32 = 30700;

/// A categorization rule, `d = rule:<id>`
/// (headway:cashflow/wrestle-depart-cash).
pub const RULE: u32 = 30701;

/// A per-transaction account override, `d = ov:<record-id>`
/// (headway:cashflow/name-draw-animal).
pub const OVERRIDE: u32 = 30702;

/// A per-transaction split, `d = split:<record-id>`
/// (headway:cashflow/name-draw-animal).
pub const SPLIT: u32 = 30703;

/// A link between the two legs of a transfer, `d = xfer:<a>|<b>` with the
/// record ids sorted (headway:cashflow/name-draw-animal).
pub const TRANSFER: u32 = 30704;

/// A journal segment's manifest, `d = seg:<sha256>`
/// (headway:cashflow/occur-then-item).
pub const SEGMENT: u32 = 30705;

/// Chart-of-accounts metadata for one account, `d = acct:<account>`
/// (headway:cashflow/dream-accident-reveal).
pub const ACCOUNT: u32 = 30706;

/// Every allocated kind. 30707–30709 are still free.
pub const ALL: [u32; 7] = [SETTING, RULE, OVERRIDE, SPLIT, TRANSFER, SEGMENT, ACCOUNT];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocated_kinds_are_distinct_addressable_and_in_range() {
        for (i, kind) in ALL.iter().enumerate() {
            assert!(RANGE.contains(kind), "{kind} is outside {RANGE:?}");
            assert!((30000..40000).contains(kind), "{kind} isn't addressable");
            assert!(!ALL[..i].contains(kind), "{kind} is allocated twice");
        }
    }
}
