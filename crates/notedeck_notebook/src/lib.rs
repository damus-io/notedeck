pub mod convert;
mod editor;
pub mod event;
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
use nostrdb::{Ndb, NoteKey, Subscription, Transaction};
use notedeck::{AppContext, AppResponse, PrivateRelaySync, fan_out_unseen_notes};
use std::collections::HashMap;

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
/// nostrdb. [`NotebookSync`] keeps a long-lived reducer over the account's events
/// and the [`CanvasView`] folded from them, folding only freshly-arrived notes
/// in as an ndb subscription reports them. Every edit is turned into a signed
/// event ingested locally (see [`store`]); there is deliberately no relay
/// publishing yet.
pub struct Notebook {
    /// Which canvas this instance manages (single canvas for now).
    canvas_id: String,
    /// Subscription-backed cache of the reduced canvas and the vault note list
    /// (egui-free), driven by one [`NotebookSync`].
    sync: NotebookSync,
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
    /// The vault rows to render, projected from [`NotebookSync::notes`] whenever
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

    /// The currently rendered canvas (folded view converted to `jsoncanvas`).
    /// Exposed for tests/introspection.
    pub fn canvas(&self) -> &JsonCanvas {
        &self.canvas
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
            UiIntent::Create { pos, text } => Some(CanvasAction::AddNode {
                kind: NodeKind::Text,
                geo: Geometry {
                    x: pos.x as i64,
                    y: pos.y as i64,
                    w: NEW_NODE_SIZE.x as u64,
                    h: NEW_NODE_HEIGHT as u64,
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
                    .sync
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
        // Copy out what the edit needs so the `sync` borrow ends before `wake`.
        let Some((content, prev)) = self
            .sync
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
            .sync
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
            sync: NotebookSync::default(),
            private_sync: PrivateRelaySync::new("notebook"),
            canvas: JsonCanvas::default(),
            scene_rect: Rect::from_min_max(Pos2::ZERO, Pos2::ZERO),
            loaded: false,
            live: HashMap::new(),
            anim_pos: HashMap::new(),
            rendered_heights: HashMap::new(),
            selected: None,
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
            .update(ctx, event::notebook_filter(&author));

        // Keep a live subscription and re-fold only when something changed (first
        // load, account switch, or an async ingest landing — including CLI
        // ingests into the embedded relay). On a fresh fold, rebuild the
        // renderable canvas and drop now-stale drag overrides (the new fold
        // carries the committed positions).
        let poll = self.sync.poll(ctx.ndb, &author, &self.canvas_id);
        if poll.changed {
            if let Some(view) = self.sync.view() {
                self.canvas = view_to_canvas(view);
            }
            // Re-project the vault rows off the render loop, formatting each
            // "edited …" subtitle once here rather than every frame in `vault_ui`.
            self.vault_rows = vault_rows(ctx.i18n, self.sync.notes());
            self.live.clear();
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
        if self.sync.view().is_none()
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

        if self.sync.view().is_none() {
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

        // Render against the cached canvas, collecting the edit the user made
        // this frame (at most one — like headway's board action).
        let intent = notebook_ui(self, ctx, ui);

        // Apply it by ingesting events into the local nostrdb. Mutations need a
        // signing key; a watch-only account simply can't edit. `update`'s poll
        // fans the ingested events out to the private relays next frame.
        if let (Some(intent), Some(secret)) = (intent, &signer)
            && let Some(action) = self.intent_to_action(intent)
        {
            let view = self.sync.view().expect("view present");
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

/// Subscription-backed, *online* view of one account's notebook state — both the
/// reduced canvas and the vault note list — behind a single nostrdb subscription.
///
/// Holds a live subscription to all the account's notebook notes (canvas events
/// **and** longform, [`event::notebook_filter`] + [`event::longform_filter`]) and
/// a long-lived [`CanvasReducer`] across frames. The first poll folds the whole
/// history once to seed the reducer; every later poll feeds it only the
/// freshly-arrived notes ([`event::reduce_delta`]) — an incremental step, not a
/// re-walk. The reducer is rebuilt from scratch only on a first load or an
/// account switch. The vault list is re-derived ([`event::list_longform`])
/// alongside, only when the subscription reports a change.
///
/// The canvas fold is commutative and idempotent, so applying a delta to an
/// up-to-date reducer matches a full re-fold. Deliberately free of any egui
/// dependency so it can be unit-tested against a bare `Ndb`.
#[derive(Default)]
struct NotebookSync {
    /// The last reduced canvas. `None` means "no such canvas" (or not loaded).
    view: Option<CanvasView>,
    /// The account's browsable notes, newest-edited first (the vault list).
    notes: Vec<event::LongformNote>,
    /// The accumulator, kept alive across polls so new notes fold in
    /// incrementally. `None` until the first full fold (and again after an
    /// account switch), which is the signal to re-fold from scratch.
    reducer: Option<CanvasReducer>,
    /// Live subscription to `sub_author`'s **canvas** notes. Its freshly-polled
    /// keys drive the incremental fold *and* are the fan-out set — all canvas
    /// kinds, safe to publish in the clear.
    sub: Option<Subscription>,
    /// Live subscription to `sub_author`'s **longform** notes, kept apart from
    /// [`Self::sub`] so its keys never enter the fan-out set (longform is
    /// PNS-wrapped). Its only job is to signal that the vault list may have
    /// changed, prompting a re-list.
    vault_sub: Option<Subscription>,
    /// The account the subscriptions/caches belong to, so we resubscribe and
    /// re-derive on an account switch.
    sub_author: Option<Pubkey>,
    /// Test-only count of full-history re-folds, to assert an ordinary change
    /// folds in as a delta rather than re-walking the whole log.
    #[cfg(test)]
    full_reloads: u32,
}

/// The result of a [`NotebookSync::poll`].
#[derive(Default)]
struct PollResponse {
    /// The cached canvas or vault list was (re)derived this call — a first load,
    /// an account switch, or new notes folding in — so the caller rebuilds the
    /// renderable canvas and schedules follow-up repaints.
    changed: bool,
    /// **Canvas** note keys folded in *incrementally* this call, for the caller
    /// to fan out to the account's private relays (see
    /// [`notedeck::fan_out_unseen_notes`]). Empty on a full reload (historical
    /// notes, not new ingests) and on a no-op. Deliberately excludes longform
    /// keys: those are PNS-wrapped and must never be published in the clear (see
    /// [`Notebook::save_editor`] / `headway:notebook/merry-patch-boost`).
    fresh: Vec<NoteKey>,
}

impl NotebookSync {
    /// Ensure a live subscription to `author`, drain it, and update both the
    /// cached canvas and the vault list. See [`PollResponse`] for the returned
    /// change flag and freshly-arrived (canvas-only) keys.
    fn poll(&mut self, ndb: &mut Ndb, author: &Pubkey, canvas_id: &str) -> PollResponse {
        self.sync_subscription(ndb, author);

        let Some(canvas_sub) = self.sub else {
            // Subscribe failed: degrade to a full reload each frame so edits show.
            self.reload(ndb, author, canvas_id);
            return PollResponse {
                changed: true,
                fresh: Vec::new(),
            };
        };

        // Drain both subscriptions each poll. The canvas keys are the fold delta
        // and the fan-out set; the longform sub only tells us whether the vault
        // list needs re-listing (its keys never fan out — see the field docs).
        let canvas_keys = ndb.poll_for_notes(canvas_sub, 64);
        let vault_changed = self
            .vault_sub
            .map(|s| !ndb.poll_for_notes(s, 64).is_empty())
            .unwrap_or(false);

        // First load (or just resubscribed): fold the whole history once to seed
        // the reducer and list the vault. The keys drained above are historical,
        // so they're deliberately dropped rather than fanned out.
        if self.reducer.is_none() {
            self.reload(ndb, author, canvas_id);
            return PollResponse {
                changed: true,
                fresh: Vec::new(),
            };
        }

        // Nothing new since the last poll: the caches stand, no re-derive.
        if canvas_keys.is_empty() && !vault_changed {
            return PollResponse::default();
        }

        // Incremental: fold the freshly-arrived canvas notes into the live reducer
        // (commutative/idempotent, so this matches a full re-fold without walking
        // the whole history), and re-list the vault only when longform changed.
        if let Ok(txn) = Transaction::new(ndb) {
            if !canvas_keys.is_empty() {
                let reducer = self.reducer.as_mut().expect("reducer present");
                event::reduce_delta(reducer, ndb, &txn, &canvas_keys);
                self.view = event::pick_canvas(reducer, author, canvas_id);
            }
            if vault_changed {
                self.notes = event::list_longform(ndb, &txn, author);
            }
        }
        PollResponse {
            changed: true,
            fresh: canvas_keys,
        }
    }

    /// The cached canvas, if one has been folded.
    fn view(&self) -> Option<&CanvasView> {
        self.view.as_ref()
    }

    /// The cached vault note list (newest-edited first).
    fn notes(&self) -> &[event::LongformNote] {
        &self.notes
    }

    /// Re-derive everything from the whole event history into a fresh reducer
    /// (seeding or after an account switch): the canvas view and the vault list.
    fn reload(&mut self, ndb: &Ndb, author: &Pubkey, canvas_id: &str) {
        let Ok(txn) = Transaction::new(ndb) else {
            return;
        };
        let reducer = event::fold_canvas(ndb, &txn, author);
        self.view = reducer
            .as_ref()
            .and_then(|r| event::pick_canvas(r, author, canvas_id));
        self.reducer = reducer;
        self.notes = event::list_longform(ndb, &txn, author);
        #[cfg(test)]
        {
            self.full_reloads += 1;
        }
    }

    /// Ensure live subscriptions to `author`'s canvas and longform notes,
    /// resubscribing (and dropping the caches) on an account switch. A fresh
    /// subscription only reports *future* ingests, so the next poll does a
    /// one-off full re-derive to pick up what's already there.
    fn sync_subscription(&mut self, ndb: &mut Ndb, author: &Pubkey) {
        if self.sub.is_some() && self.sub_author.as_ref() == Some(author) {
            return;
        }
        for old in [self.sub.take(), self.vault_sub.take()]
            .into_iter()
            .flatten()
        {
            let _ = ndb.unsubscribe(old);
        }
        self.sub = ndb.subscribe(&[event::notebook_filter(author)]).ok();
        self.vault_sub = ndb.subscribe(&[event::longform_filter(author)]).ok();
        self.sub_author = Some(*author);
        // New account (or first run): drop the caches so the next poll re-derives.
        self.view = None;
        self.reducer = None;
        self.notes.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{self, CANVAS_ID, CanvasAction, NoPublish};
    use enostr::FullKeypair;
    use futures_util::StreamExt;
    use nostrdb::{Config, SubscriptionStream};

    /// A headless harness driving a [`NotebookSync`] against a bare `Ndb` — the
    /// subscription / poll / refold logic with no egui in sight. Mirrors
    /// headway's `TestSync`.
    struct TestSync {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        sync: NotebookSync,
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
                sync: NotebookSync::default(),
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
            self.sync
                .poll(&mut self.ndb, &self.kp.pubkey, CANVAS_ID)
                .changed
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
