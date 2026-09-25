//! Parsed events: the typed form of each headway event kind, the
//! [`HeadwayEvent`] sum over them, and [`parse`], which reads a [`Note`] into
//! one. Round-trip tests for every builder live here too.

use nostrdb::Note;
use nostrdb_net::NoteId;

use super::kinds::{
    KIND_BLOCKERS, KIND_BOARD, KIND_COMMENT, KIND_COVER_NOTE, KIND_ISSUE, KIND_LABEL,
    KIND_PLACEMENT, KIND_RELATED, KIND_RELATION, KIND_SEQUENCE, NS_SUBJECT, NS_TAG,
};
use super::model::{BoardCoord, ColumnDef, Field};

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
    Blockers(BlockerSet),
    Related(RelatedSet),
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
        KIND_BLOCKERS => parse_blockers(note).map(HeadwayEvent::Blockers),
        KIND_RELATED => parse_related(note).map(HeadwayEvent::Related),
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

/// Parse a NIP-22 comment (kind 1111). The root issue is the uppercase `E`; the
/// parent is the lowercase `e`, and the lowercase `k` tells us whether that
/// parent is another comment (a threaded reply) or the issue (a top-level
/// comment). See [`build_comment`](super::build_comment).
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
        build_blockers, build_board, build_comment, build_cover_note, build_issue, build_labels,
        build_placement, build_related, build_relation, build_sequence, build_subject_edit,
    };
    use crate::event::model::board_address;

    /// Sign `builder` with `kp` and parse the result back into a [`HeadwayEvent`].
    pub(crate) fn roundtrip(builder: NoteBuilder, kp: &FullKeypair) -> HeadwayEvent {
        let note = builder
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("build note");
        parse(&note).expect("parse headway event")
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
