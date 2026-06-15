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
//! Effective state is resolved as **latest-authorised-wins** (placement,
//! subject, cover note) or an **additive union** (labels), where "authorised"
//! means the event's author is the card author or the board's author
//! (maintainer). This mirrors the ngitstack/gitworkshop "Shared Issue / Patch /
//! PR Metadata" spec.
//!
//! This module is pure: it builds and parses notes and reduces a set of them
//! into a [`BoardView`]. Relay/ndb plumbing lives in the app layer.

use std::collections::HashMap;

use enostr::{NoteId, Pubkey};
use nostrdb::{Note, NoteBuildOptions, NoteBuilder};

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

const NS_SUBJECT: &str = "#subject";
const NS_TAG: &str = "#t";

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
pub fn build_labels<'a>(issue: &NoteId, labels: &[String]) -> NoteBuilder<'a> {
    let mut b = base(KIND_LABEL, "")
        .start_tag()
        .tag_str("e")
        .tag_id(issue.bytes())
        .start_tag()
        .tag_str("L")
        .tag_str(NS_TAG);

    for label in labels {
        b = b.start_tag().tag_str("l").tag_str(label).tag_str(NS_TAG);
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

/// A parsed headway event of any of the recognised kinds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadwayEvent {
    Board(BoardEvent),
    Issue(IssueEvent),
    Placement(PlacementEvent),
    Subject(SubjectEdit),
    Labels(LabelSet),
    Cover(CoverNote),
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

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("e") => issue_id = tag.get_id(1).copied(),
            Some("col") => col = tag.get_str(1).map(|s| s.to_owned()),
            Some("rank") => rank = tag.get_str(1).map(|s| s.to_owned()),
            _ => {}
        }
    }

    Some(PlacementEvent {
        author: *note.pubkey(),
        issue_id: issue_id?,
        col: col?,
        rank: rank?,
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

/// A card as rendered: a stable id plus its resolved fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardView {
    pub id: NoteId,
    pub title: String,
    pub description: String,
    pub labels: Vec<String>,
    /// Fractional rank within its column; cards are sorted ascending.
    pub rank: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnView {
    pub id: String,
    pub name: String,
    pub cards: Vec<CardView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardView {
    pub id: String,
    pub author: [u8; 32],
    pub title: String,
    pub description: String,
    pub columns: Vec<ColumnView>,
}

/// Resolve a set of headway events into the boards they describe.
///
/// For each board the latest board event (by `created_at`) wins. Cards are
/// placed by their latest *authorised* placement (`col` + `rank`), with title /
/// description / labels resolved the same way. Cards with no placement, or whose
/// placement points at an unknown column, fall into the first column, ordered by
/// creation time after the explicitly placed cards.
pub fn reduce(events: &[HeadwayEvent]) -> Vec<BoardView> {
    // Latest board event per (author, board_id).
    let mut boards: HashMap<(Vec<u8>, String), &BoardEvent> = HashMap::new();
    // Latest issue per id (issues are immutable, but a relay may hand us dups).
    let mut issues: HashMap<[u8; 32], &IssueEvent> = HashMap::new();
    let mut placements: HashMap<[u8; 32], &PlacementEvent> = HashMap::new();
    let mut subjects: HashMap<[u8; 32], &SubjectEdit> = HashMap::new();
    let mut covers: HashMap<[u8; 32], &CoverNote> = HashMap::new();
    let mut label_sets: Vec<&LabelSet> = Vec::new();

    for event in events {
        match event {
            HeadwayEvent::Board(b) => {
                let key = (b.author.to_vec(), b.id.clone());
                if boards
                    .get(&key)
                    .is_none_or(|cur| b.created_at > cur.created_at)
                {
                    boards.insert(key, b);
                }
            }
            HeadwayEvent::Issue(i) => {
                issues.insert(i.id, i);
            }
            HeadwayEvent::Placement(p) => {
                if placements
                    .get(&p.issue_id)
                    .is_none_or(|cur| newer(p.created_at, &p.author, cur.created_at, &cur.author))
                {
                    placements.insert(p.issue_id, p);
                }
            }
            HeadwayEvent::Subject(s) => {
                if subjects
                    .get(&s.issue_id)
                    .is_none_or(|cur| newer(s.created_at, &s.author, cur.created_at, &cur.author))
                {
                    subjects.insert(s.issue_id, s);
                }
            }
            HeadwayEvent::Cover(c) => {
                if covers
                    .get(&c.issue_id)
                    .is_none_or(|cur| newer(c.created_at, &c.author, cur.created_at, &cur.author))
                {
                    covers.insert(c.issue_id, c);
                }
            }
            HeadwayEvent::Labels(l) => label_sets.push(l),
        }
    }

    let mut views: Vec<BoardView> = Vec::new();

    for ((author, board_id), board) in &boards {
        // Group this board's cards by resolved column id.
        let mut by_col: HashMap<String, Vec<CardView>> = HashMap::new();
        let mut fallback: Vec<(u64, CardView)> = Vec::new();
        let col_ids: Vec<&str> = board.columns.iter().map(|c| c.id.as_str()).collect();

        for issue in issues.values() {
            if &issue.board_author.to_vec() != author || &issue.board_id != board_id {
                continue;
            }

            // Authority: the card author or the board author may amend the card.
            let authorised = |who: &[u8; 32]| who == &issue.author || who == &board.author;

            let title = subjects
                .get(&issue.id)
                .filter(|s| authorised(&s.author))
                .map(|s| s.subject.clone())
                .unwrap_or_else(|| issue.subject.clone());

            let description = covers
                .get(&issue.id)
                .filter(|c| authorised(&c.author))
                .map(|c| c.body.clone())
                .unwrap_or_else(|| issue.body.clone());

            let mut labels = issue.inline_labels.clone();
            for set in &label_sets {
                if set.issue_id == issue.id && authorised(&set.author) {
                    labels.extend(set.labels.iter().cloned());
                }
            }
            labels.sort();
            labels.dedup();

            let placement = placements.get(&issue.id).filter(|p| authorised(&p.author));
            let rank = placement.map(|p| p.rank.clone()).unwrap_or_default();
            let card = CardView {
                id: NoteId::new(issue.id),
                title,
                description,
                labels,
                rank,
            };

            match placement {
                Some(p) if col_ids.contains(&p.col.as_str()) => {
                    by_col.entry(p.col.clone()).or_default().push(card);
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

        views.push(BoardView {
            id: board_id.clone(),
            author: board.author,
            title: board.title.clone(),
            description: board.description.clone(),
            columns,
        });
    }

    // Stable output order: by board id.
    views.sort_by(|a, b| a.id.cmp(&b.id));
    views
}

/// "Latest authorised wins" comparator: newer `created_at` wins, ties broken by
/// author bytes so the result is deterministic.
fn newer(a_at: u64, a_who: &[u8; 32], b_at: u64, b_who: &[u8; 32]) -> bool {
    (a_at, a_who) > (b_at, b_who)
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
}
