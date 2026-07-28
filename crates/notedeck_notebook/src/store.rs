//! Persistence for notebook canvases.
//!
//! This is the app-layer bridge between the pure schema in [`crate::event`] and
//! nostrdb, mirroring [`headway::store`](../../headway/src/store.rs). It seeds a
//! canvas and translates UI intents ([`CanvasAction`]) into signed nostr events
//! ingested into a local nostrdb. Every ingested event is also handed to a
//! [`Publisher`], the single seam for fanning changes outward to a relay: the
//! egui app ingests straight into the nostrdb its embedded relay serves and so
//! uses [`NoPublish`], while a CLI would keep its own nostrdb and publish each
//! event to the running app's relay.
//!
//! ## Superseding addressable events
//!
//! Transforms, content overlays, edges and the canvas document are all
//! addressable (latest-wins, keyed per author). nostr `created_at` is whole
//! seconds, so two edits to the same element in the same second would *tie* and
//! the reducer would keep the older one — silently dropping the edit. Canvas
//! edits (drag-release, then drag again) fire fast enough that this is real, so
//! every re-placement is stamped strictly past the version it edits (the
//! `*_at` timestamps the reducer surfaces on the view), mirroring headway's
//! `next_after`.

use enostr::{NoteId, Pubkey};
use nostrdb::{IngestMetadata, Ndb, NoteBuilder};

use crate::event::{
    self, CanvasView, EdgeEnds, Geometry, NodeContent, NodeKind, NodeView, build_canvas,
    build_content, build_edge, build_edge_tombstone, build_node, build_node_tombstone,
    build_transform, canvas_address, rank_between,
};

/// The single canvas the notebook manages for now. Multi-canvas support will
/// turn this into a per-canvas identifier carried on [`crate::Notebook`].
pub const CANVAS_ID: &str = "notebook";

/// A UI intent to mutate the canvas. Collected during rendering and applied
/// afterwards by [`apply`], which turns each variant into one or more ingested
/// events.
pub enum CanvasAction {
    /// Create a new node, placed on top of the stack.
    AddNode {
        kind: NodeKind,
        geo: Geometry,
        content: NodeContent,
    },
    /// Move and/or resize a node — a full geometry snapshot, preserving the
    /// node's current z-rank and color. Covers both drag (move) and resize.
    SetGeometry { node: NoteId, geo: Geometry },
    /// Replace a node's editable content (text/url/file/…).
    EditContent { node: NoteId, content: NodeContent },
    /// Recolor a node (`None` clears the color), preserving geometry and z-rank.
    Recolor { node: NoteId, color: Option<String> },
    /// Restack a node so it lands at display index `to_index` (back-to-front).
    Restack { node: NoteId, to_index: usize },
    /// Remove a node from the canvas (reversible tombstone).
    DeleteNode { node: NoteId },
    /// Create or replace an edge (addressable, latest-wins — same path for a new
    /// edge and an edit to an existing one).
    SetEdge {
        edge_id: String,
        from: NoteId,
        to: NoteId,
        ends: EdgeEnds,
    },
    /// Remove an edge from the canvas (reversible tombstone).
    DeleteEdge {
        edge_id: String,
        from: NoteId,
        to: NoteId,
    },
    /// Rename the canvas.
    Rename { title: String },
    /// Replace the membership list.
    SetMembers { members: Vec<Pubkey> },
    /// Set the visibility mode (open surfaces everyone's contributions).
    SetOpen { open: bool },
}

/// A sink for events that have been ingested locally and should also be fanned
/// out — typically published to a relay. [`ingest`] hands every event it stores
/// to the publisher as a ready-to-send NIP-01 `["EVENT", {...}]` frame, in the
/// order they were ingested.
pub trait Publisher {
    /// Called once per successfully ingested event with its `["EVENT", {...}]`
    /// JSON frame, ready to write to a relay websocket.
    fn publish(&mut self, event_frame: &str);
}

/// A [`Publisher`] that drops everything: local ingest only, no fan-out. Used by
/// the egui app, whose embedded relay already serves the same nostrdb it ingests
/// into, so there is nothing to publish.
pub struct NoPublish;

impl Publisher for NoPublish {
    fn publish(&mut self, _event_frame: &str) {}
}

/// Sign `builder` with `secret` and ingest the resulting note into the local
/// nostrdb, then hand its `["EVENT", {...}]` frame to `publisher`. Returns the
/// note id, or `None` if building/ingesting failed (in which case nothing is
/// published).
pub fn ingest(
    ndb: &Ndb,
    builder: NoteBuilder,
    secret: &[u8; 32],
    publisher: &mut dyn Publisher,
) -> Option<NoteId> {
    let note = builder.sign(secret).build()?;
    let id = NoteId::new(*note.id());
    let json = enostr::ClientMessage::event(&note).ok()?.to_json().ok()?;
    ndb.process_event_with(&json, IngestMetadata::new().client(true))
        .ok()?;
    publisher.publish(&json);
    Some(id)
}

/// Seed a fresh, closed canvas document for `author` (no nodes). The canvas is
/// the anchor every node/edge points at; nodes are added later via
/// [`CanvasAction::AddNode`].
pub fn seed_canvas(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    canvas_id: &str,
    title: &str,
    publisher: &mut dyn Publisher,
) {
    let _ = author;
    ingest(
        ndb,
        build_canvas(canvas_id, title, &[], false),
        secret,
        publisher,
    );
}

/// Apply one [`CanvasAction`] against the current `view`, ingesting the events it
/// implies. `view` is the pre-action snapshot, used to read a node's current
/// z-rank/color (a transform is a full snapshot) and to stamp superseding
/// timestamps.
pub fn apply(
    ndb: &Ndb,
    canvas_id: &str,
    view: &CanvasView,
    author: &Pubkey,
    secret: &[u8; 32],
    action: CanvasAction,
    publisher: &mut dyn Publisher,
) {
    let addr = canvas_address(author, canvas_id);

    match action {
        CanvasAction::AddNode { kind, geo, content } => {
            let Some(node) = ingest(
                ndb,
                build_node(&addr, kind, &geo, &content),
                secret,
                publisher,
            ) else {
                return;
            };
            // Stack it on top: a rank above every current node's.
            let top = view.nodes.last().map(|n| n.z.as_str());
            let z = rank_between(top, None);
            ingest(
                ndb,
                build_transform(canvas_id, &addr, &node, &geo, &z, None),
                secret,
                publisher,
            );
        }
        CanvasAction::SetGeometry { node, geo } => {
            let (z, color, after) = transform_basis(view, node);
            ingest(
                ndb,
                build_transform(canvas_id, &addr, &node, &geo, &z, color.as_deref())
                    .created_at(next_after(after)),
                secret,
                publisher,
            );
        }
        CanvasAction::EditContent { node, content } => {
            let after = find_node(view, node).map_or(0, |n| n.edited_at);
            ingest(
                ndb,
                build_content(canvas_id, &addr, &node, &content).created_at(next_after(after)),
                secret,
                publisher,
            );
        }
        CanvasAction::Recolor { node, color } => {
            let (z, _old, after) = transform_basis(view, node);
            let geo = find_node(view, node).map(|n| n.geo).unwrap_or_default();
            ingest(
                ndb,
                build_transform(canvas_id, &addr, &node, &geo, &z, color.as_deref())
                    .created_at(next_after(after)),
                secret,
                publisher,
            );
        }
        CanvasAction::Restack { node, to_index } => {
            let (_z, color, after) = transform_basis(view, node);
            let geo = find_node(view, node).map(|n| n.geo).unwrap_or_default();
            let z = rank_for_restack(&view.nodes, node, to_index);
            ingest(
                ndb,
                build_transform(canvas_id, &addr, &node, &geo, &z, color.as_deref())
                    .created_at(next_after(after)),
                secret,
                publisher,
            );
        }
        CanvasAction::DeleteNode { node } => {
            let (z, _color, after) = transform_basis(view, node);
            ingest(
                ndb,
                build_node_tombstone(canvas_id, &addr, &node, &z).created_at(next_after(after)),
                secret,
                publisher,
            );
        }
        CanvasAction::SetEdge {
            edge_id,
            from,
            to,
            ends,
        } => {
            let after = find_edge(view, &edge_id).map_or(0, |e| e.placed_at);
            ingest(
                ndb,
                build_edge(canvas_id, &addr, &edge_id, &from, &to, &ends)
                    .created_at(next_after(after)),
                secret,
                publisher,
            );
        }
        CanvasAction::DeleteEdge { edge_id, from, to } => {
            let after = find_edge(view, &edge_id).map_or(0, |e| e.placed_at);
            ingest(
                ndb,
                build_edge_tombstone(canvas_id, &addr, &edge_id, &from, &to)
                    .created_at(next_after(after)),
                secret,
                publisher,
            );
        }
        CanvasAction::Rename { title } => republish_canvas(
            ndb,
            canvas_id,
            view,
            secret,
            &title,
            &members_of(view),
            view.open,
            publisher,
        ),
        CanvasAction::SetMembers { members } => republish_canvas(
            ndb,
            canvas_id,
            view,
            secret,
            &view.title,
            &members,
            view.open,
            publisher,
        ),
        CanvasAction::SetOpen { open } => republish_canvas(
            ndb,
            canvas_id,
            view,
            secret,
            &view.title,
            &members_of(view),
            open,
            publisher,
        ),
    }
}

/// The basis for a re-published transform of `node`: its current z-rank (a
/// non-empty fallback so the node stays explicitly stacked), current color, and
/// the timestamp to supersede. Defaults are used if the node isn't in the view.
fn transform_basis(view: &CanvasView, node: NoteId) -> (String, Option<String>, u64) {
    match find_node(view, node) {
        Some(n) => (non_empty_rank(&n.z), n.color.clone(), n.placed_at),
        None => (non_empty_rank(""), None, 0),
    }
}

/// Republish the canvas document with new title/members/mode, preserving the
/// rest. Addressable, so a republish supersedes the prior one by `created_at`;
/// stamp strictly past the version we're editing so the edit always wins (same
/// reasoning as [`next_after`]).
#[allow(clippy::too_many_arguments)]
fn republish_canvas(
    ndb: &Ndb,
    canvas_id: &str,
    view: &CanvasView,
    secret: &[u8; 32],
    title: &str,
    members: &[Pubkey],
    open: bool,
    publisher: &mut dyn Publisher,
) {
    let created_at = now_secs().max(view.created_at + 1);
    ingest(
        ndb,
        build_canvas(canvas_id, title, members, open).created_at(created_at),
        secret,
        publisher,
    );
}

/// The `created_at` to stamp on a re-placement that must supersede a prior
/// addressable event made at `prev`. Whole-second nostr timestamps mean an edit
/// in the same second would tie the reducer's latest-wins and silently no-op;
/// stamp strictly past `prev` so the new event always wins.
fn next_after(prev: u64) -> u64 {
    now_secs().max(prev + 1)
}

/// Current wall-clock time in whole seconds since the Unix epoch (nostr's
/// `created_at` unit). Falls back to 0 if the clock is before the epoch.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The view's current members as [`Pubkey`]s, for republishing the canvas
/// document (the view stores raw bytes).
fn members_of(view: &CanvasView) -> Vec<Pubkey> {
    view.members.iter().map(|m| Pubkey::new(*m)).collect()
}

/// Find a node anywhere on the canvas (visible or pending) by id.
fn find_node(view: &CanvasView, id: NoteId) -> Option<&NodeView> {
    view.nodes
        .iter()
        .chain(view.pending.iter())
        .find(|n| n.id == id)
}

/// Find an edge by id.
fn find_edge<'a>(view: &'a CanvasView, edge_id: &str) -> Option<&'a event::EdgeView> {
    view.edges.iter().find(|e| e.id == edge_id)
}

/// A transform needs a z-rank; fall back to a midpoint when the node has none
/// (it was never explicitly stacked) so a move doesn't strip its ordering.
fn non_empty_rank(rank: &str) -> String {
    if rank.is_empty() {
        "m".to_string()
    } else {
        rank.to_string()
    }
}

/// Compute a fractional z-rank that lands `node` at display index `to_index`
/// among `nodes` (sorted back-to-front by rank). Excludes the moving node from
/// the neighbour search so an in-place restack doesn't fence itself. Mirrors
/// headway's `rank_for_insert`.
fn rank_for_restack(nodes: &[NodeView], node: NoteId, to_index: usize) -> String {
    let others: Vec<&NodeView> = nodes.iter().filter(|n| n.id != node).collect();

    let pos = match nodes.iter().position(|n| n.id == node) {
        Some(cur) if cur < to_index => to_index - 1,
        _ => to_index,
    };
    let pos = pos.min(others.len());

    let left = pos
        .checked_sub(1)
        .and_then(|i| others.get(i))
        .map(|n| n.z.as_str())
        .filter(|s| !s.is_empty());
    let right = others
        .get(pos)
        .map(|n| n.z.as_str())
        .filter(|s| !s.is_empty());
    rank_between(left, right)
}

/// Convenience re-export so the app layer can load a canvas without naming the
/// event module directly.
pub use event::load_canvas;

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;
    use futures_util::StreamExt;
    use nostrdb::{Config, Ndb, SubscriptionStream, Transaction};

    /// A headless harness: ingest actions against a bare `Ndb` and wait on a
    /// subscription (not a sleep loop) for the canvas to reflect them.
    struct TestNdb {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        stream: SubscriptionStream,
    }

    impl TestNdb {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            let kp = FullKeypair::generate();
            let sub = ndb
                .subscribe(&[event::notebook_filter(&kp.pubkey)])
                .unwrap();
            let stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
            Self {
                ndb,
                _dir: dir,
                kp,
                stream,
            }
        }

        fn secret(&self) -> [u8; 32] {
            self.kp.secret_key.secret_bytes()
        }

        /// Drain the subscription until the loaded canvas satisfies `pred`.
        fn wait<F>(&mut self, pred: F) -> CanvasView
        where
            F: Fn(&CanvasView) -> bool,
        {
            pollster::block_on(async {
                loop {
                    {
                        let txn = Transaction::new(&self.ndb).unwrap();
                        if let Some(view) = load_canvas(&self.ndb, &txn, &self.kp.pubkey, CANVAS_ID)
                            && pred(&view)
                        {
                            return view;
                        }
                    }
                    self.stream.next().await.expect("subscription open");
                }
            })
        }

        fn apply(&self, view: &CanvasView, action: CanvasAction) {
            super::apply(
                &self.ndb,
                CANVAS_ID,
                view,
                &self.kp.pubkey,
                &self.secret(),
                action,
                &mut NoPublish,
            );
        }
    }

    /// Every node has had its placement transform folded (a non-empty z-rank),
    /// so stacking computations see the real top of the stack.
    fn settled(v: &CanvasView) -> bool {
        v.nodes.iter().all(|n| !n.z.is_empty())
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
        w: 200,
        h: 80,
    };

    #[test]
    fn seed_then_add_nodes() {
        let mut t = TestNdb::new();
        seed_canvas(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            CANVAS_ID,
            "Canvas",
            &mut NoPublish,
        );
        let view = t.wait(|v| v.title == "Canvas");
        assert!(view.nodes.is_empty());

        t.apply(
            &view,
            CanvasAction::AddNode {
                kind: NodeKind::Text,
                geo: GEO,
                content: text("first"),
            },
        );
        // Wait until the node's placement transform has folded (non-empty z), so
        // the next add stacks against the real top rank rather than a node whose
        // rank hasn't landed yet.
        let view = t.wait(|v| v.nodes.len() == 1 && settled(v));
        assert_eq!(view.nodes[0].content.text, "first");

        t.apply(
            &view,
            CanvasAction::AddNode {
                kind: NodeKind::Text,
                geo: Geometry { x: 300, ..GEO },
                content: text("second"),
            },
        );
        // Newest is stacked on top (last in back-to-front order).
        let view = t.wait(|v| v.nodes.len() == 2 && settled(v));
        assert_eq!(view.nodes.last().unwrap().content.text, "second");
    }

    #[test]
    fn move_then_edit_then_delete() {
        let mut t = TestNdb::new();
        seed_canvas(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            CANVAS_ID,
            "Canvas",
            &mut NoPublish,
        );
        let view = t.wait(|v| v.title == "Canvas");
        t.apply(
            &view,
            CanvasAction::AddNode {
                kind: NodeKind::Text,
                geo: GEO,
                content: text("node"),
            },
        );
        let view = t.wait(|v| v.nodes.len() == 1);
        let node = view.nodes[0].id;

        t.apply(
            &view,
            CanvasAction::SetGeometry {
                node,
                geo: Geometry {
                    x: 500,
                    y: 250,
                    w: 220,
                    h: 90,
                },
            },
        );
        let view = t.wait(|v| v.nodes.first().is_some_and(|n| n.geo.x == 500));
        assert_eq!(view.nodes[0].geo.y, 250);

        t.apply(
            &view,
            CanvasAction::EditContent {
                node,
                content: text("renamed"),
            },
        );
        let view = t.wait(|v| v.nodes.first().is_some_and(|n| n.content.text == "renamed"));
        // The edit didn't clobber the move.
        assert_eq!(view.nodes[0].geo.x, 500);

        t.apply(&view, CanvasAction::DeleteNode { node });
        let view = t.wait(|v| v.nodes.is_empty());
        assert!(view.nodes.is_empty());
    }

    #[test]
    fn edges_follow_nodes_and_delete() {
        let mut t = TestNdb::new();
        seed_canvas(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            CANVAS_ID,
            "Canvas",
            &mut NoPublish,
        );
        let view = t.wait(|v| v.title == "Canvas");
        t.apply(
            &view,
            CanvasAction::AddNode {
                kind: NodeKind::Text,
                geo: GEO,
                content: text("a"),
            },
        );
        let view = t.wait(|v| v.nodes.len() == 1);
        t.apply(
            &view,
            CanvasAction::AddNode {
                kind: NodeKind::Text,
                geo: Geometry { x: 400, ..GEO },
                content: text("b"),
            },
        );
        let view = t.wait(|v| v.nodes.len() == 2);
        let a = view
            .nodes
            .iter()
            .find(|n| n.content.text == "a")
            .unwrap()
            .id;
        let b = view
            .nodes
            .iter()
            .find(|n| n.content.text == "b")
            .unwrap()
            .id;

        t.apply(
            &view,
            CanvasAction::SetEdge {
                edge_id: "e1".to_string(),
                from: a,
                to: b,
                ends: EdgeEnds {
                    to_end: Some("arrow".to_string()),
                    ..Default::default()
                },
            },
        );
        let view = t.wait(|v| v.edges.len() == 1);
        assert_eq!(view.edges[0].from, a);

        t.apply(
            &view,
            CanvasAction::DeleteEdge {
                edge_id: "e1".to_string(),
                from: a,
                to: b,
            },
        );
        let view = t.wait(|v| v.edges.is_empty());
        assert!(view.edges.is_empty());
    }

    #[test]
    fn restack_reorders() {
        let mut t = TestNdb::new();
        seed_canvas(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            CANVAS_ID,
            "Canvas",
            &mut NoPublish,
        );
        let mut view = t.wait(|v| v.title == "Canvas");
        // Add three nodes a, b, c — each new node stacks on top, so the
        // back-to-front order is [a, b, c].
        for label in ["a", "b", "c"] {
            t.apply(
                &view,
                CanvasAction::AddNode {
                    kind: NodeKind::Text,
                    geo: GEO,
                    content: text(label),
                },
            );
            let want = view.nodes.len() + 1;
            view = t.wait(|v| v.nodes.len() == want && settled(v));
        }
        let order: Vec<String> = view.nodes.iter().map(|n| n.content.text.clone()).collect();
        assert_eq!(order, ["a", "b", "c"]);

        // Move "c" to the bottom (index 0).
        let c = view
            .nodes
            .iter()
            .find(|n| n.content.text == "c")
            .unwrap()
            .id;
        t.apply(
            &view,
            CanvasAction::Restack {
                node: c,
                to_index: 0,
            },
        );
        let view = t.wait(|v| v.nodes.first().is_some_and(|n| n.content.text == "c"));
        let order: Vec<String> = view.nodes.iter().map(|n| n.content.text.clone()).collect();
        assert_eq!(order, ["c", "a", "b"]);
    }

    #[test]
    fn open_surfaces_pending() {
        let mut t = TestNdb::new();
        seed_canvas(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            CANVAS_ID,
            "Canvas",
            &mut NoPublish,
        );
        let view = t.wait(|v| v.title == "Canvas");
        t.apply(&view, CanvasAction::SetOpen { open: true });
        let view = t.wait(|v| v.open);
        assert!(view.open);

        t.apply(
            &view,
            CanvasAction::Rename {
                title: "Renamed".to_string(),
            },
        );
        let view = t.wait(|v| v.title == "Renamed");
        assert_eq!(view.title, "Renamed");
        // The mode edit didn't get clobbered by the rename.
        assert!(view.open);
    }
}
