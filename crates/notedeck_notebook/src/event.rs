//! Nostr event model for collaborative JSON Canvas notebooks.
//!
//! This mirrors the event-sourced design of [`headway`](../../headway): the
//! canvas you see is a pure function of a set of append-only nostr events, never
//! a record you mutate in place. A [`CanvasReducer`] folds the events into a
//! [`CanvasView`], deterministically and order-independently, so replaying the
//! log always lands in the same canvas.
//!
//! Where headway has one immutable anchor (the issue) with small per-card
//! overlays, a canvas has a larger, hotter mutable surface (drag/resize fire
//! constantly), so the split is finer:
//!
//! | concept            | kind    | mechanism                                       |
//! | ------------------ | ------- | ----------------------------------------------- |
//! | canvas (document)  | `31606` | addressable; `d` = canvas id, title + members   |
//! | node creation      | `1606`  | immutable; node id = this event's id, `a` → canvas |
//! | node transform     | `31608` | addressable; `d` = `canvas:node`; x/y/w/h/z/color/del |
//! | node content       | `31609` | addressable; `d` = `canvas:node:c`; text/url/file/… |
//! | edge               | `31610` | addressable; `d` = `canvas:edge`; from/to/sides/del |
//!
//! **Geometry and content are separate overlays** so a move by one author and a
//! text edit by another merge cleanly (independent latest-wins, no lost update)
//! — the same reason headway keeps placement separate from subject.
//!
//! ## Authority is a visibility filter, not a validity gate
//!
//! Anyone may append events to any canvas (it's permissionless). What the
//! *canvas owner + listed members* control is only what's **surfaced**: by
//! default the view shows the owner's and members' nodes/edits and collects
//! everyone else's node creations into [`CanvasView::pending`]. Flipping the
//! canvas to **open** mode surfaces everyone. So a stranger can always *propose*
//! (extend) a canvas; they just don't appear in the default view until promoted
//! to a member or the canvas is opened.
//!
//! Resolution is **latest-surfaced-wins** per overlay, ties broken by author
//! bytes. Because overlays are addressable (`d` keyed per element), a relay
//! keeps only the latest version *per author* — so the per-(element, author)
//! maps the reducer holds stay bounded by the number of collaborators.
//!
//! This module is pure: it builds/parses notes and reduces them. ndb/relay
//! plumbing lives in the app layer (mirroring `headway::store`).

use std::collections::HashMap;

use enostr::{NoteId, Pubkey};
use nostrdb::{Filter, Ndb, Note, NoteBuildOptions, NoteBuilder, NoteKey, Transaction};

/// Canvas document: addressable, `d` = canvas id, holds title + membership.
pub const KIND_CANVAS: u32 = 31606;
/// Node creation: immutable (regular kind), so the note id is the stable node id.
pub const KIND_NODE: u32 = 1606;
/// Node transform: addressable, the hot path (drag/resize/restack/recolor/delete).
pub const KIND_TRANSFORM: u32 = 31608;
/// Node content: addressable, the node's editable payload (text/url/file/label…).
pub const KIND_CONTENT: u32 = 31609;
/// Edge: addressable, a whole edge (endpoints, sides, ends, color, label, delete).
pub const KIND_EDGE: u32 = 31610;

/// Ephemeral live-presence kind (cursors / in-flight drags). Reserved: these are
/// broadcast at high frequency and **never folded into the document** — durable
/// state is published only on gesture-end via [`KIND_TRANSFORM`]. Not yet used.
pub const KIND_PRESENCE: u32 = 21606;

/// Every kind the notebook cares about, for querying / subscribing.
pub const NOTEBOOK_KINDS: [u32; 5] = [
    KIND_CANVAS,
    KIND_NODE,
    KIND_TRANSFORM,
    KIND_CONTENT,
    KIND_EDGE,
];

/// The four JSON Canvas node types. A node's type is fixed at creation (it lives
/// on the immutable creation event); changing type means making a new node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Text,
    File,
    Link,
    Group,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Text => "text",
            NodeKind::File => "file",
            NodeKind::Link => "link",
            NodeKind::Group => "group",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(NodeKind::Text),
            "file" => Some(NodeKind::File),
            "link" => Some(NodeKind::Link),
            "group" => Some(NodeKind::Group),
            _ => None,
        }
    }
}

/// A node's geometry: top-left position and size, in JSON Canvas pixel units
/// (`x`/`y` are `i64`, `width`/`height` are `u64`, matching the `jsoncanvas` crate).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Geometry {
    pub x: i64,
    pub y: i64,
    pub w: u64,
    pub h: u64,
}

/// The editable, type-specific content of a node, carried both on the creation
/// snapshot and on content overlays. `text` is the event body (markdown for text
/// nodes); the rest are tags relevant to a given node type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeContent {
    pub text: String,
    pub url: Option<String>,
    pub file: Option<String>,
    pub subpath: Option<String>,
    pub label: Option<String>,
    pub background: Option<String>,
    pub background_style: Option<String>,
}

/// The addressable coordinate of a canvas: `31606:<author-hex>:<canvas-id>`.
pub fn canvas_address(author: &Pubkey, canvas_id: &str) -> String {
    format!("{KIND_CANVAS}:{}:{canvas_id}", author.hex())
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

/// Build a canvas document (kind 31606) with its title, member list and mode.
pub fn build_canvas<'a>(
    canvas_id: &str,
    title: &str,
    members: &[Pubkey],
    open: bool,
) -> NoteBuilder<'a> {
    let mut b = base(KIND_CANVAS, "")
        .start_tag()
        .tag_str("d")
        .tag_str(canvas_id)
        .start_tag()
        .tag_str("title")
        .tag_str(title)
        .start_tag()
        .tag_str("mode")
        .tag_str(if open { "open" } else { "closed" });

    for m in members {
        b = b.start_tag().tag_str("p").tag_id(m.bytes());
    }

    b
}

/// Append the content-bearing tags (everything but `text`, which is the body).
fn tag_content<'a>(mut b: NoteBuilder<'a>, content: &NodeContent) -> NoteBuilder<'a> {
    let pairs = [
        ("url", &content.url),
        ("file", &content.file),
        ("subpath", &content.subpath),
        ("label", &content.label),
        ("background", &content.background),
        ("bgstyle", &content.background_style),
    ];
    for (key, val) in pairs {
        if let Some(v) = val {
            b = b.start_tag().tag_str(key).tag_str(v);
        }
    }
    b
}

/// Build a node creation event (kind 1606), immutable. The note id becomes the
/// node id. Carries the type plus a creation snapshot of geometry and content,
/// used as the fallback until overlays supersede them.
pub fn build_node<'a>(
    canvas_addr: &str,
    kind: NodeKind,
    geo: &Geometry,
    content: &'a NodeContent,
) -> NoteBuilder<'a> {
    let b = base(KIND_NODE, &content.text)
        .start_tag()
        .tag_str("a")
        .tag_str(canvas_addr)
        .start_tag()
        .tag_str("type")
        .tag_str(kind.as_str());
    let b = tag_geometry(b, geo);
    tag_content(b, content)
}

fn tag_geometry<'a>(b: NoteBuilder<'a>, geo: &Geometry) -> NoteBuilder<'a> {
    b.start_tag()
        .tag_str("x")
        .tag_str(&geo.x.to_string())
        .start_tag()
        .tag_str("y")
        .tag_str(&geo.y.to_string())
        .start_tag()
        .tag_str("w")
        .tag_str(&geo.w.to_string())
        .start_tag()
        .tag_str("h")
        .tag_str(&geo.h.to_string())
}

/// The addressable `d` of a node transform: `<canvas-id>:<node-hex>`.
fn transform_d(canvas_id: &str, node: &NoteId) -> String {
    format!("{canvas_id}:{}", node.hex())
}

/// Build a node transform (kind 31608): a full geometry snapshot plus the node's
/// z-rank and optional color. Latest-surfaced-wins, so a drag/resize/restack
/// republishes just this one small event.
pub fn build_transform<'a>(
    canvas_id: &str,
    canvas_addr: &str,
    node: &NoteId,
    geo: &Geometry,
    z: &str,
    color: Option<&str>,
) -> NoteBuilder<'a> {
    let b = base(KIND_TRANSFORM, "")
        .start_tag()
        .tag_str("d")
        .tag_str(&transform_d(canvas_id, node))
        .start_tag()
        .tag_str("a")
        .tag_str(canvas_addr)
        .start_tag()
        .tag_str("e")
        .tag_id(node.bytes());
    let b = tag_geometry(b, geo);
    let b = b.start_tag().tag_str("z").tag_str(z);
    match color {
        Some(c) => b.start_tag().tag_str("color").tag_str(c),
        None => b,
    }
}

/// Build a tombstone transform that removes `node` from the canvas. Reversible
/// (republish a normal transform to restore it), mirroring headway's
/// `COL_DELETED` placement.
pub fn build_node_tombstone<'a>(
    canvas_id: &str,
    canvas_addr: &str,
    node: &NoteId,
    z: &str,
) -> NoteBuilder<'a> {
    build_transform(canvas_id, canvas_addr, node, &Geometry::default(), z, None)
        .start_tag()
        .tag_str("del")
        .tag_str("1")
}

/// Build a node content overlay (kind 31609): the node's editable payload.
pub fn build_content<'a>(
    canvas_id: &str,
    canvas_addr: &str,
    node: &NoteId,
    content: &'a NodeContent,
) -> NoteBuilder<'a> {
    let b = base(KIND_CONTENT, &content.text)
        .start_tag()
        .tag_str("d")
        .tag_str(&format!("{}:c", transform_d(canvas_id, node)))
        .start_tag()
        .tag_str("a")
        .tag_str(canvas_addr)
        .start_tag()
        .tag_str("e")
        .tag_id(node.bytes());
    tag_content(b, content)
}

/// The endpoints and decorations of an edge, as supplied to [`build_edge`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EdgeEnds {
    pub from_side: Option<String>,
    pub from_end: Option<String>,
    pub to_side: Option<String>,
    pub to_end: Option<String>,
    pub color: Option<String>,
    pub label: Option<String>,
}

/// Build an edge (kind 31610): a whole edge keyed by `<canvas-id>:<edge-id>`.
/// Edges have no immutable payload, so the whole edge is one addressable,
/// latest-surfaced-wins event (no separate creation anchor).
pub fn build_edge<'a>(
    canvas_id: &str,
    canvas_addr: &str,
    edge_id: &str,
    from: &NoteId,
    to: &NoteId,
    ends: &EdgeEnds,
) -> NoteBuilder<'a> {
    let mut b = base(KIND_EDGE, "")
        .start_tag()
        .tag_str("d")
        .tag_str(&format!("{canvas_id}:{edge_id}"))
        .start_tag()
        .tag_str("a")
        .tag_str(canvas_addr)
        .start_tag()
        .tag_str("from")
        .tag_id(from.bytes())
        .start_tag()
        .tag_str("to")
        .tag_id(to.bytes());

    let opt = [
        ("fromside", &ends.from_side),
        ("fromend", &ends.from_end),
        ("toside", &ends.to_side),
        ("toend", &ends.to_end),
        ("color", &ends.color),
        ("label", &ends.label),
    ];
    for (key, val) in opt {
        if let Some(v) = val {
            b = b.start_tag().tag_str(key).tag_str(v);
        }
    }
    b
}

/// Build an edge tombstone removing `edge_id` from the canvas.
pub fn build_edge_tombstone<'a>(
    canvas_id: &str,
    canvas_addr: &str,
    edge_id: &str,
    from: &NoteId,
    to: &NoteId,
) -> NoteBuilder<'a> {
    build_edge(
        canvas_id,
        canvas_addr,
        edge_id,
        from,
        to,
        &EdgeEnds::default(),
    )
    .start_tag()
    .tag_str("del")
    .tag_str("1")
}

// ---------------------------------------------------------------------------
// Parsed events
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasEvent {
    pub id: String,
    pub author: [u8; 32],
    pub title: String,
    pub members: Vec<[u8; 32]>,
    pub open: bool,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeEvent {
    pub id: [u8; 32],
    pub author: [u8; 32],
    pub canvas_author: [u8; 32],
    pub canvas_id: String,
    pub kind: NodeKind,
    pub geo: Geometry,
    pub content: NodeContent,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransformEvent {
    pub author: [u8; 32],
    pub node_id: [u8; 32],
    pub geo: Geometry,
    pub z: String,
    pub color: Option<String>,
    pub deleted: bool,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentEvent {
    pub author: [u8; 32],
    pub node_id: [u8; 32],
    pub content: NodeContent,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeEvent {
    pub author: [u8; 32],
    pub edge_id: String,
    pub from: [u8; 32],
    pub to: [u8; 32],
    pub ends: EdgeEnds,
    pub deleted: bool,
    pub created_at: u64,
}

/// A parsed notebook event of any recognised kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotebookEvent {
    Canvas(CanvasEvent),
    Node(NodeEvent),
    Transform(TransformEvent),
    Content(ContentEvent),
    Edge(EdgeEvent),
}

/// Parse a note into a [`NotebookEvent`], or `None` if it isn't a recognised /
/// well-formed notebook event.
pub fn parse(note: &Note) -> Option<NotebookEvent> {
    match note.kind() {
        KIND_CANVAS => parse_canvas(note).map(NotebookEvent::Canvas),
        KIND_NODE => parse_node(note).map(NotebookEvent::Node),
        KIND_TRANSFORM => parse_transform(note).map(NotebookEvent::Transform),
        KIND_CONTENT => parse_content(note).map(NotebookEvent::Content),
        KIND_EDGE => parse_edge(note).map(NotebookEvent::Edge),
        _ => None,
    }
}

fn parse_canvas(note: &Note) -> Option<CanvasEvent> {
    let mut id = None;
    let mut title = String::new();
    let mut members = Vec::new();
    let mut open = false;

    for tag in note.tags() {
        match tag.get_str(0) {
            Some("d") => id = tag.get_str(1).map(|s| s.to_owned()),
            Some("title") => {
                if let Some(t) = tag.get_str(1) {
                    title = t.to_owned();
                }
            }
            Some("mode") => open = tag.get_str(1) == Some("open"),
            Some("p") => {
                if let Some(pk) = tag.get_id(1) {
                    members.push(*pk);
                }
            }
            _ => {}
        }
    }

    Some(CanvasEvent {
        id: id?,
        author: *note.pubkey(),
        title,
        members,
        open,
        created_at: note.created_at(),
    })
}

/// Read the content-bearing tags shared by node creation and content overlays.
/// The text body comes from the note content, passed in separately.
fn read_content(note: &Note, text: String) -> NodeContent {
    let mut c = NodeContent {
        text,
        ..Default::default()
    };
    for tag in note.tags() {
        let val = tag.get_str(1).map(|s| s.to_owned());
        match tag.get_str(0) {
            Some("url") => c.url = val,
            Some("file") => c.file = val,
            Some("subpath") => c.subpath = val,
            Some("label") => c.label = val,
            Some("background") => c.background = val,
            Some("bgstyle") => c.background_style = val,
            _ => {}
        }
    }
    c
}

fn read_geometry(note: &Note) -> Geometry {
    let mut g = Geometry::default();
    for tag in note.tags() {
        let num = tag.get_str(1).and_then(|s| s.parse::<i64>().ok());
        match tag.get_str(0) {
            Some("x") => g.x = num.unwrap_or(0),
            Some("y") => g.y = num.unwrap_or(0),
            Some("w") => g.w = num.unwrap_or(0).max(0) as u64,
            Some("h") => g.h = num.unwrap_or(0).max(0) as u64,
            _ => {}
        }
    }
    g
}

fn parse_node(note: &Note) -> Option<NodeEvent> {
    let mut canvas = None;
    let mut kind = None;
    for tag in note.tags() {
        match tag.get_str(0) {
            Some("a") => canvas = tag.get_str(1).and_then(parse_canvas_address),
            Some("type") => kind = tag.get_str(1).and_then(NodeKind::parse),
            _ => {}
        }
    }
    let (canvas_author, canvas_id) = canvas?;

    Some(NodeEvent {
        id: *note.id(),
        author: *note.pubkey(),
        canvas_author,
        canvas_id,
        kind: kind?,
        geo: read_geometry(note),
        content: read_content(note, note.content().to_owned()),
        created_at: note.created_at(),
    })
}

fn parse_transform(note: &Note) -> Option<TransformEvent> {
    let mut node_id = None;
    let mut z = String::new();
    let mut color = None;
    let mut deleted = false;
    for tag in note.tags() {
        match tag.get_str(0) {
            Some("e") => node_id = tag.get_id(1).copied(),
            Some("z") => {
                if let Some(v) = tag.get_str(1) {
                    z = v.to_owned();
                }
            }
            Some("color") => color = tag.get_str(1).map(|s| s.to_owned()),
            Some("del") => deleted = tag.get_str(1) == Some("1"),
            _ => {}
        }
    }

    Some(TransformEvent {
        author: *note.pubkey(),
        node_id: node_id?,
        geo: read_geometry(note),
        z,
        color,
        deleted,
        created_at: note.created_at(),
    })
}

fn parse_content(note: &Note) -> Option<ContentEvent> {
    let mut node_id = None;
    for tag in note.tags() {
        if tag.get_str(0) == Some("e") {
            node_id = tag.get_id(1).copied();
        }
    }

    Some(ContentEvent {
        author: *note.pubkey(),
        node_id: node_id?,
        content: read_content(note, note.content().to_owned()),
        created_at: note.created_at(),
    })
}

fn parse_edge(note: &Note) -> Option<EdgeEvent> {
    let mut edge_id = None;
    let mut from = None;
    let mut to = None;
    let mut ends = EdgeEnds::default();
    let mut deleted = false;

    for tag in note.tags() {
        let val = tag.get_str(1).map(|s| s.to_owned());
        match tag.get_str(0) {
            // `d` is `<canvas-id>:<edge-id>`; the edge id is the part after the
            // last ':' (canvas ids may themselves contain ':').
            Some("d") => edge_id = val.and_then(|d| d.rsplit_once(':').map(|(_, e)| e.to_owned())),
            Some("from") => from = tag.get_id(1).copied(),
            Some("to") => to = tag.get_id(1).copied(),
            Some("fromside") => ends.from_side = val,
            Some("fromend") => ends.from_end = val,
            Some("toside") => ends.to_side = val,
            Some("toend") => ends.to_end = val,
            Some("color") => ends.color = val,
            Some("label") => ends.label = val,
            Some("del") => deleted = val.as_deref() == Some("1"),
            _ => {}
        }
    }

    Some(EdgeEvent {
        author: *note.pubkey(),
        edge_id: edge_id?,
        from: from?,
        to: to?,
        ends,
        deleted,
        created_at: note.created_at(),
    })
}

/// Parse a `31606:<author-hex>:<canvas-id>` address into `(author, canvas_id)`.
fn parse_canvas_address(addr: &str) -> Option<([u8; 32], String)> {
    let mut parts = addr.splitn(3, ':');
    if parts.next()? != KIND_CANVAS.to_string() {
        return None;
    }
    let author = Pubkey::from_hex(parts.next()?).ok()?;
    Some((*author.bytes(), parts.next()?.to_owned()))
}

// ---------------------------------------------------------------------------
// Reducer: events -> view model
// ---------------------------------------------------------------------------

/// A node as rendered: its resolved geometry, content and stacking rank.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeView {
    pub id: NoteId,
    pub author: [u8; 32],
    pub kind: NodeKind,
    pub geo: Geometry,
    /// Fractional z-rank; nodes are drawn back-to-front, sorted ascending.
    pub z: String,
    pub color: Option<String>,
    pub content: NodeContent,
    /// When the node was created (its immutable creation event's timestamp).
    pub created_at: u64,
    /// Timestamp of the winning transform (else the creation time). The store
    /// stamps a re-placement strictly past this so an addressable transform
    /// always supersedes the one it edits, even within the same wall-clock second.
    pub placed_at: u64,
    /// Timestamp of the winning content overlay (else the creation time). Same
    /// supersede role as `placed_at`, for content edits.
    pub edited_at: u64,
}

/// An edge as rendered, with its endpoints resolved to node ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeView {
    pub id: String,
    pub author: [u8; 32],
    pub from: NoteId,
    pub to: NoteId,
    pub ends: EdgeEnds,
    /// Timestamp of the winning edge event, so the store can supersede it.
    pub placed_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasView {
    pub id: String,
    pub author: [u8; 32],
    pub title: String,
    pub members: Vec<[u8; 32]>,
    pub open: bool,
    /// Timestamp of the winning canvas document, so the store can supersede it
    /// when republishing title/members/mode.
    pub created_at: u64,
    /// Surfaced nodes, ordered back-to-front by z-rank.
    pub nodes: Vec<NodeView>,
    /// Surfaced edges whose endpoints both resolve to a live surfaced node.
    pub edges: Vec<EdgeView>,
    /// Node creations by non-members, hidden from the main view while the canvas
    /// is closed — proposals awaiting promotion (or an open toggle). Always empty
    /// when `open`. Sorted deterministically by node id.
    pub pending: Vec<NodeView>,
}

/// A predicate over author bytes deciding whose overlays may win — `surfaced`
/// for a visible node, "only the creator" for a pending one.
type AllowFn<'a> = Box<dyn Fn(&[u8; 32]) -> bool + 'a>;

/// Something carrying an authorship + timestamp, for latest-surfaced-wins.
trait Stamped {
    fn at(&self) -> u64;
    fn who(&self) -> &[u8; 32];
}

impl Stamped for TransformEvent {
    fn at(&self) -> u64 {
        self.created_at
    }
    fn who(&self) -> &[u8; 32] {
        &self.author
    }
}
impl Stamped for ContentEvent {
    fn at(&self) -> u64 {
        self.created_at
    }
    fn who(&self) -> &[u8; 32] {
        &self.author
    }
}
impl Stamped for EdgeEvent {
    fn at(&self) -> u64 {
        self.created_at
    }
    fn who(&self) -> &[u8; 32] {
        &self.author
    }
}

/// Pick the latest overlay among the authors allowed by `allow`, ties broken by
/// author bytes so the result is deterministic.
fn pick_latest<'a, T: Stamped>(
    by_author: Option<&'a HashMap<[u8; 32], T>>,
    allow: &impl Fn(&[u8; 32]) -> bool,
) -> Option<&'a T> {
    by_author?
        .values()
        .filter(|t| allow(t.who()))
        .max_by(|a, b| (a.at(), a.who()).cmp(&(b.at(), b.who())))
}

/// Keep `cand` in `slot` only if it's newer (latest-wins, author-bytes tiebreak).
fn keep_newer<T: Stamped>(slot: &mut HashMap<[u8; 32], T>, author: [u8; 32], cand: T) {
    let win = slot
        .get(&author)
        .is_none_or(|cur| (cand.at(), cand.who()) > (cur.at(), cur.who()));
    if win {
        slot.insert(author, cand);
    }
}

/// Accumulates notebook events into the maps needed to resolve effective canvas
/// state. Commutative + idempotent: each overlay is a latest-wins map keyed by
/// `(element, author)`, so an event's effect doesn't depend on when (or how
/// often) it's seen — which is what lets the app fold the whole history once and
/// then feed only freshly-arrived notes (see [`reduce_delta`]).
#[derive(Default)]
pub struct CanvasReducer {
    /// Latest canvas event per (author, canvas_id).
    canvases: HashMap<(Vec<u8>, String), CanvasEvent>,
    /// Node creations by node id (immutable; relays may hand us duplicates).
    nodes: HashMap<[u8; 32], NodeEvent>,
    /// Per node, the latest transform from each author.
    transforms: HashMap<[u8; 32], HashMap<[u8; 32], TransformEvent>>,
    /// Per node, the latest content overlay from each author.
    contents: HashMap<[u8; 32], HashMap<[u8; 32], ContentEvent>>,
    /// Per edge id, the latest edge event from each author.
    edges: HashMap<String, HashMap<[u8; 32], EdgeEvent>>,
}

impl CanvasReducer {
    /// Fold a single event into the accumulator.
    pub fn ingest(&mut self, event: NotebookEvent) {
        match event {
            NotebookEvent::Canvas(c) => {
                let key = (c.author.to_vec(), c.id.clone());
                if self
                    .canvases
                    .get(&key)
                    .is_none_or(|cur| c.created_at > cur.created_at)
                {
                    self.canvases.insert(key, c);
                }
            }
            NotebookEvent::Node(n) => {
                self.nodes.insert(n.id, n);
            }
            NotebookEvent::Transform(t) => {
                keep_newer(self.transforms.entry(t.node_id).or_default(), t.author, t);
            }
            NotebookEvent::Content(c) => {
                keep_newer(self.contents.entry(c.node_id).or_default(), c.author, c);
            }
            NotebookEvent::Edge(e) => {
                keep_newer(
                    self.edges.entry(e.edge_id.clone()).or_default(),
                    e.author,
                    e,
                );
            }
        }
    }

    /// Resolve the accumulated events into the canvases they describe.
    pub fn finalize(&self) -> Vec<CanvasView> {
        let mut views: Vec<CanvasView> = Vec::new();

        for ((author, canvas_id), canvas) in &self.canvases {
            // A node/overlay/edge is *surfaced* if the canvas is open, or its
            // author is the owner or a listed member. Strangers can still append;
            // their contributions just sit in `pending` until promoted.
            let surfaced = |who: &[u8; 32]| {
                canvas.open || who == &canvas.author || canvas.members.contains(who)
            };

            let mut nodes: Vec<NodeView> = Vec::new();
            let mut pending: Vec<NodeView> = Vec::new();
            // Node ids that resolved to a live (non-deleted) surfaced node, so an
            // edge can check both its endpoints exist before being drawn.
            let mut live: HashMap<[u8; 32], ()> = HashMap::new();

            for node in self.nodes.values() {
                if &node.canvas_author.to_vec() != author || &node.canvas_id != canvas_id {
                    continue;
                }

                let visible = surfaced(&node.author);
                // Visible nodes resolve overlays from any surfaced author; a
                // pending (stranger) node resolves only its own creator's edits.
                let allow: AllowFn = if visible {
                    Box::new(surfaced)
                } else {
                    let creator = node.author;
                    Box::new(move |w: &[u8; 32]| w == &creator)
                };

                let transform = pick_latest(self.transforms.get(&node.id), &allow);
                // A live tombstone removes the node entirely (drop, don't list).
                if transform.is_some_and(|t| t.deleted) {
                    continue;
                }

                let geo = transform.map(|t| t.geo).unwrap_or(node.geo);
                let z = transform.map(|t| t.z.clone()).unwrap_or_default();
                let color = transform.and_then(|t| t.color.clone());
                let placed_at = transform.map(|t| t.created_at).unwrap_or(node.created_at);
                let content_overlay = pick_latest(self.contents.get(&node.id), &allow);
                let content = content_overlay
                    .map(|c| c.content.clone())
                    .unwrap_or_else(|| node.content.clone());
                let edited_at = content_overlay
                    .map(|c| c.created_at)
                    .unwrap_or(node.created_at);

                let view = NodeView {
                    id: NoteId::new(node.id),
                    author: node.author,
                    kind: node.kind,
                    geo,
                    z,
                    color,
                    content,
                    created_at: node.created_at,
                    placed_at,
                    edited_at,
                };

                if visible {
                    live.insert(node.id, ());
                    nodes.push(view);
                } else {
                    pending.push(view);
                }
            }

            // Back-to-front: by z-rank, falling back to creation time for nodes
            // that have never been explicitly stacked (empty rank sorts first).
            nodes.sort_by(|a, b| {
                (a.z.as_str(), a.created_at, a.id.bytes()).cmp(&(
                    b.z.as_str(),
                    b.created_at,
                    b.id.bytes(),
                ))
            });
            pending.sort_by(|a, b| a.id.bytes().cmp(b.id.bytes()));

            let mut edges: Vec<EdgeView> = self
                .edges
                .values()
                .filter_map(|by_author| pick_latest(Some(by_author), &surfaced))
                .filter(|e| !e.deleted && live.contains_key(&e.from) && live.contains_key(&e.to))
                .map(|e| EdgeView {
                    id: e.edge_id.clone(),
                    author: e.author,
                    from: NoteId::new(e.from),
                    to: NoteId::new(e.to),
                    ends: e.ends.clone(),
                    placed_at: e.created_at,
                })
                .collect();
            edges.sort_by(|a, b| a.id.cmp(&b.id));

            views.push(CanvasView {
                id: canvas_id.clone(),
                author: canvas.author,
                title: canvas.title.clone(),
                members: canvas.members.clone(),
                open: canvas.open,
                created_at: canvas.created_at,
                nodes,
                edges,
                pending,
            });
        }

        views.sort_by(|a, b| a.id.cmp(&b.id));
        views
    }
}

/// Resolve a set of notebook events into the canvases they describe.
pub fn reduce(events: &[NotebookEvent]) -> Vec<CanvasView> {
    let mut reducer = CanvasReducer::default();
    for event in events {
        reducer.ingest(event.clone());
    }
    reducer.finalize()
}

// ---------------------------------------------------------------------------
// ndb loading
// ---------------------------------------------------------------------------

/// A filter for every notebook event authored by `author`.
///
/// Single-author for now (like early headway): this captures a canvas and all of
/// *its owner's* nodes/overlays/edges. Surfacing other collaborators' events
/// will additionally need an `#a`-tag filter on the canvas address — the reducer
/// is already multi-author (it keys overlays by author); only this query is
/// owner-scoped.
pub fn notebook_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .authors([author.bytes()])
        .kinds(NOTEBOOK_KINDS.iter().map(|k| *k as u64))
        .limit(20000)
        .build()
}

/// Fold all of `author`'s notebook events out of `ndb` into a fresh reducer.
pub fn fold_canvas(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Option<CanvasReducer> {
    ndb.fold(
        txn,
        &[notebook_filter(author)],
        CanvasReducer::default(),
        |mut acc, note| {
            if let Some(event) = parse(&note) {
                acc.ingest(event);
            }
            acc
        },
    )
    .ok()
}

/// Fold a batch of freshly-arrived notes (by `keys`) into an existing reducer.
/// Sound because the fold is commutative and idempotent.
pub fn reduce_delta(reducer: &mut CanvasReducer, ndb: &Ndb, txn: &Transaction, keys: &[NoteKey]) {
    for key in keys {
        if let Ok(note) = ndb.get_note_by_key(txn, *key)
            && let Some(event) = parse(&note)
        {
            reducer.ingest(event);
        }
    }
}

/// Pick the canvas with `canvas_id` authored by `author` out of a reducer.
pub fn pick_canvas(
    reducer: &CanvasReducer,
    author: &Pubkey,
    canvas_id: &str,
) -> Option<CanvasView> {
    reducer
        .finalize()
        .into_iter()
        .find(|v| v.id == canvas_id && &v.author == author.bytes())
}

/// One-shot [`fold_canvas`] + [`pick_canvas`] for callers that don't keep the
/// reducer around.
pub fn load_canvas(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    canvas_id: &str,
) -> Option<CanvasView> {
    pick_canvas(&fold_canvas(ndb, txn, author)?, author, canvas_id)
}

/// Whether `kind` is one of the notebook's addressable (latest-wins, keyed per
/// `(kind, d-tag)`) kinds. Everything but the immutable node-creation event is
/// addressable. Used by the CLI sync to push only the winning revision of each
/// addressable element rather than every stale one (see [`relay_sync::frames_where`]).
pub fn is_addressable(kind: u32) -> bool {
    matches!(
        kind,
        KIND_CANVAS | KIND_TRANSFORM | KIND_CONTENT | KIND_EDGE
    )
}

/// Render a whole canvas as JSON — the machine-readable form of the CLI's `show`.
pub fn canvas_json(view: &CanvasView) -> serde_json::Value {
    serde_json::json!({
        "id": view.id,
        "title": view.title,
        "author": Pubkey::new(view.author).hex(),
        "open": view.open,
        "members": view.members.iter().map(|m| Pubkey::new(*m).hex()).collect::<Vec<_>>(),
        "nodes": view.nodes.iter().map(node_json).collect::<Vec<_>>(),
        "edges": view.edges.iter().map(edge_json).collect::<Vec<_>>(),
        "pending": view.pending.iter().map(node_json).collect::<Vec<_>>(),
    })
}

/// Render a single node as JSON. See [`canvas_json`].
pub fn node_json(node: &NodeView) -> serde_json::Value {
    serde_json::json!({
        "id": node.id.hex(),
        "kind": node.kind.as_str(),
        "x": node.geo.x,
        "y": node.geo.y,
        "w": node.geo.w,
        "h": node.geo.h,
        "z": node.z,
        "color": node.color,
        "text": node.content.text,
    })
}

/// Render a single edge as JSON. See [`canvas_json`].
pub fn edge_json(edge: &EdgeView) -> serde_json::Value {
    serde_json::json!({
        "id": edge.id,
        "from": edge.from.hex(),
        "to": edge.to.hex(),
        "from_side": edge.ends.from_side,
        "to_side": edge.ends.to_side,
        "to_end": edge.ends.to_end,
        "color": edge.ends.color,
        "label": edge.ends.label,
    })
}

// ---------------------------------------------------------------------------
// Fractional ranking (z-order). Mirrors `headway::event::rank_between`.
// ---------------------------------------------------------------------------

const RANK_LOW: u8 = b'a' - 1;
const RANK_HIGH: u8 = b'z' + 1;

/// Produce a rank string that sorts strictly between `left` and `right` (each an
/// optional existing rank); `None` means "open". Used for node z-order, so
/// restacking a node republishes one transform without reindexing the canvas.
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

    fn parse_signed(builder: NoteBuilder, kp: &FullKeypair) -> NotebookEvent {
        let note = builder
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("build note");
        parse(&note).expect("parse notebook event")
    }

    fn node_id(kp: &FullKeypair, builder: NoteBuilder) -> NoteId {
        let note = builder
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("build note");
        NoteId::new(*note.id())
    }

    fn text(s: &str) -> NodeContent {
        NodeContent {
            text: s.to_string(),
            ..Default::default()
        }
    }

    const GEO: Geometry = Geometry {
        x: 0,
        y: 0,
        w: 100,
        h: 40,
    };

    #[test]
    fn canvas_roundtrips() {
        let owner = FullKeypair::generate();
        let member = FullKeypair::generate();
        let NotebookEvent::Canvas(c) =
            parse_signed(build_canvas("c1", "Sketch", &[member.pubkey], true), &owner)
        else {
            panic!("expected canvas");
        };
        assert_eq!(c.id, "c1");
        assert_eq!(c.title, "Sketch");
        assert!(c.open);
        assert_eq!(c.members, vec![*member.pubkey.bytes()]);
        assert_eq!(c.author, *owner.pubkey.bytes());
    }

    #[test]
    fn node_transform_content_edge_roundtrip() {
        let kp = FullKeypair::generate();
        let addr = canvas_address(&kp.pubkey, "c1");
        let geo = Geometry {
            x: -10,
            y: 20,
            w: 250,
            h: 60,
        };

        let NotebookEvent::Node(n) =
            parse_signed(build_node(&addr, NodeKind::Text, &geo, &text("hi")), &kp)
        else {
            panic!("node");
        };
        assert_eq!(n.kind, NodeKind::Text);
        assert_eq!(n.geo, geo);
        assert_eq!(n.content.text, "hi");
        assert_eq!(n.canvas_id, "c1");

        let id = node_id(&kp, build_node(&addr, NodeKind::Text, &geo, &text("hi")));

        let NotebookEvent::Transform(t) =
            parse_signed(build_transform("c1", &addr, &id, &geo, "m", Some("3")), &kp)
        else {
            panic!("transform");
        };
        assert_eq!(t.node_id, *id.bytes());
        assert_eq!(t.z, "m");
        assert_eq!(t.color.as_deref(), Some("3"));
        assert!(!t.deleted);

        let NotebookEvent::Content(c) =
            parse_signed(build_content("c1", &addr, &id, &text("edited")), &kp)
        else {
            panic!("content");
        };
        assert_eq!(c.content.text, "edited");

        let other = node_id(&kp, build_node(&addr, NodeKind::Text, &geo, &text("b")));
        let ends = EdgeEnds {
            to_end: Some("arrow".to_string()),
            label: Some("calls".to_string()),
            ..Default::default()
        };
        let NotebookEvent::Edge(e) =
            parse_signed(build_edge("c1", &addr, "e1", &id, &other, &ends), &kp)
        else {
            panic!("edge");
        };
        assert_eq!(e.edge_id, "e1");
        assert_eq!(e.from, *id.bytes());
        assert_eq!(e.to, *other.bytes());
        assert_eq!(e.ends.label.as_deref(), Some("calls"));
    }

    /// A transform overrides the creation snapshot; a content overlay overrides
    /// the creation text; nodes sort by z-rank.
    #[test]
    fn reduce_resolves_overlays_and_z_order() {
        let owner = FullKeypair::generate();
        let addr = canvas_address(&owner.pubkey, "c1");

        let a = node_id(&owner, build_node(&addr, NodeKind::Text, &GEO, &text("A")));
        let b = node_id(&owner, build_node(&addr, NodeKind::Text, &GEO, &text("B")));

        let events = vec![
            parse_signed(build_canvas("c1", "C", &[], false), &owner),
            parse_signed(build_node(&addr, NodeKind::Text, &GEO, &text("A")), &owner),
            parse_signed(build_node(&addr, NodeKind::Text, &GEO, &text("B")), &owner),
            // A ranked after B; A moved + retitled.
            parse_signed(build_transform("c1", &addr, &b, &GEO, "g", None), &owner),
            parse_signed(
                build_transform("c1", &addr, &a, &Geometry { x: 500, ..GEO }, "t", None),
                &owner,
            ),
            parse_signed(build_content("c1", &addr, &a, &text("A (edited)")), &owner),
        ];

        let views = reduce(&events);
        assert_eq!(views.len(), 1);
        let nodes = &views[0].nodes;
        assert_eq!(nodes.len(), 2);
        // "g" (B) sorts before "t" (A).
        assert_eq!(nodes[0].id, b);
        assert_eq!(nodes[1].id, a);
        assert_eq!(nodes[1].geo.x, 500);
        assert_eq!(nodes[1].content.text, "A (edited)");
    }

    /// A stranger's node is hidden (pending) while the canvas is closed, and
    /// surfaced once it's open.
    #[test]
    fn reduce_filters_non_members_until_open() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = canvas_address(&owner.pubkey, "c1");

        let closed = vec![
            parse_signed(build_canvas("c1", "C", &[], false), &owner),
            parse_signed(
                build_node(&addr, NodeKind::Text, &GEO, &text("mine")),
                &owner,
            ),
            parse_signed(
                build_node(&addr, NodeKind::Text, &GEO, &text("theirs")),
                &stranger,
            ),
        ];
        let view = &reduce(&closed)[0];
        assert_eq!(view.nodes.len(), 1, "only the owner's node is surfaced");
        assert_eq!(view.nodes[0].content.text, "mine");
        assert_eq!(view.pending.len(), 1, "the stranger's node is pending");
        assert_eq!(view.pending[0].content.text, "theirs");

        let open = vec![
            parse_signed(build_canvas("c1", "C", &[], true), &owner),
            parse_signed(
                build_node(&addr, NodeKind::Text, &GEO, &text("mine")),
                &owner,
            ),
            parse_signed(
                build_node(&addr, NodeKind::Text, &GEO, &text("theirs")),
                &stranger,
            ),
        ];
        let view = &reduce(&open)[0];
        assert_eq!(view.nodes.len(), 2, "open surfaces everyone");
        assert!(view.pending.is_empty());
    }

    /// A member's edit to the owner's node wins; a stranger's later edit does
    /// not (while closed).
    #[test]
    fn reduce_member_can_edit_stranger_cannot() {
        let owner = FullKeypair::generate();
        let member = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = canvas_address(&owner.pubkey, "c1");
        let node = node_id(
            &owner,
            build_node(&addr, NodeKind::Text, &GEO, &text("orig")),
        );

        let mut events = vec![
            parse_signed(build_canvas("c1", "C", &[member.pubkey], false), &owner),
            parse_signed(
                build_node(&addr, NodeKind::Text, &GEO, &text("orig")),
                &owner,
            ),
        ];
        // Stranger renames it later (higher created_at) — must be ignored.
        let mut bad = match parse_signed(
            build_content("c1", &addr, &node, &text("hijacked")),
            &stranger,
        ) {
            NotebookEvent::Content(c) => c,
            _ => unreachable!(),
        };
        bad.created_at += 5;
        events.push(NotebookEvent::Content(bad));
        // Member renames it (earlier) — should still win over the creation text.
        events.push(parse_signed(
            build_content("c1", &addr, &node, &text("by member")),
            &member,
        ));

        let view = &reduce(&events)[0];
        assert_eq!(view.nodes[0].content.text, "by member");
    }

    #[test]
    fn reduce_tombstone_drops_node_and_its_edges() {
        let owner = FullKeypair::generate();
        let addr = canvas_address(&owner.pubkey, "c1");
        let a = node_id(&owner, build_node(&addr, NodeKind::Text, &GEO, &text("A")));
        let b = node_id(&owner, build_node(&addr, NodeKind::Text, &GEO, &text("B")));

        let mut events = vec![
            parse_signed(build_canvas("c1", "C", &[], false), &owner),
            parse_signed(build_node(&addr, NodeKind::Text, &GEO, &text("A")), &owner),
            parse_signed(build_node(&addr, NodeKind::Text, &GEO, &text("B")), &owner),
            parse_signed(build_transform("c1", &addr, &a, &GEO, "g", None), &owner),
            parse_signed(build_transform("c1", &addr, &b, &GEO, "m", None), &owner),
            parse_signed(
                build_edge("c1", &addr, "e1", &a, &b, &EdgeEnds::default()),
                &owner,
            ),
        ];

        let view = &reduce(&events)[0];
        assert_eq!(view.nodes.len(), 2);
        assert_eq!(view.edges.len(), 1);

        // Tombstone B with a later transform.
        let mut tomb = match parse_signed(build_node_tombstone("c1", &addr, &b, "m"), &owner) {
            NotebookEvent::Transform(t) => t,
            _ => unreachable!(),
        };
        tomb.created_at += 5;
        events.push(NotebookEvent::Transform(tomb));

        let view = &reduce(&events)[0];
        assert_eq!(view.nodes.len(), 1, "B is gone");
        assert_eq!(view.nodes[0].id, a);
        assert!(view.edges.is_empty(), "the dangling edge is dropped");
    }

    #[test]
    fn rank_between_orders_consistently() {
        let mut last = rank_between(None, None);
        for _ in 0..50 {
            let next = rank_between(Some(&last), None);
            assert!(next > last);
            last = next;
        }
        let a = rank_between(None, None);
        let b = rank_between(Some(&a), None);
        let mid = rank_between(Some(&a), Some(&b));
        assert!(mid > a && mid < b);
    }

    /// End-to-end through a real nostrdb: ingest signed events and wait on a
    /// subscription (not a sleep loop) for them to commit, then fold them back
    /// out with [`load_canvas`] and check the canvas reconstructs.
    #[test]
    fn load_canvas_roundtrips_through_ndb() {
        use futures_util::StreamExt;
        use nostrdb::{Config, IngestMetadata, Ndb, SubscriptionStream};

        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let kp = FullKeypair::generate();
        let addr = canvas_address(&kp.pubkey, "c1");

        // Subscribe *before* ingesting so every commit wakes the stream.
        let sub = ndb.subscribe(&[notebook_filter(&kp.pubkey)]).unwrap();
        let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);

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

        ingest(build_canvas("c1", "Canvas", &[], false));
        let a = ingest(build_node(&addr, NodeKind::Text, &GEO, &text("A")));
        ingest(build_transform("c1", &addr, &a, &GEO, "m", None));
        ingest(build_content("c1", &addr, &a, &text("A (renamed)")));

        // Wait for all four events to commit by draining the subscription.
        pollster::block_on(async {
            let mut seen = 0;
            while seen < 4 {
                seen += stream.next().await.expect("subscription open").len();
            }
        });

        let txn = Transaction::new(&ndb).unwrap();
        let view = load_canvas(&ndb, &txn, &kp.pubkey, "c1").expect("canvas materialised");
        assert_eq!(view.title, "Canvas");
        assert_eq!(view.nodes.len(), 1);
        assert_eq!(view.nodes[0].content.text, "A (renamed)");
    }
}
