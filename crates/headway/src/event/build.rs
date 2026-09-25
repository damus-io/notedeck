//! Builders: one [`NoteBuilder`] per headway event kind, the write-side inverse
//! of [`parse`](fn@super::parse).

use nostrdb::{NoteBuildOptions, NoteBuilder};
use nostrdb_net::{NoteId, Pubkey};

use super::kinds::{
    BOARD_PREF_D, KIND_BLOCKERS, KIND_BOARD, KIND_BOARD_PREF, KIND_COMMENT, KIND_COVER_NOTE,
    KIND_ISSUE, KIND_LABEL, KIND_PLACEMENT, KIND_RELATED, KIND_RELATION, KIND_SEQUENCE, NS_SUBJECT,
    NS_TAG,
};
use super::model::{COL_ARCHIVED, ColumnDef, Field};
use super::parse::Container;

fn base<'a>(kind: u32, content: &'a str) -> NoteBuilder<'a> {
    NoteBuilder::new()
        .content(content)
        .kind(kind)
        .options(NoteBuildOptions::default())
}

/// Build a board event (kind 30619) with its ordered columns.
pub fn build_board<'a>(
    board_id: &str,
    title: &str,
    description: &str,
    columns: &[ColumnDef],
) -> NoteBuilder<'a> {
    let mut b = base(KIND_BOARD, "")
        .start_tag()
        .tag_str("d")
        .tag_str(board_id)
        .start_tag()
        .tag_str("title")
        .tag_str(title);

    if !description.is_empty() {
        b = b.start_tag().tag_str("description").tag_str(description);
    }

    for col in columns {
        b = b
            .start_tag()
            .tag_str("col")
            .tag_str(&col.id)
            .tag_str(&col.name);
        // A terminal column carries a trailing "terminal" marker; a plain column
        // omits it, so old readers ignore the extra element and old boards parse
        // as all-non-terminal (falling back to last-column doneness).
        if col.terminal {
            b = b.tag_str("terminal");
        }
    }

    b
}

/// Build the per-account board-selection preference note (kind 30623): a
/// parameterized-replaceable note whose content is the selected board's
/// coordinate ([`BoardCoord::coordinate`](super::BoardCoord::coordinate)) and whose fixed `d` ([`BOARD_PREF_D`])
/// makes each save supersede the last. The caller signs it with the account key
/// and PNS-wraps it — see [`crate::store::save_board_pref`], which owns the
/// `coordinate` string this borrows.
pub fn build_board_pref(coordinate: &str) -> NoteBuilder<'_> {
    base(KIND_BOARD_PREF, coordinate)
        .start_tag()
        .tag_str("d")
        .tag_str(BOARD_PREF_D)
}

/// Build a card (NIP-34 issue, kind 1621) anchored to `board_addr`. The body is
/// the event content; `subject` is the initial title.
pub fn build_issue<'a>(board_addr: &str, subject: &str, body: &'a str) -> NoteBuilder<'a> {
    base(KIND_ISSUE, body)
        .start_tag()
        .tag_str("a")
        .tag_str(board_addr)
        .start_tag()
        .tag_str("subject")
        .tag_str(subject)
}

/// Build a placement event (kind 30620) assigning `issue` to `col` at `rank`.
pub fn build_placement<'a>(
    board_id: &str,
    board_addr: &str,
    issue: &NoteId,
    col: &str,
    rank: &str,
) -> NoteBuilder<'a> {
    base(KIND_PLACEMENT, "")
        .start_tag()
        .tag_str("d")
        .tag_str(&format!("{board_id}:{}", issue.hex()))
        .start_tag()
        .tag_str("a")
        .tag_str(board_addr)
        .start_tag()
        .tag_str("e")
        .tag_id(issue.bytes())
        .start_tag()
        .tag_str("col")
        .tag_str(col)
        .start_tag()
        .tag_str("rank")
        .tag_str(rank)
}

/// Build an *archive* placement for `issue`: a placement into the
/// [`COL_ARCHIVED`] sentinel that also records `from_col`, the column the card
/// is being archived from, so a later restore can put it back where it was.
/// `rank` is preserved (reuse the card's current rank) so restore keeps its slot.
pub fn build_archive_placement<'a>(
    board_id: &str,
    board_addr: &str,
    issue: &NoteId,
    from_col: &str,
    rank: &str,
) -> NoteBuilder<'a> {
    build_placement(board_id, board_addr, issue, COL_ARCHIVED, rank)
        .start_tag()
        .tag_str("from")
        .tag_str(from_col)
}

/// Build a subject (title) edit for `issue` (NIP-32 label, `#subject`).
pub fn build_subject_edit<'a>(issue: &NoteId, subject: &str) -> NoteBuilder<'a> {
    base(KIND_LABEL, "")
        .start_tag()
        .tag_str("e")
        .tag_id(issue.bytes())
        .start_tag()
        .tag_str("L")
        .tag_str(NS_SUBJECT)
        .start_tag()
        .tag_str("l")
        .tag_str(subject)
        .tag_str(NS_SUBJECT)
}

/// Build a scalar [`Field`] edit for `issue` (NIP-32 label in the field's `L`
/// namespace, carrying one `l` value). Republishing supersedes it
/// latest-authorised-wins, so an empty (or, for priority, `"none"`) `value`
/// clears the field. The value is the field's wire form — e.g. `Priority::as_str`,
/// a `Date`'s `YYYY-MM-DD`, or an estimate's decimal.
pub fn build_field<'a>(issue: &NoteId, field: Field, value: &str) -> NoteBuilder<'a> {
    let ns = field.namespace();
    base(KIND_LABEL, "")
        .start_tag()
        .tag_str("e")
        .tag_id(issue.bytes())
        .start_tag()
        .tag_str("L")
        .tag_str(ns)
        .start_tag()
        .tag_str("l")
        .tag_str(value)
        .tag_str(ns)
}

/// Build a label event for `issue` (NIP-32, `#t` namespace), one `l` per label.
///
/// Generic over the label string type so both `&[&str]` (e.g. seed literals) and
/// `&[String]` callers work without an intermediate allocation.
pub fn build_labels<'a, S: AsRef<str>>(issue: &NoteId, labels: &[S]) -> NoteBuilder<'a> {
    let mut b = base(KIND_LABEL, "")
        .start_tag()
        .tag_str("e")
        .tag_id(issue.bytes())
        .start_tag()
        .tag_str("L")
        .tag_str(NS_TAG);

    for label in labels {
        b = b
            .start_tag()
            .tag_str("l")
            .tag_str(label.as_ref())
            .tag_str(NS_TAG);
    }

    b
}

/// Build a relation event (kind 30621) making `child` a subissue of `parent`,
/// or detaching it when `parent` is `None`. Addressable on the child, so the
/// newest authorised relation is the child's one parent slot.
pub fn build_relation<'a>(child: &NoteId, parent: Option<&NoteId>) -> NoteBuilder<'a> {
    let mut b = base(KIND_RELATION, "")
        .start_tag()
        .tag_str("d")
        .tag_str(&child.hex())
        .start_tag()
        .tag_str("e")
        .tag_id(child.bytes());

    if let Some(parent) = parent {
        b = b.start_tag().tag_str("parent").tag_id(parent.bytes());
    }

    b
}

/// Build a blockers event (kind 30624) recording the complete set of cards
/// `blocked` is blocked by, one `blocked-by` tag per blocker. Addressable on the
/// blocked card (`d` = its id), so the newest authorised set wins — passing an
/// empty `blockers` clears every dependency. The `e` tag mirrors `d` so the
/// card-anchored fan-out ([`card_meta_filter`](super::card_meta_filter)) can reach it by the blocked card.
pub fn build_blockers<'a>(blocked: &NoteId, blockers: &[NoteId]) -> NoteBuilder<'a> {
    let mut b = base(KIND_BLOCKERS, "")
        .start_tag()
        .tag_str("d")
        .tag_str(&blocked.hex())
        .start_tag()
        .tag_str("e")
        .tag_id(blocked.bytes());

    for blocker in blockers {
        b = b.start_tag().tag_str("blocked-by").tag_id(blocker.bytes());
    }

    b
}

/// Build a related-to event (kind 30625) recording the complete set of cards
/// `card` is related to, one `related` tag per partner. Addressable on `card`
/// (`d` = its id), so the newest authorised set wins — passing an empty `related`
/// clears every relation on this endpoint. The `e` tag mirrors `d` so the
/// card-anchored fan-out ([`card_meta_filter`](super::card_meta_filter)) can reach it by `card`. The
/// relation is symmetric, so it is stored on one endpoint only and the reducer
/// renders it on both.
pub fn build_related<'a>(card: &NoteId, related: &[NoteId]) -> NoteBuilder<'a> {
    let mut b = base(KIND_RELATED, "")
        .start_tag()
        .tag_str("d")
        .tag_str(&card.hex())
        .start_tag()
        .tag_str("e")
        .tag_id(card.bytes());

    for other in related {
        b = b.start_tag().tag_str("related").tag_id(other.bytes());
    }

    b
}

/// Build a sequence event (kind 30622) positioning `issue` at fractional `rank`
/// within `container`. Addressable by `d = <container>:<issue-id>` so republishing
/// supersedes the previous position latest-authorised-wins. `rank` comes from
/// [`rank_between`](super::rank_between), the same kernel that ranks cards within a column.
pub fn build_sequence<'a>(container: &Container, issue: &NoteId, rank: &str) -> NoteBuilder<'a> {
    base(KIND_SEQUENCE, "")
        .start_tag()
        .tag_str("d")
        .tag_str(&format!("{}:{}", container.wire(), issue.hex()))
        .start_tag()
        .tag_str("e")
        .tag_id(issue.bytes())
        .start_tag()
        .tag_str("rank")
        .tag_str(rank)
}

/// Build a cover note (kind 1624) — the editable card description for `issue`.
pub fn build_cover_note<'a>(issue: &NoteId, author: &Pubkey, body: &'a str) -> NoteBuilder<'a> {
    base(KIND_COVER_NOTE, body)
        .start_tag()
        .tag_str("e")
        .tag_id(issue.bytes())
        .start_tag()
        .tag_str("p")
        .tag_id(author.bytes())
        .start_tag()
        .tag_str("k")
        .tag_str(&KIND_ISSUE.to_string())
}

/// Build a NIP-22 comment (kind 1111) on `issue` (authored by `issue_author`).
///
/// The thread **root** (uppercase `E`/`K`/`P`) is always the issue, carried on
/// every comment — including replies — so the reducer can attach a comment to its
/// card directly without walking the reply chain. The **parent** (lowercase
/// `e`/`k`/`p`) is the issue itself for a top-level comment, or `reply_to`
/// (another kind-1111 comment, with its author) for a threaded reply. This
/// matches how gitworkshop/ngit comment on NIP-34 issues.
pub fn build_comment<'a>(
    issue: &NoteId,
    issue_author: &Pubkey,
    reply_to: Option<(&NoteId, &Pubkey)>,
    body: &'a str,
) -> NoteBuilder<'a> {
    // Root scope: the issue. The `E` event tag carries the issue author in its
    // 4th element (relay hint left empty in slot 3), per NIP-22.
    let b = base(KIND_COMMENT, body)
        .start_tag()
        .tag_str("E")
        .tag_id(issue.bytes())
        .tag_str("")
        .tag_id(issue_author.bytes())
        .start_tag()
        .tag_str("K")
        .tag_str(&KIND_ISSUE.to_string())
        .start_tag()
        .tag_str("P")
        .tag_id(issue_author.bytes());

    // Parent: the comment being replied to, or the issue itself for a top-level
    // comment. `k` is what distinguishes the two (1111 vs 1621).
    let (parent_id, parent_author, parent_kind) = match reply_to {
        Some((cid, cauthor)) => (cid, cauthor, KIND_COMMENT),
        None => (issue, issue_author, KIND_ISSUE),
    };
    b.start_tag()
        .tag_str("e")
        .tag_id(parent_id.bytes())
        .tag_str("")
        .tag_id(parent_author.bytes())
        .start_tag()
        .tag_str("k")
        .tag_str(&parent_kind.to_string())
        .start_tag()
        .tag_str("p")
        .tag_id(parent_author.bytes())
}
