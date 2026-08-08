pub mod convert;
mod editor;
pub mod event;
mod reference;
pub mod render;
pub mod store;
mod ui;
pub mod wordid;

use crate::convert::view_to_canvas;
use crate::editor::{
    EditorAction, LongformEditor, SavedLongform, VaultAction, VaultRow, VaultState, editor_ui,
    vault_rows, vault_ui,
};
use crate::event::{CanvasReducer, CanvasView};
use crate::store::CanvasAction;
use crate::ui::{node_rect, notebook_ui, side_str};
use egui::{Pos2, Rect};
use enostr::{NoteId, Pubkey};
use jsoncanvas::{JsonCanvas, NodeId, edge::Side};
use nostrdb::{Filter, Ndb, NoteKey, Subscription, Transaction};
use notedeck::{AppContext, AppResponse, PrivateRelaySync, fan_out_unseen_notes};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Whether `kind` is a notebook event that the app draws an inline widget for, so
/// a click on that widget (raised as an [`notedeck::AppAction::Note`]) routes into
/// the Notebook app rather than the timeline. Only kind-1606 nodes qualify today —
/// that's the one kind [`render::NotebookNodeRenderer`] handles. [`event::KIND_CANVAS`]
/// is deliberately excluded: there's no canvas renderer to click, so nothing emits
/// a canvas open, and routing one would have no node to focus.
pub fn is_notebook_kind(kind: u32) -> bool {
    kind == event::KIND_NODE
}

/// Where a clicked notebook node should land, resolved from its kind-1606 event by
/// [`resolve_open_target`].
struct NotebookOpenTarget {
    /// The canvas the node lives on (its `d` id).
    canvas_id: String,
    /// The node to focus, keyed by its `jsoncanvas` id (the hex of its creation
    /// event id — see [`convert`] and `Notebook::intent_to_action`).
    node: NodeId,
}

/// Resolve a kind-1606 node note into the [`NotebookOpenTarget`] for
/// [`Notebook::open`]. `None` for anything that isn't a well-formed node event.
fn resolve_open_target(ndb: &Ndb, note_id: NoteId) -> Option<NotebookOpenTarget> {
    let txn = Transaction::new(ndb).ok()?;
    let note = ndb.get_note_by_id(&txn, note_id.bytes()).ok()?;
    let event::NotebookEvent::Node(node) = event::parse(&note)? else {
        return None;
    };
    Some(NotebookOpenTarget {
        canvas_id: node.canvas_id,
        node: NoteId::new(node.id).hex().parse().ok()?,
    })
}

/// A node's in-progress geometry override during a live drag or resize. Each
/// axis is independently optional: a plain body drag sets only [`pos`]; a resize
/// sets whichever of [`pos`]/[`width`]/[`height`] its handle controls. Held in
/// [`Notebook::live`] and cleared on the next fold, which carries the committed
/// geometry.
///
/// [`pos`]: LiveGeometry::pos
/// [`width`]: LiveGeometry::width
/// [`height`]: LiveGeometry::height
#[derive(Default, Clone, Copy)]
pub(crate) struct LiveGeometry {
    /// Top-left override (a live drag, or the shifted anchor of a left/top resize).
    pub pos: Option<Pos2>,
    /// Exact width override; narrowing reflows the node's content.
    pub width: Option<f32>,
    /// Declared-height override: the height the user dragged the box to. The box
    /// still never renders shorter than its content, so this can shrink the box
    /// down to (but not below) the content floor. See [`Notebook::node_rect`].
    pub height: Option<f32>,
}

/// An Obsidian-style infinite canvas, backed by nostr events in the local
/// nostrdb. A shared [`NotebookCache`] keeps a long-lived reducer over the
/// account's canvas events and folds only freshly-arrived notes in as an ndb
/// subscription reports them; the foreground canvas and every inline widget read
/// that same realtime state. Every edit is turned into a signed event ingested
/// locally (see [`store`]) and fanned out to the account's private relays.
pub struct Notebook {
    /// Which canvas this instance manages (single canvas for now).
    canvas_id: String,
    /// The one canvas-data engine (see [`NotebookCache`]): the account's folded
    /// canvas reducer behind a per-frame-pumped [`notedeck::RealtimeCache`], shared
    /// (behind `Rc<RefCell<…>>`) with the inline widgets this app registers so a
    /// chip drawn elsewhere reads the same realtime canvas as the open surface.
    cache: Rc<RefCell<NotebookCache>>,
    /// The account's longform vault (a separate subscription + list). Kept apart
    /// from [`cache`](Self::cache) because longform notes stand alone and must
    /// never fold into a canvas (see [`event::KIND_LONGFORM`]); only the foreground
    /// reads the vault, so — unlike the canvas cache — it isn't shared out.
    vault_sync: VaultSync,
    /// The active canvas ([`canvas_id`](Self::canvas_id)) projected from
    /// [`cache`](Self::cache) on the last change — the [`CanvasView`] the foreground
    /// render and edit path read without re-folding. `None` until the first fold,
    /// or when no canvas with this id exists yet.
    view: Option<CanvasView>,
    /// Inbound cross-device sync: declares a live + full-history subscription to
    /// the account's private relays each frame, and resolves the outbound
    /// publish targets.
    private_sync: PrivateRelaySync,
    /// The folded canvas converted to `jsoncanvas` for rendering. Rebuilt
    /// whenever the sync reports a change.
    canvas: JsonCanvas,
    scene_rect: Rect,
    loaded: bool,
    /// Per-node live geometry overrides applied during an in-progress drag or
    /// resize, in canvas coords/pixels. Cleared when a fresh fold lands (which
    /// then carries the committed geometry). See [`LiveGeometry`].
    live: HashMap<NodeId, LiveGeometry>,
    /// This frame's eased top-left per node, in canvas coords — egui interpolates
    /// toward each node's committed position, so a node that jumped (a drag
    /// release, or a `notebook move` over the relay) slides instead of teleporting.
    /// Read by [`Notebook::node_rect`] so nodes, edges and handles follow together.
    /// Rebuilt every frame from egui's animation manager (the real state lives
    /// there, keyed by [`move_anim_ids`]).
    anim_pos: HashMap<NodeId, Pos2>,
    /// Per-node *intrinsic* content height (margins included), measured last
    /// frame — the height the node's content (markdown, embedded note widgets)
    /// actually needs, independent of the box it was drawn in. [`Notebook::node_rect`]
    /// uses it as the box's floor: the box never renders shorter than this, so
    /// edges and connection handles land on the real box edge, and a resize can
    /// shrink the declared height no further than the content requires. Measuring
    /// the padded box instead would ratchet the height and forbid shrinking.
    rendered_heights: HashMap<NodeId, f32>,
    /// Currently selected node, if any.
    selected: Option<NodeId>,
    /// A node to focus, set by [`open`](Notebook::open) when its inline widget is
    /// clicked elsewhere in the app (e.g. a `notebook:<word-id>` chip in a note or
    /// Dave chat). Resolved by [`process_pending_open`](Notebook::process_pending_open)
    /// on the next render: select the node once its canvas has folded in.
    pending_open: Option<NoteId>,
    /// The viewport centre an in-flight reveal pan is easing toward, in canvas
    /// coords. Set by [`reveal_node`](Notebook::reveal_node) so a node opened from
    /// a reference glides into view instead of snapping; [`update_pan`](Notebook::update_pan)
    /// eases `scene_rect` toward it each frame and clears it on arrival. `None`
    /// when no pan is running, so ordinary user panning owns `scene_rect`.
    pan_target: Option<Pos2>,
    /// The node an edge is currently being dragged from, if any. Persisted across
    /// frames so its side handles stay alive (and the egui drag keeps its id)
    /// even once the pointer leaves the source node.
    connecting: Option<NodeId>,
    /// Inline text-editing state.
    edit: NodeEdit,
    /// A node awaiting delete confirmation. Set by the Delete key (with a node
    /// selected) or the node's context menu; while it's `Some`, a confirmation
    /// modal is shown and the actual delete fires only once confirmed.
    confirm_delete: Option<NodeId>,
    /// Whether we've auto-seeded a canvas this session, so we don't seed twice
    /// while the first seed is still materialising.
    seeded: bool,
    /// Countdown of follow-up repaints after an async ingest, so we keep waking
    /// to poll the subscription until the writer thread goes quiet.
    repaint_frames: u8,
    /// The full-screen longform (NIP-23) editor, when open. `None` shows the
    /// canvas; `Some` replaces it with [`editor_ui`]. Opened from the toolbar's
    /// "＋ Note" button and dismissed by the editor's Close.
    editor: Option<LongformEditor>,
    /// The vault sidebar's transient interaction state (an inline rename in
    /// progress, or a delete awaiting confirmation). Driven by [`vault_ui`], which
    /// surfaces each completed interaction as a [`VaultAction`].
    vault: VaultState,
    /// The vault rows to render, projected from [`VaultSync::notes`] whenever
    /// the sync reports a change (see [`vault_rows`]). Precomputed off the render
    /// loop so the per-row "edited …" subtitle is never formatted per frame.
    vault_rows: Vec<VaultRow>,
}

/// Inline text-editing state for the notebook canvas. Nodes are tracked by their
/// `jsoncanvas` id (the rendered id); the backend maps that to a nostr id when
/// committing an edit.
pub(crate) enum NodeEdit {
    /// No node is being edited.
    Idle,
    /// An existing text node is being edited; `buffer` holds the working text,
    /// committed on blur (or a delete if blanked), discarded on Esc.
    Editing {
        node: NodeId,
        buffer: String,
        request_focus: bool,
    },
    /// A brand-new text node being composed at a canvas position — not yet
    /// created. Committed on blur (discarded if blank or on Esc), so an empty box
    /// never reaches the canvas.
    Creating {
        pos: Pos2,
        buffer: String,
        request_focus: bool,
    },
}

/// A committed edit the UI surfaced this frame, keyed by the rendered
/// `jsoncanvas` node id. The backend ([`Notebook::render`]) turns each into a
/// nostr [`CanvasAction`] (online) or an in-place canvas mutation (local).
pub(crate) enum UiIntent {
    /// A node was dragged to `pos` (its new top-left, in canvas coords).
    Move { node: NodeId, pos: Pos2 },
    /// A node was resized: `pos` is its (possibly shifted, for a left/top-edge
    /// drag) top-left and `width`/`height` its new size, in canvas coords. The
    /// height is the declared box height, clamped up to the content when rendered
    /// (see [`Notebook::node_rect`]).
    Resize {
        node: NodeId,
        pos: Pos2,
        width: f32,
        height: f32,
    },
    /// An existing text node's text was edited.
    EditText { node: NodeId, text: String },
    /// A new text node was composed at `pos`.
    Create { pos: Pos2, text: String },
    /// A vault note was dragged onto the canvas and dropped at `pos` — drops a
    /// note-embed [`Link`](event::NodeKind::Link) node referencing that note by
    /// naddr, the same node paste-aware [`Create`](Self::Create) builds from a lone
    /// `nostr:` reference.
    DropNoteEmbed {
        author: Pubkey,
        d: String,
        pos: Pos2,
    },
    /// A node was deleted (its text was blanked).
    Delete { node: NodeId },
    /// An edge was drawn from one node's side to another node's side.
    Connect {
        from: NodeId,
        from_side: Side,
        to: NodeId,
        to_side: Side,
    },
    /// An existing edge was removed (its midpoint delete handle was clicked).
    DisconnectEdge {
        edge_id: String,
        from: NodeId,
        to: NodeId,
    },
}

/// Size of the compose editor box shown while typing a freshly-created node, in
/// canvas pixels — deliberately roomy so there's space to type.
pub(crate) const NEW_NODE_SIZE: egui::Vec2 = egui::vec2(250.0, 120.0);

/// Committed height of a freshly-created node, in canvas pixels. A tight value
/// so the box hugs its content (which drives the box taller as needed; the box
/// renders at whichever of the declared and content heights is taller, see
/// [`Notebook::node_rect`]). Kept below the roomier [`NEW_NODE_SIZE`] compose box
/// so a new node settles onto its content, not the editor's typing height.
const NEW_NODE_HEIGHT: f32 = 40.0;

/// `text` (trimmed) as a single whole `nostr:` reference, or `None` when it's
/// anything more than a lone reference. Used to promote a composed body that is
/// exactly one reference into a note-embed [`Link`](event::NodeKind::Link) node,
/// so it renders as a full note rather than an inline chip in a one-line text
/// node. Reuses the built-in [`NostrRefParser`](notedeck::NostrRefParser) so the
/// match rule stays identical to inline rendering.
fn whole_nostr_reference(text: &str) -> Option<&str> {
    use notedeck::{NostrRefParser, ReferenceParser};
    let trimmed = text.trim();
    let range = NostrRefParser.find(trimmed)?;
    (range.start == 0 && range.end == trimmed.len()).then_some(trimmed)
}

/// The [`CanvasAction`] that drops a note-embed node at `pos`: a `Link` carrying
/// the lone `nostr:` reference, drawn full-node by its kind renderer rather than as
/// an inline chip. Shared by paste-aware create and vault drag-drop so both promote
/// a reference identically. Sized like a freshly-created node; the embed grows the
/// box to its content height (see [`Notebook::node_rect`]).
fn embed_node_action(reference: String, pos: Pos2) -> CanvasAction {
    use crate::event::{Geometry, NodeContent, NodeKind};
    CanvasAction::AddNode {
        kind: NodeKind::Link,
        geo: Geometry {
            x: pos.x as i64,
            y: pos.y as i64,
            w: NEW_NODE_SIZE.x as u64,
            h: NEW_NODE_HEIGHT as u64,
        },
        content: NodeContent {
            url: Some(reference),
            ..Default::default()
        },
    }
}

/// How long a node's slide-to-new-position animation runs, in seconds. Matches
/// headway's card-move feel.
const MOVE_ANIM_SECS: f32 = 0.28;

/// The egui animation-manager ids holding a node's animated x and y. egui keeps
/// the previous value per id and eases toward a new target on its own, so feeding
/// the committed position each frame is all the slide needs.
fn move_anim_ids(id: &NodeId) -> (egui::Id, egui::Id) {
    (
        egui::Id::new(("notebook-move-x", id)),
        egui::Id::new(("notebook-move-y", id)),
    )
}

/// How long the reveal-node camera pan runs, in seconds. A touch longer than a
/// node slide since it sweeps the whole viewport rather than one box.
const PAN_ANIM_SECS: f32 = 0.4;

/// The egui animation-manager ids holding the panned viewport centre's x and y.
/// A single reused pair (unlike per-node [`move_anim_ids`]) — only one reveal
/// pan runs at a time.
fn pan_anim_ids() -> (egui::Id, egui::Id) {
    (
        egui::Id::new("notebook-pan-x"),
        egui::Id::new("notebook-pan-y"),
    )
}

impl Notebook {
    pub fn new() -> Self {
        Notebook::default()
    }

    /// The node's current rect, accounting for any live drag/resize override and
    /// the actual rendered height measured last frame (content can overflow the
    /// declared height, so the visible box — what edges should anchor to — is
    /// taller than the canvas geometry).
    pub(crate) fn node_rect(&self, id: &NodeId, node: &jsoncanvas::Node) -> Rect {
        let default = node_rect(node.node());
        let live = self.live.get(id).copied().unwrap_or_default();
        // Position precedence: a live drag/resize (the user's hand) wins; else a
        // move animation in flight; else the committed geometry.
        let min = live
            .pos
            .or_else(|| self.anim_pos.get(id).copied())
            .unwrap_or(default.min);
        // Width is exact (the live override, else committed).
        let width = live.width.unwrap_or(default.width());
        // Height is the user's declared height clamped up to the content: the box
        // renders at whichever is taller, so it can be resized shorter than it is
        // now (down to the content) yet never clips content. `rendered_heights`
        // holds the *intrinsic* content height, so this no longer ratchets.
        let declared = live.height.unwrap_or(default.height());
        let height = self
            .rendered_heights
            .get(id)
            .map_or(declared, |content| content.max(declared));
        Rect::from_min_size(min, egui::vec2(width, height))
    }

    /// The width/height a resize should commit: the live override for whichever
    /// axis the user dragged, else the committed value — so an unchanged axis is a
    /// no-op write that can't clobber, e.g., a height floor on a width-only drag.
    fn resize_size(&self, id: &NodeId) -> Option<(f32, f32)> {
        let node = self.canvas.get_nodes().get(id)?.node();
        let live = self.live.get(id).copied().unwrap_or_default();
        Some((
            live.width.unwrap_or(node.width as f32),
            live.height.unwrap_or(node.height as f32),
        ))
    }

    /// The node's current top-left position (after any live-drag override).
    pub fn node_position(&self, id: &NodeId) -> Option<Pos2> {
        let node = self.canvas.get_nodes().get(id)?;
        Some(self.node_rect(id, node).min)
    }

    /// The currently selected node, if any.
    pub fn selected(&self) -> Option<&NodeId> {
        self.selected.as_ref()
    }

    /// Focus a node referenced from elsewhere in the app — raised when its inline
    /// widget (drawn by [`render::NotebookNodeRenderer`]) is clicked in another app
    /// like a note or Dave chat. `note` is the kind-1606 node event; the focus
    /// happens on the next render (see [`process_pending_open`](Self::process_pending_open)).
    pub fn open(&mut self, note: NoteId) {
        self.pending_open = Some(note);
        // Wake so the focus is processed even if nothing else is repainting.
        self.wake();
    }

    /// Act on a pending [`open`](Self::open): resolve the node's canvas and reveal
    /// it — select it *and* pan the canvas to it — once that canvas has folded in.
    /// Runs early each render.
    ///
    /// The notebook is single-canvas for now, so a node on a different canvas is a
    /// no-op (multi-canvas switching isn't built yet). A node on the open canvas is
    /// revealed as soon as it folds *and* the scene has been laid out; until then
    /// the request stays pending and [`open`](Self::open)'s repaint burst keeps us
    /// ticking. The scene wait matters on the very first render after switching to
    /// the app: [`notebook_ui`](crate::ui::notebook_ui) initialises `scene_rect`
    /// from the viewport (clearing `loaded`), and it runs *after* this — so panning
    /// before that first layout would just be overwritten.
    #[profiling::function]
    fn process_pending_open(&mut self, ndb: &Ndb, ctx: &egui::Context) {
        let Some(note_id) = self.pending_open else {
            return;
        };
        let Some(target) = resolve_open_target(ndb, note_id) else {
            // Not a notebook node we can route to (unresolved / unexpected kind).
            self.pending_open = None;
            return;
        };
        if target.canvas_id != self.canvas_id {
            // A node on a canvas this instance doesn't manage. TODO: multi-canvas
            // switching — for now there's nowhere to focus it, so drop the request.
            self.pending_open = None;
            return;
        }
        // The node's rect this frame, or `None` if the canvas hasn't folded it in
        // yet — retry on the next repaint. Computed before any `&mut self` so the
        // canvas borrow is released before `reveal_node` moves the view.
        let Some(center) = self
            .canvas
            .get_nodes()
            .get(&target.node)
            .map(|node| self.node_rect(&target.node, node).center())
        else {
            return;
        };
        if !self.loaded {
            // The scene hasn't been laid out this session, so `scene_rect` isn't a
            // real viewport yet (and `notebook_ui` would overwrite our pan this
            // frame). Wait for the first canvas render; the wake burst keeps us
            // ticking until `loaded` flips.
            return;
        }
        self.reveal_node(ctx, target.node, center);
        self.pending_open = None;
        self.wake();
    }

    /// Select `node` and start an animated pan so it eases to the centre of the
    /// viewport. `center` is the node's centre in canvas coords. Only the pan is
    /// started here — [`update_pan`](Self::update_pan) drives `scene_rect` toward
    /// it over the next frames. Used by [`process_pending_open`] to reveal a node
    /// opened from elsewhere, which may sit anywhere on the canvas.
    ///
    /// egui's animation manager eases from the value it last saw for an id, so we
    /// seed it at the *current* viewport centre (time 0) — otherwise the first
    /// `update_pan` would ease from a stale value (or snap straight to the target).
    fn reveal_node(&mut self, ctx: &egui::Context, node: NodeId, center: Pos2) {
        self.selected = Some(node);
        let start = self.scene_rect.center();
        let (x_id, y_id) = pan_anim_ids();
        ctx.animate_value_with_time(x_id, start.x, 0.0);
        ctx.animate_value_with_time(y_id, start.y, 0.0);
        self.pan_target = Some(center);
    }

    /// Ease `scene_rect` toward a pending reveal pan ([`pan_target`](Self::pan_target),
    /// set by [`reveal_node`](Self::reveal_node)) via egui's animation manager,
    /// which self-schedules the repaints. A pan only — `scene_rect`'s size (zoom
    /// and aspect) is preserved each frame; only the centre moves. Clears the
    /// target once the centre has arrived, handing `scene_rect` back to ordinary
    /// user panning. A no-op when nothing is panning.
    ///
    /// Runs each render before the scene is drawn, so this frame's pan is visible
    /// immediately.
    fn update_pan(&mut self, ctx: &egui::Context) {
        let Some(target) = self.pan_target else {
            return;
        };
        let (x_id, y_id) = pan_anim_ids();
        let x = ctx.animate_value_with_time(x_id, target.x, PAN_ANIM_SECS);
        let y = ctx.animate_value_with_time(y_id, target.y, PAN_ANIM_SECS);
        let center = Pos2::new(x, y);
        self.scene_rect = Rect::from_center_size(center, self.scene_rect.size());
        // Within a subpixel of the target: the ease is done, stop overriding.
        if (center - target).length() <= 0.5 {
            self.pan_target = None;
        }
    }

    /// The currently rendered canvas (folded view converted to `jsoncanvas`).
    /// Exposed for tests/introspection.
    pub fn canvas(&self) -> &JsonCanvas {
        &self.canvas
    }

    /// The visible region of the canvas in canvas coordinates — what the Scene
    /// maps onto the viewport (its size encodes the zoom). Exposed for
    /// tests/introspection so a test can assert a revealed node lands in view.
    pub fn scene_rect(&self) -> Rect {
        self.scene_rect
    }

    /// The cached vault note list (newest-edited first). Exposed for
    /// tests/introspection so a seed barrier can wait for every longform note to
    /// fold in before snapshotting.
    pub fn notes(&self) -> &[event::LongformNote] {
        self.vault_sync.notes()
    }

    /// Whether the full-screen longform editor is open (the canvas is hidden
    /// while it is). Exposed for tests/introspection.
    pub fn editor_is_open(&self) -> bool {
        self.editor.is_some()
    }

    /// The `(d, created_at)` of the note the open editor has persisted, or `None`
    /// if the editor is closed or its note is unsaved. Exposed for
    /// tests/introspection.
    pub fn editor_saved(&self) -> Option<(&str, u64)> {
        self.editor
            .as_ref()?
            .saved
            .as_ref()
            .map(|s| (s.d.as_str(), s.created_at))
    }

    /// Translate a UI intent (keyed by the rendered `jsoncanvas` id) into a nostr
    /// [`CanvasAction`]. Reads the current canvas for a moved node's size (a
    /// transform is a full geometry snapshot). `None` if the node id isn't a
    /// valid nostr id (so it can be filtered out).
    fn intent_to_action(&self, intent: UiIntent) -> Option<CanvasAction> {
        use crate::event::{EdgeEnds, Geometry, NodeContent, NodeKind};
        match intent {
            UiIntent::Move { node, pos } => {
                let g = self.canvas.get_nodes().get(&node)?.node();
                Some(CanvasAction::SetGeometry {
                    node: NoteId::from_hex(node.as_str()).ok()?,
                    geo: Geometry {
                        x: pos.x as i64,
                        y: pos.y as i64,
                        w: g.width,
                        h: g.height,
                    },
                })
            }
            UiIntent::Resize {
                node,
                pos,
                width,
                height,
            } => Some(CanvasAction::SetGeometry {
                node: NoteId::from_hex(node.as_str()).ok()?,
                geo: Geometry {
                    x: pos.x as i64,
                    y: pos.y as i64,
                    w: width as u64,
                    h: height as u64,
                },
            }),
            UiIntent::EditText { node, text } => Some(CanvasAction::EditContent {
                node: NoteId::from_hex(node.as_str()).ok()?,
                content: NodeContent {
                    text,
                    ..Default::default()
                },
            }),
            UiIntent::Delete { node } => Some(CanvasAction::DeleteNode {
                node: NoteId::from_hex(node.as_str()).ok()?,
            }),
            UiIntent::Create { pos, text } => {
                let geo = Geometry {
                    x: pos.x as i64,
                    y: pos.y as i64,
                    w: NEW_NODE_SIZE.x as u64,
                    h: NEW_NODE_HEIGHT as u64,
                };
                // A composed body that is *exactly* one `nostr:` reference
                // becomes a note-embed node — a Link carrying the reference,
                // drawn full-node by its kind renderer — rather than a text node
                // that would only render it as an inline chip on one line.
                Some(match whole_nostr_reference(&text) {
                    Some(reference) => embed_node_action(reference.to_owned(), pos),
                    None => CanvasAction::AddNode {
                        kind: NodeKind::Text,
                        geo,
                        content: NodeContent {
                            text,
                            ..Default::default()
                        },
                    },
                })
            }
            // A vault note dragged onto the canvas drops the same note-embed node
            // paste-aware create builds, but from the note's coordinate: resolve its
            // `nostr:naddr` and place the node at the drop position.
            UiIntent::DropNoteEmbed { author, d, pos } => {
                Some(embed_node_action(event::longform_naddr(&author, &d)?, pos))
            }
            UiIntent::Connect {
                from,
                from_side,
                to,
                to_side,
            } => {
                let from_id = NoteId::from_hex(from.as_str()).ok()?;
                let to_id = NoteId::from_hex(to.as_str()).ok()?;
                // Edge ids are stable per ordered pair, so re-drawing the same
                // connection updates that edge (latest-wins) rather than stacking
                // duplicates. No ':' — the reducer's `d` parse splits on the last.
                Some(CanvasAction::SetEdge {
                    edge_id: format!("{}-{}", from.as_str(), to.as_str()),
                    from: from_id,
                    to: to_id,
                    ends: EdgeEnds {
                        from_side: Some(side_str(&from_side).to_string()),
                        to_side: Some(side_str(&to_side).to_string()),
                        to_end: Some("arrow".to_string()),
                        ..Default::default()
                    },
                })
            }
            UiIntent::DisconnectEdge { edge_id, from, to } => Some(CanvasAction::DeleteEdge {
                edge_id,
                from: NoteId::from_hex(from.as_str()).ok()?,
                to: NoteId::from_hex(to.as_str()).ok()?,
            }),
        }
    }

    /// Schedule a short burst of repaints so a just-ingested event (ingest is
    /// async, on a writer thread) gets polled and surfaced promptly.
    fn wake(&mut self) {
        self.repaint_frames = 8;
    }

    /// Rebuild `anim_pos` for this frame by easing each node toward its committed
    /// position via egui's animation manager. egui remembers the previous value
    /// per id and interpolates whenever the target changes, so a node that jumped
    /// slides on its own — and self-schedules the repaints to do so.
    ///
    /// A node under the user's hand is pinned (zero animation time) to the drag
    /// position instead, so egui's stored value tracks the hand and a release
    /// doesn't snap back to the pre-drag spot before settling.
    fn update_anim_positions(&mut self, ctx: &egui::Context) {
        self.anim_pos.clear();
        let committed: Vec<(NodeId, Pos2)> = self
            .canvas
            .get_nodes()
            .iter()
            .map(|(id, node)| (id.clone(), node_rect(node.node()).min))
            .collect();

        for (id, target) in committed {
            let (x_id, y_id) = move_anim_ids(&id);
            if let Some(dragged) = self.live.get(&id).and_then(|l| l.pos) {
                ctx.animate_value_with_time(x_id, dragged.x, 0.0);
                ctx.animate_value_with_time(y_id, dragged.y, 0.0);
                continue; // drawn via the live-drag override
            }
            let x = ctx.animate_value_with_time(x_id, target.x, MOVE_ANIM_SECS);
            let y = ctx.animate_value_with_time(y_id, target.y, MOVE_ANIM_SECS);
            self.anim_pos.insert(id, Pos2::new(x, y));
        }
    }

    /// Burn down the repaint countdown, requesting a delayed repaint each step.
    /// Driven from [`update`](notedeck::App::update) (which runs every frame for
    /// all opened apps), so the poll/fan-out loop keeps ticking even
    /// off-foreground.
    fn pump_repaint(&mut self, ctx: &egui::Context) {
        if self.repaint_frames > 0 {
            self.repaint_frames -= 1;
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
    }

    /// Render the full-screen longform editor and act on the frame's
    /// [`EditorAction`]: Save persists the buffers; Close persists any pending
    /// changes (save-on-close) and returns to the canvas.
    fn editor_mode(
        &mut self,
        ctx: &mut AppContext,
        ui: &mut egui::Ui,
        author: &Pubkey,
        signer: &Option<[u8; 32]>,
    ) {
        match editor_ui(self.editor.as_mut().expect("editor present"), ctx, ui) {
            None => {}
            Some(EditorAction::Save) => self.save_editor(ctx, author, signer),
            Some(EditorAction::Close) => {
                self.save_editor(ctx, author, signer);
                self.editor = None;
            }
        }
    }

    /// Persist the open editor's buffers as a signed kind-30023 event: a fresh
    /// note the first time (minting a `d`), a superseding edit thereafter. A
    /// watch-only account can't sign, so this is a no-op there; a blank new note
    /// is skipped (mirrors the canvas's blank-node discard). On success the
    /// editor's clean baseline advances and a repaint burst is scheduled so the
    /// async ingest is polled promptly.
    ///
    /// Longform is ingested **local-only** ([`store::NoPublish`]): the note is
    /// PNS-wrapped, and the canvas fan-out path ([`fan_out_unseen_notes`]) fans
    /// *unwrapped* notes, so routing longform through it would leak the plaintext
    /// article. Cross-device longform sync (fanning the kind-1080 wrapper, plus an
    /// inbound 1080 subscription) is tracked as `headway:notebook/merry-patch-boost`.
    fn save_editor(&mut self, ctx: &mut AppContext, author: &Pubkey, signer: &Option<[u8; 32]>) {
        let Some(secret) = signer else { return };
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        // Nothing to write: unchanged since the last save, or an empty new note.
        if !editor.dirty() || (editor.saved.is_none() && editor.is_blank()) {
            return;
        }
        let input = event::LongformInput {
            title: editor.title.clone(),
            content: editor.content.clone(),
            ..Default::default()
        };
        // Copy out the supersede baseline so the `editor` borrow ends before we
        // touch `ctx`/re-borrow `self.editor` below.
        let prev = editor.saved.as_ref().map(|s| (s.d.clone(), s.created_at));

        let saved = match prev {
            None => {
                store::create_longform(ctx.ndb, author, secret, &input, None, &mut store::NoPublish)
            }
            Some((d, created_at)) => store::edit_longform(
                ctx.ndb,
                author,
                secret,
                &d,
                created_at,
                &input,
                &mut store::NoPublish,
            ),
        };

        let Some(saved) = saved else { return };
        if let Some(editor) = self.editor.as_mut() {
            editor.mark_saved(SavedLongform {
                d: saved.d,
                created_at: saved.created_at,
            });
        }
        self.wake();
    }

    /// Apply a completed [`VaultAction`] from the sidebar. Returns whether the
    /// action takes over the view (only Open does — it opens the editor; Rename
    /// and Delete persist in place).
    fn apply_vault_action(
        &mut self,
        ctx: &mut AppContext,
        author: &Pubkey,
        signer: &Option<[u8; 32]>,
        action: VaultAction,
    ) -> bool {
        match action {
            VaultAction::Open { d } => {
                let Some(editor) = self
                    .vault_sync
                    .notes()
                    .iter()
                    .find(|n| n.d == d)
                    .map(LongformEditor::open)
                else {
                    return false;
                };
                self.editor = Some(editor);
                true
            }
            VaultAction::Rename { d, title } => {
                self.rename_note(ctx, author, signer, &d, title);
                false
            }
            VaultAction::Delete { d } => {
                self.delete_note(ctx, author, signer, &d);
                false
            }
        }
    }

    /// Persist an inline rename: supersede note `d` with a title-only edit that
    /// keeps its body. Needs a signing key (a watch-only account can't edit); the
    /// current revision (looked up by `d`) supplies the body and the supersede
    /// baseline. A no-op if the title is unchanged.
    fn rename_note(
        &mut self,
        ctx: &mut AppContext,
        author: &Pubkey,
        signer: &Option<[u8; 32]>,
        d: &str,
        title: String,
    ) {
        let Some(secret) = signer else { return };
        // Copy out what the edit needs so the vault borrow ends before `wake`.
        let Some((content, prev)) = self
            .vault_sync
            .notes()
            .iter()
            .find(|n| n.d == d)
            .filter(|n| n.title != title)
            .map(|n| (n.content.clone(), n.created_at))
        else {
            return;
        };
        let input = event::LongformInput {
            title,
            content,
            ..Default::default()
        };
        store::edit_longform(
            ctx.ndb,
            author,
            secret,
            d,
            prev,
            &input,
            &mut store::NoPublish,
        );
        self.wake();
    }

    /// Persist a confirmed vault delete: tombstone note `d`, superseding its
    /// current revision (whose `created_at`, looked up by `d`, is the supersede
    /// baseline). Needs a signing key.
    fn delete_note(
        &mut self,
        ctx: &mut AppContext,
        author: &Pubkey,
        signer: &Option<[u8; 32]>,
        d: &str,
    ) {
        let Some(secret) = signer else { return };
        let Some(prev) = self
            .vault_sync
            .notes()
            .iter()
            .find(|n| n.d == d)
            .map(|n| n.created_at)
        else {
            return;
        };
        store::delete_longform(ctx.ndb, author, secret, d, prev, &mut store::NoPublish);
        self.wake();
    }
}

impl Default for Notebook {
    fn default() -> Self {
        Notebook {
            canvas_id: store::CANVAS_ID.to_string(),
            cache: Rc::new(RefCell::new(NotebookCache::default())),
            vault_sync: VaultSync::default(),
            view: None,
            private_sync: PrivateRelaySync::new("notebook"),
            canvas: JsonCanvas::default(),
            scene_rect: Rect::from_min_max(Pos2::ZERO, Pos2::ZERO),
            loaded: false,
            live: HashMap::new(),
            anim_pos: HashMap::new(),
            rendered_heights: HashMap::new(),
            selected: None,
            pending_open: None,
            pan_target: None,
            connecting: None,
            edit: NodeEdit::Idle,
            confirm_delete: None,
            seeded: false,
            repaint_frames: 0,
            editor: None,
            vault: VaultState::default(),
            vault_rows: Vec::new(),
        }
    }
}

impl notedeck::App for Notebook {
    /// Contribute the notebook's inline renderers:
    /// - the **longform** (kind 30023) renderer, so a `nostr:naddr…` reference —
    ///   inline in a text node, or as a note-embed node — previews the note.
    ///   Kind-1 (`nevent`/`note`) references are already covered by the columns
    ///   app's `NoteKindRenderer`; longform is the notebook's own document type.
    /// - the **node** (kind 1606) renderer, so a `notebook:<word-id>` (or `nostr:`)
    ///   reference to a canvas node draws a live chip/card of its current content.
    fn kind_renderers(&self) -> Vec<Box<dyn notedeck::KindRenderer>> {
        // The node renderer shares the app's one canvas cache (cloned in, like
        // headway's issue renderer), so a node referenced by word id or `nostr:`
        // ref folds the same realtime canvas the foreground UI and the reference
        // parser read — a live edit shows on the chip, not just the open canvas.
        vec![
            Box::new(render::LongformKindRenderer),
            Box::new(render::NotebookNodeRenderer::new(self.cache.clone())),
        ]
    }

    /// Contribute the `notebook:<word-id>` reference parser so a node reference
    /// written inline in any note/comment/chat resolves to the node's kind-1606
    /// creation event (drawn by the node renderer). Shares the app's one
    /// [`NotebookCache`] (see [`Self::cache`]) — cloning the `Rc` in — so a node
    /// referenced by word id resolves off the same realtime-pumped canvas fold the
    /// foreground UI reads, and a realtime edit is reflected in the resolution.
    fn reference_parsers(&self) -> Vec<Box<dyn notedeck::ReferenceParser>> {
        vec![Box::new(reference::NotebookRefParser::new(
            self.cache.clone(),
        ))]
    }

    /// Background sync, run every frame for all *opened* apps (not just the
    /// foreground one) — which is what lets edits ingested by the `notebook` CLI
    /// while the user is on another tab still sync out. Polls the account's canvas
    /// subscription, fans freshly-ingested events out to its private relays, and
    /// auto-seeds a default canvas. Rendering happens separately in [`render`].
    fn update(&mut self, ctx: &mut AppContext<'_>, egui_ctx: &egui::Context) {
        let author = *ctx.accounts.selected_account_pubkey();
        // Copy the secret out so we don't hold a borrow on `accounts` while we
        // also touch `ndb`/`remote`. `None` for a pubkey-only (watch) account.
        let signer: Option<[u8; 32]> = ctx
            .accounts
            .selected_filled()
            .map(|f| f.secret_key.secret_bytes());

        // Declare the inbound cross-device subscription (catch-up + realtime)
        // against the account's private relays, and resolve the same set as our
        // outbound publish targets. Empty => local-only.
        let private_relays = self
            .private_sync
            .update(ctx, vec![event::notebook_filter(&author)]);

        // Advance the longform vault first (its own subscription + list): it may
        // resubscribe on an account switch, which needs `&mut Ndb`, so it must run
        // before we open the canvas read txn below. Runs every frame but only
        // re-lists when the subscription reports new longform.
        let vault_changed = self.vault_sync.poll(ctx.ndb, &author);

        // Pump the shared canvas cache under one read txn: advance this account's
        // reducer, folding in any freshly-arrived notes — our own async ingests,
        // `notebook` CLI moves into the embedded relay, remote sync. Inline widgets
        // read this same cache, so a chip drawn in another app stays as live as the
        // open canvas. On a change, re-project the active canvas (no separate
        // one-shot fold). Keep waking while edits stream in.
        let poll = Transaction::new(ctx.ndb)
            .ok()
            .map(|txn| {
                let mut cache = self.cache.borrow_mut();
                let poll = cache.poll(ctx.ndb, &txn, &author);
                if poll.changed {
                    self.view = cache.canvas(ctx.ndb, &txn, &author, &self.canvas_id);
                }
                poll
            })
            .unwrap_or_default();

        // On a fresh canvas fold, rebuild the renderable canvas and drop now-stale
        // drag overrides (the new fold carries the committed positions). Re-project
        // the vault rows off the render loop only when longform changed, formatting
        // each "edited …" subtitle once here rather than every frame in `vault_ui`.
        if poll.changed || vault_changed {
            if poll.changed {
                if let Some(view) = &self.view {
                    self.canvas = view_to_canvas(view);
                }
                self.live.clear();
            }
            if vault_changed {
                self.vault_rows = vault_rows(ctx.i18n, self.vault_sync.notes());
            }
            self.wake();
        }

        // Fan every freshly-ingested canvas event out to the private relays it
        // hasn't reached yet. This is the outbound half of cross-device sync: it
        // covers our own edits *and* events written straight into nostrdb by the
        // `notebook` CLI, which never pass through the app's edit path.
        if !poll.fresh.is_empty()
            && !private_relays.is_empty()
            && let Ok(txn) = Transaction::new(ctx.ndb)
        {
            let mut api = ctx.remote.publisher_explicit();
            fan_out_unseen_notes(&mut api, ctx.ndb, &txn, &poll.fresh, &private_relays);
        }

        // No canvas yet: auto-seed one for an account that can sign. The seeded
        // events fan out via the same poll path on a following frame. (The UI
        // feedback for this state is drawn in `render`.)
        if self.view.is_none()
            && let Some(secret) = &signer
            && !self.seeded
        {
            store::seed_canvas(
                ctx.ndb,
                &author,
                secret,
                &self.canvas_id,
                "Notebook",
                &mut store::NoPublish,
            );
            self.seeded = true;
            self.wake();
        }

        self.pump_repaint(egui_ctx);
    }

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let author = *ctx.accounts.selected_account_pubkey();
        // Copy the secret out so we don't hold a borrow on `accounts` while we
        // also touch `ndb`. `None` for a pubkey-only (watch) account.
        let signer: Option<[u8; 32]> = ctx
            .accounts
            .selected_filled()
            .map(|f| f.secret_key.secret_bytes());

        // Focus a node a click elsewhere asked us to open (see `open`). Runs after
        // `update`, so `self.canvas` already reflects this frame's fold.
        self.process_pending_open(ctx.ndb, ui.ctx());

        // Full-screen editor mode takes over the whole area; the canvas is hidden.
        // Background sync still runs in `update`, which also pumps the post-save
        // repaint burst.
        if self.editor.is_some() {
            self.editor_mode(ctx, ui, &author, &signer);
            return AppResponse::default();
        }

        let theme = notedeck::ColorTheme::current(ui.ctx());

        // Canvas mode. A top toolbar with the canvas name and a New-note button;
        // opening the editor takes over from the next frame (request a repaint so
        // there's no canvas flash).
        let mut open_editor = false;
        egui::TopBottomPanel::top("notebook-toolbar")
            .frame(egui::Frame::new().fill(theme.surface_primary).inner_margin(
                egui::Margin::symmetric(
                    notedeck::tokens::SPACING_MD as i8,
                    notedeck::tokens::SPACING_SM as i8,
                ),
            ))
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Notebook")
                            .strong()
                            .color(theme.text_primary),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("+ New note").clicked() {
                            open_editor = true;
                        }
                    });
                });
            });
        if open_editor {
            self.editor = Some(LongformEditor::new());
            ui.ctx().request_repaint();
            return AppResponse::default();
        }

        // Sync (subscription poll, private-relay fan-out, auto-seed) already ran
        // in `update` this frame; here we just render the cached canvas and vault.

        // Vault sidebar: the account's notes down the left, shown once there's at
        // least one — an empty vault stays out of the way and lets the canvas use
        // the full width. A row can be opened, renamed, or deleted; opening takes
        // over with the editor from the next frame.
        let mut vault_action = None;
        if !self.vault_rows.is_empty() {
            egui::SidePanel::left("notebook-vault")
                .resizable(true)
                .default_width(230.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme.surface_secondary)
                        .inner_margin(egui::Margin::symmetric(
                            notedeck::tokens::SPACING_XS as i8,
                            0,
                        )),
                )
                .show_inside(ui, |ui| {
                    vault_action = vault_ui(&self.vault_rows, &mut self.vault, ui);
                });
        }
        // Only Open takes over the view; Rename/Delete persist in place and let
        // the canvas keep rendering this frame.
        if let Some(action) = vault_action
            && self.apply_vault_action(ctx, &author, &signer, action)
        {
            ui.ctx().request_repaint();
            return AppResponse::default();
        }

        if self.view.is_none() {
            // No canvas yet. `update` auto-seeds one for a signing account; a
            // watch-only account can't create one.
            let msg = if signer.is_some() {
                "Setting up your canvas…"
            } else {
                "Sign in with a key to create your notebook canvas."
            };
            empty_state(ui, msg);
            return AppResponse::default();
        }

        // Ease each node toward its committed position for this frame (egui drives
        // the slide and its repaints) before drawing.
        self.update_anim_positions(ui.ctx());

        // Advance an in-flight reveal pan (a node opened from a reference glides
        // into view) before the scene reads `scene_rect` below.
        self.update_pan(ui.ctx());

        // Render against the cached canvas, collecting the edit the user made
        // this frame (at most one — like headway's board action).
        let intent = notebook_ui(self, ctx, ui);

        // Apply it by ingesting events into the local nostrdb. Mutations need a
        // signing key; a watch-only account simply can't edit. `update`'s poll
        // fans the ingested events out to the private relays next frame.
        if let (Some(intent), Some(secret)) = (intent, &signer)
            && let Some(action) = self.intent_to_action(intent)
        {
            let view = self.view.as_ref().expect("view present");
            store::apply(
                ctx.ndb,
                &self.canvas_id,
                view,
                &author,
                secret,
                action,
                &mut store::NoPublish,
            );
            self.wake();
        }

        AppResponse::default()
    }
}

/// A simple centered message for when there's no canvas to show yet.
fn empty_state(ui: &mut egui::Ui, message: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(message);
    });
}

/// notedeck_notebook's [`notedeck::Reducer`] adapter over the pure-data
/// [`CanvasReducer`], so a generic [`notedeck::RealtimeCache`] can drive the canvas
/// fold. A newtype rather than a direct `impl notedeck::Reducer for CanvasReducer`
/// so the reducer layer ([`event`]) stays free of any `notedeck` dependency and
/// egui-testable in isolation; the app layer supplies the framework glue. Its trait
/// methods forward straight to the existing free functions ([`event::fold_canvas`]
/// / [`event::reduce_delta`]) and [`CanvasReducer::finalize`].
///
/// The reducer holds *all* of the author's canvases ([`event::fold_canvas`] folds
/// an account's whole notebook history), so one cache entry backs the foreground
/// canvas and every inline reference to any of that author's canvases.
struct NotebookReducer(CanvasReducer);

impl notedeck::Reducer for NotebookReducer {
    type View = CanvasView;

    fn filter(author: &Pubkey) -> Filter {
        event::notebook_filter(author)
    }

    fn fold(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Option<Self> {
        event::fold_canvas(ndb, txn, author).map(NotebookReducer)
    }

    fn reduce_delta(&mut self, ndb: &Ndb, txn: &Transaction, keys: &[NoteKey]) -> Vec<NoteKey> {
        event::reduce_delta(&mut self.0, ndb, txn, keys)
    }

    fn finalize(&self) -> Vec<CanvasView> {
        self.0.finalize()
    }
}

/// The canvas-data engine for the Notebook app: a per-account
/// [`notedeck::RealtimeCache`] over the [`NotebookReducer`]. Shared (behind
/// `Rc<RefCell<…>>`) between the app and everything it registers for inline
/// display, so the foreground canvas and every inline chip read the *same*
/// reducer — a realtime edit (a CLI move, a remote sync) that [`update`]'s
/// per-frame [`poll`](Self::poll) folds in is immediately visible to a chip drawn
/// in another app, not just to the open canvas.
///
/// A thin wrapper: the seed-once fold, the post-snapshot deferral (the freshness
/// guarantee — see [`event::reduce_delta`]), the memoized finalize and the pump
/// all live in the generic cache; this only adds notebook-specific convenience
/// reads. The longform vault is deliberately *not* here (see [`VaultSync`]): it's a
/// standalone kind on its own subscription, and only the foreground reads it.
///
/// [`update`]: notedeck::App::update
#[derive(Default)]
struct NotebookCache {
    authors: notedeck::RealtimeCache<NotebookReducer>,
}

impl NotebookCache {
    /// Advance `author`'s reducer and report the change — the per-frame pump called
    /// from [`update`](notedeck::App::update). Fan out
    /// [`fresh`](notedeck::PollResponse::fresh) and wake on
    /// [`changed`](notedeck::PollResponse::changed). Thin delegate to the generic
    /// [`RealtimeCache`](notedeck::RealtimeCache), which owns the
    /// subscribe/seed/delta/pending discipline; a chip's lazy fold-on-render goes
    /// through [`canvas`](Self::canvas).
    fn poll(&mut self, ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> notedeck::PollResponse {
        self.authors.poll(ndb, txn, author)
    }

    /// Advance `author`'s reducer, (re)finalize it *at most once per fold*, and run
    /// `read` against the memoized [`CanvasView`]s. `None` only when the reducer
    /// hasn't seeded yet. Delegates to
    /// [`RealtimeCache::with_views`](notedeck::RealtimeCache::with_views): the
    /// foreground canvas and every inline widget resolve through it, so on a steady
    /// frame the first read finalizes and the rest reuse the memo. `read` extracts
    /// owned data from the borrowed slice so the cache borrow drops before drawing.
    fn with_canvases<R>(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        read: impl FnOnce(&[CanvasView]) -> R,
    ) -> Option<R> {
        self.authors.with_views(ndb, txn, author, read)
    }

    /// Fold and pick a single canvas (`canvas_id`) authored by `author`, seeding the
    /// reducer on first touch. `None` before the first fold or when no such canvas
    /// exists. The foreground canvas and every inline canvas widget resolve through
    /// this (via the memoized [`with_canvases`](Self::with_canvases)), rather than a
    /// separate one-shot fold.
    #[profiling::function]
    fn canvas(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        canvas_id: &str,
    ) -> Option<CanvasView> {
        self.with_canvases(ndb, txn, author, |canvases| {
            event::find_canvas(canvases, author, canvas_id).cloned()
        })
        .flatten()
    }
}

/// The account's longform (NIP-23) vault: a live subscription to its kind-30023
/// notes and the newest-edited-first list folded from them. Kept apart from
/// [`NotebookCache`] on purpose — longform notes stand alone (never folded into a
/// canvas, see [`event::KIND_LONGFORM`]) and are PNS-wrapped, so their keys must
/// never enter the canvas fan-out set. Only the foreground reads the vault, so it's
/// app-owned rather than shared out to the inline widgets.
///
/// The list is a full re-query ([`event::list_longform`], which resolves the
/// latest revision per `d`) rather than an accumulate-fold, so — unlike the canvas
/// cache — there's no incremental reducer to keep; the subscription is only a
/// change signal telling us when to re-list. Deliberately egui-free so it can be
/// unit-tested against a bare `Ndb`.
#[derive(Default)]
struct VaultSync {
    /// The account's browsable notes, newest-edited first.
    notes: Vec<event::LongformNote>,
    /// Live subscription to `author`'s longform notes; drained each poll only to
    /// learn whether the list changed (its keys never fan out — longform is
    /// PNS-wrapped, see the struct docs).
    sub: Option<Subscription>,
    /// The account the subscription belongs to, so we resubscribe and re-list on an
    /// account switch.
    author: Option<Pubkey>,
}

impl VaultSync {
    /// Ensure a live subscription to `author`'s longform notes and re-list when it
    /// reports a change, returning whether the list was (re)derived this call.
    /// Resubscribes (needs `&mut Ndb`) and re-lists once on first run or an account
    /// switch — a fresh subscription reports only *future* ingests, so we catch up
    /// on what's already there right away.
    fn poll(&mut self, ndb: &mut Ndb, author: &Pubkey) -> bool {
        // First run or account switch: (re)subscribe and seed the list once.
        if self.sub.is_none() || self.author.as_ref() != Some(author) {
            if let Some(old) = self.sub.take() {
                let _ = ndb.unsubscribe(old);
            }
            self.sub = ndb.subscribe(&[event::longform_filter(author)]).ok();
            self.author = Some(*author);
            if let Ok(txn) = Transaction::new(ndb) {
                self.notes = event::list_longform(ndb, &txn, author);
            }
            return true;
        }

        // Steady state: re-list only when the subscription reports new longform.
        let changed = self
            .sub
            .map(|s| !ndb.poll_for_notes(s, 64).is_empty())
            .unwrap_or(false);
        if changed && let Ok(txn) = Transaction::new(ndb) {
            self.notes = event::list_longform(ndb, &txn, author);
        }
        changed
    }

    /// The cached vault note list (newest-edited first).
    fn notes(&self) -> &[event::LongformNote] {
        &self.notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{self, CANVAS_ID, CanvasAction, NoPublish};
    use enostr::FullKeypair;
    use futures_util::StreamExt;
    use nostrdb::{Config, SubscriptionStream};

    /// Only a body that is *exactly* one `nostr:` reference (whitespace aside)
    /// promotes to a note-embed node; a reference mixed with other text, or plain
    /// prose, stays a text node.
    #[test]
    fn whole_nostr_reference_gates_on_the_lone_reference() {
        let reference = "nostr:nevent1qqs0000000000000000000000000000000000000000000000000000";
        assert_eq!(whole_nostr_reference(reference), Some(reference));
        // Surrounding whitespace is trimmed but the match must still span it all.
        assert_eq!(
            whole_nostr_reference(&format!("  {reference}\n")),
            Some(reference)
        );
        // A reference with trailing prose is a text node (renders inline instead).
        assert_eq!(
            whole_nostr_reference(&format!("{reference} see this")),
            None
        );
        assert_eq!(whole_nostr_reference("just some notes"), None);
        assert_eq!(whole_nostr_reference(""), None);
    }

    /// A headless harness driving a [`NotebookCache`] against a bare `Ndb` — the
    /// subscribe / poll / fold logic with no egui in sight. Mirrors headway's
    /// `TestSync`; each read opens its own short-lived txn, as the app does.
    struct TestCache {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        cache: NotebookCache,
        stream: SubscriptionStream,
    }

    impl TestCache {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            let kp = FullKeypair::generate();
            // A separate subscription we can await on to know when ingests commit
            // (the cache's own subscription is polled, not awaited).
            let sub = ndb
                .subscribe(&[event::notebook_filter(&kp.pubkey)])
                .unwrap();
            let stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
            Self {
                ndb,
                _dir: dir,
                kp,
                cache: NotebookCache::default(),
                stream,
            }
        }

        fn secret(&self) -> [u8; 32] {
            self.kp.secret_key.secret_bytes()
        }

        /// Await `n` committed notes on the side subscription.
        fn await_notes(&mut self, n: usize) {
            pollster::block_on(async {
                let mut seen = 0;
                while seen < n {
                    seen += self.stream.next().await.expect("subscription open").len();
                }
            });
        }

        /// Pump the cache under a fresh read txn, returning the poll response.
        fn poll(&mut self) -> notedeck::PollResponse {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache.poll(&self.ndb, &txn, &self.kp.pubkey)
        }

        /// Resolve the managed canvas under a fresh read txn.
        fn canvas(&mut self) -> Option<CanvasView> {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache
                .canvas(&self.ndb, &txn, &self.kp.pubkey, CANVAS_ID)
        }

        fn full_reloads(&self) -> u32 {
            self.cache.authors.stats().full_reloads
        }

        fn add_node(&mut self, view: &CanvasView, label: &str) {
            store::apply(
                &self.ndb,
                CANVAS_ID,
                view,
                &self.kp.pubkey,
                &self.secret(),
                CanvasAction::AddNode {
                    kind: event::NodeKind::Text,
                    geo: event::Geometry {
                        x: 0,
                        y: 0,
                        w: 200,
                        h: 80,
                    },
                    content: text(label),
                },
                &mut NoPublish,
            );
        }
    }

    fn text(s: &str) -> event::NodeContent {
        event::NodeContent {
            text: s.to_string(),
            ..Default::default()
        }
    }

    /// An ordinary edit folds in as a delta — the whole history is re-walked only
    /// on the first load, not on every change. The cache seeds once and thereafter
    /// only folds deltas (the generic [`notedeck::RealtimeCache`] invariant,
    /// observed here through the notebook adapter).
    #[test]
    fn cache_seeds_once_then_folds_deltas() {
        let mut t = TestCache::new();
        store::seed_canvas(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            CANVAS_ID,
            "Canvas",
            &mut NoPublish,
        );
        t.await_notes(1);

        // First poll seeds the reducer with a full fold; the canvas materialises.
        assert!(t.poll().changed);
        assert_eq!(t.full_reloads(), 1);
        let view = t.canvas().expect("canvas seeded");
        assert_eq!(view.title, "Canvas");

        // Add a node; its two events fold in as a delta, no extra full reload.
        t.add_node(&view, "hello");
        t.await_notes(2);

        let node = loop {
            t.poll();
            if let Some(v) = t.canvas()
                && !v.nodes.is_empty()
            {
                break v.nodes[0].content.text.clone();
            }
        };
        assert_eq!(node, "hello");
        assert_eq!(t.full_reloads(), 1, "delta fold, not a re-walk");
    }

    /// The freshness fix this cache exists to spread: a canvas note committed
    /// *after* the read txn a delta was polled with must be retained and retried,
    /// not silently dropped (the bug when `event::reduce_delta` returned `()`).
    /// Recovers by folding the deferred key as a delta, not by re-seeding.
    #[test]
    fn cache_retries_deltas_committed_after_read_txn() {
        let mut t = TestCache::new();
        store::seed_canvas(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            CANVAS_ID,
            "Canvas",
            &mut NoPublish,
        );
        t.await_notes(1);

        // Seed the cache (opens its own subscription) and grab the seeded canvas.
        t.poll();
        let view = t.canvas().expect("canvas seeded");

        // Open a stale snapshot, *then* commit a node's events after it: they enter
        // the cache's subscription inbox but are invisible to this older txn.
        let stale = Transaction::new(&t.ndb).unwrap();
        t.add_node(&view, "late");
        t.await_notes(2);

        let resp = t.cache.poll(&t.ndb, &stale, &t.kp.pubkey);
        drop(stale);
        assert!(!resp.changed, "a deferred-only advance reads as a no-op");
        assert!(
            t.cache.authors.pending_len(&t.kp.pubkey) >= 1,
            "the undrained keys are retained for retry, not dropped"
        );

        // A fresh snapshot resolves the retained keys as a delta.
        let canvas = t.canvas().expect("canvas present");
        assert_eq!(canvas.nodes.len(), 1, "the retained delta folded in");
        assert_eq!(canvas.nodes[0].content.text, "late");
        assert_eq!(t.cache.authors.pending_len(&t.kp.pubkey), 0);
        assert_eq!(
            t.full_reloads(),
            1,
            "recovered by folding a delta, not by re-seeding"
        );
    }
}
