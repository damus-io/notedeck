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

use std::collections::HashMap;

use enostr::{NoteId, Pubkey};
use nostrdb::{Filter, Ndb, Note, NoteBuildOptions, NoteBuilder, NoteKey, Transaction};

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

const NS_SUBJECT: &str = "#subject";
const NS_TAG: &str = "#t";

/// Sentinel placement column id meaning the card has been removed from the
/// board. A card whose latest *authorised* placement points here is dropped by
/// the reducer. This is a reversible "tombstone" (re-place the card to restore
/// it) rather than a NIP-09 deletion, which keeps removal under the same
/// authority/latest-wins rules as every other placement.
pub const COL_DELETED: &str = "__deleted__";

/// Sentinel placement column id meaning the card has been *archived*: taken off
/// the active board but kept (and recoverable) rather than tombstoned. A card
/// whose latest *authorised* placement points here is collected onto
/// [`BoardView::archived`] instead of a column. The archive placement also
/// carries a `from` tag (the column it was archived from) so a restore lands the
/// card back where it was — see [`build_archive_placement`]. Like `COL_DELETED`
/// this keeps archival under the same authority/latest-wins rules as any
/// placement.
pub const COL_ARCHIVED: &str = "__archived__";

/// A column definition as carried on the board event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnDef {
    pub id: String,
    pub name: String,
}

impl ColumnDef {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }
}

/// The addressable coordinate of a board: `30619:<author-hex>:<board-id>`.
pub fn board_address(author: &Pubkey, board_id: &str) -> String {
    format!("{KIND_BOARD}:{}:{board_id}", author.hex())
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

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
    }

    b
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

// ---------------------------------------------------------------------------
// Parsed events
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardEvent {
    pub id: String,
    pub author: [u8; 32],
    pub title: String,
    pub description: String,
    pub columns: Vec<ColumnDef>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacementEvent {
    pub author: [u8; 32],
    pub issue_id: [u8; 32],
    pub col: String,
    pub rank: String,
    /// The column the card was archived *from*, present only on archive
    /// placements (`col == COL_ARCHIVED`). Lets a restore put the card back
    /// where it was rather than reflowing it to the first column.
    pub from: Option<String>,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubjectEdit {
    pub author: [u8; 32],
    pub issue_id: [u8; 32],
    pub subject: String,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelSet {
    pub author: [u8; 32],
    pub issue_id: [u8; 32],
    pub labels: Vec<String>,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverNote {
    pub author: [u8; 32],
    pub issue_id: [u8; 32],
    pub body: String,
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

/// A parsed headway event of any of the recognised kinds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadwayEvent {
    Board(BoardEvent),
    Issue(IssueEvent),
    Placement(PlacementEvent),
    Subject(SubjectEdit),
    Labels(LabelSet),
    Cover(CoverNote),
    Comment(CommentEvent),
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
        KIND_COMMENT => parse_comment(note).map(HeadwayEvent::Comment),
        _ => None,
    }
}

fn parse_board(note: &Note) -> Option<BoardEvent> {
    let mut id = None;
    let mut title = String::new();
    let mut description = String::new();
    let mut columns = Vec::new();

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("d") => id = tag.get_str(1).map(|s| s.to_owned()),
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
                    columns.push(ColumnDef::new(cid, name));
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
        created_at: note.created_at(),
    })
}

fn parse_issue(note: &Note) -> Option<IssueEvent> {
    let mut subject = String::new();
    let mut board = None;
    let mut inline_labels = Vec::new();

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("a") => board = tag.get_str(1).and_then(parse_board_address),
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

    let (board_author, board_id) = board?;

    Some(IssueEvent {
        id: *note.id(),
        author: *note.pubkey(),
        board_author,
        board_id,
        subject,
        body: note.content().to_owned(),
        inline_labels,
        created_at: note.created_at(),
    })
}

fn parse_placement(note: &Note) -> Option<PlacementEvent> {
    let mut issue_id = None;
    let mut col = None;
    let mut rank = None;
    let mut from = None;

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("e") => issue_id = tag.get_id(1).copied(),
            Some("col") => col = tag.get_str(1).map(|s| s.to_owned()),
            Some("rank") => rank = tag.get_str(1).map(|s| s.to_owned()),
            Some("from") => from = tag.get_str(1).map(|s| s.to_owned()),
            _ => {}
        }
    }

    Some(PlacementEvent {
        author: *note.pubkey(),
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

/// Parse a NIP-22 comment (kind 1111). The root issue is the uppercase `E`; the
/// parent is the lowercase `e`, and the lowercase `k` tells us whether that
/// parent is another comment (a threaded reply) or the issue (a top-level
/// comment). See [`build_comment`].
fn parse_comment(note: &Note) -> Option<CommentEvent> {
    let mut issue_id = None;
    let mut parent_e = None;
    let mut parent_kind = None;

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("E") => issue_id = tag.get_id(1).copied(),
            Some("e") => parent_e = tag.get_id(1).copied(),
            Some("k") => parent_kind = tag.get_str(1).map(|s| s.to_owned()),
            _ => {}
        }
    }

    // A reply names another comment as its parent (`k` == 1111); a top-level
    // comment's parent is the issue itself, so it carries no parent comment.
    let parent_id = match (parent_kind.as_deref(), parent_e) {
        (Some(k), Some(e)) if k == KIND_COMMENT.to_string() => Some(e),
        _ => None,
    };

    Some(CommentEvent {
        id: *note.id(),
        author: *note.pubkey(),
        issue_id: issue_id?,
        parent_id,
        body: note.content().to_owned(),
        created_at: note.created_at(),
    })
}

/// Parse a `30619:<author-hex>:<board-id>` address into `(author, board_id)`.
fn parse_board_address(addr: &str) -> Option<([u8; 32], String)> {
    let mut parts = addr.splitn(3, ':');
    let kind = parts.next()?;
    if kind != KIND_BOARD.to_string() {
        return None;
    }
    let author_hex = parts.next()?;
    let board_id = parts.next()?;
    let author = Pubkey::from_hex(author_hex).ok()?;
    Some((*author.bytes(), board_id.to_owned()))
}

// ---------------------------------------------------------------------------
// Reducer: events -> view model
// ---------------------------------------------------------------------------

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

/// A card as rendered: a stable id plus its resolved fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardView {
    pub id: NoteId,
    /// The issue author. Needed to address comments at the card (NIP-22 root
    /// `P`) and to attribute the card itself.
    pub author: [u8; 32],
    pub title: String,
    pub description: String,
    pub labels: Vec<String>,
    /// Fractional rank within its column; cards are sorted ascending.
    pub rank: String,
    /// `created_at` of the winning placement (0 if the card is unplaced). A
    /// re-placement (move/delete/archive) must stamp a strictly-greater
    /// timestamp so it wins latest-wins even within the same wall-clock second.
    pub placed_at: u64,
    /// Comments on the card, oldest first (sorted by `created_at`, then id).
    pub comments: Vec<CommentView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnView {
    pub id: String,
    pub name: String,
    pub cards: Vec<CardView>,
}

/// An archived card plus the column it was archived from, for the archived view
/// and restore. `from` is `None` if the card was archived before origin
/// tracking existed, or its origin column has since been forgotten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchivedCard {
    pub card: CardView,
    pub from: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

/// Render `view` as a stable, machine-readable JSON value: a curated schema for
/// external tooling (e.g. the CLI's `--json`) with hex ids, independent of the
/// internal view types.
pub fn board_json(view: &BoardView) -> serde_json::Value {
    serde_json::json!({
        "id": view.id,
        "title": view.title,
        "description": view.description,
        "columns": view.columns.iter().map(|c| serde_json::json!({
            "id": c.id,
            "name": c.name,
            "cards": c.cards.iter().map(card_json).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "archived": view.archived.iter().map(|a| {
            let mut card = card_json(&a.card);
            card["from"] = serde_json::json!(a.from);
            card
        }).collect::<Vec<_>>(),
    })
}

/// Render a single card as JSON. See [`board_json`].
pub fn card_json(card: &CardView) -> serde_json::Value {
    serde_json::json!({
        "id": card.id.hex(),
        "author": Pubkey::new(card.author).hex(),
        "title": card.title,
        "description": card.description,
        "labels": card.labels,
        "rank": card.rank,
        "comments": card.comments.iter().map(comment_json).collect::<Vec<_>>(),
    })
}

/// Render a single comment as JSON. See [`card_json`].
pub fn comment_json(comment: &CommentView) -> serde_json::Value {
    serde_json::json!({
        "id": comment.id.hex(),
        "author": Pubkey::new(comment.author).hex(),
        "parent": comment.parent.map(|p| p.hex()),
        "body": comment.body,
        "created_at": comment.created_at,
    })
}

/// Accumulates headway events into the maps needed to resolve effective board
/// state, applying latest-authorised-wins as each event arrives. Keeping the
/// reduction incremental lets it run *inside* an [`Ndb::fold`] over the index
/// (see [`fold_board`]) and lets the app cache a live reducer and feed it only
/// freshly-arrived notes (see [`reduce_delta`]) instead of re-folding the whole
/// history. Both are sound because the fold is commutative and idempotent: each
/// overlay is a latest-authorised-wins map keyed by id, so an event's effect
/// doesn't depend on when (or how often) it's seen.
#[derive(Default)]
pub struct BoardReducer {
    /// Latest board event per (author, board_id).
    boards: HashMap<(Vec<u8>, String), BoardEvent>,
    /// Issues by id (immutable, but a relay may hand us duplicates).
    issues: HashMap<[u8; 32], IssueEvent>,
    placements: HashMap<[u8; 32], PlacementEvent>,
    subjects: HashMap<[u8; 32], SubjectEdit>,
    covers: HashMap<[u8; 32], CoverNote>,
    /// Latest label set per issue. Each label event is the *complete* set for
    /// the card (snapshot semantics), so the newest authorised one wins — this
    /// is what makes label *removal* expressible: republish the set without it.
    labels: HashMap<[u8; 32], LabelSet>,
    /// Comments by comment id. Append-only — every comment is kept (unlike the
    /// latest-wins overlays above) and grouped onto its issue at finalize. Keying
    /// by comment id dedupes the duplicates a relay may hand us.
    comments: HashMap<[u8; 32], CommentEvent>,
}

impl BoardReducer {
    /// Fold a single event into the accumulator.
    pub fn ingest(&mut self, event: HeadwayEvent) {
        match event {
            HeadwayEvent::Board(b) => {
                let key = (b.author.to_vec(), b.id.clone());
                if self
                    .boards
                    .get(&key)
                    .is_none_or(|cur| b.created_at > cur.created_at)
                {
                    self.boards.insert(key, b);
                }
            }
            HeadwayEvent::Issue(i) => {
                self.issues.insert(i.id, i);
            }
            HeadwayEvent::Placement(p) => {
                if self
                    .placements
                    .get(&p.issue_id)
                    .is_none_or(|cur| newer(p.created_at, &p.author, cur.created_at, &cur.author))
                {
                    self.placements.insert(p.issue_id, p);
                }
            }
            HeadwayEvent::Subject(s) => {
                if self
                    .subjects
                    .get(&s.issue_id)
                    .is_none_or(|cur| newer(s.created_at, &s.author, cur.created_at, &cur.author))
                {
                    self.subjects.insert(s.issue_id, s);
                }
            }
            HeadwayEvent::Cover(c) => {
                if self
                    .covers
                    .get(&c.issue_id)
                    .is_none_or(|cur| newer(c.created_at, &c.author, cur.created_at, &cur.author))
                {
                    self.covers.insert(c.issue_id, c);
                }
            }
            HeadwayEvent::Labels(l) => {
                if self
                    .labels
                    .get(&l.issue_id)
                    .is_none_or(|cur| newer(l.created_at, &l.author, cur.created_at, &cur.author))
                {
                    self.labels.insert(l.issue_id, l);
                }
            }
            HeadwayEvent::Comment(c) => {
                // Append-only and immutable: keep the first sighting; later
                // duplicates of the same id are no-ops.
                self.comments.entry(c.id).or_insert(c);
            }
        }
    }

    /// Assemble the accumulated events into board views.
    /// Resolve the accumulated events into the boards they describe. Takes
    /// `&self` so a cached reducer can be re-finalized after a delta without
    /// being consumed.
    pub fn finalize(&self) -> Vec<BoardView> {
        let mut views: Vec<BoardView> = Vec::new();

        for ((author, board_id), board) in &self.boards {
            // Group this board's cards by resolved column id.
            let mut by_col: HashMap<String, Vec<CardView>> = HashMap::new();
            let mut fallback: Vec<(u64, CardView)> = Vec::new();
            let mut archived: Vec<ArchivedCard> = Vec::new();
            let col_ids: Vec<&str> = board.columns.iter().map(|c| c.id.as_str()).collect();

            for issue in self.issues.values() {
                if &issue.board_author.to_vec() != author || &issue.board_id != board_id {
                    continue;
                }

                // Authority: the card author or the board author may amend the card.
                let authorised = |who: &[u8; 32]| who == &issue.author || who == &board.author;

                let title = self
                    .subjects
                    .get(&issue.id)
                    .filter(|s| authorised(&s.author))
                    .map(|s| s.subject.clone())
                    .unwrap_or_else(|| issue.subject.clone());

                let description = self
                    .covers
                    .get(&issue.id)
                    .filter(|c| authorised(&c.author))
                    .map(|c| c.body.clone())
                    .unwrap_or_else(|| issue.body.clone());

                // Labels resolve latest-authorised-wins: the newest authorised
                // label event is the card's complete set, overriding the issue's
                // inline labels. (Removal = republish the set without the label.)
                let mut labels = self
                    .labels
                    .get(&issue.id)
                    .filter(|s| authorised(&s.author))
                    .map(|s| s.labels.clone())
                    .unwrap_or_else(|| issue.inline_labels.clone());
                labels.sort();
                labels.dedup();

                let placement = self
                    .placements
                    .get(&issue.id)
                    .filter(|p| authorised(&p.author));

                let rank = placement.map(|p| p.rank.clone()).unwrap_or_default();
                let placed_at = placement.map(|p| p.created_at).unwrap_or(0);

                // Comments thread under the issue (the NIP-22 root). Append-only,
                // shown oldest first; the id breaks same-second ties.
                let mut comments: Vec<CommentView> = self
                    .comments
                    .values()
                    .filter(|c| c.issue_id == issue.id)
                    .map(|c| CommentView {
                        id: NoteId::new(c.id),
                        author: c.author,
                        parent: c.parent_id.map(NoteId::new),
                        body: c.body.clone(),
                        created_at: c.created_at,
                    })
                    .collect();
                comments.sort_by(|a, b| {
                    (a.created_at, a.id.bytes()).cmp(&(b.created_at, b.id.bytes()))
                });

                let card = CardView {
                    id: NoteId::new(issue.id),
                    author: issue.author,
                    title,
                    description,
                    labels,
                    rank,
                    placed_at,
                    comments,
                };

                match placement.map(|p| p.col.as_str()) {
                    // A tombstone placement removes the card from the board.
                    Some(COL_DELETED) => continue,
                    // Archived: kept off the columns but recoverable, with its
                    // origin column so a restore lands it back where it was.
                    Some(COL_ARCHIVED) => archived.push(ArchivedCard {
                        card,
                        from: placement.and_then(|p| p.from.clone()),
                    }),
                    Some(col) if col_ids.contains(&col) => {
                        by_col.entry(col.to_string()).or_default().push(card);
                    }
                    _ => fallback.push((issue.created_at, card)),
                }
            }

            let mut columns: Vec<ColumnView> = board
                .columns
                .iter()
                .map(|def| {
                    let mut cards = by_col.remove(&def.id).unwrap_or_default();
                    cards.sort_by(|a, b| a.rank.cmp(&b.rank));
                    ColumnView {
                        id: def.id.clone(),
                        name: def.name.clone(),
                        cards,
                    }
                })
                .collect();

            // Unplaced cards fall into the first column, oldest first.
            if let Some(first) = columns.first_mut() {
                fallback.sort_by_key(|(created, _)| *created);
                first
                    .cards
                    .extend(fallback.into_iter().map(|(_, card)| card));
            }

            // Stable order so the archived view and snapshots don't churn.
            archived.sort_by(|a, b| a.card.id.bytes().cmp(b.card.id.bytes()));

            views.push(BoardView {
                id: board_id.clone(),
                author: board.author,
                title: board.title.clone(),
                description: board.description.clone(),
                created_at: board.created_at,
                columns,
                archived,
            });
        }

        // Stable output order: by board id.
        views.sort_by(|a, b| a.id.cmp(&b.id));
        views
    }
}

/// Resolve a set of headway events into the boards they describe.
///
/// For each board the latest board event (by `created_at`) wins. Cards are
/// placed by their latest *authorised* placement (`col` + `rank`), with title /
/// description / labels resolved the same way. Cards with no placement, or whose
/// placement points at an unknown column, fall into the first column, ordered by
/// creation time after the explicitly placed cards.
pub fn reduce(events: &[HeadwayEvent]) -> Vec<BoardView> {
    let mut reducer = BoardReducer::default();
    for event in events {
        reducer.ingest(event.clone());
    }
    reducer.finalize()
}

/// "Latest authorised wins" comparator: newer `created_at` wins, ties broken by
/// author bytes so the result is deterministic.
fn newer(a_at: u64, a_who: &[u8; 32], b_at: u64, b_who: &[u8; 32]) -> bool {
    (a_at, a_who) > (b_at, b_who)
}

// ---------------------------------------------------------------------------
// ndb loading
// ---------------------------------------------------------------------------

/// Every kind headway cares about, for querying / subscribing.
pub const HEADWAY_KINDS: [u32; 6] = [
    KIND_BOARD,
    KIND_ISSUE,
    KIND_PLACEMENT,
    KIND_LABEL,
    KIND_COVER_NOTE,
    KIND_COMMENT,
];

/// A filter for every headway event authored by `author`.
///
/// Headway is single-author per board for now, so filtering by author captures
/// the board, its cards and all metadata in one query. Collaborative boards will
/// additionally need `#a`/`#e` filters to pull in other authors' events.
pub fn headway_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .authors([author.bytes()])
        .kinds(HEADWAY_KINDS.iter().map(|k| *k as u64))
        .limit(5000)
        .build()
}

/// Fold all of `author`'s headway events out of `ndb` into a fresh reducer.
///
/// The reduction runs inside the [`Ndb::fold`] index walk via [`BoardReducer`],
/// so no intermediate event `Vec` is built. nostrdb doesn't replace addressable
/// events, so the placement/board history is walked in full and the reducer
/// resolves the effective state; `query_replaceable_filtered` can narrow the
/// addressable kinds (board, placement) to their latest versions later.
///
/// The caller can keep the returned reducer and feed later arrivals into it with
/// [`reduce_delta`] rather than re-folding the whole history.
pub fn fold_board(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Option<BoardReducer> {
    let filters = [headway_filter(author)];
    ndb.fold(txn, &filters, BoardReducer::default(), |mut acc, note| {
        if let Some(event) = parse(&note) {
            acc.ingest(event);
        }
        acc
    })
    .ok()
}

/// Fold a batch of freshly-arrived notes (identified by `keys`) into an existing
/// reducer. Sound because the fold is commutative and idempotent: applying a
/// delta to an up-to-date reducer yields the same state as a full re-fold, so
/// the app can subscribe-then-poll instead of walking the history every frame.
/// Notes that aren't recognised headway events are skipped.
pub fn reduce_delta(reducer: &mut BoardReducer, ndb: &Ndb, txn: &Transaction, keys: &[NoteKey]) {
    for key in keys {
        if let Ok(note) = ndb.get_note_by_key(txn, *key)
            && let Some(event) = parse(&note)
        {
            reducer.ingest(event);
        }
    }
}

/// Pick the board with `board_id` authored by `author` out of a reducer's
/// resolved boards, if it exists.
pub fn pick_board(reducer: &BoardReducer, author: &Pubkey, board_id: &str) -> Option<BoardView> {
    reducer
        .finalize()
        .into_iter()
        .find(|v| v.id == board_id && &v.author == author.bytes())
}

/// Pick a single card's *resolved* [`CardView`] (latest subject, labels, cover
/// and placement applied) out of a folded board, by the issue's note id.
/// Searches the live columns and the archived set. `None` if the board or the
/// card within it is absent. Unlike parsing the kind-1621 note directly — which
/// only yields its creation-time snapshot — this reflects later edits.
pub fn pick_card(
    reducer: &BoardReducer,
    author: &Pubkey,
    board_id: &str,
    issue_id: &[u8; 32],
) -> Option<CardView> {
    let want = NoteId::new(*issue_id);
    let board = pick_board(reducer, author, board_id)?;
    board
        .columns
        .into_iter()
        .flat_map(|col| col.cards)
        .chain(board.archived.into_iter().map(|a| a.card))
        .find(|card| card.id == want)
}

/// Fold `author`'s headway events out of `ndb` and reduce them into the board
/// with the given `board_id`, if it exists. A one-shot [`fold_board`] +
/// [`pick_board`] for callers that don't keep the reducer around.
pub fn load_board(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    board_id: &str,
) -> Option<BoardView> {
    pick_board(&fold_board(ndb, txn, author)?, author, board_id)
}

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
    use enostr::FullKeypair;

    /// Sign `builder` with `kp` and parse the result back into a [`HeadwayEvent`].
    fn roundtrip(builder: NoteBuilder, kp: &FullKeypair) -> HeadwayEvent {
        let note = builder
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("build note");
        parse(&note).expect("parse headway event")
    }

    fn note_id(kp: &FullKeypair, builder: NoteBuilder) -> NoteId {
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

    /// Build a full board (board + two issues + placements) and reduce it,
    /// checking columns, ordering and the metadata overrides.
    #[test]
    fn reduce_builds_board_view() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let mut events = Vec::new();
        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        events.push(parse_owned(build_board("b1", "Board", "", &cols), &owner));

        let i1 = note_id(&owner, build_issue(&addr, "First", ""));
        let i2 = note_id(&owner, build_issue(&addr, "Second", ""));
        events.push(parse_owned(build_issue(&addr, "First", ""), &owner));
        events.push(parse_owned(build_issue(&addr, "Second", ""), &owner));

        // Both into "todo": i2 ranked before i1.
        events.push(parse_owned(
            build_placement("b1", &addr, &i1, "todo", "t"),
            &owner,
        ));
        events.push(parse_owned(
            build_placement("b1", &addr, &i2, "todo", "g"),
            &owner,
        ));
        // Rename i1, label it, give it a description.
        events.push(parse_owned(
            build_subject_edit(&i1, "First (edited)"),
            &owner,
        ));
        events.push(parse_owned(build_labels(&i1, &["bug".to_string()]), &owner));
        events.push(parse_owned(
            build_cover_note(&i1, &owner.pubkey, "details"),
            &owner,
        ));

        let views = reduce(&events);
        assert_eq!(views.len(), 1);
        let view = &views[0];
        assert_eq!(view.columns.len(), 2);

        let todo = &view.columns[0];
        assert_eq!(todo.id, "todo");
        // Sorted by rank ascending: "g" (Second) before "t" (First).
        assert_eq!(todo.cards.len(), 2);
        assert_eq!(todo.cards[0].title, "Second");
        assert_eq!(todo.cards[1].title, "First (edited)");
        assert_eq!(todo.cards[1].labels, vec!["bug".to_string()]);
        assert_eq!(todo.cards[1].description, "details");

        assert!(view.columns[1].cards.is_empty());
    }

    #[test]
    fn reduce_ignores_unauthorised_edits() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "Original", ""));
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Original", ""), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            // A stranger tries to rename the card: must be ignored.
            parse_owned(build_subject_edit(&i1, "Hijacked"), &stranger),
        ];

        let views = reduce(&events);
        assert_eq!(views[0].columns[0].cards[0].title, "Original");
    }

    /// Labels are snapshot/latest-wins, not an additive union: republishing the
    /// set without a label removes it. The newer (whole) set must win.
    #[test]
    fn reduce_label_removal_replaces_the_set() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "Card", ""));

        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Card", ""), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            parse_owned(
                build_labels(&i1, &["bug".to_string(), "ux".to_string()]),
                &owner,
            ),
        ];

        // Republish the set without "bug" — a later event so it wins latest-wins.
        let mut shrunk = match parse_owned(build_labels(&i1, &["ux".to_string()]), &owner) {
            HeadwayEvent::Labels(l) => l,
            _ => unreachable!(),
        };
        shrunk.created_at += 1;
        events.push(HeadwayEvent::Labels(shrunk));

        let views = reduce(&events);
        // "bug" is gone; only "ux" remains (not a union of both).
        assert_eq!(views[0].columns[0].cards[0].labels, vec!["ux".to_string()]);
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

    /// Comments fold onto their card oldest-first, deduped by id, and a reply
    /// keeps its parent link.
    #[test]
    fn reduce_attaches_comments_to_cards() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "Card", ""));

        // Two comments and a reply; stamp increasing created_at so order is fixed.
        let comment_id = |kp: &FullKeypair, b: NoteBuilder| {
            NoteId::new(*b.sign(&kp.secret_key.secret_bytes()).build().unwrap().id())
        };
        let c1 = comment_id(&owner, build_comment(&i1, &owner.pubkey, None, "one"));

        let stamp = |ev: HeadwayEvent, at: u64| match ev {
            HeadwayEvent::Comment(mut c) => {
                c.created_at = at;
                HeadwayEvent::Comment(c)
            }
            other => other,
        };

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Card", ""), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            stamp(
                parse_owned(build_comment(&i1, &owner.pubkey, None, "one"), &owner),
                10,
            ),
            stamp(
                parse_owned(build_comment(&i1, &owner.pubkey, None, "two"), &owner),
                20,
            ),
            stamp(
                parse_owned(
                    build_comment(&i1, &owner.pubkey, Some((&c1, &owner.pubkey)), "re: one"),
                    &owner,
                ),
                30,
            ),
        ];

        let views = reduce(&events);
        let card = &views[0].columns[0].cards[0];
        assert_eq!(card.comments.len(), 3);
        // Oldest first.
        assert_eq!(card.comments[0].body, "one");
        assert_eq!(card.comments[1].body, "two");
        assert_eq!(card.comments[2].body, "re: one");
        // The reply points back at the first comment; top-level ones don't.
        assert_eq!(card.comments[0].parent, None);
        assert_eq!(card.comments[2].parent, Some(c1));
    }

    /// A relay may hand us the same comment twice; the reducer keeps one.
    #[test]
    fn reduce_dedupes_duplicate_comments() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "Card", ""));
        let comment = parse_owned(build_comment(&i1, &owner.pubkey, None, "dup"), &owner);

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Card", ""), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            comment.clone(),
            comment,
        ];

        let views = reduce(&events);
        assert_eq!(views[0].columns[0].cards[0].comments.len(), 1);
    }

    #[test]
    fn reduce_skips_deleted_cards() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let keep = note_id(&owner, build_issue(&addr, "Keep", ""));
        let gone = note_id(&owner, build_issue(&addr, "Gone", ""));

        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Keep", ""), &owner),
            parse_owned(build_issue(&addr, "Gone", ""), &owner),
            parse_owned(build_placement("b1", &addr, &keep, "todo", "m"), &owner),
            parse_owned(build_placement("b1", &addr, &gone, "todo", "t"), &owner),
        ];

        // Tombstone the second card with a later placement.
        let mut tombstone = match parse_owned(
            build_placement("b1", &addr, &gone, COL_DELETED, "t"),
            &owner,
        ) {
            HeadwayEvent::Placement(p) => p,
            _ => unreachable!(),
        };
        // Ensure the tombstone wins the latest-wins race deterministically.
        tombstone.created_at += 1;
        events.push(HeadwayEvent::Placement(tombstone));

        let views = reduce(&events);
        let cards = &views[0].columns[0].cards;
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "Keep");
    }

    #[test]
    fn reduce_archives_cards_with_their_origin() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let card = note_id(&owner, build_issue(&addr, "Shelve me", ""));

        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Shelve me", ""), &owner),
            parse_owned(build_placement("b1", &addr, &card, "done", "m"), &owner),
        ];

        // Archive it from "done" with a later placement so it wins latest-wins.
        let mut archive = match parse_owned(
            build_archive_placement("b1", &addr, &card, "done", "m"),
            &owner,
        ) {
            HeadwayEvent::Placement(p) => p,
            _ => unreachable!(),
        };
        archive.created_at += 1;
        events.push(HeadwayEvent::Placement(archive));

        let views = reduce(&events);
        // Gone from every column, present in `archived` with its origin recorded.
        assert!(views[0].columns.iter().all(|c| c.cards.is_empty()));
        assert_eq!(views[0].archived.len(), 1);
        assert_eq!(views[0].archived[0].card.title, "Shelve me");
        assert_eq!(views[0].archived[0].from.as_deref(), Some("done"));
    }

    #[test]
    fn reduce_falls_back_unplaced_cards_to_first_column() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Loose card", ""), &owner),
        ];

        let views = reduce(&events);
        assert_eq!(views[0].columns[0].cards.len(), 1);
        assert_eq!(views[0].columns[0].cards[0].title, "Loose card");
    }

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

    /// End-to-end through a real nostrdb: build + sign events, ingest them, then
    /// fold them back out with [`load_board`] and check the board reconstructs
    /// (including a subject rename overriding the issue's original subject).
    #[test]
    fn load_board_roundtrips_through_ndb() {
        use nostrdb::{Config, IngestMetadata, Ndb, Transaction};
        use std::time::{Duration, Instant};

        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let kp = FullKeypair::generate();
        let addr = board_address(&kp.pubkey, "headway");

        let ingest = |b: NoteBuilder| -> NoteId {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            let id = NoteId::new(*note.id());
            let json = enostr::ClientMessage::event(&note)
                .unwrap()
                .to_json()
                .unwrap();
            ndb.process_event_with(&json, IngestMetadata::new().client(true))
                .unwrap();
            id
        };

        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];
        ingest(build_board("headway", "Headway", "", &cols));
        let a = ingest(build_issue(&addr, "Card A", ""));
        let b = ingest(build_issue(&addr, "Card B", ""));
        ingest(build_placement("headway", &addr, &a, "todo", "g"));
        ingest(build_placement("headway", &addr, &b, "done", "m"));
        ingest(build_subject_edit(&a, "Card A (renamed)"));

        // ndb ingests on a writer thread; poll until the board materialises.
        let deadline = Instant::now() + Duration::from_secs(5);
        let view = loop {
            let txn = Transaction::new(&ndb).unwrap();
            if let Some(view) = load_board(&ndb, &txn, &kp.pubkey, "headway")
                && view.columns[0].cards.len() == 1
                && view.columns[1].cards.len() == 1
            {
                break view;
            }
            assert!(
                Instant::now() < deadline,
                "board did not materialise in ndb"
            );
            std::thread::sleep(Duration::from_millis(20));
        };

        assert_eq!(view.columns.len(), 2);
        assert_eq!(view.columns[0].name, "Todo");
        assert_eq!(view.columns[0].cards[0].title, "Card A (renamed)");
        assert_eq!(view.columns[1].cards[0].title, "Card B");
    }

    /// [`pick_card`] resolves a single card to its *current* state — the latest
    /// subject and label edits applied — not the issue's creation-time snapshot,
    /// and returns `None` for an unknown card id.
    #[test]
    fn pick_card_resolves_current_state() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder| {
            let note = b.sign(&owner.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "Original", "body"));
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols)),
            parse_owned(build_issue(&addr, "Original", "body")),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m")),
            parse_owned(build_subject_edit(&i1, "Renamed")),
            parse_owned(build_labels(&i1, &["bug".to_string()])),
        ];

        let mut reducer = BoardReducer::default();
        for event in &events {
            reducer.ingest(event.clone());
        }

        let card = pick_card(&reducer, &owner.pubkey, "b1", i1.bytes()).unwrap();
        assert_eq!(card.title, "Renamed");
        assert_eq!(card.labels, vec!["bug".to_string()]);

        // Unknown card id -> None.
        assert!(pick_card(&reducer, &owner.pubkey, "b1", &[0u8; 32]).is_none());
    }
}
