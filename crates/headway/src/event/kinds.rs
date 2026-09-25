//! The headway event kinds: the `KIND_*` numbers, the tag namespaces and fixed
//! `d` values they share, the [`HEADWAY_KINDS`] roster and the addressable-range
//! classifier [`is_addressable`].

/// Headway board: addressable, `d` = board id, holds title/description and the
/// ordered column list.
pub const KIND_BOARD: u32 = 30619;

/// NIP-34 issue == a card.
pub const KIND_ISSUE: u32 = 1621;

/// NIP-32 label event. Carries both after-the-fact labels (`#t`) and subject
/// edits (`#subject`), distinguished by the `L` namespace.
pub const KIND_LABEL: u32 = 1985;

/// gitworkshop cover note == an editable card description.
pub const KIND_COVER_NOTE: u32 = 1624;

/// Headway card placement: addressable, `d` = `<board-id>:<issue-id>`, records
/// the card's column and fractional rank.
pub const KIND_PLACEMENT: u32 = 30620;

/// NIP-22 generic comment == a comment on a card. gitworkshop/ngit comment on
/// NIP-34 issues the same way (kind 1111, *not* kind-1 replies).
pub const KIND_COMMENT: u32 = 1111;

/// Headway card relation: addressable, `d` = child issue id, `parent` names the
/// parent issue. Child-side, so each child has exactly one parent slot —
/// re-parenting republishes the slot and a relation with no `parent` tag
/// detaches. See `crates/notedeck_headway/docs/subissues-design.md`.
pub const KIND_RELATION: u32 = 30621;

/// Headway card sequence: addressable, `d` = `<container>:<issue-id>`, records a
/// fractional `rank` positioning the card within a [`Container`](super::Container) (board root or
/// parent card). The cross-cutting work-order axis — orthogonal to the column
/// `rank` on [`KIND_PLACEMENT`] — resolved latest-authorised-wins. See the
/// `birth-plate-alien` card design.
pub const KIND_SEQUENCE: u32 = 30622;

/// Headway per-account board-selection preference: addressable (parameterized
/// replaceable), `d` = [`BOARD_PREF_D`] so there's exactly one per author,
/// content = the last-selected board slug. Written PNS-wrapped
/// ([`crate::store::save_board_pref`]) and never synced — it's the local
/// replacement for the old `headway-boards.json`, read latest-wins by
/// [`load_board_pref`](super::load_board_pref).
pub const KIND_BOARD_PREF: u32 = 30623;

/// The fixed `d` tag on every [`KIND_BOARD_PREF`] note: one preference slot per
/// account, superseded latest-wins on each board switch.
pub(super) const BOARD_PREF_D: &str = "selected-board";

/// Headway card blockers: addressable, `d` = the blocked issue id, carrying zero
/// or more `blocked-by` tags each naming a blocker's issue event id. The set is a
/// snapshot (like labels), so the newest authorised event is the card's complete
/// blocker set — republishing without a blocker removes it. A directed
/// dependency edge distinct from the parent/subissue axis ([`KIND_RELATION`]): a
/// card may have both. The blockers reference event ids, so they may point at
/// cards on other boards. See `headway:headway/goat-couple-flush`.
pub const KIND_BLOCKERS: u32 = 30624;

/// Headway card related-to relations: addressable, `d` = one endpoint card's id,
/// carrying zero or more `related` tags each naming another card's event id. The
/// set is a snapshot (like [`KIND_BLOCKERS`]), so the newest authorised event is
/// that endpoint's complete related set — republishing without an id removes it.
///
/// The *undirected, semantics-free* sibling of the blocking edges ([`KIND_BLOCKERS`])
/// and the parent axis ([`KIND_RELATION`]): "A relates to B" is Linear's "Relates"
/// — purely informational "see also," never a prerequisite (that's blocking), a
/// decomposition (that's a subissue), or a work-order (that's a sequence). Because
/// it is symmetric, the edge is stored on *one* endpoint and rendered on both: the
/// reducer unions each card's own set with every set that names it (see
/// [`BoardReducer::resolve_card`]). It never feeds the ready set, sequence rank or
/// any rollup — context only. The related ids reference event ids, so they may
/// point at cards on other boards. See `headway:headway/obscure-demand-actor`.
pub const KIND_RELATED: u32 = 30625;

pub(super) const NS_SUBJECT: &str = "#subject";

pub(super) const NS_TAG: &str = "#t";

/// Every kind headway cares about, for querying / subscribing.
pub const HEADWAY_KINDS: [u32; 10] = [
    KIND_BOARD,
    KIND_ISSUE,
    KIND_PLACEMENT,
    KIND_LABEL,
    KIND_COVER_NOTE,
    KIND_COMMENT,
    KIND_RELATION,
    KIND_SEQUENCE,
    KIND_BLOCKERS,
    KIND_RELATED,
];

/// Whether `kind` is one of headway's addressable (latest-wins, keyed per
/// `(kind, d-tag)`) kinds — every 30000-range kind headway publishes (board,
/// placement, relation, sequence, blockers, related), as opposed to the
/// immutable regular events (issue, label, cover, comment).
///
/// Used by the CLI's relay sync to push only the *winning* revision of each
/// addressable coordinate rather than every stale one the append-only cache
/// still holds (see `nostrdb_net::relay::sync::frames_where`). The local cache
/// keeps every revision; a relay holds only the latest and rejects the rest as
/// `replaced: have newer event`, so pushing stale revisions never converges and
/// re-flushes on every run.
///
/// Range-based on purpose: NIP-01 defines 30000–39999 as addressable, so any
/// addressable kind added to [`HEADWAY_KINDS`] later is covered automatically —
/// a narrower per-kind list is exactly what silently re-broke this each time a
/// new addressable kind landed.
pub fn is_addressable(kind: u32) -> bool {
    (30_000..40_000).contains(&kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The relay-sync dedup keys off [`is_addressable`]: an addressable kind is
    /// deduped to its winning revision before the push, an immutable one is passed
    /// through as-is. Getting this wrong is the recurring "CLI re-flushes
    /// superseded edits every run" bug — a two-kind hardcode silently re-broke it
    /// each time a new addressable kind (relation, sequence, blockers, related)
    /// landed. Pin the contract against the real kind roster so a newly-added kind
    /// can't slip through classified as immutable.
    #[test]
    fn is_addressable_covers_every_addressable_headway_kind() {
        // Every 30000-range kind (parameterized-replaceable, NIP-01) is
        // addressable; the immutable regular events are not.
        for kind in HEADWAY_KINDS {
            assert_eq!(
                is_addressable(kind),
                (30_000..40_000).contains(&kind),
                "kind {kind} is classified against the wrong side of the addressable range"
            );
        }

        // Spot-check the two classes explicitly so the intent is legible even if
        // the roster changes.
        for addressable in [
            KIND_BOARD,
            KIND_PLACEMENT,
            KIND_RELATION,
            KIND_SEQUENCE,
            KIND_BLOCKERS,
            KIND_RELATED,
        ] {
            assert!(is_addressable(addressable), "{addressable} is addressable");
        }
        for immutable in [KIND_ISSUE, KIND_LABEL, KIND_COVER_NOTE, KIND_COMMENT] {
            assert!(!is_addressable(immutable), "{immutable} is immutable");
        }
    }
}
