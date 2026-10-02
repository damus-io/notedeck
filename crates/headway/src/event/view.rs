//! The view model the reducer resolves a board into: [`BoardView`] and its
//! columns, cards, subissues, edges, comments and activity rows.

use nostrdb_net::NoteId;

use super::model::{Date, Field, Priority, ReviewFields, ReviewLocation, column_is_terminal};

/// A comment on a card, resolved off its issue. Comments are append-only (no
/// latest-wins overlay), so this is simply the parsed event in render form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentView {
    pub id: NoteId,
    pub author: [u8; 32],
    /// The parent comment for a threaded reply; `None` for a top-level comment.
    /// Stored for forward-compatibility — comments currently render flat.
    pub parent: Option<NoteId>,
    pub body: String,
    pub created_at: u64,
}

/// A review record on a card, resolved off its issue: who recorded which commit
/// and where (see [`ReviewFields`]). Append-only like [`CommentView`], so this is
/// the parsed event in render form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewView {
    pub id: NoteId,
    pub author: [u8; 32],
    pub created_at: u64,
    pub fields: ReviewFields,
    /// The inline review comments on this record's commit, oldest first. They
    /// thread under the record, so they never show in the card's
    /// [`comments`](CardView::comments).
    pub comments: Vec<ReviewCommentView>,
}

/// An inline review comment on a record's commit
/// ([`ReviewCommentEvent`](super::ReviewCommentEvent) in render form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewCommentView {
    pub id: NoteId,
    pub author: [u8; 32],
    /// The parent review comment for a threaded reply; `None` for a top-level
    /// one.
    pub parent: Option<NoteId>,
    /// The lines it points at; `None` for a comment on the commit as a whole.
    pub location: Option<ReviewLocation>,
    pub body: String,
    pub created_at: u64,
}

/// One entry of a card's derived activity timeline: who did what, when. Folded
/// from the card's full event history (the superseded placements, subject
/// edits, label sets, cover notes and relations the latest-wins overlays would
/// otherwise discard), so it needs no storage of its own — every row is just a
/// reading of an event that already exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityView {
    pub author: [u8; 32],
    pub created_at: u64,
    pub kind: ActivityKind,
}

/// What an [`ActivityView`] row says happened. Column-bearing variants carry
/// the display *name* (resolved against the rendered board) plus the column's
/// index for status-icon rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    /// The card was created (the kind-1621 issue event itself).
    Created,
    /// The card moved between columns. `from` is `None` when the previous
    /// placement isn't a real column on this board (e.g. a lost event).
    Moved {
        from: Option<String>,
        to: String,
        /// Index of `to` on the rendered board, for the status circle.
        to_idx: Option<usize>,
    },
    /// The card was archived off the board.
    Archived,
    /// The card came back from the archived (or deleted) sentinel.
    Restored { to: String, to_idx: Option<usize> },
    /// The card's title was edited.
    Renamed { to: String },
    /// The card's description (cover note) was edited.
    DescriptionEdited,
    /// The card's label set changed; either side may be empty (but not both).
    LabelsChanged {
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// A scalar [`Field`] changed to the wire value `to` (empty = cleared).
    FieldChanged { field: Field, to: String },
    /// The card was made a subissue of `parent` (title resolved when known).
    ParentSet {
        parent: NoteId,
        title: Option<String>,
    },
    /// The card was detached from its parent.
    ParentRemoved,
    /// A review record was added: `commit` (full sha) recorded on `host`, either
    /// absent when the record didn't carry it. See [`ReviewView`].
    Review {
        commit: Option<String>,
        host: Option<String>,
    },
}

/// The rollup half of headway's one definition of done: a card with at least one
/// live (non-archived) subissue is done when every one of those is done. Takes
/// the doneness of the live subissues only, so callers filter archived ones out
/// first. An empty set is *not* done — a card with no live subissues is a leaf,
/// done only by where it sits, so a plain card or a freshly cut epic never
/// counts as finished.
///
/// The reducer applies it recursively while resolving [`SubissueView::done`] and
/// [`EdgeRef::done`], and [`BoardView::card_is_done`] applies it over those
/// already-rolled-up subissues, so a finished epic is never handed out as work
/// and never holds a blocked card back — while staying in whatever column it sits.
pub fn subissues_all_done(live_done: impl IntoIterator<Item = bool>) -> bool {
    let mut any = false;
    for done in live_done {
        if !done {
            return false;
        }
        any = true;
    }
    any
}

/// A direct subissue of a card, resolved for display on its parent. Doneness is
/// derived — from where the child sits on its board(s), or from its own
/// subissues all being done ([`subissues_all_done`]) — never a stored checkbox
/// (see `crates/notedeck_headway/docs/subissues-design.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubissueView {
    pub id: NoteId,
    /// Resolved title (subject overlay applied).
    pub title: String,
    /// Column id of a live placement — the one on the board being rendered when
    /// there is one, else the first by board id for determinism. `None` when the
    /// child is unplaced or archived everywhere.
    pub column: Option<String>,
    /// Done = every live placement sits in a terminal column of its board, or the
    /// child is archived everywhere it's placed, or it has live subissues and
    /// every one of them is done (recursively, see [`subissues_all_done`]).
    pub done: bool,
    /// The child has no live placement but at least one archived one.
    pub archived: bool,
    /// Work-order rank of this child within its parent (fractional), `None` when
    /// unsequenced — sequenced children sort ahead of unsequenced ones, which
    /// keep their creation order. See the `birth-plate-alien` design.
    pub seq: Option<String>,
}

/// A dependency edge resolved for display: the other card's id, resolved title,
/// and whether it is *cleared*. For a card's `blocked_by` edges, `done` means the
/// blocker is done by the same rule as a subissue ([`SubissueView::done`]): in a
/// terminal column, archived everywhere, or every live subissue of it done. Symmetric for the reverse `blocks` edges,
/// where `done` reflects the blocked card. See [`CardView::blocked_by`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeRef {
    pub id: NoteId,
    pub title: String,
    /// The referenced card is cleared (done or archived), so it no longer holds
    /// work back.
    pub done: bool,
}

/// A card as rendered: a stable id plus its resolved fields.
#[derive(Debug, PartialEq, Eq)]
pub struct CardView {
    pub id: NoteId,
    /// The issue author. Needed to address comments at the card (NIP-22 root
    /// `P`) and to attribute the card itself.
    pub author: [u8; 32],
    pub title: String,
    pub description: String,
    pub labels: Vec<String>,
    /// Resolved priority (latest-authorised-wins overlay), [`Priority::None`]
    /// when the card was never prioritised.
    pub priority: Priority,
    /// Resolved due date, `None` when unset (or cleared). See [`Field::Due`].
    pub due: Option<Date>,
    /// Resolved estimate (arbitrary points), `None` when unset. See
    /// [`Field::Estimate`].
    pub estimate: Option<u32>,
    /// Fractional rank within its column; cards are sorted ascending.
    pub rank: String,
    /// Cross-cutting work-order rank within the board root (fractional, sorted
    /// ascending), `None` when the card was never sequenced. Independent of
    /// [`CardView::rank`] — that is the within-column spatial order, this is the
    /// board-wide "what to work on next" order. See the `birth-plate-alien` design.
    pub seq: Option<String>,
    /// `created_at` of the winning placement (0 if the card is unplaced). A
    /// re-placement (move/delete/archive) must stamp a strictly-greater
    /// timestamp so it wins latest-wins even within the same wall-clock second.
    pub placed_at: u64,
    /// `created_at` of the issue event — when the card was created. The issue
    /// is immutable, so this never moves.
    pub created_at: u64,
    /// When the card's content last changed: the newest authorised amendment
    /// (title, description or label edit), comment or review record, falling back to
    /// `created_at` if the card was never touched. Placements are board-scoped
    /// and tracked by `placed_at` instead, keeping this board-agnostic — the
    /// same issue shows the same `updated_at` on every board it's placed on.
    pub updated_at: u64,
    /// Comments on the card, oldest first (sorted by `created_at`, then id).
    pub comments: Vec<CommentView>,
    /// Authorised review records on the card, **newest first** (sorted by
    /// `created_at` descending, then id) — the first is the latest commit.
    /// Records with fully identical [`ReviewFields`](super::model::ReviewFields)
    /// are collapsed to the newest one, whoever wrote them: a retried done step
    /// re-records the same commit, and two authors only produce identical
    /// fields by recording the same commit from the same host and path. A
    /// record that differs in any field (a new explainer, another host) is kept.
    pub reviews: Vec<ReviewView>,
    /// The card's derived activity timeline (created / moved / renamed / …),
    /// oldest first. See [`ActivityView`]; comments are kept separately above
    /// and interleaved by the renderer.
    pub activity: Vec<ActivityView>,
    /// The parent card when this one is a subissue (authorised relation slot).
    pub parent: Option<NoteId>,
    /// Direct subissues in work-order: sequenced children first (by `seq` rank),
    /// then unsequenced ones by `(created_at, id)`. See [`SubissueView::seq`].
    pub subissues: Vec<SubissueView>,
    /// Cards this one is *blocked by* — its dependency edges (authorised
    /// [`BlockerSet`](super::BlockerSet)). Orthogonal to [`parent`](CardView::parent): a card can be
    /// both a subissue and blocked. Each edge carries the blocker's cleared state,
    /// so [`is_blocked`](CardView::is_blocked) needs no further lookup.
    pub blocked_by: Vec<EdgeRef>,
    /// The reverse edges: cards that name this one as a blocker. Only those the
    /// reducer has folded (same or, cross-board, another folded board) appear.
    pub blocks: Vec<EdgeRef>,
    /// Cards this one is *related to* — the undirected, semantics-free "see also"
    /// relation ([`RelatedSet`](super::RelatedSet)). Symmetric: the reducer unions this card's own
    /// stored set with every set that names it, so both endpoints list each other.
    /// Purely informational — never consulted by [`is_blocked`](Self::is_blocked),
    /// the ready set, sequence rank or any rollup. Each edge carries the partner's
    /// cleared state for display only. See [`KIND_RELATED`](super::KIND_RELATED).
    pub related: Vec<EdgeRef>,
}

impl CardView {
    /// Is this card held back by at least one unfinished blocker? The write path
    /// keeps the [`BlockerSet`](super::BlockerSet) free of cycles, and the reducer resolves each
    /// blocker's doneness, so this is a pure read over [`blocked_by`](Self::blocked_by).
    pub fn is_blocked(&self) -> bool {
        self.blocked_by.iter().any(|b| !b.done)
    }

    /// Does this card have at least one live subissue, all of them done? Such a
    /// card is done wherever it sits ([`subissues_all_done`]); a card with no live
    /// subissues is a leaf and this is `false`.
    pub fn subissues_done(&self) -> bool {
        subissues_all_done(
            self.subissues
                .iter()
                .filter(|s| !s.archived)
                .map(|s| s.done),
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ColumnView {
    pub id: String,
    pub name: String,
    /// Terminal ("done") column — see [`ColumnDef::terminal`](super::ColumnDef::terminal) and
    /// [`column_is_terminal`]. Carried through from the board definition so the
    /// folded view can decide doneness without the reducer.
    pub terminal: bool,
    pub cards: Vec<CardView>,
}

/// An archived card plus the column it was archived from, for the archived view
/// and restore. `from` is `None` if the card was archived before origin
/// tracking existed, or its origin column has since been forgotten.
#[derive(Debug, PartialEq, Eq)]
pub struct ArchivedCard {
    pub card: CardView,
    pub from: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BoardView {
    pub id: String,
    pub author: [u8; 32],
    pub title: String,
    pub description: String,
    /// `created_at` of the winning board event. Republishing an addressable
    /// board edit must carry a strictly-greater timestamp so the latest version
    /// wins; same-second nostr timestamps would otherwise tie (see
    /// `store::republish_board`).
    pub created_at: u64,
    pub columns: Vec<ColumnView>,
    /// Cards archived off this board, with their origin column for restore.
    /// Sorted deterministically by card id.
    pub archived: Vec<ArchivedCard>,
}

impl BoardView {
    /// Find a live (non-archived) card on the board by id. A shared read helper
    /// so lookups over the folded view — e.g. [`crate::traversal`]'s DFS — don't
    /// each re-open the columns-then-cards scan.
    pub fn card(&self, id: NoteId) -> Option<&CardView> {
        self.columns
            .iter()
            .flat_map(|c| c.cards.iter())
            .find(|c| c.id == id)
    }

    /// Whether the column `col_id` is terminal on this board. Thin adapter over
    /// [`column_is_terminal`] using the board's own column order and flags.
    pub fn column_is_terminal(&self, col_id: &str) -> bool {
        column_is_terminal(
            self.columns.iter().map(|c| (c.id.as_str(), c.terminal)),
            col_id,
        )
    }

    /// Whether card `id` is done — the folded-view analogue of
    /// [`SubissueView::done`]: it sits in a terminal ("done") column of this
    /// board, or it has live subissues and every one is done
    /// ([`CardView::subissues_done`]; each subissue's `done` is already rolled up
    /// by the reducer, so this is recursive). The card keeps its column either
    /// way. `false` when the card is archived or off-board (not in a live
    /// column). Shared by [`crate::traversal`] and [`crate::graph`] so doneness
    /// is decided one way.
    pub fn card_is_done(&self, id: NoteId) -> bool {
        self.columns
            .iter()
            .find_map(|col| col.cards.iter().find(|c| c.id == id).map(|c| (col, c)))
            .is_some_and(|(col, card)| self.column_is_terminal(&col.id) || card.subissues_done())
    }
}
