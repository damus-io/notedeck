//! Nostr event model for headway boards.
//!
//! Cards are NIP-34 issues (kind 1621) anchored to a headway *board* (a custom
//! addressable kind). Because the issue event is immutable, everything mutable
//! about a card — its title, labels, description and which column it sits in —
//! lives in *separate* events:
//!
//! | concept           | kind    | mechanism                                  |
//! | ----------------- | ------- | ------------------------------------------ |
//! | board             | `30619` | addressable; `d` = board id, ordered `col` |
//! | card              | `1621`  | NIP-34 issue, `a` → board                  |
//! | title edit        | `1985`  | NIP-32 label, `L`/`l` namespace `#subject` |
//! | labels            | `1985`  | NIP-32 label, `L`/`l` namespace `#t`       |
//! | description edit  | `1624`  | gitworkshop cover note                     |
//! | placement         | `30620` | addressable; `col` + fractional `rank`     |
//! | relation          | `30621` | addressable; `d` = child, `parent` tag     |
//! | sequence          | `30622` | addressable; `d` = `<container>:<issue>`   |
//! | blockers          | `30624` | addressable; `d` = blocked, `blocked-by` tags |
//! | related           | `30625` | addressable; `d` = a card, `related` tags |
//!
//! Effective state is resolved as **latest-authorised-wins** for every overlay
//! (placement, subject, cover note, and labels — each label event carries the
//! card's complete set, so the newest one wins), where "authorised"
//! means the event's author is the card author or the board's author
//! (maintainer). This mirrors the ngitstack/gitworkshop "Shared Issue / Patch /
//! PR Metadata" spec.
//!
//! This module is pure: it builds and parses notes and reduces a set of them
//! into a [`BoardView`]. Relay/ndb plumbing lives in the app layer.

mod build;
mod json;
mod kinds;
mod load;
mod model;
mod parse;
mod reduce;
mod resolve;
mod view;

pub use build::{
    build_archive_placement, build_blockers, build_board, build_board_pref, build_comment,
    build_cover_note, build_field, build_issue, build_labels, build_placement, build_related,
    build_relation, build_sequence, build_subject_edit,
};
pub use json::{activity_json, board_json, card_json, comment_json};
pub use kinds::{
    HEADWAY_KINDS, KIND_BLOCKERS, KIND_BOARD, KIND_BOARD_PREF, KIND_COMMENT, KIND_COVER_NOTE,
    KIND_ISSUE, KIND_LABEL, KIND_PLACEMENT, KIND_RELATED, KIND_RELATION, KIND_SEQUENCE,
    is_addressable,
};
pub(crate) use load::team_sealed;
pub use load::{
    board_pref_created_at, board_scoped_filters, card_meta_filter, comment_filter,
    current_blockers, current_related, fold_board, fold_shared_board, headway_filter, load_board,
    load_board_pref, load_shared_board, reduce_delta,
};
pub use model::{
    BoardCoord, COL_ARCHIVED, COL_DELETED, ColumnDef, Date, Field, Priority, board_address,
    column_is_terminal,
};
pub use parse::{
    BlockerSet, BoardEvent, CommentEvent, Container, CoverNote, FieldEdit, HeadwayEvent,
    IssueEvent, LabelSet, PlacementEvent, RelatedSet, RelationEvent, SequenceEvent, SubjectEdit,
    parse,
};
pub use reduce::{BoardReducer, reduce};
pub use resolve::{
    ColumnPos, LocatedCard, ResolvedCard, all_cards, card_in_board, card_with_column_in_board,
    find_board, locate_card, locate_card_in_boards, pick_board, pick_card, pick_card_with_column,
    resolve_card, resolve_card_by_wordid,
};
pub use view::{
    ActivityKind, ActivityView, ArchivedCard, BoardView, CardView, ColumnView, CommentView,
    EdgeRef, SubissueView,
};

// ---------------------------------------------------------------------------
// Fractional ranking
// ---------------------------------------------------------------------------

/// Smallest rank digit value below `'a'` and above `'z'` used as open bounds.
const RANK_LOW: u8 = b'a' - 1;

const RANK_HIGH: u8 = b'z' + 1;

/// Produce a rank string that sorts strictly between `left` and `right` (each an
/// optional existing rank). `None` means "open" — i.e. `rank_between(None, None)`
/// is the first rank, `rank_between(Some(last), None)` appends after `last`, and
/// `rank_between(None, Some(first))` prepends before `first`.
///
/// Ranks are lowercase `a`–`z` strings compared lexicographically. Appending and
/// inserting-between are unbounded (ranks just grow in length), but prepending
/// repeatedly walks toward `"a"` and nothing sorts before `"a"`; exhausting the
/// low end requires a rank rebalance (future work). New boards seed from the
/// midpoint to keep headroom on both sides.
pub fn rank_between(left: Option<&str>, right: Option<&str>) -> String {
    let l = left.unwrap_or("").as_bytes();
    let r = right.unwrap_or("").as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    let mut right_open = false;

    loop {
        let lc = l.get(i).copied().unwrap_or(RANK_LOW);
        let rc = if right_open {
            RANK_HIGH
        } else {
            r.get(i).copied().unwrap_or(RANK_HIGH)
        };

        let mid = (lc + rc) / 2;
        if mid != lc {
            out.push(mid);
            return String::from_utf8(out).expect("ascii rank");
        }

        // lc and rc are adjacent (or equal): keep this digit and descend. Once
        // we've committed a digit equal to lc while rc == lc + 1, every deeper
        // digit is already < right, so the right bound is released.
        out.push(if lc == RANK_LOW { b'a' } else { lc });
        if rc == lc + 1 {
            right_open = true;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_between_appends_in_increasing_order() {
        let mut last = rank_between(None, None);
        for _ in 0..50 {
            let next = rank_between(Some(&last), None);
            assert!(next > last, "{next:?} should be > {last:?}");
            last = next;
        }
    }

    #[test]
    fn rank_between_prepends_in_decreasing_order() {
        // Prepending repeatedly walks toward the "a" floor; a few levels are
        // always available (a real rebalance is needed to go below "a", which
        // is tracked as future work — see `rank_between` docs).
        let mut first = rank_between(None, None);
        for _ in 0..3 {
            let prev = rank_between(None, Some(&first));
            assert!(prev < first, "{prev:?} should be < {first:?}");
            assert!(prev.bytes().all(|b| b.is_ascii_lowercase()));
            first = prev;
        }
    }

    #[test]
    fn rank_between_inserts_strictly_between() {
        let a = rank_between(None, None);
        let b = rank_between(Some(&a), None);
        for _ in 0..50 {
            let mid = rank_between(Some(&a), Some(&b));
            assert!(
                mid > a && mid < b,
                "{mid:?} not strictly between {a:?},{b:?}"
            );
        }
        // Adjacent ranks still admit an in-between value by growing length.
        let lo = "m".to_string();
        let hi = "n".to_string();
        let mid = rank_between(Some(&lo), Some(&hi));
        assert!(mid > lo && mid < hi, "{mid:?} not between {lo:?},{hi:?}");
    }
}
