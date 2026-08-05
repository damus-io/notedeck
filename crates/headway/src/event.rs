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

use std::collections::{HashMap, HashSet};

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
/// Headway card relation: addressable, `d` = child issue id, `parent` names the
/// parent issue. Child-side, so each child has exactly one parent slot —
/// re-parenting republishes the slot and a relation with no `parent` tag
/// detaches. See `crates/notedeck_headway/docs/subissues-design.md`.
pub const KIND_RELATION: u32 = 30621;
/// Headway card sequence: addressable, `d` = `<container>:<issue-id>`, records a
/// fractional `rank` positioning the card within a [`Container`] (board root or
/// parent card). The cross-cutting work-order axis — orthogonal to the column
/// `rank` on [`KIND_PLACEMENT`] — resolved latest-authorised-wins. See the
/// `birth-plate-alien` card design.
pub const KIND_SEQUENCE: u32 = 30622;

const NS_SUBJECT: &str = "#subject";
const NS_TAG: &str = "#t";

/// A single-value scalar overlay on a card. Each [`Field`] is carried as a
/// kind-1985 NIP-32 label in its own `L` namespace with one `l` value, resolved
/// latest-authorised-wins exactly like the subject overlay ([`build_field`],
/// [`FieldEdit`]). Publishing an empty (or, for priority, `"none"`) value clears
/// the field.
///
/// This is deliberately only for *single scalar* fields — multi-valued concerns
/// (labels, a set) and entity references (a parent relation, a board placement)
/// keep their own mechanisms rather than being forced through here. The plumbing
/// (builder, parse, reducer overlay, activity row) is generic over the field;
/// each field's *value type* and rendering stay typed at the edges, landing in a
/// typed [`CardView`] field (e.g. [`CardView::priority`], [`CardView::due`],
/// [`CardView::estimate`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    Priority,
    Due,
    Estimate,
}

impl Field {
    /// The NIP-32 `L` namespace that carries this field on a kind-1985 label.
    fn namespace(self) -> &'static str {
        match self {
            Field::Priority => "#priority",
            Field::Due => "#due",
            Field::Estimate => "#estimate",
        }
    }

    /// The field carried by an `L` namespace, or `None` if it isn't a scalar
    /// field namespace (e.g. `#subject`/`#t`, which are handled separately).
    fn from_namespace(ns: &str) -> Option<Field> {
        match ns {
            "#priority" => Some(Field::Priority),
            "#due" => Some(Field::Due),
            "#estimate" => Some(Field::Estimate),
            _ => None,
        }
    }

    /// A human label for the field, used in the activity timeline and JSON.
    pub fn label(self) -> &'static str {
        match self {
            Field::Priority => "priority",
            Field::Due => "due",
            Field::Estimate => "estimate",
        }
    }
}

/// A card's priority. Ordered least-to-most urgent so a "sort by priority"
/// descends from [`Priority::Urgent`]; [`Priority::None`] (the default, "no
/// priority") sorts last, matching Linear. Carried as the [`Field::Priority`]
/// scalar overlay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// No priority set — the default when a card has never been prioritised.
    #[default]
    None,
    Low,
    Medium,
    High,
    Urgent,
}

impl Priority {
    /// The stable wire/JSON string for this priority.
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::None => "none",
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }

    /// Parse a priority from its wire string (case-insensitive). `"med"` is
    /// accepted as an alias for `"medium"`. Unknown values (and `"none"`) map to
    /// [`Priority::None`], so a malformed overlay reads as "no priority" rather
    /// than failing the fold.
    pub fn parse(s: &str) -> Priority {
        match s.trim().to_ascii_lowercase().as_str() {
            "urgent" => Priority::Urgent,
            "high" => Priority::High,
            "medium" | "med" => Priority::Medium,
            "low" => Priority::Low,
            _ => Priority::None,
        }
    }
}

/// A calendar day — the value type of the [`Field::Due`] due-date overlay. Day
/// granularity (not an instant): a due date is "the 30th", independent of
/// timezone. Fields are ordered year→month→day so the derived `Ord` is
/// chronological, which is exactly the sort the list view wants. Rendered and
/// parsed as ISO `YYYY-MM-DD`, which also happens to sort lexicographically the
/// same way, so the wire form sorts correctly too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Date {
    /// Parse an ISO `YYYY-MM-DD` date, validating the month and the day against
    /// that month's length (leap years included). `None` for anything malformed
    /// or out of range, so a junk overlay reads as "no due date".
    pub fn parse(s: &str) -> Option<Date> {
        let (y, rest) = s.trim().split_once('-')?;
        let (m, d) = rest.split_once('-')?;
        let year: i32 = y.parse().ok()?;
        let month: u8 = m.parse().ok()?;
        let day: u8 = d.parse().ok()?;
        if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        Some(Date { year, month, day })
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// Days in `month` of `year` (1-indexed month), honouring leap years for
/// February. Used to validate [`Date::parse`].
fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

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

/// Build a sequence event (kind 30622) positioning `issue` at fractional `rank`
/// within `container`. Addressable by `d = <container>:<issue-id>` so republishing
/// supersedes the previous position latest-authorised-wins. `rank` comes from
/// [`rank_between`], the same kernel that ranks cards within a column.
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
/// ordering — the same fractional-rank kernel ([`rank_between`]) positions a card
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
/// `rank` is a [`rank_between`] string, compared lexicographically; absent
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
    Field(FieldEdit),
    Cover(CoverNote),
    Comment(CommentEvent),
    Relation(RelationEvent),
    Sequence(SequenceEvent),
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
        KIND_RELATION => parse_relation(note).map(HeadwayEvent::Relation),
        KIND_SEQUENCE => parse_sequence(note).map(HeadwayEvent::Sequence),
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
    let mut board = None;
    let mut col = None;
    let mut rank = None;
    let mut from = None;

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("e") => issue_id = tag.get_id(1).copied(),
            Some("a") => board = tag.get_str(1).and_then(parse_board_address),
            Some("col") => col = tag.get_str(1).map(|s| s.to_owned()),
            Some("rank") => rank = tag.get_str(1).map(|s| s.to_owned()),
            Some("from") => from = tag.get_str(1).map(|s| s.to_owned()),
            _ => {}
        }
    }

    let (board_author, board_id) = board?;

    Some(PlacementEvent {
        author: *note.pubkey(),
        board_author,
        board_id,
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

/// Parse a relation (kind 30621). The child is the `e` tag; a missing `parent`
/// tag is a detach, not a malformed event. See [`build_relation`].
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
}

/// A direct subissue of a card, resolved for display on its parent. Doneness is
/// positional — derived from where the child sits on its board(s) — never a
/// stored checkbox (see `crates/notedeck_headway/docs/subissues-design.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubissueView {
    pub id: NoteId,
    /// Resolved title (subject overlay applied).
    pub title: String,
    /// Column id of a live placement — the one on the board being rendered when
    /// there is one, else the first by board id for determinism. `None` when the
    /// child is unplaced or archived everywhere.
    pub column: Option<String>,
    /// Done = every live placement sits in the last column of its board, or the
    /// child is archived everywhere it's placed.
    pub done: bool,
    /// The child has no live placement but at least one archived one.
    pub archived: bool,
    /// Work-order rank of this child within its parent (fractional), `None` when
    /// unsequenced — sequenced children sort ahead of unsequenced ones, which
    /// keep their creation order. See the `birth-plate-alien` design.
    pub seq: Option<String>,
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
    /// (title, description or label edit) or comment, falling back to
    /// `created_at` if the card was never touched. Placements are board-scoped
    /// and tracked by `placed_at` instead, keeping this board-agnostic — the
    /// same issue shows the same `updated_at` on every board it's placed on.
    pub updated_at: u64,
    /// Comments on the card, oldest first (sorted by `created_at`, then id).
    pub comments: Vec<CommentView>,
    /// The card's derived activity timeline (created / moved / renamed / …),
    /// oldest first. See [`ActivityView`]; comments are kept separately above
    /// and interleaved by the renderer.
    pub activity: Vec<ActivityView>,
    /// The parent card when this one is a subissue (authorised relation slot).
    pub parent: Option<NoteId>,
    /// Direct subissues in work-order: sequenced children first (by `seq` rank),
    /// then unsequenced ones by `(created_at, id)`. See [`SubissueView::seq`].
    pub subissues: Vec<SubissueView>,
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
}

/// Render `view` as a stable, machine-readable JSON value: a curated schema for
/// external tooling (e.g. the CLI's `--json`) with hex ids plus the `words`
/// word-id used to address cards/comments, independent of the internal view
/// types.
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
        "words": crate::wordid::encode(card.id.bytes()),
        "author": Pubkey::new(card.author).hex(),
        "title": card.title,
        "description": card.description,
        "labels": card.labels,
        "priority": card.priority.as_str(),
        "due": card.due.map(|d| d.to_string()),
        "estimate": card.estimate,
        "rank": card.rank,
        "seq": card.seq,
        "created_at": card.created_at,
        "updated_at": card.updated_at,
        "parent": card.parent.map(|p| p.hex()),
        "parent_words": card.parent.map(|p| crate::wordid::encode(p.bytes())),
        "subissues": card.subissues.iter().map(|s| serde_json::json!({
            "id": s.id.hex(),
            "words": crate::wordid::encode(s.id.bytes()),
            "title": s.title,
            "column": s.column,
            "done": s.done,
            "archived": s.archived,
            "seq": s.seq,
        })).collect::<Vec<_>>(),
        "comments": card.comments.iter().map(comment_json).collect::<Vec<_>>(),
        "activity": card.activity.iter().map(activity_json).collect::<Vec<_>>(),
    })
}

/// Render one activity-timeline entry as JSON: a `type` discriminant plus that
/// variant's fields, flattened. See [`card_json`].
pub fn activity_json(activity: &ActivityView) -> serde_json::Value {
    let mut v = match &activity.kind {
        ActivityKind::Created => serde_json::json!({"type": "created"}),
        ActivityKind::Moved { from, to, .. } => {
            serde_json::json!({"type": "moved", "from": from, "to": to})
        }
        ActivityKind::Archived => serde_json::json!({"type": "archived"}),
        ActivityKind::Restored { to, .. } => serde_json::json!({"type": "restored", "to": to}),
        ActivityKind::Renamed { to } => serde_json::json!({"type": "renamed", "to": to}),
        ActivityKind::DescriptionEdited => serde_json::json!({"type": "description_edited"}),
        ActivityKind::LabelsChanged { added, removed } => {
            serde_json::json!({"type": "labels_changed", "added": added, "removed": removed})
        }
        ActivityKind::FieldChanged { field, to } => {
            serde_json::json!({"type": "field_changed", "field": field.label(), "to": to})
        }
        ActivityKind::ParentSet { parent, title } => serde_json::json!({
            "type": "parent_set",
            "parent": parent.hex(),
            "parent_words": crate::wordid::encode(parent.bytes()),
            "parent_title": title,
        }),
        ActivityKind::ParentRemoved => serde_json::json!({"type": "parent_removed"}),
    };
    v["author"] = serde_json::json!(Pubkey::new(activity.author).hex());
    v["created_at"] = serde_json::json!(activity.created_at);
    v
}

/// Render a single comment as JSON. See [`card_json`].
pub fn comment_json(comment: &CommentView) -> serde_json::Value {
    serde_json::json!({
        "id": comment.id.hex(),
        "words": crate::wordid::encode(comment.id.bytes()),
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
/// Identifies a card's placement on a specific board. The same issue placed on
/// two boards has two distinct keys (and two independent column/rank slots).
#[derive(Clone, PartialEq, Eq, Hash)]
struct PlacementKey {
    board_author: [u8; 32],
    board_id: String,
    issue_id: [u8; 32],
}

/// One raw event retained for the activity timeline: a clone of the parsed
/// event as it arrived, kept even after a newer one supersedes it in the
/// latest-wins overlays. Stored in a [`std::collections::BTreeSet`] per issue,
/// so duplicate deliveries dedupe by value (keeping ingest idempotent) and
/// iteration order is deterministic.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ActivityRecord {
    Placement(PlacementEvent),
    Subject(SubjectEdit),
    Cover(CoverNote),
    Labels(LabelSet),
    Field(FieldEdit),
    Relation(RelationEvent),
}

impl ActivityRecord {
    fn created_at(&self) -> u64 {
        match self {
            ActivityRecord::Placement(p) => p.created_at,
            ActivityRecord::Subject(s) => s.created_at,
            ActivityRecord::Cover(c) => c.created_at,
            ActivityRecord::Labels(l) => l.created_at,
            ActivityRecord::Field(f) => f.created_at,
            ActivityRecord::Relation(r) => r.created_at,
        }
    }
}

#[derive(Default)]
pub struct BoardReducer {
    /// Latest board event per (author, board_id).
    boards: HashMap<(Vec<u8>, String), BoardEvent>,
    /// Issues by id (immutable, but a relay may hand us duplicates).
    issues: HashMap<[u8; 32], IssueEvent>,
    /// Placements keyed by board + card — one per `(board, card)`. A card can be
    /// placed on several boards at once, so the key includes the board, not just
    /// the issue. Latest-authorised-wins within each key (a re-`move` on the same
    /// board supersedes the previous slot).
    placements: HashMap<PlacementKey, PlacementEvent>,
    subjects: HashMap<[u8; 32], SubjectEdit>,
    covers: HashMap<[u8; 32], CoverNote>,
    /// Latest scalar [`Field`] overlays per issue, one slot per field
    /// (latest-authorised-wins). Absent = the field was never set; the resolved
    /// typed value comes from parsing [`FieldEdit::value`] at finalize.
    fields: HashMap<[u8; 32], HashMap<Field, FieldEdit>>,
    /// Latest label set per issue. Each label event is the *complete* set for
    /// the card (snapshot semantics), so the newest authorised one wins — this
    /// is what makes label *removal* expressible: republish the set without it.
    labels: HashMap<[u8; 32], LabelSet>,
    /// Comments by comment id. Append-only — every comment is kept (unlike the
    /// latest-wins overlays above) and grouped onto its issue at finalize. Keying
    /// by comment id dedupes the duplicates a relay may hand us.
    comments: HashMap<[u8; 32], CommentEvent>,
    /// Latest relation per *child* issue — the child's one parent slot.
    /// Latest-authorised-wins like every other overlay; authority needs the
    /// issue maps so it's checked at resolve time, not here.
    relations: HashMap<[u8; 32], RelationEvent>,
    /// Latest sequence overlay per `(container, issue)` — the card's fractional
    /// work-order rank within that container (board root or parent card).
    /// Latest-authorised-wins; authority needs the issue maps so it's checked at
    /// resolve time, not here (like placements). Absent = the card is unsequenced.
    seqs: HashMap<(Container, [u8; 32]), SequenceEvent>,
    /// Full mutation history per issue, feeding the derived activity timeline
    /// ([`CardView::activity`]). The overlays above keep only the winner;
    /// nostrdb keeps every superseded event, so the fold sees them all and this
    /// set remembers them. Value-deduped, so re-ingesting stays idempotent.
    history: HashMap<[u8; 32], std::collections::BTreeSet<ActivityRecord>>,
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
                self.remember(p.issue_id, ActivityRecord::Placement(p.clone()));
                let key = PlacementKey {
                    board_author: p.board_author,
                    board_id: p.board_id.clone(),
                    issue_id: p.issue_id,
                };
                if self
                    .placements
                    .get(&key)
                    .is_none_or(|cur| newer(p.created_at, &p.author, cur.created_at, &cur.author))
                {
                    self.placements.insert(key, p);
                }
            }
            HeadwayEvent::Subject(s) => {
                self.remember(s.issue_id, ActivityRecord::Subject(s.clone()));
                if self
                    .subjects
                    .get(&s.issue_id)
                    .is_none_or(|cur| newer(s.created_at, &s.author, cur.created_at, &cur.author))
                {
                    self.subjects.insert(s.issue_id, s);
                }
            }
            HeadwayEvent::Cover(c) => {
                self.remember(c.issue_id, ActivityRecord::Cover(c.clone()));
                if self
                    .covers
                    .get(&c.issue_id)
                    .is_none_or(|cur| newer(c.created_at, &c.author, cur.created_at, &cur.author))
                {
                    self.covers.insert(c.issue_id, c);
                }
            }
            HeadwayEvent::Labels(l) => {
                self.remember(l.issue_id, ActivityRecord::Labels(l.clone()));
                if self
                    .labels
                    .get(&l.issue_id)
                    .is_none_or(|cur| newer(l.created_at, &l.author, cur.created_at, &cur.author))
                {
                    self.labels.insert(l.issue_id, l);
                }
            }
            HeadwayEvent::Field(f) => {
                self.remember(f.issue_id, ActivityRecord::Field(f.clone()));
                let slot = self.fields.entry(f.issue_id).or_default();
                if slot
                    .get(&f.field)
                    .is_none_or(|cur| newer(f.created_at, &f.author, cur.created_at, &cur.author))
                {
                    slot.insert(f.field, f);
                }
            }
            HeadwayEvent::Comment(c) => {
                // Append-only and immutable: keep the first sighting; later
                // duplicates of the same id are no-ops.
                self.comments.entry(c.id).or_insert(c);
            }
            HeadwayEvent::Relation(r) => {
                self.remember(r.child_id, ActivityRecord::Relation(r.clone()));
                if self
                    .relations
                    .get(&r.child_id)
                    .is_none_or(|cur| newer(r.created_at, &r.author, cur.created_at, &cur.author))
                {
                    self.relations.insert(r.child_id, r);
                }
            }
            HeadwayEvent::Sequence(s) => {
                // Deliberately not remembered into activity history: reseqs are
                // high-churn work-order shuffles and would bury meaningful events
                // (moves, renames) in noise. See the `birth-plate-alien` design.
                let key = (s.container.clone(), s.issue_id);
                if self
                    .seqs
                    .get(&key)
                    .is_none_or(|cur| newer(s.created_at, &s.author, cur.created_at, &cur.author))
                {
                    self.seqs.insert(key, s);
                }
            }
        }
    }

    /// Retain `record` on `issue`'s activity history. Value-deduped by the
    /// set, so duplicate relay deliveries are no-ops and ingest stays
    /// idempotent and commutative.
    fn remember(&mut self, issue: [u8; 32], record: ActivityRecord) {
        self.history.entry(issue).or_default().insert(record);
    }

    /// Derive `issue`'s activity timeline for the board being rendered: replay
    /// its retained history chronologically and emit a row for every visible
    /// state change (see [`ActivityKind`]). Rules that keep the timeline
    /// honest rather than noisy:
    ///
    /// - Unauthorised events are ignored, exactly like the overlays.
    /// - Records stamped at (or before) the issue's own `created_at` are part
    ///   of card creation — the write paths stamp genuine amendments strictly
    ///   later (see `store::next_after`) — so only the `Created` row shows.
    /// - The first placement is where the card started, not a move; placements
    ///   on other boards and same-column re-ranks (drag reorders) are skipped.
    /// - Label rows are the *diff* between consecutive authorised sets.
    fn card_activity(
        &self,
        issue: &IssueEvent,
        board_author: &[u8; 32],
        board_id: &str,
    ) -> Vec<ActivityView> {
        let authorised = |who: &[u8; 32]| who == &issue.author || who == board_author;
        let mut out = vec![ActivityView {
            author: issue.author,
            created_at: issue.created_at,
            kind: ActivityKind::Created,
        }];
        let Some(records) = self.history.get(&issue.id) else {
            return out;
        };

        let board = self
            .boards
            .get(&(board_author.to_vec(), board_id.to_owned()));
        let col_name = |col: &str| {
            board
                .and_then(|b| b.columns.iter().find(|c| c.id == col))
                .map(|c| c.name.clone())
                .unwrap_or_else(|| col.to_owned())
        };
        let col_idx = |col: &str| board.and_then(|b| b.columns.iter().position(|c| c.id == col));

        // Chronological replay; the stable sort keeps the set's deterministic
        // order for same-second records.
        let mut sorted: Vec<&ActivityRecord> = records.iter().collect();
        sorted.sort_by_key(|r| r.created_at());

        // Running state the diffs are computed against.
        let mut prev_col: Option<&str> = None;
        let mut labels: Vec<&str> = issue.inline_labels.iter().map(String::as_str).collect();
        labels.sort_unstable();
        labels.dedup();
        let mut has_parent = false;
        // The last wire value seen per scalar field, so a field row is emitted
        // only on an actual change (an empty entry means "never set").
        let mut field_values: HashMap<Field, String> = HashMap::new();

        for rec in sorted {
            // Creation-time records still seed the running state (so the first
            // post-creation diff is computed against them) but emit no row.
            let silent = rec.created_at() <= issue.created_at;
            match rec {
                ActivityRecord::Placement(p) => {
                    if p.board_author != *board_author
                        || p.board_id != board_id
                        || !authorised(&p.author)
                    {
                        continue;
                    }
                    let from = prev_col.replace(p.col.as_str());
                    if silent || from.is_none() || from == Some(p.col.as_str()) {
                        continue;
                    }
                    let kind = match (p.col.as_str(), from) {
                        (COL_DELETED, _) => continue,
                        (COL_ARCHIVED, _) => ActivityKind::Archived,
                        (to, Some(COL_ARCHIVED | COL_DELETED)) => ActivityKind::Restored {
                            to: col_name(to),
                            to_idx: col_idx(to),
                        },
                        (to, from) => ActivityKind::Moved {
                            from: from.map(col_name),
                            to: col_name(to),
                            to_idx: col_idx(to),
                        },
                    };
                    out.push(ActivityView {
                        author: p.author,
                        created_at: p.created_at,
                        kind,
                    });
                }
                ActivityRecord::Subject(s) => {
                    if silent || !authorised(&s.author) {
                        continue;
                    }
                    out.push(ActivityView {
                        author: s.author,
                        created_at: s.created_at,
                        kind: ActivityKind::Renamed {
                            to: s.subject.clone(),
                        },
                    });
                }
                ActivityRecord::Cover(c) => {
                    if silent || !authorised(&c.author) {
                        continue;
                    }
                    out.push(ActivityView {
                        author: c.author,
                        created_at: c.created_at,
                        kind: ActivityKind::DescriptionEdited,
                    });
                }
                ActivityRecord::Labels(l) => {
                    if !authorised(&l.author) {
                        continue;
                    }
                    let mut new: Vec<&str> = l.labels.iter().map(String::as_str).collect();
                    new.sort_unstable();
                    new.dedup();
                    let added: Vec<String> = new
                        .iter()
                        .filter(|x| !labels.contains(x))
                        .map(|x| x.to_string())
                        .collect();
                    let removed: Vec<String> = labels
                        .iter()
                        .filter(|x| !new.contains(x))
                        .map(|x| x.to_string())
                        .collect();
                    labels = new;
                    if silent || (added.is_empty() && removed.is_empty()) {
                        continue;
                    }
                    out.push(ActivityView {
                        author: l.author,
                        created_at: l.created_at,
                        kind: ActivityKind::LabelsChanged { added, removed },
                    });
                }
                ActivityRecord::Field(f) => {
                    if !authorised(&f.author) {
                        continue;
                    }
                    // Normalise "no value" so priority's explicit "none" and an
                    // empty due/estimate both read as cleared and don't churn.
                    let norm = |v: &str| match f.field {
                        Field::Priority if Priority::parse(v) == Priority::None => String::new(),
                        _ => v.trim().to_string(),
                    };
                    let to = norm(&f.value);
                    let changed = field_values.get(&f.field) != Some(&to);
                    field_values.insert(f.field, to.clone());
                    if silent || !changed {
                        continue;
                    }
                    out.push(ActivityView {
                        author: f.author,
                        created_at: f.created_at,
                        kind: ActivityKind::FieldChanged { field: f.field, to },
                    });
                }
                ActivityRecord::Relation(r) => {
                    if !self.relation_authorised(r, board_author) {
                        continue;
                    }
                    let was = has_parent;
                    has_parent = r.parent_id.is_some();
                    if silent {
                        continue;
                    }
                    let kind = match r.parent_id {
                        Some(p) => ActivityKind::ParentSet {
                            parent: NoteId::new(p),
                            title: self.card_title(&p, board_author),
                        },
                        // A detach with no prior attach says nothing.
                        None if !was => continue,
                        None => ActivityKind::ParentRemoved,
                    };
                    out.push(ActivityView {
                        author: r.author,
                        created_at: r.created_at,
                        kind,
                    });
                }
            }
        }

        out
    }

    /// Resolve an issue's effective title (subject overlay applied), for
    /// naming other cards inside activity rows. `None` if the issue is unknown.
    fn card_title(&self, issue_id: &[u8; 32], board_author: &[u8; 32]) -> Option<String> {
        let issue = self.issues.get(issue_id)?;
        let authorised = |who: &[u8; 32]| who == &issue.author || who == board_author;
        Some(
            self.subjects
                .get(issue_id)
                .filter(|s| authorised(&s.author))
                .map(|s| s.subject.clone())
                .unwrap_or_else(|| issue.subject.clone()),
        )
    }

    /// A relation is honoured when its author is the child's author, the named
    /// parent's author, or the board author — the authorised set of the other
    /// overlays extended to both endpoints of the edge.
    fn relation_authorised(&self, r: &RelationEvent, board_author: &[u8; 32]) -> bool {
        if &r.author == board_author {
            return true;
        }
        if self
            .issues
            .get(&r.child_id)
            .is_some_and(|c| c.author == r.author)
        {
            return true;
        }
        r.parent_id
            .and_then(|p| self.issues.get(&p))
            .is_some_and(|p| p.author == r.author)
    }

    /// Resolve one child of a parent card into a [`SubissueView`], deriving its
    /// doneness from its placements. Returns `None` when the child issue is
    /// unknown or has been tombstoned off every board it was placed on (it
    /// vanishes from the parent exactly like it vanishes from boards).
    /// `board_id`/`board_author` are the board being rendered, used to prefer
    /// its column when the child is placed on several boards.
    fn subissue_view(
        &self,
        child_id: &[u8; 32],
        board_author: &[u8; 32],
        board_id: &str,
        seq: Option<String>,
    ) -> Option<SubissueView> {
        let child = self.issues.get(child_id)?;
        let authorised = |who: &[u8; 32]| who == &child.author || who == board_author;

        let title = self
            .subjects
            .get(child_id)
            .filter(|s| authorised(&s.author))
            .map(|s| s.subject.clone())
            .unwrap_or_else(|| child.subject.clone());

        /// One live (non-deleted, non-archived) placement of the child, with its
        /// per-board doneness already judged.
        struct LivePlacement<'a> {
            board_author: &'a [u8; 32],
            board_id: &'a str,
            col: &'a str,
            /// Done on that board = sitting in its last column.
            done: bool,
        }

        // The child's winning placements, one per board, authorised like the
        // board fold: by the child's author or that placement's board author.
        let mut placed = 0usize;
        let mut archived_somewhere = false;
        let mut live: Vec<LivePlacement> = Vec::new();

        for (key, p) in &self.placements {
            if &key.issue_id != child_id
                || (p.author != child.author && p.author != key.board_author)
            {
                continue;
            }
            placed += 1;
            match p.col.as_str() {
                COL_DELETED => {}
                COL_ARCHIVED => archived_somewhere = true,
                col => {
                    let done = self
                        .boards
                        .get(&(key.board_author.to_vec(), key.board_id.clone()))
                        .and_then(|b| b.columns.last())
                        .is_some_and(|last| last.id == col);
                    live.push(LivePlacement {
                        board_author: &key.board_author,
                        board_id: &key.board_id,
                        col,
                        done,
                    });
                }
            }
        }

        // Every placement is a tombstone: the child is deleted, drop it.
        if placed > 0 && live.is_empty() && !archived_somewhere {
            return None;
        }

        // Prefer the rendered board's column; else the first by board id so the
        // result doesn't churn with hash order.
        live.sort_by(|a, b| (a.board_author, a.board_id).cmp(&(b.board_author, b.board_id)));
        let column = live
            .iter()
            .find(|p| p.board_author == board_author && p.board_id == board_id)
            .or_else(|| live.first())
            .map(|p| p.col.to_owned());

        let archived = live.is_empty() && archived_somewhere;
        let done = if live.is_empty() {
            archived
        } else {
            live.iter().all(|p| p.done)
        };

        Some(SubissueView {
            id: NoteId::new(*child_id),
            title,
            column,
            done,
            archived,
            seq,
        })
    }

    /// Resolve a card's effective content (title, description, labels, comments)
    /// from the issue and its overlay events, given the `rank`/`placed_at` of the
    /// placement it's being shown under. `board_author` is the authority alongside
    /// the card author for amend events. Board-agnostic: the same issue placed on
    /// two boards yields the same content, only the rank/slot differ (`board_id`
    /// is only a display preference for subissue columns, not authority).
    fn card_view(
        &self,
        issue: &IssueEvent,
        board_author: &[u8; 32],
        board_id: &str,
        rank: String,
        placed_at: u64,
    ) -> CardView {
        // Authority: the card author or the board author may amend the card.
        let authorised = |who: &[u8; 32]| who == &issue.author || who == board_author;

        let subject = self
            .subjects
            .get(&issue.id)
            .filter(|s| authorised(&s.author));
        let title = subject
            .map(|s| s.subject.clone())
            .unwrap_or_else(|| issue.subject.clone());

        let cover = self.covers.get(&issue.id).filter(|c| authorised(&c.author));
        let description = cover
            .map(|c| c.body.clone())
            .unwrap_or_else(|| issue.body.clone());

        // Labels resolve latest-authorised-wins: the newest authorised label
        // event is the card's complete set, overriding the issue's inline labels.
        // (Removal = republish the set without the label.)
        let label_set = self.labels.get(&issue.id).filter(|s| authorised(&s.author));
        let mut labels = label_set
            .map(|s| s.labels.clone())
            .unwrap_or_else(|| issue.inline_labels.clone());
        labels.sort();
        labels.dedup();

        // Scalar field overlays (priority/due/estimate) resolve
        // latest-authorised-wins from the per-issue field slots; an unauthorised
        // edit is ignored, leaving the field unset. Each value is parsed into its
        // typed form here at the read site.
        let field_slots = self.fields.get(&issue.id);
        let field = |f: Field| {
            field_slots
                .and_then(|m| m.get(&f))
                .filter(|e| authorised(&e.author))
        };
        let priority = field(Field::Priority).map_or(Priority::None, |e| Priority::parse(&e.value));
        let due = field(Field::Due).and_then(|e| Date::parse(&e.value));
        let estimate = field(Field::Estimate).and_then(|e| e.value.trim().parse::<u32>().ok());
        // Newest authorised field edit, folded into `updated_at` below.
        let fields_touched = field_slots.map_or(0, |m| {
            m.values()
                .filter(|e| authorised(&e.author))
                .map(|e| e.created_at)
                .max()
                .unwrap_or(0)
        });

        // Comments thread under the issue (the NIP-22 root). Append-only, shown
        // oldest first; the id breaks same-second ties.
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
        comments.sort_by(|a, b| (a.created_at, a.id.bytes()).cmp(&(b.created_at, b.id.bytes())));

        // The newest touch wins: creation, the winning amendments, or the last
        // comment. Placements deliberately don't count (see the field docs).
        let updated_at = issue
            .created_at
            .max(subject.map_or(0, |s| s.created_at))
            .max(cover.map_or(0, |c| c.created_at))
            .max(label_set.map_or(0, |l| l.created_at))
            .max(fields_touched)
            .max(comments.last().map_or(0, |c| c.created_at));

        // This card as a child: its one relation slot names its parent.
        let parent = self
            .relations
            .get(&issue.id)
            .filter(|r| self.relation_authorised(r, board_author))
            .and_then(|r| r.parent_id)
            .map(NoteId::new);

        // This card as a parent: every issue whose authorised relation names it.
        // One level only — a cycle renders as two cards pointing at each other,
        // never a loop (the write path refuses to create one; see store::apply).
        let children: Vec<&RelationEvent> = self
            .relations
            .values()
            .filter(|r| r.parent_id.as_ref() == Some(&issue.id))
            .filter(|r| self.relation_authorised(r, board_author))
            .collect();
        // Each child's work-order rank is scoped to THIS card as its container,
        // authorised like the relation edge: the child's author, this parent's
        // author, or the board author may sequence it.
        let child_seq = |child_id: &[u8; 32]| -> Option<String> {
            let entry = self.seqs.get(&(Container::Card(issue.id), *child_id))?;
            let child_author = self.issues.get(child_id).map(|c| c.author);
            let ok = &entry.author == board_author
                || child_author == Some(entry.author)
                || entry.author == issue.author;
            ok.then(|| entry.rank.clone())
        };
        let mut children: Vec<(&RelationEvent, Option<String>)> = children
            .into_iter()
            .map(|r| {
                let seq = child_seq(&r.child_id);
                (r, seq)
            })
            .collect();
        // Sequenced children lead in rank order; unsequenced fall back to creation
        // order (`created_at`, then id). `is_none()` sorts false < true, so a
        // sequenced (`Some`) child always precedes an unsequenced (`None`) one.
        children.sort_by_cached_key(|(r, seq)| {
            let created = self
                .issues
                .get(&r.child_id)
                .map_or(u64::MAX, |c| c.created_at);
            (
                seq.is_none(),
                seq.clone().unwrap_or_default(),
                created,
                r.child_id,
            )
        });
        let subissues = children
            .into_iter()
            .filter_map(|(r, seq)| self.subissue_view(&r.child_id, board_author, board_id, seq))
            .collect();

        // Board-root work-order rank for this card, authorised like its own
        // overlays (the card author or the board author may sequence it).
        let seq = self
            .seqs
            .get(&(Container::BoardRoot(board_id.to_string()), issue.id))
            .filter(|e| authorised(&e.author))
            .map(|e| e.rank.clone());

        CardView {
            id: NoteId::new(issue.id),
            author: issue.author,
            title,
            description,
            labels,
            priority,
            due,
            estimate,
            rank,
            seq,
            placed_at,
            created_at: issue.created_at,
            updated_at,
            comments,
            activity: self.card_activity(issue, board_author, board_id),
            parent,
            subissues,
        }
    }

    /// Assemble the accumulated events into board views.
    /// Resolve the accumulated events into the boards they describe. Takes
    /// `&self` so a cached reducer can be re-finalized after a delta without
    /// being consumed.
    #[profiling::function]
    pub fn finalize(&self) -> Vec<BoardView> {
        let mut views: Vec<BoardView> = Vec::new();

        // Issues with a live placement on *some* board. An issue with none is a
        // placement-less orphan (e.g. its placement event never reached us) and
        // is shown via its origin board's `a` tag below; one that was explicitly
        // moved/deleted has a placement and so is governed purely by placements.
        let placed_anywhere: HashSet<[u8; 32]> =
            self.placements.keys().map(|k| k.issue_id).collect();

        for ((author, board_id), board) in &self.boards {
            // Group this board's cards by resolved column id.
            let mut by_col: HashMap<String, Vec<CardView>> = HashMap::new();
            let mut fallback: Vec<(u64, CardView)> = Vec::new();
            let mut archived: Vec<ArchivedCard> = Vec::new();
            let col_ids: Vec<&str> = board.columns.iter().map(|c| c.id.as_str()).collect();

            // Placement-driven membership: each live placement targeting this
            // board puts its issue on the board, in the placement's column.
            for (key, placement) in &self.placements {
                if key.board_author.as_slice() != author.as_slice() || &key.board_id != board_id {
                    continue;
                }
                let Some(issue) = self.issues.get(&key.issue_id) else {
                    continue;
                };
                // Only the card author or the board author may place a card.
                if placement.author != issue.author && placement.author != board.author {
                    continue;
                }

                let card = self.card_view(
                    issue,
                    &board.author,
                    board_id,
                    placement.rank.clone(),
                    placement.created_at,
                );

                match placement.col.as_str() {
                    // A tombstone placement removes the card from the board.
                    COL_DELETED => continue,
                    // Archived: kept off the columns but recoverable, with its
                    // origin column so a restore lands it back where it was.
                    COL_ARCHIVED => archived.push(ArchivedCard {
                        card,
                        from: placement.from.clone(),
                    }),
                    col if col_ids.contains(&col) => {
                        by_col.entry(col.to_string()).or_default().push(card);
                    }
                    _ => fallback.push((issue.created_at, card)),
                }
            }

            // Orphan fallback: issues anchored to this board by their `a` tag but
            // with no placement on any board (a lost placement event). Show them
            // so a card never vanishes just because its placement didn't arrive.
            for issue in self.issues.values() {
                if issue.board_author.as_slice() != author.as_slice()
                    || &issue.board_id != board_id
                    || placed_anywhere.contains(&issue.id)
                {
                    continue;
                }
                let card = self.card_view(issue, &board.author, board_id, String::new(), 0);
                fallback.push((issue.created_at, card));
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
pub const HEADWAY_KINDS: [u32; 8] = [
    KIND_BOARD,
    KIND_ISSUE,
    KIND_PLACEMENT,
    KIND_LABEL,
    KIND_COVER_NOTE,
    KIND_COMMENT,
    KIND_RELATION,
    KIND_SEQUENCE,
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
#[profiling::function]
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
#[profiling::function]
pub fn reduce_delta(reducer: &mut BoardReducer, ndb: &Ndb, txn: &Transaction, keys: &[NoteKey]) {
    for key in keys {
        if let Ok(note) = ndb.get_note_by_key(txn, *key)
            && let Some(event) = parse(&note)
        {
            reducer.ingest(event);
        }
    }
}

/// Find the board with `board_id` authored by `author` in an *already-finalized*
/// board set, without re-finalizing. The steady-state inline path finalizes a
/// reducer once per frame (memoized) and then resolves every reference against
/// that one `&[BoardView]` through this, rather than re-walking the reducer per
/// reference (see [`pick_board`], which finalizes on each call).
pub fn find_board<'a>(
    boards: &'a [BoardView],
    author: &Pubkey,
    board_id: &str,
) -> Option<&'a BoardView> {
    boards
        .iter()
        .find(|v| v.id == board_id && &v.author == author.bytes())
}

/// Pick the board with `board_id` authored by `author` out of a reducer's
/// resolved boards, if it exists. Finalizes the reducer; a caller resolving many
/// references against one reducer per frame should finalize once and reuse the
/// result via [`find_board`] instead.
#[profiling::function]
pub fn pick_board(reducer: &BoardReducer, author: &Pubkey, board_id: &str) -> Option<BoardView> {
    find_board(&reducer.finalize(), author, board_id).cloned()
}

/// Pick a single card's *resolved* [`CardView`] (latest subject, labels, cover
/// and placement applied) out of a folded board, by the issue's note id.
/// Searches the live columns and the archived set. `None` if the board or the
/// card within it is absent. Unlike parsing the kind-1621 note directly — which
/// only yields its creation-time snapshot — this reflects later edits.
#[profiling::function]
pub fn pick_card(
    reducer: &BoardReducer,
    author: &Pubkey,
    board_id: &str,
    issue_id: &[u8; 32],
) -> Option<CardView> {
    card_in_board(&pick_board(reducer, author, board_id)?, issue_id)
}

/// Pick a card's resolved [`CardView`] out of an *already-finalized* board,
/// searching its live columns then its archived set. The re-finalize-free core of
/// [`pick_card`]: the inline render path finalizes once per frame and resolves
/// each referenced card through this.
pub fn card_in_board(view: &BoardView, issue_id: &[u8; 32]) -> Option<CardView> {
    let want = NoteId::new(*issue_id);
    view.columns
        .iter()
        .flat_map(|col| col.cards.iter())
        .chain(view.archived.iter().map(|a| &a.card))
        .find(|card| card.id == want)
        .cloned()
}

/// A card's position among a board's live columns: which column it sits in and
/// how many columns there are. Enough to derive a positional (Linear-style)
/// status indicator, which maps the first column to backlog and the last to done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnPos {
    /// Zero-based index of the card's column in board order.
    pub index: usize,
    /// Total number of live columns on the board.
    pub count: usize,
}

/// A card resolved for inline display: its [`CardView`] plus the live
/// [`ColumnPos`] used to show a status indicator. See [`pick_card_with_column`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCard {
    /// The card's resolved state (latest subject, labels and cover applied).
    pub card: CardView,
    /// The card's live column position, or `None` when it is archived (not in a
    /// live column).
    pub column: Option<ColumnPos>,
}

/// Like [`pick_card`], but also resolves the card's live [`ColumnPos`] so an
/// inline reference can show a status indicator. Returns `None` when the board
/// or card is absent.
#[profiling::function]
pub fn pick_card_with_column(
    reducer: &BoardReducer,
    author: &Pubkey,
    board_id: &str,
    issue_id: &[u8; 32],
) -> Option<ResolvedCard> {
    card_with_column_in_board(&pick_board(reducer, author, board_id)?, issue_id)
}

/// Resolve a card *and* its live [`ColumnPos`] out of an *already-finalized*
/// board. The re-finalize-free core of [`pick_card_with_column`]: the inline chip
/// render path finalizes once per frame and resolves each referenced card through
/// this.
pub fn card_with_column_in_board(view: &BoardView, issue_id: &[u8; 32]) -> Option<ResolvedCard> {
    let want = NoteId::new(*issue_id);
    let count = view.columns.len();
    for (index, col) in view.columns.iter().enumerate() {
        if let Some(card) = col.cards.iter().find(|c| c.id == want) {
            return Some(ResolvedCard {
                card: card.clone(),
                column: Some(ColumnPos { index, count }),
            });
        }
    }
    view.archived
        .iter()
        .map(|a| &a.card)
        .find(|card| card.id == want)
        .cloned()
        .map(|card| ResolvedCard { card, column: None })
}

/// A card resolved for inline display together with the board it currently lives
/// on. For a card that was moved across boards this is the *destination* board —
/// where [`finalize`](BoardReducer::finalize) actually places it — not the origin
/// board recorded in the card's `a` tag. See [`locate_card`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedCard {
    /// The board the card is live on — the [`BoardView`] this resolution came
    /// from, and the board a click on the card should open.
    pub board_id: String,
    /// The card's resolved state (latest subject, labels and cover applied).
    pub card: CardView,
    /// The card's live [`ColumnPos`], or `None` when it is archived (off the live
    /// columns).
    pub column: Option<ColumnPos>,
}

/// Resolve a card for inline display across *every* board `author` owns, rather
/// than assuming the board recorded in the card's `a` tag.
///
/// A card's `a` tag records its *origin* board, but a cross-board move deletes it
/// there and places it on the destination — membership follows the live placement
/// ([`finalize`](BoardReducer::finalize) is placement-driven). Resolving against
/// the stale `a`-tag board would find the card deleted (or absent) and render an
/// "invalid" chip that opens the wrong board, so we scan the finalized boards and
/// return the card where it is actually shown.
///
/// A live column placement is preferred over an archived one, and among live
/// boards the newest placement (by [`CardView::placed_at`]) wins — a card is
/// normally placed on exactly one board, so a move is unambiguous; a genuine
/// multi-board placement resolves to its most-recently-touched board. `None` when
/// the card is on no board of this author (deleted everywhere, or its board isn't
/// folded — the caller falls back to the card's creation-time snapshot).
pub fn locate_card(
    reducer: &BoardReducer,
    author: &Pubkey,
    issue_id: &[u8; 32],
) -> Option<LocatedCard> {
    locate_card_in_boards(&reducer.finalize(), author, issue_id)
}

/// Resolve a card across an *already-finalized* board set, preferring a live
/// column placement over an archived one and the newest placement among live
/// boards. The re-finalize-free core of [`locate_card`]: the inline render path
/// finalizes once per frame (memoized) and locates each referenced card through
/// this, mirroring [`card_with_column_in_board`]'s split from [`pick_card_with_column`].
pub fn locate_card_in_boards(
    boards: &[BoardView],
    author: &Pubkey,
    issue_id: &[u8; 32],
) -> Option<LocatedCard> {
    let want = NoteId::new(*issue_id);
    boards
        .iter()
        .filter(|board| &board.author == author.bytes())
        .filter_map(|board| {
            let count = board.columns.len();
            // A live column hit resolves to a status; prefer it over archived.
            for (index, col) in board.columns.iter().enumerate() {
                if let Some(card) = col.cards.iter().find(|c| c.id == want) {
                    return Some(LocatedCard {
                        board_id: board.id.clone(),
                        card: card.clone(),
                        column: Some(ColumnPos { index, count }),
                    });
                }
            }
            board
                .archived
                .iter()
                .map(|a| &a.card)
                .find(|c| c.id == want)
                .cloned()
                .map(|card| LocatedCard {
                    board_id: board.id.clone(),
                    card,
                    column: None,
                })
        })
        .max_by(|a, b| {
            a.column
                .is_some()
                .cmp(&b.column.is_some())
                .then(a.card.placed_at.cmp(&b.card.placed_at))
        })
}

/// Fold `author`'s headway events out of `ndb` and reduce them into the board
/// with the given `board_id`, if it exists. A one-shot [`fold_board`] +
/// [`pick_board`] for callers that don't keep the reducer around.
#[profiling::function]
pub fn load_board(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    board_id: &str,
) -> Option<BoardView> {
    pick_board(&fold_board(ndb, txn, author)?, author, board_id)
}

/// Every card on `view` in a stable order — the live columns' cards, in column
/// then rank order, followed by the archived cards. Shared by callers that
/// resolve a card by re-encoding each one (word ids, hex prefixes).
pub fn all_cards(view: &BoardView) -> impl Iterator<Item = &CardView> {
    view.columns
        .iter()
        .flat_map(|c| c.cards.iter())
        .chain(view.archived.iter().map(|a| &a.card))
}

/// Resolve a card on `view` by its three-word id (`word-word-word`, *without*
/// the board slug prefix) by re-encoding every card and matching — exactly how a
/// git short hash resolves, and how the CLI's `resolve_card` matches word ids.
/// Archived cards are included. `None` if no card encodes to `words`.
///
/// Shared by the CLI and the inline `headway` [reference
/// parser](../../notedeck_headway) so both agree on what a word id resolves to.
#[profiling::function]
pub fn resolve_card_by_wordid(view: &BoardView, words: &str) -> Option<NoteId> {
    all_cards(view)
        .find(|c| crate::wordid::encode(c.id.bytes()) == words)
        .map(|c| c.id)
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

    /// `created_at` pins to the immutable issue event; `updated_at` follows the
    /// newest amendment or comment and doesn't count placements (moves are
    /// tracked by `placed_at`).
    #[test]
    fn reduce_resolves_card_timestamps() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        // Explicit timestamps make the issue id (and the fold) deterministic.
        let i1 = note_id(&owner, build_issue(&addr, "First", "").created_at(1_000));
        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "First", "").created_at(1_000), &owner),
            parse_owned(
                build_placement("b1", &addr, &i1, "todo", "m").created_at(5_000),
                &owner,
            ),
        ];

        // Untouched card: updated_at falls back to creation, and the (later)
        // placement doesn't drag it forward.
        let card = reduce(&events)[0].columns[0].cards[0].clone();
        assert_eq!(card.created_at, 1_000);
        assert_eq!(card.updated_at, 1_000);

        // A rename bumps updated_at without moving created_at.
        events.push(parse_owned(
            build_subject_edit(&i1, "Renamed").created_at(2_000),
            &owner,
        ));
        let card = reduce(&events)[0].columns[0].cards[0].clone();
        assert_eq!(card.created_at, 1_000);
        assert_eq!(card.updated_at, 2_000);

        // A comment counts as an update too.
        events.push(parse_owned(
            build_comment(&i1, &owner.pubkey, None, "hi").created_at(3_000),
            &owner,
        ));
        assert_eq!(reduce(&events)[0].columns[0].cards[0].updated_at, 3_000);
    }

    /// The activity timeline replays a card's full history: creation-time
    /// events are silent (only `Created` shows), then every move, rename,
    /// label diff, description edit, archive/restore and parent change gets a
    /// chronological row. Unauthorised events and same-column re-ranks don't.
    #[test]
    fn reduce_derives_activity_timeline() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("doing", "Doing"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let card = note_id(&owner, build_issue(&addr, "Card", "").created_at(1_000));
        let parent = note_id(&owner, build_issue(&addr, "Epic", "").created_at(900));
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Epic", "").created_at(900), &owner),
            parse_owned(build_issue(&addr, "Card", "").created_at(1_000), &owner),
            // Creation-time placement + labels: part of creation, no rows.
            parse_owned(
                build_placement("b1", &addr, &card, "todo", "m").created_at(1_000),
                &owner,
            ),
            parse_owned(build_labels(&card, &["bug"]).created_at(1_000), &owner),
            // The history proper, one event per second.
            parse_owned(
                build_placement("b1", &addr, &card, "doing", "m").created_at(2_000),
                &owner,
            ),
            // Same-column re-rank (drag reorder): no row.
            parse_owned(
                build_placement("b1", &addr, &card, "doing", "t").created_at(2_500),
                &owner,
            ),
            parse_owned(
                build_subject_edit(&card, "Card v2").created_at(3_000),
                &owner,
            ),
            // A stranger's rename is ignored, exactly like the overlays.
            parse_owned(
                build_subject_edit(&card, "hijacked").created_at(3_500),
                &stranger,
            ),
            parse_owned(
                build_labels(&card, &["bug", "ui"]).created_at(4_000),
                &owner,
            ),
            parse_owned(build_labels(&card, &["ui"]).created_at(5_000), &owner),
            parse_owned(
                build_cover_note(&card, &owner.pubkey, "details").created_at(6_000),
                &owner,
            ),
            parse_owned(
                build_archive_placement("b1", &addr, &card, "doing", "t").created_at(7_000),
                &owner,
            ),
            parse_owned(
                build_placement("b1", &addr, &card, "doing", "t").created_at(8_000),
                &owner,
            ),
            parse_owned(
                build_relation(&card, Some(&parent)).created_at(9_000),
                &owner,
            ),
            parse_owned(build_relation(&card, None).created_at(10_000), &owner),
        ];

        let view = &reduce(&events)[0];
        let card = view.columns[1]
            .cards
            .iter()
            .find(|c| c.id == card)
            .expect("card in doing");

        let kinds: Vec<&ActivityKind> = card.activity.iter().map(|a| &a.kind).collect();
        assert_eq!(
            kinds,
            vec![
                &ActivityKind::Created,
                &ActivityKind::Moved {
                    from: Some("Todo".into()),
                    to: "Doing".into(),
                    to_idx: Some(1),
                },
                &ActivityKind::Renamed {
                    to: "Card v2".into()
                },
                &ActivityKind::LabelsChanged {
                    added: vec!["ui".into()],
                    removed: vec![],
                },
                &ActivityKind::LabelsChanged {
                    added: vec![],
                    removed: vec!["bug".into()],
                },
                &ActivityKind::DescriptionEdited,
                &ActivityKind::Archived,
                &ActivityKind::Restored {
                    to: "Doing".into(),
                    to_idx: Some(1),
                },
                &ActivityKind::ParentSet {
                    parent,
                    title: Some("Epic".into()),
                },
                &ActivityKind::ParentRemoved,
            ]
        );
        // Rows are chronological and stamped with the underlying events' times.
        assert_eq!(card.activity[0].created_at, 1_000);
        assert_eq!(card.activity[1].created_at, 2_000);
        assert!(
            card.activity
                .windows(2)
                .all(|w| w[0].created_at <= w[1].created_at)
        );
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
    fn priority_parses_and_orders() {
        assert_eq!(Priority::parse("Urgent"), Priority::Urgent);
        assert_eq!(Priority::parse(" high "), Priority::High);
        assert_eq!(Priority::parse("med"), Priority::Medium);
        assert_eq!(Priority::parse("none"), Priority::None);
        assert_eq!(Priority::parse("nonsense"), Priority::None);
        // "no priority" sorts below every real priority (Linear ordering).
        assert!(Priority::None < Priority::Low);
        assert!(Priority::Low < Priority::Urgent);
        assert_eq!(Priority::High.as_str(), "high");
    }

    #[test]
    fn date_parses_and_orders() {
        assert_eq!(
            Date::parse("2026-07-30"),
            Some(Date {
                year: 2026,
                month: 7,
                day: 30
            })
        );
        assert_eq!(Date::parse("2024-02-29").map(|d| d.day), Some(29)); // leap
        assert_eq!(Date::parse("2026-02-29"), None); // not a leap year
        assert_eq!(Date::parse("2026-13-01"), None); // bad month
        assert_eq!(Date::parse("nonsense"), None);
        assert!(Date::parse("2026-01-31") < Date::parse("2026-02-01"));
        assert_eq!(Date::parse("2026-07-30").unwrap().to_string(), "2026-07-30");
    }

    #[test]
    fn reduce_resolves_scalar_fields_latest_authorised() {
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
                build_field(&i1, Field::Priority, Priority::Low.as_str()),
                &owner,
            ),
            parse_owned(build_field(&i1, Field::Due, "2026-07-30"), &owner),
            parse_owned(build_field(&i1, Field::Estimate, "3"), &owner),
        ];
        let card = |events: &[HeadwayEvent]| reduce(events)[0].columns[0].cards[0].clone();
        let c = card(&events);
        assert_eq!(c.priority, Priority::Low);
        assert_eq!(c.due.unwrap().to_string(), "2026-07-30");
        assert_eq!(c.estimate, Some(3));

        // A later priority overlay wins latest-wins — raise it to Urgent.
        let mut bumped = match parse_owned(
            build_field(&i1, Field::Priority, Priority::Urgent.as_str()),
            &owner,
        ) {
            HeadwayEvent::Field(f) => f,
            _ => unreachable!(),
        };
        bumped.created_at += 1;
        events.push(HeadwayEvent::Field(bumped));
        assert_eq!(card(&events).priority, Priority::Urgent);

        // Clearing one field republishes an empty value; fields are independent.
        let mut cleared = match parse_owned(build_field(&i1, Field::Due, ""), &owner) {
            HeadwayEvent::Field(f) => f,
            _ => unreachable!(),
        };
        cleared.created_at += 2;
        events.push(HeadwayEvent::Field(cleared));
        let c = card(&events);
        assert_eq!(c.due, None);
        assert_eq!(c.priority, Priority::Urgent); // other fields untouched
        assert_eq!(c.estimate, Some(3));
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

    /// Membership is placement-driven: one issue placed on two boards appears on
    /// both, and removing the placement from one board leaves it on the other
    /// (the same card, not a copy).
    #[test]
    fn reduce_places_one_card_on_multiple_boards() {
        let owner = FullKeypair::generate();
        let addr1 = board_address(&owner.pubkey, "b1");
        let addr2 = board_address(&owner.pubkey, "b2");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        // Anchored to b1, but placed on both b1 and b2 (the second is a "link").
        let card = note_id(&owner, build_issue(&addr1, "Shared", ""));
        let mut events = vec![
            parse_owned(build_board("b1", "One", "", &cols), &owner),
            parse_owned(build_board("b2", "Two", "", &cols), &owner),
            parse_owned(build_issue(&addr1, "Shared", ""), &owner),
            parse_owned(build_placement("b1", &addr1, &card, "todo", "m"), &owner),
            parse_owned(build_placement("b2", &addr2, &card, "todo", "m"), &owner),
        ];

        // Views sort by board id: [b1, b2]. The card shows on both.
        let views = reduce(&events);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].columns[0].cards.len(), 1, "on b1");
        assert_eq!(views[1].columns[0].cards.len(), 1, "on b2");
        assert_eq!(views[1].columns[0].cards[0].title, "Shared");

        // Remove it from b1 (a tombstone placement on b1 only). b2 keeps it.
        let mut tombstone = match parse_owned(
            build_placement("b1", &addr1, &card, COL_DELETED, "m"),
            &owner,
        ) {
            HeadwayEvent::Placement(p) => p,
            _ => unreachable!(),
        };
        tombstone.created_at += 1;
        events.push(HeadwayEvent::Placement(tombstone));

        let views = reduce(&events);
        assert!(views[0].columns[0].cards.is_empty(), "removed from b1");
        assert_eq!(views[1].columns[0].cards.len(), 1, "still on b2");
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

    /// Parent/child resolve on both ends: the child gains a `parent` pointer and
    /// the parent lists its children with doneness derived from their columns
    /// (last column of the board = done).
    #[test]
    fn reduce_resolves_subissues_with_positional_doneness() {
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

        let epic = note_id(&owner, build_issue(&addr, "Epic", "").created_at(1_000));
        let c1 = note_id(
            &owner,
            build_issue(&addr, "Child one", "").created_at(1_001),
        );
        let c2 = note_id(
            &owner,
            build_issue(&addr, "Child two", "").created_at(1_002),
        );

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Epic", "").created_at(1_000), &owner),
            parse_owned(
                build_issue(&addr, "Child one", "").created_at(1_001),
                &owner,
            ),
            parse_owned(
                build_issue(&addr, "Child two", "").created_at(1_002),
                &owner,
            ),
            parse_owned(build_placement("b1", &addr, &epic, "todo", "g"), &owner),
            // c1 done (last column), c2 still in todo.
            parse_owned(build_placement("b1", &addr, &c1, "done", "m"), &owner),
            parse_owned(build_placement("b1", &addr, &c2, "todo", "t"), &owner),
            parse_owned(build_relation(&c1, Some(&epic)), &owner),
            parse_owned(build_relation(&c2, Some(&epic)), &owner),
        ];

        let views = reduce(&events);
        let todo = &views[0].columns[0];

        let epic_card = todo.cards.iter().find(|c| c.id == epic).unwrap();
        assert_eq!(epic_card.parent, None);
        assert_eq!(epic_card.subissues.len(), 2);
        // Ordered by child created_at: c1 (done) then c2 (not).
        assert_eq!(epic_card.subissues[0].title, "Child one");
        assert!(epic_card.subissues[0].done);
        assert_eq!(epic_card.subissues[0].column.as_deref(), Some("done"));
        assert_eq!(epic_card.subissues[1].title, "Child two");
        assert!(!epic_card.subissues[1].done);
        assert_eq!(epic_card.subissues[1].column.as_deref(), Some("todo"));

        let child = todo.cards.iter().find(|c| c.id == c2).unwrap();
        assert_eq!(child.parent, Some(epic));
        assert!(child.subissues.is_empty());
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

    /// Subissues sort by sequence: sequenced children lead in rank order, then
    /// unsequenced ones fall back to creation order.
    #[test]
    fn reduce_orders_subissues_by_sequence() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];
        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };
        let epic = note_id(&owner, build_issue(&addr, "Epic", "").created_at(1_000));
        let c1 = note_id(
            &owner,
            build_issue(&addr, "Child one", "").created_at(1_001),
        );
        let c2 = note_id(
            &owner,
            build_issue(&addr, "Child two", "").created_at(1_002),
        );
        let c3 = note_id(
            &owner,
            build_issue(&addr, "Child three", "").created_at(1_003),
        );

        let epic_container = Container::Card(*epic.bytes());
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Epic", "").created_at(1_000), &owner),
            parse_owned(
                build_issue(&addr, "Child one", "").created_at(1_001),
                &owner,
            ),
            parse_owned(
                build_issue(&addr, "Child two", "").created_at(1_002),
                &owner,
            ),
            parse_owned(
                build_issue(&addr, "Child three", "").created_at(1_003),
                &owner,
            ),
            parse_owned(build_placement("b1", &addr, &epic, "todo", "g"), &owner),
            parse_owned(build_placement("b1", &addr, &c1, "todo", "h"), &owner),
            parse_owned(build_placement("b1", &addr, &c2, "todo", "i"), &owner),
            parse_owned(build_placement("b1", &addr, &c3, "todo", "j"), &owner),
            parse_owned(build_relation(&c1, Some(&epic)), &owner),
            parse_owned(build_relation(&c2, Some(&epic)), &owner),
            parse_owned(build_relation(&c3, Some(&epic)), &owner),
            // Sequence c3 before c2 within the epic; leave c1 unsequenced.
            parse_owned(build_sequence(&epic_container, &c3, "g"), &owner),
            parse_owned(build_sequence(&epic_container, &c2, "m"), &owner),
        ];

        let views = reduce(&events);
        let epic_card = views[0].columns[0]
            .cards
            .iter()
            .find(|c| c.id == epic)
            .unwrap();
        let order: Vec<&str> = epic_card
            .subissues
            .iter()
            .map(|s| s.title.as_str())
            .collect();
        assert_eq!(order, ["Child three", "Child two", "Child one"]);
        assert_eq!(epic_card.subissues[0].seq.as_deref(), Some("g"));
        assert_eq!(epic_card.subissues[1].seq.as_deref(), Some("m"));
        assert_eq!(epic_card.subissues[2].seq, None);
    }

    /// A board-root sequence overlay resolves onto a top-level card's `seq`,
    /// leaving its column `rank` untouched (independent axes).
    #[test]
    fn reduce_resolves_board_root_sequence() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];
        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };
        let card = note_id(&owner, build_issue(&addr, "Card", "").created_at(1_000));
        let root = Container::BoardRoot("b1".into());
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Card", "").created_at(1_000), &owner),
            parse_owned(build_placement("b1", &addr, &card, "todo", "m"), &owner),
            parse_owned(build_sequence(&root, &card, "an"), &owner),
        ];
        let views = reduce(&events);
        let cv = views[0].columns[0]
            .cards
            .iter()
            .find(|c| c.id == card)
            .unwrap();
        assert_eq!(cv.seq.as_deref(), Some("an"));
        assert_eq!(cv.rank, "m");
    }

    /// A newer authorised sequence supersedes an older one; a stranger's newer
    /// sequence shadows the slot but is ignored at resolve (like other overlays).
    #[test]
    fn sequence_latest_authorised_wins_and_ignores_strangers() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];
        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };
        let card = note_id(&owner, build_issue(&addr, "Card", "").created_at(1_000));
        let root = Container::BoardRoot("b1".into());
        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Card", "").created_at(1_000), &owner),
            parse_owned(build_placement("b1", &addr, &card, "todo", "m"), &owner),
            parse_owned(build_sequence(&root, &card, "g").created_at(2_000), &owner),
        ];
        let find_seq = |evs: &[HeadwayEvent]| -> Option<String> {
            reduce(evs)[0].columns[0]
                .cards
                .iter()
                .find(|c| c.id == card)
                .unwrap()
                .seq
                .clone()
        };
        assert_eq!(find_seq(&events).as_deref(), Some("g"));

        // Newer authorised reseq wins.
        events.push(parse_owned(
            build_sequence(&root, &card, "t").created_at(3_000),
            &owner,
        ));
        assert_eq!(find_seq(&events).as_deref(), Some("t"));

        // A stranger's even-newer seq shadows the slot but isn't honoured.
        events.push(parse_owned(
            build_sequence(&root, &card, "z").created_at(4_000),
            &stranger,
        ));
        assert_eq!(find_seq(&events), None, "stranger ignored");
    }

    /// The relation slot is latest-authorised-wins: a newer relation re-parents,
    /// a newer detach clears, and a stranger's relation is ignored outright.
    #[test]
    fn reduce_reparents_latest_wins_and_ignores_strangers() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let e1 = note_id(&owner, build_issue(&addr, "Epic 1", "").created_at(1_000));
        let e2 = note_id(&owner, build_issue(&addr, "Epic 2", "").created_at(1_001));
        let child = note_id(&owner, build_issue(&addr, "Child", "").created_at(1_002));

        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Epic 1", "").created_at(1_000), &owner),
            parse_owned(build_issue(&addr, "Epic 2", "").created_at(1_001), &owner),
            parse_owned(build_issue(&addr, "Child", "").created_at(1_002), &owner),
            parse_owned(build_placement("b1", &addr, &e1, "todo", "g"), &owner),
            parse_owned(build_placement("b1", &addr, &e2, "todo", "m"), &owner),
            parse_owned(build_placement("b1", &addr, &child, "todo", "t"), &owner),
            parse_owned(build_relation(&child, Some(&e1)).created_at(2_000), &owner),
        ];

        let find = |views: &Vec<BoardView>, id: NoteId| -> CardView {
            views[0].columns[0]
                .cards
                .iter()
                .find(|c| c.id == id)
                .unwrap()
                .clone()
        };

        // A stranger's relation must not re-parent the card. (Like every
        // overlay, ingest is authority-blind and authority is applied at
        // resolve: the stranger's newer event shadows the owner's older slot
        // rather than losing to it, so the card reads as unparented — but the
        // hijack itself never takes effect.)
        events.push(parse_owned(
            build_relation(&child, Some(&e2)).created_at(3_000),
            &stranger,
        ));
        let views = reduce(&events);
        assert_eq!(find(&views, child).parent, None, "stranger ignored");
        assert!(find(&views, e2).subissues.is_empty(), "hijack inert");

        // The owner re-parents: newest authorised slot wins on both ends.
        events.push(parse_owned(
            build_relation(&child, Some(&e2)).created_at(4_000),
            &owner,
        ));
        let views = reduce(&events);
        assert_eq!(find(&views, child).parent, Some(e2));
        assert!(find(&views, e1).subissues.is_empty());
        assert_eq!(find(&views, e2).subissues.len(), 1);

        // And a detach (no parent tag) clears it.
        events.push(parse_owned(
            build_relation(&child, None).created_at(5_000),
            &owner,
        ));
        let views = reduce(&events);
        assert_eq!(find(&views, child).parent, None);
        assert!(find(&views, e2).subissues.is_empty());
    }

    /// An archived-everywhere child counts as done (filed away); a tombstoned
    /// child drops off its parent's subissue list entirely.
    #[test]
    fn reduce_subissue_doneness_for_archived_and_deleted_children() {
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

        let epic = note_id(&owner, build_issue(&addr, "Epic", "").created_at(1_000));
        let shelved = note_id(&owner, build_issue(&addr, "Shelved", "").created_at(1_001));
        let gone = note_id(&owner, build_issue(&addr, "Gone", "").created_at(1_002));

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Epic", "").created_at(1_000), &owner),
            parse_owned(build_issue(&addr, "Shelved", "").created_at(1_001), &owner),
            parse_owned(build_issue(&addr, "Gone", "").created_at(1_002), &owner),
            parse_owned(build_placement("b1", &addr, &epic, "todo", "g"), &owner),
            parse_owned(
                build_archive_placement("b1", &addr, &shelved, "done", "m"),
                &owner,
            ),
            parse_owned(
                build_placement("b1", &addr, &gone, COL_DELETED, "t"),
                &owner,
            ),
            parse_owned(build_relation(&shelved, Some(&epic)), &owner),
            parse_owned(build_relation(&gone, Some(&epic)), &owner),
        ];

        let views = reduce(&events);
        let epic_card = views[0].columns[0]
            .cards
            .iter()
            .find(|c| c.id == epic)
            .unwrap();

        // The deleted child vanished; the archived one counts as done.
        assert_eq!(epic_card.subissues.len(), 1);
        assert_eq!(epic_card.subissues[0].title, "Shelved");
        assert!(epic_card.subissues[0].done);
        assert!(epic_card.subissues[0].archived);
        assert_eq!(epic_card.subissues[0].column, None);
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

    #[test]
    fn pick_card_with_column_resolves_position() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("backlog", "Backlog"),
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder| {
            let note = b.sign(&owner.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "In the middle", "body"));
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols)),
            parse_owned(build_issue(&addr, "In the middle", "body")),
            // Placed in the second of three columns.
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m")),
        ];

        let mut reducer = BoardReducer::default();
        for event in &events {
            reducer.ingest(event.clone());
        }

        let resolved = pick_card_with_column(&reducer, &owner.pubkey, "b1", i1.bytes()).unwrap();
        assert_eq!(resolved.card.title, "In the middle");
        assert_eq!(resolved.column, Some(ColumnPos { index: 1, count: 3 }));

        // Unknown card id -> None.
        assert!(pick_card_with_column(&reducer, &owner.pubkey, "b1", &[0u8; 32]).is_none());
    }

    /// A card moved across boards keeps its origin board in its `a` tag but lives
    /// on the destination via its placement. [`locate_card`] resolves it on the
    /// destination (where it's actually shown), not the stale origin — the board
    /// an inline chip must read for its status and a click must open.
    #[test]
    fn locate_card_resolves_cross_board_move() {
        let owner = FullKeypair::generate();
        // The card's `a` tag anchors it to "notedeck" (its origin board).
        let origin = board_address(&owner.pubkey, "notedeck");
        let dest = board_address(&owner.pubkey, "dave");
        let cols = vec![
            ColumnDef::new("backlog", "Backlog"),
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("in-progress", "In Progress"),
            ColumnDef::new("in-review", "In Review"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder| {
            let note = b.sign(&owner.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        // Moved card: created on notedeck, deleted there, placed live on dave.
        let moved = note_id(&owner, build_issue(&origin, "Moved", "body"));
        // Orphan: anchored to notedeck by its `a` tag, never placed anywhere.
        let orphan = note_id(&owner, build_issue(&origin, "Orphan", "body"));

        let events = vec![
            parse_owned(build_board("notedeck", "Notedeck", "", &cols)),
            parse_owned(build_board("dave", "Dave", "", &cols)),
            parse_owned(build_issue(&origin, "Moved", "body")),
            parse_owned(build_issue(&origin, "Orphan", "body")),
            // Origin history: placed then deleted (the cross-board move's origin half).
            parse_owned(
                build_placement("notedeck", &origin, &moved, "todo", "m").created_at(1_000),
            ),
            parse_owned(
                build_placement("notedeck", &origin, &moved, COL_DELETED, "m").created_at(2_000),
            ),
            // Destination: live in In Progress (index 2 of 5).
            parse_owned(
                build_placement("dave", &dest, &moved, "in-progress", "m").created_at(3_000),
            ),
        ];

        let mut reducer = BoardReducer::default();
        for event in &events {
            reducer.ingest(event.clone());
        }

        // The moved card resolves on dave (the live placement), not its `a`-tag
        // origin — with its real column position.
        let located = locate_card(&reducer, &owner.pubkey, moved.bytes()).unwrap();
        assert_eq!(located.board_id, "dave");
        assert_eq!(located.card.title, "Moved");
        assert_eq!(located.column, Some(ColumnPos { index: 2, count: 5 }));

        // The bug this guards: the board-scoped resolver keyed on the origin `a`-tag
        // board finds the card deleted there and returns nothing.
        assert!(
            pick_card_with_column(&reducer, &owner.pubkey, "notedeck", moved.bytes()).is_none(),
            "card is deleted on its origin board"
        );

        // An orphan (no placement anywhere) still resolves on its origin board via
        // the finalize fallback — first column.
        let located = locate_card(&reducer, &owner.pubkey, orphan.bytes()).unwrap();
        assert_eq!(located.board_id, "notedeck");
        assert_eq!(located.column, Some(ColumnPos { index: 0, count: 5 }));

        // Unknown card id -> None.
        assert!(locate_card(&reducer, &owner.pubkey, &[0u8; 32]).is_none());
    }
}
