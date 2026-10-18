//! Parsed events: the typed form of each headway event kind, the
//! [`HeadwayEvent`] sum over them, and [`parse`], which reads a [`Note`] into
//! one. Round-trip tests for every builder live here too.

use nostrdb::Note;
use nostrdb_net::NoteId;

use super::kinds::{
    KIND_BLOCKERS, KIND_BOARD, KIND_COMMENT, KIND_COVER_NOTE, KIND_ISSUE, KIND_LABEL,
    KIND_PLACEMENT, KIND_RELATED, KIND_RELATION, KIND_REVIEW, KIND_REVIEW_SOURCE, KIND_SEQUENCE,
    NS_SUBJECT, NS_TAG,
};
use super::model::{
    BoardCoord, ColumnDef, Field, LineSide, ReviewFields, ReviewLocation, ReviewSourceRole,
};
use crate::query::{DisplayProps, ViewSpec};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardEvent {
    pub id: String,
    pub author: [u8; 32],
    pub title: String,
    pub description: String,
    pub columns: Vec<ColumnDef>,
    /// Whether this revision tombstones the board (a bare `["deleted"]` tag). A
    /// deleted board drops out of board listings but keeps its cards, and a later
    /// revision without the tag restores it — see
    /// [`build_board_def`](super::build_board_def).
    pub deleted: bool,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueEvent {
    pub id: [u8; 32],
    pub author: [u8; 32],
    /// The board this card belongs to, as `(author, board_id)` from the `a` tag.
    pub board_author: [u8; 32],
    pub board_id: String,
    pub subject: String,
    pub body: String,
    /// Inline `t` labels on the issue itself.
    pub inline_labels: Vec<String>,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlacementEvent {
    pub author: [u8; 32],
    /// The board this placement targets, as `(author, board_id)` from the `a`
    /// tag. Membership is placement-driven: a card shows on whichever board(s)
    /// it has a live placement for, so the same issue can be placed on several
    /// boards at once (each with its own column and rank).
    pub board_author: [u8; 32],
    pub board_id: String,
    pub issue_id: [u8; 32],
    pub col: String,
    pub rank: String,
    /// The column the card was archived *from*, present only on archive
    /// placements (`col == COL_ARCHIVED`). Lets a restore put the card back
    /// where it was rather than reflowing it to the first column.
    pub from: Option<String>,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SubjectEdit {
    pub author: [u8; 32],
    pub issue_id: [u8; 32],
    pub subject: String,
    pub created_at: u64,
}

/// A resolved scalar [`Field`] overlay event: which field, and its wire value
/// (the field's typed value is parsed from `value` at the read site).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FieldEdit {
    pub author: [u8; 32],
    pub issue_id: [u8; 32],
    pub field: Field,
    pub value: String,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LabelSet {
    pub author: [u8; 32],
    pub issue_id: [u8; 32],
    pub labels: Vec<String>,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CoverNote {
    pub author: [u8; 32],
    pub issue_id: [u8; 32],
    pub body: String,
    pub created_at: u64,
}

/// The scope a [`SequenceEvent`] ranks a card within: a card is sequenced among
/// the siblings of one container. The container is the *only* varying part of the
/// ordering — the same fractional-rank kernel ([`rank_between`](super::rank_between)) positions a card
/// within a column (today's [`PlacementEvent::rank`]), within a board's top level,
/// or within a parent card. v1 carries the latter two; the `<type>:` wire prefix
/// leaves room for future grouping containers (milestone/cycle/project) with no
/// wire change. See the `birth-plate-alien` card design.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Container {
    /// A board's top level: orders the board's cards across columns. Carries the
    /// board id (its slug).
    BoardRoot(String),
    /// A parent card: orders that card's subissues. Carries the parent issue id.
    Card([u8; 32]),
}

impl Container {
    /// The wire form used as the leading segments of a sequence event's `d` tag:
    /// `board:<board-id>` or `card:<parent-hex>`. Neither a board slug nor a hex
    /// id contains `:`, so it round-trips through [`Container::parse`].
    pub fn wire(&self) -> String {
        match self {
            Container::BoardRoot(id) => format!("board:{id}"),
            Container::Card(id) => format!("card:{}", NoteId::new(*id).hex()),
        }
    }

    /// Parse the container portion of a `d` tag (everything before the trailing
    /// `:<issue-hex>`). `None` for an unknown type or a malformed id.
    pub fn parse(s: &str) -> Option<Container> {
        let (kind, id) = s.split_once(':')?;
        match kind {
            "board" => Some(Container::BoardRoot(id.to_string())),
            "card" => Some(Container::Card(*NoteId::from_hex(id).ok()?.bytes())),
            _ => None,
        }
    }
}

/// A card's fractional position within a [`Container`] — the cross-cutting
/// work-order rank. Addressable overlay (kind 30622), latest-authorised-wins.
/// `rank` is a [`rank_between`](super::rank_between) string, compared lexicographically; absent
/// (never published) means the card is unsequenced and falls back to creation
/// order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SequenceEvent {
    pub author: [u8; 32],
    pub container: Container,
    pub issue_id: [u8; 32],
    pub rank: String,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelationEvent {
    pub author: [u8; 32],
    /// The subissue this relation is about (the addressable `d` slot).
    pub child_id: [u8; 32],
    /// The parent issue, or `None` for a detach (relation republished without a
    /// `parent` tag).
    pub parent_id: Option<[u8; 32]>,
    pub created_at: u64,
}

/// A card's dependency set: the cards it is *blocked by*. The addressable slot is
/// keyed on the blocked card (`blocked_id`), and `blockers` is the complete set
/// (snapshot semantics like [`LabelSet`]). See [`build_blockers`](super::build_blockers).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockerSet {
    pub author: [u8; 32],
    /// The card these blockers hold back (the addressable `d` slot).
    pub blocked_id: [u8; 32],
    /// The blocker card ids, in the order the event listed them.
    pub blockers: Vec<[u8; 32]>,
    pub created_at: u64,
}

/// A card's related-to set: the cards it is *related to* on this endpoint. The
/// addressable slot is keyed on the storing card (`card_id`), and `related` is the
/// complete set (snapshot semantics like [`BlockerSet`]). The relation is
/// undirected, so the reducer unions this with the reverse edges (sets naming
/// `card_id`) when rendering. See [`build_related`](super::build_related).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelatedSet {
    pub author: [u8; 32],
    /// The card storing this set (the addressable `d` slot / one endpoint).
    pub card_id: [u8; 32],
    /// The related card ids, in the order the event listed them.
    pub related: Vec<[u8; 32]>,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentEvent {
    pub id: [u8; 32],
    pub author: [u8; 32],
    /// The issue (kind 1621) this comment threads under — the NIP-22 root `E`.
    pub issue_id: [u8; 32],
    /// The parent *comment* when this is a threaded reply (lowercase `e` with
    /// `k` == 1111); `None` for a top-level comment, whose parent is the issue.
    pub parent_id: Option<[u8; 32]>,
    pub body: String,
    pub created_at: u64,
}

/// An inline review comment (kind 1111 rooted on a kind-1626 review record):
/// a comment on lines of the record's commit, in gitworkshop's inline-comment
/// shape. It threads under the record, not the card, and never shows in the
/// card's own comments. See [`build_review_comment`](super::build_review_comment).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewCommentEvent {
    pub id: [u8; 32],
    pub author: [u8; 32],
    /// The review record this comment threads under — the NIP-22 root `E`.
    pub record_id: [u8; 32],
    /// The parent review comment for a threaded reply; `None` for a top-level
    /// comment, whose parent is the record.
    pub parent_id: Option<[u8; 32]>,
    /// The lines it points at; `None` for a comment on the commit as a whole.
    pub location: Option<ReviewLocation>,
    pub body: String,
    pub created_at: u64,
}

/// A review record (kind 1626) on a card: the structured commit/host/explainer
/// metadata an agent records when it finishes the card. Append-only like
/// [`CommentEvent`]. See [`build_review`](super::build_review).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReviewEvent {
    pub id: [u8; 32],
    pub author: [u8; 32],
    /// The card (issue, kind 1621) this record is about — the `e` tag.
    pub issue_id: [u8; 32],
    pub fields: ReviewFields,
    pub created_at: u64,
}

/// A review source (kind 1627): one named text file belonging to a review
/// record — an explainer's Markdown source or its code walk. See
/// [`build_review_source`](super::build_review_source).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSourceEvent {
    pub id: [u8; 32],
    pub author: [u8; 32],
    /// The card (issue, kind 1621) — the `e` tag.
    pub issue_id: [u8; 32],
    /// The review record (kind 1626) this file belongs to — the `review` tag.
    pub record_id: [u8; 32],
    /// What the file is for — the `role` tag.
    pub role: ReviewSourceRole,
    /// The file name, e.g. `juice-road-castle.md`.
    pub name: String,
    /// The file's text — the event content.
    pub text: String,
    pub created_at: u64,
}

/// A parsed headway event of any of the recognised kinds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadwayEvent {
    Board(BoardEvent),
    Issue(IssueEvent),
    Placement(PlacementEvent),
    Subject(SubjectEdit),
    Labels(LabelSet),
    Field(FieldEdit),
    Cover(CoverNote),
    Comment(CommentEvent),
    ReviewComment(ReviewCommentEvent),
    Relation(RelationEvent),
    Sequence(SequenceEvent),
    Blockers(BlockerSet),
    Related(RelatedSet),
    Review(ReviewEvent),
    ReviewSource(ReviewSourceEvent),
}

/// Parse a note into a [`HeadwayEvent`], or `None` if it isn't a recognised /
/// well-formed headway event.
pub fn parse(note: &Note) -> Option<HeadwayEvent> {
    match note.kind() {
        KIND_BOARD => parse_board(note).map(HeadwayEvent::Board),
        KIND_ISSUE => parse_issue(note).map(HeadwayEvent::Issue),
        KIND_PLACEMENT => parse_placement(note).map(HeadwayEvent::Placement),
        KIND_LABEL => parse_label(note),
        KIND_COVER_NOTE => parse_cover(note).map(HeadwayEvent::Cover),
        KIND_COMMENT => parse_comment(note),
        KIND_RELATION => parse_relation(note).map(HeadwayEvent::Relation),
        KIND_SEQUENCE => parse_sequence(note).map(HeadwayEvent::Sequence),
        KIND_BLOCKERS => parse_blockers(note).map(HeadwayEvent::Blockers),
        KIND_RELATED => parse_related(note).map(HeadwayEvent::Related),
        KIND_REVIEW => parse_review(note).map(HeadwayEvent::Review),
        KIND_REVIEW_SOURCE => parse_review_source(note).map(HeadwayEvent::ReviewSource),
        _ => None,
    }
}

/// The owner of the board a card's issue note (kind 1621) belongs to, from
/// its `a` tag: the one field of [`IssueEvent`] that says whose fold the
/// card lives in. Allocation-free, unlike [`parse`], so a caller can ask it
/// for many notes every frame. `None` for another kind or a malformed tag.
pub fn issue_board_author(note: &Note) -> Option<[u8; 32]> {
    if note.kind() != KIND_ISSUE {
        return None;
    }
    let addr = note
        .tags()
        .iter()
        .find(|tag| tag.get_str(0) == Some("a"))?
        .get_str(1)?;
    let mut parts = addr.splitn(3, ':');
    if parts.next()?.parse::<u32>().ok()? != KIND_BOARD {
        return None;
    }
    let mut owner = [0u8; 32];
    hex::decode_to_slice(parts.next()?, &mut owner).ok()?;
    // A coordinate with no slug isn't a board's.
    parts.next()?;
    Some(owner)
}

/// What one revision of a saved list view (kind 30626) holds: a live view's
/// spec, or the tombstone a delete wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewRevision {
    /// A live view.
    Live(ViewSpec),
    /// A deleted view (a `["deleted"]` tag): it shows nothing until a later
    /// revision revives the slug.
    Deleted,
}

/// Read a saved list view's tags (see [`build_view`](super::build_view)) back
/// into its spec. A missing tag is that field's default — a missing `show`
/// the default display set — and an unknown tag, grouping, ordering or
/// property name is skipped, so a newer writer's view still loads.
pub fn parse_view(note: &Note) -> ViewRevision {
    let mut spec = ViewSpec::default();
    let mut show = None;

    for tag in note.tags() {
        let value = tag.get_str(1);
        match tag.get_str(0) {
            Some("deleted") => return ViewRevision::Deleted,
            Some("name") => spec.name = value.unwrap_or_default().to_owned(),
            Some("q") => spec.query = value.unwrap_or_default().to_owned(),
            Some("group") => {
                if let Some(by) = value.and_then(|v| v.parse().ok()) {
                    spec.grouping.by = by;
                }
            }
            Some("subgroup") => {
                if let Some(by) = value.and_then(|v| v.parse().ok()) {
                    spec.grouping.sub = by;
                }
            }
            Some("empty") => spec.grouping.show_empty = true,
            Some("sort") => {
                if let Some(by) = value.and_then(|v| v.parse().ok()) {
                    spec.sorting.by = by;
                }
            }
            Some("desc") => spec.sorting.descending = true,
            Some("completed-by-recency") => spec.sorting.completed_by_recency = true,
            Some("show") => {
                let mut set = DisplayProps::NONE;
                for i in 1..tag.count() {
                    if let Some(prop) = tag.get_str(i).and_then(|v| v.parse().ok()) {
                        set.set(prop, true);
                    }
                }
                show = Some(set);
            }
            Some("hide-subissues") => spec.hide_subissues = true,
            Some("board") => spec.board = value.map(str::to_owned),
            _ => {}
        }
    }

    spec.display = show.unwrap_or_default();
    ViewRevision::Live(spec)
}

fn parse_board(note: &Note) -> Option<BoardEvent> {
    let mut id = None;
    let mut title = String::new();
    let mut description = String::new();
    let mut columns = Vec::new();
    let mut deleted = false;

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("d") => id = tag.get_str(1).map(|s| s.to_owned()),
            Some("deleted") => deleted = true,
            Some("title") => {
                if let Some(t) = tag.get_str(1) {
                    title = t.to_owned();
                }
            }
            Some("description") => {
                if let Some(d) = tag.get_str(1) {
                    description = d.to_owned();
                }
            }
            Some("col") => {
                if let (Some(cid), Some(name)) = (tag.get_str(1), tag.get_str(2)) {
                    let mut def = ColumnDef::new(cid, name);
                    def.terminal = tag.get_str(3) == Some("terminal");
                    columns.push(def);
                }
            }
            _ => {}
        }
    }

    Some(BoardEvent {
        id: id?,
        author: *note.pubkey(),
        title,
        description,
        columns,
        deleted,
        created_at: note.created_at(),
    })
}

fn parse_issue(note: &Note) -> Option<IssueEvent> {
    let mut subject = String::new();
    let mut board = None;
    let mut inline_labels = Vec::new();

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("a") => board = tag.get_str(1).and_then(BoardCoord::parse),
            Some("subject") => {
                if let Some(s) = tag.get_str(1) {
                    subject = s.to_owned();
                }
            }
            Some("t") => {
                if let Some(t) = tag.get_str(1) {
                    inline_labels.push(t.to_owned());
                }
            }
            _ => {}
        }
    }

    let board = board?;

    Some(IssueEvent {
        id: *note.id(),
        author: *note.pubkey(),
        board_author: board.owner,
        board_id: board.slug,
        subject,
        body: note.content().to_owned(),
        inline_labels,
        created_at: note.created_at(),
    })
}

fn parse_placement(note: &Note) -> Option<PlacementEvent> {
    let mut issue_id = None;
    let mut board = None;
    let mut col = None;
    let mut rank = None;
    let mut from = None;

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("e") => issue_id = tag.get_id(1).copied(),
            Some("a") => board = tag.get_str(1).and_then(BoardCoord::parse),
            Some("col") => col = tag.get_str(1).map(|s| s.to_owned()),
            Some("rank") => rank = tag.get_str(1).map(|s| s.to_owned()),
            Some("from") => from = tag.get_str(1).map(|s| s.to_owned()),
            _ => {}
        }
    }

    let board = board?;

    Some(PlacementEvent {
        author: *note.pubkey(),
        board_author: board.owner,
        board_id: board.slug,
        issue_id: issue_id?,
        col: col?,
        rank: rank?,
        from,
        created_at: note.created_at(),
    })
}

fn parse_label(note: &Note) -> Option<HeadwayEvent> {
    let mut issue_id = None;
    let mut namespace = None;
    let mut values: Vec<String> = Vec::new();

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("e") => issue_id = tag.get_id(1).copied(),
            Some("L") => namespace = tag.get_str(1).map(|s| s.to_owned()),
            Some("l") => {
                if let Some(v) = tag.get_str(1) {
                    values.push(v.to_owned());
                }
            }
            _ => {}
        }
    }

    let issue_id = issue_id?;
    let author = *note.pubkey();
    let created_at = note.created_at();

    match namespace.as_deref() {
        Some(NS_SUBJECT) => Some(HeadwayEvent::Subject(SubjectEdit {
            author,
            issue_id,
            subject: values.into_iter().next()?,
            created_at,
        })),
        Some(NS_TAG) => Some(HeadwayEvent::Labels(LabelSet {
            author,
            issue_id,
            labels: values,
            created_at,
        })),
        Some(ns) if Field::from_namespace(ns).is_some() => Some(HeadwayEvent::Field(FieldEdit {
            author,
            issue_id,
            field: Field::from_namespace(ns)?,
            value: values.into_iter().next().unwrap_or_default(),
            created_at,
        })),
        _ => None,
    }
}

fn parse_cover(note: &Note) -> Option<CoverNote> {
    let mut issue_id = None;
    for tag in note.tags() {
        if tag.get_str(0) == Some("e") {
            issue_id = tag.get_id(1).copied();
        }
    }

    Some(CoverNote {
        author: *note.pubkey(),
        issue_id: issue_id?,
        body: note.content().to_owned(),
        created_at: note.created_at(),
    })
}

/// Parse a NIP-22 comment (kind 1111). The root is the uppercase `E`, and the
/// uppercase `K` says what it is: a review record (1626) makes it an inline
/// [`ReviewCommentEvent`], anything else (the card's 1621, or no `K` at all) a
/// card [`CommentEvent`]. The parent is the lowercase `e`, and the lowercase `k`
/// tells us whether that parent is another comment (a threaded reply) or the
/// root itself (a top-level comment). See [`build_comment`](super::build_comment)
/// and [`build_review_comment`](super::build_review_comment).
fn parse_comment(note: &Note) -> Option<HeadwayEvent> {
    let mut root = None;
    let mut root_kind = None;
    let mut parent_e = None;
    let mut parent_kind = None;
    let mut path = None;
    let mut commit = None;
    let mut lines = None;

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("E") => root = tag.get_id(1).copied(),
            Some("K") => root_kind = tag.get_str(1).and_then(|k| k.parse::<u32>().ok()),
            Some("e") => parent_e = tag.get_id(1).copied(),
            Some("k") => parent_kind = tag.get_str(1).and_then(|k| k.parse::<u32>().ok()),
            Some("f") => path = tag.get_str(1).map(str::to_owned),
            // A 64-hex (SHA-256) sha packs into an id; re-hex it, as
            // `parse_review` does.
            Some("c") => {
                commit = tag
                    .get_str(1)
                    .map(str::to_owned)
                    .or_else(|| tag.get_id(1).map(hex::encode))
            }
            Some("line") => {
                let side = match tag.get_str(2) {
                    Some("del") => LineSide::Old,
                    _ => LineSide::New,
                };
                lines = tag
                    .get_str(1)
                    .and_then(ReviewLocation::parse_lines)
                    .map(|range| (range, side));
            }
            _ => {}
        }
    }

    // A reply names another comment as its parent (`k` == 1111); a top-level
    // comment's parent is the root itself, so it carries no parent comment.
    let parent_id = match (parent_kind, parent_e) {
        (Some(KIND_COMMENT), Some(e)) => Some(e),
        _ => None,
    };
    let root = root?;

    if root_kind == Some(KIND_REVIEW) {
        // A location needs all three of its tags; with any missing the comment
        // is about the commit as a whole.
        let location = match (path, commit, lines) {
            (Some(path), Some(commit), Some((range, side))) => Some(ReviewLocation {
                path,
                commit,
                start: range.start,
                end: range.end,
                side,
            }),
            _ => None,
        };
        return Some(HeadwayEvent::ReviewComment(ReviewCommentEvent {
            id: *note.id(),
            author: *note.pubkey(),
            record_id: root,
            parent_id,
            location,
            body: note.content().to_owned(),
            created_at: note.created_at(),
        }));
    }

    Some(HeadwayEvent::Comment(CommentEvent {
        id: *note.id(),
        author: *note.pubkey(),
        issue_id: root,
        parent_id,
        body: note.content().to_owned(),
        created_at: note.created_at(),
    }))
}

/// Parse a review record (kind 1626). The card is the `e` tag (required); every
/// other known tag fills its [`ReviewFields`] slot and unknown tags are ignored.
/// See [`build_review`](super::build_review).
fn parse_review(note: &Note) -> Option<ReviewEvent> {
    let mut issue_id = None;
    let mut fields = ReviewFields::default();

    for tag in note.tags() {
        let Some(name) = tag.get_str(0) else {
            continue;
        };
        if name == "e" {
            issue_id = tag.get_id(1).copied();
            continue;
        }
        let Some(slot) = fields.slot_mut(name) else {
            continue;
        };
        // nostrdb packs a 64-hex tag value into a raw 32-byte id, which then
        // reads back only via `get_id`. A SHA-1 sha is 40-hex and stays a string,
        // but a SHA-256 repo's 64-hex sha would not — re-hex it so it survives.
        *slot = tag
            .get_str(1)
            .map(str::to_owned)
            .or_else(|| tag.get_id(1).map(hex::encode));
    }

    Some(ReviewEvent {
        id: *note.id(),
        author: *note.pubkey(),
        issue_id: issue_id?,
        fields,
        created_at: note.created_at(),
    })
}

/// Parse a review source (kind 1627). The card (`e`), the record (`review`),
/// a known `role` and a non-empty `name` are all required; a source missing
/// any of them can't be placed and is dropped — including one whose role a
/// newer writer added, which this version has no way to show. See
/// [`build_review_source`](super::build_review_source).
fn parse_review_source(note: &Note) -> Option<ReviewSourceEvent> {
    let mut issue_id = None;
    let mut record_id = None;
    let mut role = None;
    let mut name = None;

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("e") => issue_id = tag.get_id(1).copied(),
            Some("review") => record_id = tag.get_id(1).copied(),
            Some("role") => role = tag.get_str(1).and_then(ReviewSourceRole::parse),
            // Copied here: the borrow is the tag's, which ends with the loop.
            Some("name") => name = tag.get_str(1).filter(|n| !n.is_empty()).map(str::to_owned),
            _ => {}
        }
    }

    Some(ReviewSourceEvent {
        id: *note.id(),
        author: *note.pubkey(),
        issue_id: issue_id?,
        record_id: record_id?,
        role: role?,
        name: name?,
        text: note.content().to_owned(),
        created_at: note.created_at(),
    })
}

/// Parse a relation (kind 30621). The child is the `e` tag; a missing `parent`
/// tag is a detach, not a malformed event. See [`build_relation`](super::build_relation).
fn parse_relation(note: &Note) -> Option<RelationEvent> {
    let mut child_id = None;
    let mut parent_id = None;

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("e") => child_id = tag.get_id(1).copied(),
            Some("parent") => parent_id = tag.get_id(1).copied(),
            _ => {}
        }
    }

    Some(RelationEvent {
        author: *note.pubkey(),
        child_id: child_id?,
        parent_id,
        created_at: note.created_at(),
    })
}

/// Parse a blockers set (kind 30624). The blocked card is the `e` tag; each
/// `blocked-by` tag names one blocker's event id. A blockers event with no
/// `blocked-by` tags is a cleared set, not malformed. See [`build_blockers`](super::build_blockers).
pub(super) fn parse_blockers(note: &Note) -> Option<BlockerSet> {
    let mut blocked_id = None;
    let mut blockers = Vec::new();

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("e") => blocked_id = tag.get_id(1).copied(),
            // `blocked-by` values are 32-byte event ids, so they read via
            // `get_id`, not `get_str` (id tags are stored as raw bytes).
            Some("blocked-by") => {
                if let Some(id) = tag.get_id(1) {
                    blockers.push(*id);
                }
            }
            _ => {}
        }
    }

    Some(BlockerSet {
        author: *note.pubkey(),
        blocked_id: blocked_id?,
        blockers,
        created_at: note.created_at(),
    })
}

/// Parse a related-to set (kind 30625). The storing endpoint is the `e` tag; each
/// `related` tag names one partner's event id. A related event with no `related`
/// tags is a cleared set, not malformed. See [`build_related`](super::build_related).
pub(super) fn parse_related(note: &Note) -> Option<RelatedSet> {
    let mut card_id = None;
    let mut related = Vec::new();

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("e") => card_id = tag.get_id(1).copied(),
            // `related` values are 32-byte event ids, so they read via `get_id`,
            // not `get_str` (id tags are stored as raw bytes).
            Some("related") => {
                if let Some(id) = tag.get_id(1) {
                    related.push(*id);
                }
            }
            _ => {}
        }
    }

    Some(RelatedSet {
        author: *note.pubkey(),
        card_id: card_id?,
        related,
        created_at: note.created_at(),
    })
}

fn parse_sequence(note: &Note) -> Option<SequenceEvent> {
    let mut issue_id = None;
    let mut container = None;
    let mut rank = None;

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("e") => issue_id = tag.get_id(1).copied(),
            Some("d") => container = tag.get_str(1).and_then(container_from_d),
            Some("rank") => rank = tag.get_str(1).map(|s| s.to_owned()),
            _ => {}
        }
    }

    Some(SequenceEvent {
        author: *note.pubkey(),
        container: container?,
        issue_id: issue_id?,
        rank: rank?,
        created_at: note.created_at(),
    })
}

/// Split a sequence `d` tag (`<container>:<issue-hex>`) into its [`Container`],
/// peeling the trailing issue hex off the end. A container's own id (a board slug
/// or a parent hex) never contains `:`, so `rsplit_once` cleanly separates the
/// issue suffix from the container prefix.
fn container_from_d(d: &str) -> Option<Container> {
    let (container, _issue_hex) = d.rsplit_once(':')?;
    Container::parse(container)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nostrdb_net::FullKeypair;

    use nostrdb::NoteBuilder;

    use crate::event::build::{
        build_blockers, build_board, build_board_def, build_comment, build_cover_note, build_issue,
        build_labels, build_placement, build_related, build_relation, build_review,
        build_review_comment, build_review_source, build_sequence, build_subject_edit, build_view,
        build_view_tombstone,
    };
    use crate::event::model::{
        LineRange, MAX_REVIEW_SOURCE_BYTES, ReviewSource, ReviewSourceError, board_address,
    };
    use crate::query::{DisplayProp, GroupBy, Grouping, SortBy, Sorting};

    /// Sign `builder` with `kp` and parse the result back into a [`HeadwayEvent`].
    pub(crate) fn roundtrip(builder: NoteBuilder, kp: &FullKeypair) -> HeadwayEvent {
        let note = builder
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("build note");
        parse(&note).expect("parse headway event")
    }

    /// [`issue_board_author`] reads the same owner [`parse`] does, and
    /// nothing for a note that isn't an issue.
    #[test]
    fn issue_board_author_is_the_parsed_board_author() {
        let kp = FullKeypair::generate();
        let owner = FullKeypair::generate().pubkey;
        let sign = |builder: NoteBuilder| {
            builder
                .sign(&kp.secret_key.secret_bytes())
                .build()
                .expect("build note")
        };
        let issue = sign(build_issue(
            &board_address(&owner, "work"),
            "A card",
            "body",
        ));
        let Some(HeadwayEvent::Issue(parsed)) = parse(&issue) else {
            panic!("an issue");
        };
        assert_eq!(issue_board_author(&issue), Some(parsed.board_author));
        assert_eq!(issue_board_author(&issue), Some(*owner.bytes()));
        let board = sign(build_board("work", "Work", "", &[]));
        assert_eq!(issue_board_author(&board), None);
    }

    /// A saved view as written: its tags, and what [`parse_view`] reads back.
    struct ViewNote {
        tags: Vec<Vec<String>>,
        revision: ViewRevision,
    }

    /// Sign `builder` and read it back as a saved view.
    fn view_note(builder: NoteBuilder) -> ViewNote {
        let kp = FullKeypair::generate();
        let note = builder
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("build note");
        assert_eq!(note.kind(), super::super::kinds::KIND_VIEW);
        assert_eq!(note.content(), "");
        let tags = note
            .tags()
            .iter()
            .map(|tag| {
                (0..tag.count())
                    .map(|i| tag.get_str(i).unwrap_or_default().to_owned())
                    .collect()
            })
            .collect();
        ViewNote {
            tags,
            revision: parse_view(&note),
        }
    }

    /// A view with every field off its default.
    fn every_field_set() -> ViewSpec {
        ViewSpec {
            name: "Hot".to_string(),
            query: "p:high label:ui".to_string(),
            grouping: Grouping {
                by: GroupBy::Label,
                sub: GroupBy::Priority,
                show_empty: true,
            },
            sorting: Sorting {
                by: SortBy::WorkOrder,
                descending: true,
                completed_by_recency: true,
            },
            display: DisplayProps::of(&[DisplayProp::Id, DisplayProp::Board]),
            hide_subissues: true,
            board: Some("headway".to_string()),
        }
    }

    /// Owned tags from string literals, for comparing against a note's.
    fn tags(want: &[&[&str]]) -> Vec<Vec<String>> {
        want.iter()
            .map(|tag| tag.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    #[test]
    fn view_every_field_round_trips_through_tags() {
        let spec = every_field_set();
        let view = view_note(build_view("hot", &spec));
        assert_eq!(
            view.tags,
            tags(&[
                &["d", "hot"],
                &["name", "Hot"],
                &["q", "p:high label:ui"],
                &["group", "label"],
                &["subgroup", "priority"],
                &["empty"],
                &["sort", "work-order"],
                &["desc"],
                &["completed-by-recency"],
                &["show", "id", "board"],
                &["hide-subissues"],
                &["board", "headway"],
            ])
        );
        assert_eq!(view.revision, ViewRevision::Live(spec));
    }

    /// The default spec writes only `d`, `name`, `q` and `show`; an empty
    /// display set is still written, and reads back empty, not default.
    #[test]
    fn view_defaults_write_only_what_they_must() {
        let default = view_note(build_view("all", &ViewSpec::default()));
        let shown: Vec<&str> = DisplayProps::default().iter().map(|p| p.name()).collect();
        let mut show = vec!["show"];
        show.extend(shown);
        assert_eq!(
            default.tags,
            tags(&[&["d", "all"], &["name", ""], &["q", ""], &show])
        );
        assert_eq!(default.revision, ViewRevision::Live(ViewSpec::default()));

        let bare = ViewSpec {
            display: DisplayProps::NONE,
            ..ViewSpec::default()
        };
        let titles_only = view_note(build_view("bare", &bare));
        assert_eq!(titles_only.tags[3], ["show"]);
        assert_eq!(titles_only.revision, ViewRevision::Live(bare));
    }

    /// Unknown tags and names are skipped; a missing `show` is the default set.
    #[test]
    fn view_ignores_what_it_does_not_know() {
        let builder = NoteBuilder::new()
            .kind(super::super::kinds::KIND_VIEW)
            .content("")
            .start_tag()
            .tag_str("d")
            .tag_str("x")
            .start_tag()
            .tag_str("name")
            .tag_str("X")
            .start_tag()
            .tag_str("group")
            .tag_str("assignee")
            .start_tag()
            .tag_str("subgroup")
            .tag_str("status")
            .start_tag()
            .tag_str("sort")
            .tag_str("estimate")
            .start_tag()
            .tag_str("color")
            .tag_str("red");
        let view = view_note(builder);
        let want = ViewSpec {
            name: "X".to_string(),
            grouping: Grouping {
                sub: GroupBy::Status,
                ..Grouping::default()
            },
            ..ViewSpec::default()
        };
        assert_eq!(view.revision, ViewRevision::Live(want));

        let with_unknown_prop = NoteBuilder::new()
            .kind(super::super::kinds::KIND_VIEW)
            .content("")
            .start_tag()
            .tag_str("show")
            .tag_str("assignee")
            .tag_str("due");
        let ViewRevision::Live(spec) = view_note(with_unknown_prop).revision else {
            panic!("a live view");
        };
        assert_eq!(spec.display, DisplayProps::of(&[DisplayProp::Due]));
    }

    #[test]
    fn view_tombstone_is_a_bare_deleted_tag() {
        let tombstone = view_note(build_view_tombstone("hot"));
        assert_eq!(tombstone.tags, tags(&[&["d", "hot"], &["deleted"]]));
        assert_eq!(tombstone.revision, ViewRevision::Deleted);
    }

    pub(crate) fn note_id(kp: &FullKeypair, builder: NoteBuilder) -> NoteId {
        let note = builder
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("build note");
        NoteId::new(*note.id())
    }

    #[test]
    fn board_roundtrips() {
        let kp = FullKeypair::generate();
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];
        let ev = roundtrip(build_board("b1", "My Board", "a desc", &cols), &kp);

        let HeadwayEvent::Board(b) = ev else {
            panic!("expected board");
        };
        assert_eq!(b.id, "b1");
        assert_eq!(b.title, "My Board");
        assert_eq!(b.description, "a desc");
        assert_eq!(b.columns, cols);
        assert_eq!(b.author, *kp.pubkey.bytes());
        assert!(!b.deleted);
    }

    /// The deletion tombstone survives a round trip alongside the rest of the
    /// definition, which a restore republishes from.
    #[test]
    fn deleted_board_roundtrips() {
        let kp = FullKeypair::generate();
        let cols = vec![ColumnDef::new("todo", "Todo")];
        let ev = roundtrip(build_board_def("b1", "My Board", "", &cols, true), &kp);

        let HeadwayEvent::Board(b) = ev else {
            panic!("expected board");
        };
        assert!(b.deleted);
        assert_eq!(b.title, "My Board");
        assert_eq!(b.columns, cols);
    }

    #[test]
    fn issue_roundtrips_and_resolves_board() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let ev = roundtrip(build_issue(&addr, "Fix the thing", "body text"), &owner);

        let HeadwayEvent::Issue(i) = ev else {
            panic!("expected issue");
        };
        assert_eq!(i.subject, "Fix the thing");
        assert_eq!(i.body, "body text");
        assert_eq!(i.board_id, "b1");
        assert_eq!(i.board_author, *owner.pubkey.bytes());
    }

    #[test]
    fn placement_subject_labels_cover_roundtrip() {
        let kp = FullKeypair::generate();
        let issue = note_id(&kp, build_issue("30619:x:b1", "s", "b"));
        let addr = board_address(&kp.pubkey, "b1");

        let HeadwayEvent::Placement(p) =
            roundtrip(build_placement("b1", &addr, &issue, "todo", "m"), &kp)
        else {
            panic!("placement");
        };
        assert_eq!(p.issue_id, *issue.bytes());
        assert_eq!(p.col, "todo");
        assert_eq!(p.rank, "m");

        let HeadwayEvent::Subject(s) = roundtrip(build_subject_edit(&issue, "New title"), &kp)
        else {
            panic!("subject");
        };
        assert_eq!(s.subject, "New title");
        assert_eq!(s.issue_id, *issue.bytes());

        let labels = vec!["bug".to_string(), "p1".to_string()];
        let HeadwayEvent::Labels(l) = roundtrip(build_labels(&issue, &labels), &kp) else {
            panic!("labels");
        };
        assert_eq!(l.labels, labels);

        let HeadwayEvent::Cover(c) =
            roundtrip(build_cover_note(&issue, &kp.pubkey, "## hello"), &kp)
        else {
            panic!("cover");
        };
        assert_eq!(c.body, "## hello");
        assert_eq!(c.issue_id, *issue.bytes());
    }

    #[test]
    fn comment_roundtrips_top_level_and_reply() {
        let owner = FullKeypair::generate();
        let issue = note_id(&owner, build_issue("30619:x:b1", "s", "b"));

        // Top-level comment: parent is the issue, so no parent comment.
        let HeadwayEvent::Comment(top) =
            roundtrip(build_comment(&issue, &owner.pubkey, None, "first!"), &owner)
        else {
            panic!("comment");
        };
        assert_eq!(top.issue_id, *issue.bytes());
        assert_eq!(top.body, "first!");
        assert_eq!(top.parent_id, None);

        // Reply: parent is another comment (kind 1111), recorded as parent_id.
        let parent = NoteId::new(top.id);
        let HeadwayEvent::Comment(reply) = roundtrip(
            build_comment(
                &issue,
                &owner.pubkey,
                Some((&parent, &owner.pubkey)),
                "agreed",
            ),
            &owner,
        ) else {
            panic!("comment");
        };
        // Still rooted on the issue so the reducer can attach it directly…
        assert_eq!(reply.issue_id, *issue.bytes());
        // …but its parent is the comment it replies to.
        assert_eq!(reply.parent_id, Some(top.id));
    }

    /// An inline review comment carries gitworkshop's `f`/`c`/`line` tags, and
    /// its `K 1626` root makes it a review comment on the record, never a card
    /// comment. An old-side range keeps its `del`; a 64-hex (SHA-256) commit,
    /// which nostrdb packs into an id, reads back as hex.
    #[test]
    fn review_comment_roundtrips_location_and_roots_on_the_record() {
        let owner = FullKeypair::generate();
        let issue = note_id(&owner, build_issue("30619:x:b1", "s", "b"));
        let record = note_id(&owner, build_review(&issue, &full_review()));

        let new_side = ReviewLocation {
            path: "src/lib.rs".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            start: 42,
            end: 48,
            side: LineSide::New,
        };
        let HeadwayEvent::ReviewComment(top) = roundtrip(
            build_review_comment(&record, &owner.pubkey, None, Some(&new_side), "rename this"),
            &owner,
        ) else {
            panic!("review comment");
        };
        assert_eq!(top.record_id, *record.bytes());
        assert_eq!(top.parent_id, None);
        assert_eq!(top.location.as_ref(), Some(&new_side));
        assert_eq!(top.body, "rename this");

        let old_side = ReviewLocation {
            path: "old.rs".into(),
            commit: "ab".repeat(32),
            start: 3,
            end: 3,
            side: LineSide::Old,
        };
        let parent = NoteId::new(top.id);
        let HeadwayEvent::ReviewComment(reply) = roundtrip(
            build_review_comment(
                &record,
                &owner.pubkey,
                Some((&parent, &owner.pubkey)),
                Some(&old_side),
                "why delete?",
            ),
            &owner,
        ) else {
            panic!("review comment");
        };
        assert_eq!(reply.record_id, *record.bytes());
        assert_eq!(reply.parent_id, Some(top.id));
        assert_eq!(reply.location.as_ref(), Some(&old_side));

        // No location: a comment on the commit as a whole.
        let HeadwayEvent::ReviewComment(whole) = roundtrip(
            build_review_comment(&record, &owner.pubkey, None, None, "lgtm"),
            &owner,
        ) else {
            panic!("review comment");
        };
        assert_eq!(whole.location, None);
    }

    #[test]
    fn line_values_parse_single_ranges_and_reject_junk() {
        assert_eq!(
            ReviewLocation::parse_lines("42"),
            Some(LineRange { start: 42, end: 42 })
        );
        assert_eq!(
            ReviewLocation::parse_lines("42-48"),
            Some(LineRange { start: 42, end: 48 })
        );
        assert_eq!(
            ReviewLocation::parse_lines("48-42"),
            Some(LineRange { start: 42, end: 48 })
        );
        assert_eq!(ReviewLocation::parse_lines("0"), None);
        assert_eq!(ReviewLocation::parse_lines("a-b"), None);
        assert_eq!(ReviewLocation::parse_lines(""), None);
    }

    /// A review source round-trips its card, record, role, name and text — the
    /// Markdown's quotes and newlines intact — and a source without a role this
    /// version knows can't be placed, so it parses to nothing.
    #[test]
    fn review_source_roundtrips_and_needs_a_known_role() {
        let kp = FullKeypair::generate();
        let issue = note_id(&kp, build_issue("30619:x:b1", "s", "b"));
        let record = note_id(&kp, build_review(&issue, &full_review()));
        let source = ReviewSource {
            role: ReviewSourceRole::Code,
            name: "juice-road-castle.code.md".into(),
            text: "# Code\n\n```hunk src/a.rs @@ -1,2 +1,3 @@\nmatch: \"x\"\n```\n".into(),
        };

        let HeadwayEvent::ReviewSource(got) =
            roundtrip(build_review_source(&issue, &record, &source).unwrap(), &kp)
        else {
            panic!("review source");
        };
        assert_eq!(got.issue_id, *issue.bytes());
        assert_eq!(got.record_id, *record.bytes());
        assert_eq!(got.role, ReviewSourceRole::Code);
        assert_eq!(got.name, source.name);
        assert_eq!(got.text, source.text);

        let unknown_role = NoteBuilder::new()
            .kind(KIND_REVIEW_SOURCE)
            .content("x")
            .start_tag()
            .tag_str("e")
            .tag_id(issue.bytes())
            .start_tag()
            .tag_str("review")
            .tag_id(record.bytes())
            .start_tag()
            .tag_str("role")
            .tag_str("sidecar")
            .start_tag()
            .tag_str("name")
            .tag_str("a.json")
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .unwrap();
        assert_eq!(parse(&unknown_role), None);
    }

    /// The cap is on the wire form: text that fits raw but doubles when
    /// JSON-escaped is refused, as are names that are paths, and the error says
    /// which file. Nothing is ever truncated.
    #[test]
    fn review_source_refuses_paths_and_text_over_the_cap() {
        let issue = NoteId::new([1; 32]);
        let record = NoteId::new([2; 32]);
        let source = |name: &str, text: String| ReviewSource {
            role: ReviewSourceRole::Explainer,
            name: name.into(),
            text,
        };

        // Exactly at the cap, counting the two quotes the escaped form adds.
        let fits = source("a.md", "x".repeat(MAX_REVIEW_SOURCE_BYTES - 2));
        assert!(build_review_source(&issue, &record, &fits).is_ok());

        // Half the cap of quotes is the full cap once each is escaped.
        let quotes = source("a.md", "\"".repeat(MAX_REVIEW_SOURCE_BYTES / 2));
        assert_eq!(
            build_review_source(&issue, &record, &quotes).err(),
            Some(ReviewSourceError::TooLarge {
                name: "a.md".into(),
                bytes: MAX_REVIEW_SOURCE_BYTES + 2,
            })
        );

        for bad in ["", ".", "..", "docs/a.md", "a\\b.md"] {
            assert_eq!(
                build_review_source(&issue, &record, &source(bad, "x".into())).err(),
                Some(ReviewSourceError::BadName { name: bad.into() }),
                "{bad:?}"
            );
        }
    }

    /// A review record carrying every field, as `headway review` would write it.
    fn full_review() -> ReviewFields {
        ReviewFields {
            commit: Some("5ec55a6aaf57d4b9c1e0f7a2b3c4d5e6f7a8b9c0".into()),
            title: Some("headway: review-record event kind".into()),
            branch: Some("headway".into()),
            host: Some("jex0".into()),
            path: Some("/home/jb55/dev/notedeck-headway".into()),
            repo: Some("0123456789abcdef0123456789abcdef01234567".into()),
            agentium: Some("agentium:actress-mango-possible".into()),
            explainer: Some("https://claude.ai/artifact/abc".into()),
            deploy: Some("https://example.com/builds/5ec55a6aaf57/".into()),
            remote: Some("jex0:repos/notedeck".into()),
        }
    }

    #[test]
    fn review_roundtrips_every_field_and_skips_empty_ones() {
        let kp = FullKeypair::generate();
        let issue = note_id(&kp, build_issue("30619:x:b1", "s", "b"));

        let HeadwayEvent::Review(r) = roundtrip(build_review(&issue, &full_review()), &kp) else {
            panic!("review");
        };
        assert_eq!(r.issue_id, *issue.bytes());
        assert_eq!(r.fields, full_review());

        // Absent and empty fields both come back absent: no empty tags on the wire.
        let sparse = ReviewFields {
            commit: Some("abc".into()),
            branch: Some(String::new()),
            ..Default::default()
        };
        let HeadwayEvent::Review(r) = roundtrip(build_review(&issue, &sparse), &kp) else {
            panic!("review");
        };
        assert_eq!(
            r.fields,
            ReviewFields {
                commit: Some("abc".into()),
                ..Default::default()
            }
        );
    }

    /// The shas survive a real nostrdb ingest, not just an in-memory build:
    /// nostrdb packs 64-hex tag values into raw ids (the hex-tag trap), which a
    /// `get_str`-only parse would drop. A SHA-1 sha (40-hex) stays a string; a
    /// SHA-256 one (64-hex) is packed, and must still read back as hex.
    #[tokio::test]
    async fn review_shas_roundtrip_through_ndb() {
        use nostrdb::{Filter, IngestMetadata, Ndb, SubscriptionStream, Transaction};

        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &crate::test_config()).unwrap();
        let kp = FullKeypair::generate();
        let issue = note_id(&kp, build_issue("30619:x:b1", "s", "b"));

        let sha256 = "ab".repeat(32);
        let reviews = [
            full_review(),
            ReviewFields {
                commit: Some(sha256.clone()),
                repo: Some(sha256.clone()),
                ..Default::default()
            },
        ];

        // Subscribe before ingesting so the await can't miss the writer.
        let sub = ndb
            .subscribe(&[Filter::new().kinds([KIND_REVIEW as u64]).build()])
            .unwrap();
        let mut stream = SubscriptionStream::new(ndb.clone(), sub);
        for review in &reviews {
            let note = build_review(&issue, review)
                .sign(&kp.secret_key.secret_bytes())
                .build()
                .unwrap();
            let json = nostrdb_net::ClientMessage::event(&note)
                .unwrap()
                .to_json()
                .unwrap();
            ndb.process_event_with(&json, IngestMetadata::new().client(true))
                .unwrap();
        }

        let mut keys = Vec::new();
        while keys.len() < reviews.len() {
            keys.extend(
                stream
                    .wait_for_notes(reviews.len() - keys.len(), crate::INGEST_TIMEOUT)
                    .await
                    .expect("review records ingested"),
            );
        }

        let txn = Transaction::new(&ndb).unwrap();
        let mut parsed: Vec<ReviewFields> = keys
            .iter()
            .map(|k| {
                let note = ndb.get_note_by_key(&txn, *k).unwrap();
                let Some(HeadwayEvent::Review(r)) = parse(&note) else {
                    panic!("review");
                };
                assert_eq!(r.issue_id, *issue.bytes());
                r.fields
            })
            .collect();
        parsed.sort();
        let mut expected = reviews.to_vec();
        expected.sort();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn relation_roundtrips_set_and_detach() {
        let kp = FullKeypair::generate();
        let child = note_id(&kp, build_issue("30619:x:b1", "child", ""));
        let parent = note_id(&kp, build_issue("30619:x:b1", "parent", ""));

        let HeadwayEvent::Relation(r) = roundtrip(build_relation(&child, Some(&parent)), &kp)
        else {
            panic!("relation");
        };
        assert_eq!(r.child_id, *child.bytes());
        assert_eq!(r.parent_id, Some(*parent.bytes()));

        // No `parent` tag = a detach, still a well-formed relation.
        let HeadwayEvent::Relation(r) = roundtrip(build_relation(&child, None), &kp) else {
            panic!("relation");
        };
        assert_eq!(r.parent_id, None);
    }

    /// A blockers set round-trips through build/parse, preserving the blocked card
    /// and every listed blocker; an empty set is a well-formed cleared set.
    #[test]
    fn blockers_roundtrip_set_and_clear() {
        let kp = FullKeypair::generate();
        let addr = board_address(&kp.pubkey, "b1");
        let blocked = note_id(&kp, build_issue(&addr, "blocked", ""));
        let b1 = note_id(&kp, build_issue(&addr, "b1", ""));
        let b2 = note_id(&kp, build_issue(&addr, "b2", ""));

        let HeadwayEvent::Blockers(set) = roundtrip(build_blockers(&blocked, &[b1, b2]), &kp)
        else {
            panic!("blockers");
        };
        assert_eq!(set.blocked_id, *blocked.bytes());
        assert_eq!(set.blockers, vec![*b1.bytes(), *b2.bytes()]);

        // No `blocked-by` tags = a cleared set, still well-formed.
        let HeadwayEvent::Blockers(set) = roundtrip(build_blockers(&blocked, &[]), &kp) else {
            panic!("blockers");
        };
        assert!(set.blockers.is_empty());
    }

    /// A related-to set round-trips through build/parse, preserving the storing
    /// card and every listed partner; an empty set is a well-formed cleared set.
    #[test]
    fn related_roundtrips_set_and_clear() {
        let kp = FullKeypair::generate();
        let addr = board_address(&kp.pubkey, "b1");
        let card = note_id(&kp, build_issue(&addr, "card", ""));
        let r1 = note_id(&kp, build_issue(&addr, "r1", ""));
        let r2 = note_id(&kp, build_issue(&addr, "r2", ""));

        let HeadwayEvent::Related(set) = roundtrip(build_related(&card, &[r1, r2]), &kp) else {
            panic!("related");
        };
        assert_eq!(set.card_id, *card.bytes());
        assert_eq!(set.related, vec![*r1.bytes(), *r2.bytes()]);

        // No `related` tags = a cleared set, still well-formed.
        let HeadwayEvent::Related(set) = roundtrip(build_related(&card, &[]), &kp) else {
            panic!("related");
        };
        assert!(set.related.is_empty());
    }

    /// A sequence event round-trips through build/parse for both container kinds,
    /// preserving the container, issue, and rank.
    #[test]
    fn sequence_event_roundtrips() {
        let kp = FullKeypair::generate();
        let addr = board_address(&kp.pubkey, "b1");
        let issue = note_id(&kp, build_issue(&addr, "Card", ""));
        let parent = note_id(&kp, build_issue(&addr, "Parent", ""));

        for container in [
            Container::BoardRoot("b1".into()),
            Container::Card(*parent.bytes()),
        ] {
            let ev = roundtrip(build_sequence(&container, &issue, "an"), &kp);
            let HeadwayEvent::Sequence(s) = ev else {
                panic!("expected sequence");
            };
            assert_eq!(s.container, container);
            assert_eq!(s.issue_id, *issue.bytes());
            assert_eq!(s.rank, "an");
        }
    }

    /// The container wire form round-trips through parse for both kinds, and an
    /// unknown type is rejected.
    #[test]
    fn container_wire_roundtrips() {
        let card = Container::Card([7u8; 32]);
        let root = Container::BoardRoot("my-board".into());
        assert_eq!(Container::parse(&card.wire()), Some(card));
        assert_eq!(Container::parse(&root.wire()), Some(root));
        assert_eq!(Container::parse("bogus:xyz"), None);
    }
}
