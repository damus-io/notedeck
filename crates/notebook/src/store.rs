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
use nostrdb::{IngestMetadata, Ndb, Note, NoteBuilder};

use crate::event::{
    self, CanvasView, EdgeEnds, Geometry, NodeContent, NodeKind, NodeView, build_canvas,
    build_canvas_tombstone, build_content, build_edge, build_edge_tombstone, build_longform,
    build_longform_tombstone, build_node, build_node_tombstone, build_transform, canvas_address,
    rank_between,
};

/// A fixed canvas `d` used only as a fixture id in tests. Neither the egui app nor
/// the CLI depends on it in production anymore: the app tracks an active canvas
/// (its `active_canvas`) and mints a fresh `d` per canvas via [`create_canvas`],
/// and the CLI resolves `--canvas <ref|d>` through the unified ref resolver (no
/// default). It stays a valid opaque `d`, so existing accounts that already hold
/// this canvas need no migration.
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
    ingest_signed(ndb, &note, publisher)
}

/// Ingest an already-signed `note` into the local nostrdb, then hand its
/// `["EVENT", {...}]` frame to `publisher`. Split out of [`ingest`] so callers
/// that need the signed note's own fields — notably its `created_at` — can read
/// them off `note` before it's ingested. Returns the note id, or `None` if
/// building the frame or ingesting failed (in which case nothing is published).
fn ingest_signed(ndb: &Ndb, note: &Note, publisher: &mut dyn Publisher) -> Option<NoteId> {
    let id = NoteId::new(*note.id());
    let json = enostr::ClientMessage::event(note).ok()?.to_json().ok()?;
    ndb.process_event_with(&json, IngestMetadata::new().client(true))
        .ok()?;
    publisher.publish(&json);
    Some(id)
}

/// PNS-wrap a signed inner note (NIP-PNS, kind 1080) and ingest the *wrapper*
/// into the local nostrdb, then publish the wrapper frame. The inner note is
/// NIP-44-encrypted with PNS keys derived from `device_secret` — the account key
/// nostrdb was already seeded with via [`nostrdb::Ndb::add_key`] at sign-in — so
/// nostrdb transparently unwraps it on read and the inner note stays queryable by
/// its own author/kind (canvas subscriptions, [`load_longform`], the reducer).
/// On a relay, though, only the opaque kind-1080 envelope is ever visible: a
/// private note can't surface as a public NIP-23 article in anyone's feed, and
/// its contents don't leak in plaintext.
///
/// Returns the **inner** note's id (what callers and every longform query key on;
/// the wrapper's own id is an implementation detail). Used for longform notes;
/// canvas events still go through the plaintext [`ingest`] for now (their custom
/// kinds are never feed-rendered — encrypting them too is a separate change).
fn ingest_pns(
    ndb: &Ndb,
    inner: &Note,
    device_secret: &[u8; 32],
    publisher: &mut dyn Publisher,
) -> Option<NoteId> {
    let inner_id = NoteId::new(*inner.id());
    // The crypto + kind-1080 wrapper construction lives in `enostr::pns::wrap`
    // (signed by the derived PNS keypair, unlinkable to the account); here we only
    // ingest + publish the resulting envelope.
    let pns_keys = enostr::pns::derive_pns_keys(device_secret);
    let wrapper = enostr::pns::wrap(&pns_keys, &inner.json().ok()?, now_secs())?;
    ingest_signed(ndb, &wrapper, publisher)?;
    Some(inner_id)
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

/// Create a new, empty canvas for `author` with a freshly-minted opaque `d` and
/// the given title, returning that `d` so the caller can make it the active
/// canvas. This is the vault "New canvas" path: unlike [`seed_canvas`] (which
/// takes a caller-chosen id — used to re-seed a specific canvas), every call here
/// mints a distinct id, so there is no single well-known canvas. The title lives
/// in the `title` tag and is renamable via [`CanvasAction::Rename`]. Returns the
/// canvas document's note id, or `None` if signing/ingest failed.
pub fn create_canvas(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    title: &str,
    publisher: &mut dyn Publisher,
) -> Option<(String, NoteId)> {
    let _ = author;
    let d = mint_d();
    let id = ingest(ndb, build_canvas(&d, title, &[], false), secret, publisher)?;
    Some((d, id))
}

/// Delete a canvas by superseding it with a tombstone revision: an empty
/// kind-31606 tagged `del`/`1` under the same `canvas_id`. Stamps strictly past
/// `prev_created_at` (the live canvas document's timestamp) so the tombstone wins
/// latest-wins, then ingests it as a plaintext event like every other canvas
/// event (canvas kinds are never feed-rendered, so they skip the PNS wrap the
/// longform path uses). Returns the tombstone's note id, or `None` on failure.
///
/// Reversible: a later canvas revision under the same `d` revives it (see
/// [`event::build_canvas_tombstone`]).
pub fn delete_canvas(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    canvas_id: &str,
    prev_created_at: u64,
    publisher: &mut dyn Publisher,
) -> Option<NoteId> {
    let _ = author;
    ingest(
        ndb,
        build_canvas_tombstone(canvas_id).created_at(next_after(prev_created_at)),
        secret,
        publisher,
    )
}

/// The id and stable `d` of a longform note that was just created, so the caller
/// (the editor) can hold the `d` to drive later edits via [`edit_longform`].
pub struct LongformSaved {
    pub id: NoteId,
    pub d: String,
    /// The `created_at` stamped on the signed note. The editor keeps this as the
    /// supersede baseline: the next [`edit_longform`] passes it as
    /// `prev_created_at` so the edit is stamped strictly past it (see
    /// [`next_after`]).
    pub created_at: u64,
}

/// Mint a fresh, opaque `d` — 8 random bytes as 16 hex chars — for a replaceable
/// document (a longform note or a canvas). Not a [`crate::wordid`] (that encodes
/// an *existing* event id; a replaceable `d` must be chosen before the event
/// exists and stay stable across edits). Reuses the same OS RNG
/// `enostr::FullKeypair::generate` already pulls in — no new crate.
pub fn mint_d() -> String {
    use nostr::secp256k1::rand::{RngCore, rngs::OsRng};
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().fold(String::with_capacity(16), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Create a new longform note (NIP-23, kind 30023). Mints a fresh `d` when one
/// isn't supplied and returns it in [`LongformSaved`] so the editor can reuse it
/// for edits. `author` is kept in the signature for symmetry with the canvas
/// actions and a future `a`-tag; the note is keyed by `(author, d)` implicitly
/// via the signature.
pub fn create_longform(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    input: &LongformInput,
    d: Option<String>,
    publisher: &mut dyn Publisher,
) -> Option<LongformSaved> {
    let _ = author;
    let d = d.unwrap_or_else(mint_d);
    // Sign the inner NIP-23 note (read its `created_at` before it moves), then
    // PNS-wrap + ingest so relays only ever see the opaque envelope.
    let note = build_longform(&d, input).sign(secret).build()?;
    let created_at = note.created_at();
    let id = ingest_pns(ndb, &note, secret, publisher)?;
    Some(LongformSaved { id, d, created_at })
}

/// Edit an existing longform note, republishing with the same `d`. Kind 30023 is
/// replaceable, so the edit supersedes by `created_at`; stamp strictly past
/// `prev_created_at` (the loaded note's timestamp) so a same-second edit still
/// wins the reducer's latest-wins instead of tying (same reasoning as
/// [`next_after`]). Returns the same `d` and the new `created_at` in
/// [`LongformSaved`], so the caller can advance its supersede baseline for the
/// next edit.
pub fn edit_longform(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    d: &str,
    prev_created_at: u64,
    input: &LongformInput,
    publisher: &mut dyn Publisher,
) -> Option<LongformSaved> {
    let _ = author;
    let note = build_longform(d, input)
        .created_at(next_after(prev_created_at))
        .sign(secret)
        .build()?;
    let created_at = note.created_at();
    let id = ingest_pns(ndb, &note, secret, publisher)?;
    Some(LongformSaved {
        id,
        d: d.to_string(),
        created_at,
    })
}

/// Delete a longform note by superseding it with a tombstone revision: an empty
/// kind-30023 tagged `del`/`1` under the same `d`. Stamps strictly past
/// `prev_created_at` (the loaded note's timestamp) so the tombstone wins
/// latest-wins, then PNS-wraps + ingests it exactly like a normal revision — so
/// the delete stays local-only and syncs the same way. Returns the tombstone's
/// inner note id, or `None` if signing/ingest failed.
///
/// Reversible: a later [`edit_longform`] under the same `d` revives the note (see
/// [`event::build_longform_tombstone`]). Returns the tombstone's [`LongformSaved`]
/// — its `created_at` is the supersede baseline a revive would stamp past.
pub fn delete_longform(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    d: &str,
    prev_created_at: u64,
    publisher: &mut dyn Publisher,
) -> Option<LongformSaved> {
    let _ = author;
    let note = build_longform_tombstone(d)
        .created_at(next_after(prev_created_at))
        .sign(secret)
        .build()?;
    let created_at = note.created_at();
    let id = ingest_pns(ndb, &note, secret, publisher)?;
    Some(LongformSaved {
        id,
        d: d.to_string(),
        created_at,
    })
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

/// Convenience re-exports so the app layer can author/load/list a longform note
/// without naming the event module directly.
pub use event::{LongformInput, LongformNote, list_longform, load_longform};

/// Convenience re-exports for the unified vault projection so the app layer can
/// list mixed note+canvas documents without naming the event module directly.
pub use event::{VaultDoc, VaultDocKind, list_canvases, list_vault};

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

        /// Drain the subscription until the [`CANVAS_ID`] canvas satisfies `pred`.
        fn wait<F>(&mut self, pred: F) -> CanvasView
        where
            F: Fn(&CanvasView) -> bool,
        {
            self.wait_id(CANVAS_ID, pred)
        }

        /// Drain the subscription until the canvas keyed by `d` loads and
        /// satisfies `pred` — the multi-canvas form of [`wait`](Self::wait).
        fn wait_id<F>(&mut self, d: &str, pred: F) -> CanvasView
        where
            F: Fn(&CanvasView) -> bool,
        {
            pollster::block_on(async {
                loop {
                    {
                        let txn = Transaction::new(&self.ndb).unwrap();
                        if let Some(view) = load_canvas(&self.ndb, &txn, &self.kp.pubkey, d)
                            && pred(&view)
                        {
                            return view;
                        }
                    }
                    self.stream.next().await.expect("subscription open");
                }
            })
        }

        /// Drain the subscription until the canvas keyed by `d` no longer loads —
        /// its winning revision is a tombstone, so the fold drops it.
        fn gone(&mut self, d: &str) -> bool {
            pollster::block_on(async {
                loop {
                    {
                        let txn = Transaction::new(&self.ndb).unwrap();
                        if load_canvas(&self.ndb, &txn, &self.kp.pubkey, d).is_none() {
                            return true;
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

    #[test]
    fn create_mints_distinct_ids_and_delete_removes_canvas() {
        let mut t = TestNdb::new();
        // No well-known default id: each create mints its own opaque d.
        let (a, _) =
            create_canvas(&t.ndb, &t.kp.pubkey, &t.secret(), "Alpha", &mut NoPublish).unwrap();
        let (b, _) =
            create_canvas(&t.ndb, &t.kp.pubkey, &t.secret(), "Beta", &mut NoPublish).unwrap();
        assert_ne!(a, b, "each canvas gets a distinct minted d");

        // Both fold in, loadable by their own ids with the titles they were given.
        let alpha = t.wait_id(&a, |v| v.title == "Alpha");
        t.wait_id(&b, |v| v.title == "Beta");

        // Deleting one leaves the other untouched: the tombstone supersedes only
        // its own document, and the fold drops it from the vault.
        delete_canvas(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            &a,
            alpha.created_at,
            &mut NoPublish,
        )
        .unwrap();
        assert!(t.gone(&a), "a deleted canvas doesn't load");
        assert_eq!(
            t.wait_id(&b, |v| v.title == "Beta").id,
            b,
            "the sibling canvas survives the delete"
        );
    }

    /// Async harness for the standalone longform functions. Longform notes aren't
    /// in `notebook_filter`, so this subscribes to `longform_filter` and awaits
    /// commits on the stream directly — no blocking, the runtime wakes the task on
    /// subscription activity (see [`CanvasSync::poll`] for the app-side analogue).
    struct LongformTest {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        stream: SubscriptionStream,
    }

    impl LongformTest {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            let kp = FullKeypair::generate();
            // Longform notes are ingested PNS-wrapped (kind 1080); nostrdb only
            // unwraps them — exposing the inner kind-30023 to queries and this
            // subscription — once the device key is registered. The app does this
            // when the account is added; here we do it directly.
            assert!(ndb.add_key(&kp.secret_key.secret_bytes()));
            let sub = ndb
                .subscribe(&[event::longform_filter(&kp.pubkey)])
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

        /// Await `n` committed notes on the subscription (counting notes, not
        /// `next` calls, since a batch may carry several), so a following
        /// [`LongformTest::load`] sees them. Mirrors lib.rs's `await_notes`.
        async fn await_notes(&mut self, n: usize) {
            let mut seen = 0;
            while seen < n {
                seen += self.stream.next().await.expect("subscription open").len();
            }
        }

        fn load(&self, d: &str) -> Option<LongformNote> {
            let txn = Transaction::new(&self.ndb).unwrap();
            load_longform(&self.ndb, &txn, &self.kp.pubkey, d)
        }

        fn list(&self) -> Vec<LongformNote> {
            let txn = Transaction::new(&self.ndb).unwrap();
            list_longform(&self.ndb, &txn, &self.kp.pubkey)
        }
    }

    fn sample_input() -> LongformInput {
        LongformInput {
            title: "Draft".to_string(),
            summary: Some("a summary".to_string()),
            content: "# Draft\n\nfirst body".to_string(),
            published_at: Some(1_700_000_000),
            hashtags: vec!["rust".to_string(), "nostr".to_string()],
        }
    }

    #[tokio::test]
    async fn create_then_load() {
        let mut t = LongformTest::new();
        let input = sample_input();
        let saved = create_longform(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            &input,
            None,
            &mut NoPublish,
        )
        .expect("create longform");
        assert!(!saved.d.is_empty(), "a d was minted");
        assert!(saved.created_at > 0, "a created_at was stamped");

        t.await_notes(1).await;
        let note = t.load(&saved.d).expect("note loads");
        assert_eq!(note.d, saved.d);
        assert_eq!(note.title, "Draft");
        assert_eq!(note.summary.as_deref(), Some("a summary"));
        assert_eq!(note.content, "# Draft\n\nfirst body");
        assert_eq!(note.published_at, Some(1_700_000_000));
        assert_eq!(note.hashtags, vec!["rust".to_string(), "nostr".to_string()]);
        assert_eq!(note.author, *t.kp.pubkey.bytes());
    }

    #[tokio::test]
    async fn edit_supersedes() {
        let mut t = LongformTest::new();
        let input = sample_input();
        let saved = create_longform(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            &input,
            None,
            &mut NoPublish,
        )
        .expect("create longform");
        t.await_notes(1).await;
        let before = t.load(&saved.d).expect("note loads");

        let edited = LongformInput {
            content: "# Draft\n\nedited body".to_string(),
            ..input
        };
        let saved_edit = edit_longform(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            &saved.d,
            before.created_at,
            &edited,
            &mut NoPublish,
        )
        .expect("edit longform");
        // The returned supersede baseline: same d, a strictly-later created_at (so
        // a same-second edit still beats the tie).
        assert_eq!(saved_edit.d, saved.d);
        assert!(saved_edit.created_at > before.created_at);
        t.await_notes(1).await;

        let after = t.load(&saved.d).expect("note loads");
        // The edit wins and the loaded note reflects the returned baseline.
        assert_eq!(after.d, saved.d);
        assert_eq!(after.content, "# Draft\n\nedited body");
        assert_eq!(after.created_at, saved_edit.created_at);
    }

    #[tokio::test]
    async fn delete_hides_note_and_edit_revives_it() {
        let mut t = LongformTest::new();
        let saved = create_longform(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            &sample_input(),
            None,
            &mut NoPublish,
        )
        .expect("create longform");
        t.await_notes(1).await;
        let before = t.load(&saved.d).expect("note loads");
        assert_eq!(t.list().len(), 1, "the note is in the vault list");

        // Delete: a tombstone revision supersedes the note. Loads/lists then treat
        // it as gone even though nostrdb keeps the winning (tombstone) revision.
        let tomb = delete_longform(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            &saved.d,
            before.created_at,
            &mut NoPublish,
        )
        .expect("delete longform");
        assert!(
            tomb.created_at > before.created_at,
            "the tombstone supersedes the live note"
        );
        t.await_notes(1).await;
        assert!(t.load(&saved.d).is_none(), "a deleted note doesn't load");
        assert!(t.list().is_empty(), "a deleted note leaves the vault list");

        // Revive: a later edit under the same d supersedes the tombstone, so the
        // note reappears — the delete was reversible.
        let revived = LongformInput {
            content: "back again".to_string(),
            ..sample_input()
        };
        edit_longform(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            &saved.d,
            // Stamp past the tombstone (the newest revision) so the revive wins.
            tomb.created_at,
            &revived,
            &mut NoPublish,
        )
        .expect("revive longform");
        t.await_notes(1).await;
        let after = t.load(&saved.d).expect("the revived note loads again");
        assert_eq!(after.content, "back again");
        assert_eq!(t.list().len(), 1, "the revived note is back in the vault");
    }

    #[tokio::test]
    async fn distinct_ids_coexist() {
        let mut t = LongformTest::new();
        let first = LongformInput {
            title: "First".to_string(),
            content: "one".to_string(),
            ..Default::default()
        };
        let second = LongformInput {
            title: "Second".to_string(),
            content: "two".to_string(),
            ..Default::default()
        };
        let a = create_longform(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            &first,
            None,
            &mut NoPublish,
        )
        .expect("create first");
        let b = create_longform(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            &second,
            None,
            &mut NoPublish,
        )
        .expect("create second");
        assert_ne!(a.d, b.d, "fresh d per note, no collision");

        // Await both commits (in one call — they may land in a single batch),
        // then both notes resolve independently.
        t.await_notes(2).await;
        let na = t.load(&a.d).expect("first loads");
        let nb = t.load(&b.d).expect("second loads");
        assert_eq!(na.title, "First");
        assert_eq!(na.content, "one");
        assert_eq!(nb.title, "Second");
        assert_eq!(nb.content, "two");
    }

    #[tokio::test]
    async fn list_vault_merges_notes_and_canvases() {
        // Both stores in one listing: a canvas (plaintext kind-31606 events) and a
        // longform note (PNS-wrapped kind-30023). The note is unwrapped only once
        // the device key is registered; canvas events need no key.
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let kp = FullKeypair::generate();
        assert!(ndb.add_key(&kp.secret_key.secret_bytes()));
        let secret = kp.secret_key.secret_bytes();

        let (canvas_d, _) = create_canvas(&ndb, &kp.pubkey, &secret, "Board", &mut NoPublish)
            .expect("create canvas");
        let note = create_longform(
            &ndb,
            &kp.pubkey,
            &secret,
            &LongformInput {
                title: "Draft".to_string(),
                content: "body".to_string(),
                ..Default::default()
            },
            None,
            &mut NoPublish,
        )
        .expect("create note");

        // Drain a combined subscription until both documents are folded into the
        // vault (subscribe-then-poll, not a sleep — mirrors the other harnesses).
        let sub = ndb
            .subscribe(&[
                event::notebook_filter(&kp.pubkey),
                event::longform_filter(&kp.pubkey),
            ])
            .unwrap();
        let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
        let docs = loop {
            {
                let txn = Transaction::new(&ndb).unwrap();
                let docs = list_vault(&ndb, &txn, &kp.pubkey);
                if docs.len() == 2 {
                    break docs;
                }
            }
            stream.next().await.expect("subscription open");
        };

        // Exactly the two documents, one of each kind, each projected correctly.
        let note_doc = docs
            .iter()
            .find(|d| d.kind == VaultDocKind::Note)
            .expect("a note row");
        let canvas_doc = docs
            .iter()
            .find(|d| d.kind == VaultDocKind::Canvas)
            .expect("a canvas row");
        assert_eq!(note_doc.d, note.d);
        assert_eq!(note_doc.title, "Draft");
        assert_eq!(note_doc.author, kp.pubkey);
        assert_eq!(canvas_doc.d, canvas_d);
        assert_eq!(canvas_doc.title, "Board");
        assert_eq!(canvas_doc.author, kp.pubkey);
        // Sorted newest-edited-first: the list is non-increasing in `edited_at`,
        // whichever document happens to carry the later wall-clock second.
        assert!(
            docs[0].edited_at >= docs[1].edited_at,
            "vault sorted newest-edited first"
        );
    }
}
