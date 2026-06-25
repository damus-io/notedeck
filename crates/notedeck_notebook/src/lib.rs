pub mod convert;
pub mod event;
pub mod store;
mod ui;
pub mod wordid;

use crate::convert::view_to_canvas;
use crate::event::{CanvasReducer, CanvasView};
use crate::store::CanvasAction;
use crate::ui::{node_rect, notebook_ui, side_str};
use egui::{Pos2, Rect};
use enostr::{NoteId, Pubkey, RelayId};
use jsoncanvas::{JsonCanvas, NodeId, edge::Side};
use nostrdb::{Ndb, Subscription, Transaction};
use notedeck::{AppContext, AppResponse, ExplicitPublishApi};
use std::collections::HashMap;

/// A [`store::Publisher`] that fans every locally-ingested canvas event out to
/// the account's "private" relays (NIP-65 4th-entry marker) so the canvas syncs
/// across the user's own devices. With no private relay marked the relay set is
/// empty and this behaves exactly like [`store::NoPublish`] (local-only).
///
/// We publish plaintext canvas events, so they can only safely reach a private
/// (AUTH/wireguard) relay. TODO: PNS-encrypt these events (as dave does for its
/// session state via `wrap_pns`) and then we could also fan them out to the
/// user's *public* write relays without leaking canvas contents.
struct PrivateRelayPublisher<'o, 'a> {
    api: ExplicitPublishApi<'o, 'a>,
    relays: Vec<RelayId>,
}

impl store::Publisher for PrivateRelayPublisher<'_, '_> {
    fn publish(&mut self, event_frame: &str) {
        if self.relays.is_empty() {
            return;
        }
        // `ingest` hands us a ["EVENT", {…}] frame; `publish_event_json` wants
        // the bare event object, which the outbox re-frames per relay.
        match serde_json::from_str::<serde_json::Value>(event_frame)
            .ok()
            .and_then(|frame| frame.get(1).cloned())
        {
            Some(event) => self
                .api
                .publish_event_json(event.to_string(), self.relays.clone()),
            None => {} // malformed frame; local ingest already happened
        }
    }
}

/// An Obsidian-style infinite canvas, backed by nostr events in the local
/// nostrdb. [`CanvasSync`] keeps a long-lived reducer over the account's events
/// and the [`CanvasView`] folded from them, folding only freshly-arrived notes
/// in as an ndb subscription reports them. Every edit is turned into a signed
/// event ingested locally (see [`store`]); there is deliberately no relay
/// publishing yet.
pub struct Notebook {
    /// Which canvas this instance manages (single canvas for now).
    canvas_id: String,
    /// Subscription-backed cache of the reduced canvas (egui-free).
    sync: CanvasSync,
    /// The folded canvas converted to `jsoncanvas` for rendering. Rebuilt
    /// whenever the sync reports a change.
    canvas: JsonCanvas,
    scene_rect: Rect,
    loaded: bool,
    /// Per-node position overrides applied during a live drag, in canvas coords.
    /// Cleared when a fresh fold lands (which then carries the committed move).
    positions: HashMap<NodeId, Pos2>,
    /// This frame's eased top-left per node, in canvas coords — egui interpolates
    /// toward each node's committed position, so a node that jumped (a drag
    /// release, or a `notebook move` over the relay) slides instead of teleporting.
    /// Read by [`Notebook::node_rect`] so nodes, edges and handles follow together.
    /// Rebuilt every frame from egui's animation manager (the real state lives
    /// there, keyed by [`move_anim_ids`]).
    anim_pos: HashMap<NodeId, Pos2>,
    /// Per-node actual rendered height, measured last frame. A node's content
    /// (markdown, embedded note widgets) can overflow its declared height, so the
    /// visible box is taller than the canvas geometry. Edges and connection
    /// handles anchor to this measured height instead, so they land on the real
    /// box edge rather than floating inside it.
    rendered_heights: HashMap<NodeId, f32>,
    /// Currently selected node, if any.
    selected: Option<NodeId>,
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
    /// An existing text node's text was edited.
    EditText { node: NodeId, text: String },
    /// A new text node was composed at `pos`.
    Create { pos: Pos2, text: String },
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

/// Default size of a freshly-created text node, in canvas pixels.
pub(crate) const NEW_NODE_SIZE: egui::Vec2 = egui::vec2(250.0, 120.0);

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

impl Notebook {
    pub fn new() -> Self {
        Notebook::default()
    }

    /// The node's current rect, accounting for any live-drag override and the
    /// actual rendered height measured last frame (content can overflow the
    /// declared height, so the visible box — what edges should anchor to — is
    /// taller than the canvas geometry).
    pub(crate) fn node_rect(&self, id: &NodeId, node: &jsoncanvas::Node) -> Rect {
        let default = node_rect(node.node());
        // Precedence: a live drag (the user's hand) wins; else a move animation
        // in flight; else the committed geometry.
        let min = self
            .positions
            .get(id)
            .or_else(|| self.anim_pos.get(id))
            .copied()
            .unwrap_or(default.min);
        let height = self
            .rendered_heights
            .get(id)
            .copied()
            .unwrap_or(default.height());
        Rect::from_min_size(min, egui::vec2(default.width(), height))
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

    /// The currently rendered canvas (folded view converted to `jsoncanvas`).
    /// Exposed for tests/introspection.
    pub fn canvas(&self) -> &JsonCanvas {
        &self.canvas
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
            UiIntent::Create { pos, text } => Some(CanvasAction::AddNode {
                kind: NodeKind::Text,
                geo: Geometry {
                    x: pos.x as i64,
                    y: pos.y as i64,
                    w: NEW_NODE_SIZE.x as u64,
                    h: NEW_NODE_SIZE.y as u64,
                },
                content: NodeContent {
                    text,
                    ..Default::default()
                },
            }),
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
            if let Some(dragged) = self.positions.get(&id).copied() {
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
    fn pump_repaint(&mut self, ui: &egui::Ui) {
        if self.repaint_frames > 0 {
            self.repaint_frames -= 1;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
}

impl Default for Notebook {
    fn default() -> Self {
        Notebook {
            canvas_id: store::CANVAS_ID.to_string(),
            sync: CanvasSync::default(),
            canvas: JsonCanvas::default(),
            scene_rect: Rect::from_min_max(Pos2::ZERO, Pos2::ZERO),
            loaded: false,
            positions: HashMap::new(),
            anim_pos: HashMap::new(),
            rendered_heights: HashMap::new(),
            selected: None,
            connecting: None,
            edit: NodeEdit::Idle,
            confirm_delete: None,
            seeded: false,
            repaint_frames: 0,
        }
    }
}

impl notedeck::App for Notebook {
    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let author = *ctx.accounts.selected_account_pubkey();
        // Copy the secret out so we don't hold a borrow on `accounts` while we
        // also touch `ndb`. `None` for a pubkey-only (watch) account.
        let signer: Option<[u8; 32]> = ctx
            .accounts
            .selected_filled()
            .map(|f| f.secret_key.secret_bytes());

        // Keep a live subscription and re-fold only when something changed. On a
        // fresh fold, rebuild the renderable canvas and drop now-stale drag
        // overrides (the new fold carries the committed positions).
        if self.sync.poll(ctx.ndb, &author, &self.canvas_id) {
            if let Some(view) = self.sync.view() {
                self.canvas = view_to_canvas(view);
            }
            self.positions.clear();
            self.wake();
        }

        if self.sync.view().is_none() {
            // No canvas yet: auto-seed one for an account that can sign.
            match &signer {
                Some(secret) => {
                    if !self.seeded {
                        let relays = ctx.accounts.selected_account_private_relays();
                        let mut publisher = PrivateRelayPublisher {
                            api: ctx.remote.publisher_explicit(),
                            relays,
                        };
                        store::seed_canvas(
                            ctx.ndb,
                            &author,
                            secret,
                            &self.canvas_id,
                            "Notebook",
                            &mut publisher,
                        );
                        self.seeded = true;
                        self.wake();
                    }
                    empty_state(ui, "Setting up your canvas…");
                }
                None => empty_state(ui, "Sign in with a key to create your notebook canvas."),
            }
            self.pump_repaint(ui);
            return AppResponse::default();
        }

        // Ease each node toward its committed position for this frame (egui drives
        // the slide and its repaints) before drawing.
        self.update_anim_positions(ui.ctx());

        // Render against the cached canvas, collecting the edit the user made
        // this frame (at most one — like headway's board action).
        let intent = notebook_ui(self, ctx, ui);

        // Apply it by ingesting events into the local nostrdb. Mutations need a
        // signing key; a watch-only account simply can't edit.
        if let (Some(intent), Some(secret)) = (intent, &signer)
            && let Some(action) = self.intent_to_action(intent)
        {
            let view = self.sync.view().expect("view present");
            let relays = ctx.accounts.selected_account_private_relays();
            let mut publisher = PrivateRelayPublisher {
                api: ctx.remote.publisher_explicit(),
                relays,
            };
            store::apply(
                ctx.ndb,
                &self.canvas_id,
                view,
                &author,
                secret,
                action,
                &mut publisher,
            );
            self.wake();
        }

        self.pump_repaint(ui);
        AppResponse::default()
    }
}

/// A simple centered message for when there's no canvas to show yet.
fn empty_state(ui: &mut egui::Ui, message: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(message);
    });
}

/// Subscription-backed, *online* reducer for one account's canvas.
///
/// Holds a live nostrdb subscription to the account's notebook events **and a
/// long-lived [`CanvasReducer`]** that persists across frames. The first poll
/// folds the whole history once to seed the reducer; every later poll feeds it
/// only the freshly-arrived notes ([`event::reduce_delta`]) — an incremental
/// step, not a re-walk of the history. The reducer is rebuilt from scratch only
/// on a first load or an account switch.
///
/// Correct because the fold is commutative and idempotent: applying a delta to
/// an up-to-date reducer lands in the same state as a full re-fold. Deliberately
/// free of any egui dependency so it can be unit-tested against a bare `Ndb`.
#[derive(Default)]
struct CanvasSync {
    /// The last reduced canvas. `None` means "no such canvas" (or not loaded).
    view: Option<CanvasView>,
    /// The accumulator, kept alive across polls so new notes fold in
    /// incrementally. `None` until the first full fold (and again after an
    /// account switch), which is the signal to re-fold from scratch.
    reducer: Option<CanvasReducer>,
    /// Live subscription to `sub_author`'s notebook events; polling it is how we
    /// learn the canvas changed (including our own async ingests landing).
    sub: Option<Subscription>,
    /// The account `sub`/`reducer`/`view` belong to, so we resubscribe and
    /// re-fold on an account switch.
    sub_author: Option<Pubkey>,
    /// Test-only count of full-history re-folds, to assert an ordinary change
    /// folds in as a delta rather than re-walking the whole log.
    #[cfg(test)]
    full_reloads: u32,
}

impl CanvasSync {
    /// Ensure a live subscription to `author`, drain it, and update the cached
    /// canvas. Returns `true` if the canvas was (re)reduced this call — a first
    /// load, an account switch, or new notes folded in.
    fn poll(&mut self, ndb: &mut Ndb, author: &Pubkey, canvas_id: &str) -> bool {
        self.sync_subscription(ndb, author);

        let Some(sub) = self.sub else {
            // Subscribe failed: degrade to a full reload each frame so edits show.
            self.reload(ndb, author, canvas_id);
            return true;
        };

        let keys = ndb.poll_for_notes(sub, 64);

        // First load (or just resubscribed): fold the whole history once to seed
        // the long-lived reducer.
        if self.reducer.is_none() {
            self.reload(ndb, author, canvas_id);
            return true;
        }

        // Nothing new since the last poll: the cached view stands, no re-fold.
        if keys.is_empty() {
            return false;
        }

        // Incremental: fold only the freshly-arrived notes into the live reducer
        // and re-finalize. Commutative/idempotent, so this matches a full
        // re-fold without walking the whole history.
        if let Ok(txn) = Transaction::new(ndb) {
            let reducer = self.reducer.as_mut().expect("reducer present");
            event::reduce_delta(reducer, ndb, &txn, &keys);
            self.view = event::pick_canvas(reducer, author, canvas_id);
        }
        true
    }

    /// The cached canvas, if one has been folded.
    fn view(&self) -> Option<&CanvasView> {
        self.view.as_ref()
    }

    /// Re-fold the whole event history into a fresh reducer (seeding or after an
    /// account switch) and pick out our canvas.
    fn reload(&mut self, ndb: &Ndb, author: &Pubkey, canvas_id: &str) {
        let reducer = Transaction::new(ndb)
            .ok()
            .and_then(|txn| event::fold_canvas(ndb, &txn, author));
        self.view = reducer
            .as_ref()
            .and_then(|r| event::pick_canvas(r, author, canvas_id));
        self.reducer = reducer;
        #[cfg(test)]
        {
            self.full_reloads += 1;
        }
    }

    /// Ensure we hold a live subscription to `author`'s notebook events,
    /// resubscribing (and dropping the cached reducer) on an account switch. A
    /// fresh subscription only reports *future* ingests, so the next poll does a
    /// one-off full fold to pick up what's already there.
    fn sync_subscription(&mut self, ndb: &mut Ndb, author: &Pubkey) {
        if self.sub.is_some() && self.sub_author.as_ref() == Some(author) {
            return;
        }
        if let Some(old) = self.sub.take() {
            let _ = ndb.unsubscribe(old);
        }
        self.sub = ndb.subscribe(&[event::notebook_filter(author)]).ok();
        self.sub_author = Some(*author);
        // New account (or first run): drop the cache so the next poll re-folds.
        self.view = None;
        self.reducer = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{self, CANVAS_ID, CanvasAction, NoPublish};
    use enostr::FullKeypair;
    use futures_util::StreamExt;
    use nostrdb::{Config, SubscriptionStream};

    /// A headless harness driving a [`CanvasSync`] against a bare `Ndb` — the
    /// subscription / poll / refold logic with no egui in sight. Mirrors
    /// headway's `TestSync`.
    struct TestSync {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        sync: CanvasSync,
        stream: SubscriptionStream,
    }

    impl TestSync {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            let kp = FullKeypair::generate();
            // A separate subscription we can await on to know when ingests commit
            // (the sync's own subscription is polled, not awaited).
            let sub = ndb
                .subscribe(&[event::notebook_filter(&kp.pubkey)])
                .unwrap();
            let stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
            Self {
                ndb,
                _dir: dir,
                kp,
                sync: CanvasSync::default(),
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

        fn poll(&mut self) -> bool {
            self.sync.poll(&mut self.ndb, &self.kp.pubkey, CANVAS_ID)
        }

        fn apply(&mut self, action: CanvasAction) {
            let view = self.sync.view().expect("view present").clone();
            store::apply(
                &self.ndb,
                CANVAS_ID,
                &view,
                &self.kp.pubkey,
                &self.secret(),
                action,
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
    /// on the first load, not on every change.
    #[test]
    fn sync_folds_incrementally() {
        let mut t = TestSync::new();
        store::seed_canvas(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            CANVAS_ID,
            "Canvas",
            &mut NoPublish,
        );
        t.await_notes(1);

        // First poll seeds the reducer with a full fold.
        assert!(t.poll());
        assert_eq!(t.sync.full_reloads, 1);
        assert!(t.sync.view().is_some());
        assert_eq!(t.sync.view().unwrap().title, "Canvas");

        // Add a node; its two events fold in as a delta, no extra full reload.
        t.apply(CanvasAction::AddNode {
            kind: event::NodeKind::Text,
            geo: event::Geometry {
                x: 0,
                y: 0,
                w: 200,
                h: 80,
            },
            content: text("hello"),
        });
        t.await_notes(2);
        assert!(t.poll());
        assert_eq!(t.sync.full_reloads, 1, "delta fold, not a re-walk");
        let view = t.sync.view().unwrap();
        assert_eq!(view.nodes.len(), 1);
        assert_eq!(view.nodes[0].content.text, "hello");

        // A poll with nothing new doesn't reduce.
        assert!(!t.poll());
    }
}
