//! The dependency-graph *model* for an epic: a pure function over a folded
//! [`BoardView`] that produces the node set and directed blocking edges the
//! layered layout ([`notedeck_ui::graph::layered_layout`]) and the graph view
//! render. No egui, no I/O — the same shape as [`crate::traversal`], one step up.
//!
//! ## What it produces
//! A [`DependencyGraph`] of:
//! - **nodes** — one [`GraphNode`] per card, in a stable order. A node's index
//!   in [`DependencyGraph::nodes`] *is* its id for the layout: edges reference
//!   nodes by index, matching `layered_layout(node_count, &[(from, to)], ..)`
//!   where node ids run `0..node_count`.
//! - **edges** — one [`GraphEdge`] per *blocking* relationship among the node
//!   set, directed **blocker → blocked** (the `from` node blocks the `to` node),
//!   the same direction the layout ranks by.
//!
//! ## Node set (and the ghost decision)
//! The default node set is the epic's **subissue subtree** — its subissues, and
//! theirs, recursively — reusing [`crate::traversal::work_order`] (the shared
//! DFS + visited-set walk over [`Container::Card`]). The epic card itself is the
//! *container*, not a work item, so it is not a node.
//!
//! A blocking edge can point *outside* that subtree: a subtree card blocked by
//! some unrelated card, or blocking one. Dropping such an edge would hide a real
//! constraint on the critical path, so instead we keep the edge and pull the
//! outside card in as a **ghost node** ([`GraphNode::ghost`] = `true`) — present
//! so the edge has both endpoints, flagged so the view can style it as context
//! (dimmed, non-interactive) rather than as one of the epic's own cards. Ghosts
//! are only ever added on demand, when an edge needs them; a subtree with no
//! cross-boundary edges has no ghosts.
//!
//! ## Edges (blocking only)
//! Edges come solely from [`CardView::blocked_by`] / [`CardView::blocks`]
//! ([`EdgeRef`]). Parent/subissue *containment* is **not** drawn as an edge — it
//! is already conveyed by node membership (a card is in the graph because it is
//! in the subtree). Only "what blocks what" gets an arrow.
//!
//! `blocked_by` is the authoritative set on the blocked card, so every edge
//! whose *blocked* endpoint is in the subtree is read from there (internal edges
//! and upstream-ghost blockers alike). `blocks` is a reverse index that only the
//! reducer's folded boards populate, and it is needed for exactly one case: a
//! subtree card blocking a card *outside* the subtree (a downstream ghost), whose
//! own `blocked_by` we never walk. Reading internal edges from one side only
//! keeps every relationship a single arrow — see [`GraphEdge`] for the
//! deduplication contract.
//!
//! ## Flat vs collapsed
//! Two builders share these node/edge types:
//! - [`dependency_graph`] — the **flat** graph: every card in the subtree is its
//!   own node. Faithful, but a deep epic renders as a cloud and parent cards
//!   float unconnected to their own children (containment isn't an edge).
//! - [`collapsed_dependency_graph`] — one node per **direct** subissue, each
//!   standing in for its whole subtree with a [`SubtreeProgress`] pill, blocks
//!   projected onto those representatives. Keeps a deep epic at one granularity;
//!   the view drills into a node by re-running it with that card as the epic.
//!
//! [`EdgeRef`]: crate::event::EdgeRef

use std::collections::{HashMap, HashSet};

use nostrdb_net::NoteId;

use crate::event::{BoardView, ColumnPos, Container};
use crate::traversal::work_order;

/// How much of a node's subissue subtree is done — the "X/Y" a collapsed node
/// shows as a progress pill. Present only on a node that *has* a subtree; a leaf
/// card carries `None` (see [`GraphNode::progress`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubtreeProgress {
    /// Descendants sitting in a terminal ("done") column ([`BoardView::card_is_done`]).
    pub done: usize,
    /// Total live descendants in the subtree (the recursive [`work_order`]).
    pub total: usize,
}

/// One card in the graph: its id, live column position, whether it is a *ghost*
/// (pulled in only because an edge crosses the epic's subtree, not one of the
/// epic's own cards), and — when it stands in for a whole subtree — that
/// subtree's progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphNode {
    /// The card this node stands for.
    pub id: NoteId,
    /// The card's live [`ColumnPos`] on the board (via the position half of
    /// [`crate::event::card_with_column_in_board`]), or `None` when the card is
    /// not on a live column of *this* board — archived here, or a ghost that
    /// lives on another board entirely. Enough to derive a status indicator and,
    /// for a blocker, whether its dependency is cleared.
    pub column: Option<ColumnPos>,
    /// `true` when this node was added only to anchor a cross-subtree edge — a
    /// blocker or blocked card outside the epic's subissue subtree. The view
    /// styles ghosts as context rather than as the epic's cards.
    pub ghost: bool,
    /// The node's subissue-subtree progress, `Some` iff the card has at least one
    /// live descendant. In the collapsed graph ([`collapsed_dependency_graph`])
    /// this marks an *expandable* node — one that stands in for a whole subtree —
    /// and drives its progress pill; ghosts never carry it.
    pub progress: Option<SubtreeProgress>,
}

/// A directed blocking edge: the `from` node blocks the `to` node (arrow points
/// at the blocked card, the direction the layout ranks by). `from`/`to` are
/// indices into [`DependencyGraph::nodes`].
///
/// Each `(from, to)` pair appears **at most once**: a relationship between two
/// subtree cards is read only from the blocked card's [`CardView::blocked_by`],
/// never doubled by the blocker's [`CardView::blocks`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphEdge {
    /// Index (into [`DependencyGraph::nodes`]) of the blocker.
    pub from: usize,
    /// Index (into [`DependencyGraph::nodes`]) of the blocked card.
    pub to: usize,
    /// The blocking dependency is *satisfied*: the blocker (the `from` node) is
    /// cleared — done (in its board's last column) or archived — so the edge no
    /// longer holds work back. Mirrors [`crate::event::EdgeRef::done`]'s
    /// blocker-cleared meaning, so the view can dim resolved arrows.
    pub done: bool,
}

/// The dependency graph of an epic: nodes plus the directed blocking edges among
/// them, ready to hand to [`notedeck_ui::graph::layered_layout`] (node ids are
/// node indices; edges are `(from, to)` blocker→blocked pairs) and the chip
/// renderer. Built by [`dependency_graph`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DependencyGraph {
    /// The graph's nodes; a node's index here is its id for the layout.
    pub nodes: Vec<GraphNode>,
    /// The directed blocking edges, deduplicated (see [`GraphEdge`]).
    pub edges: Vec<GraphEdge>,
}

/// Build the [`DependencyGraph`] for the epic card `epic_id` out of the folded
/// `view`. Pure: no I/O, no egui.
///
/// Nodes are the epic's subissue subtree in [`work_order`] (DFS pre-order),
/// followed by any ghost nodes in the order their edges are discovered — a
/// deterministic order for a given `view`. Edges are the blocking relationships
/// among those nodes; see the [module docs](self) for the node-set, ghost, and
/// edge-direction rules.
///
/// An unknown or unplaced `epic_id` (no live card, or a card with no subissues)
/// yields an empty graph, never a panic.
pub fn dependency_graph(view: &BoardView, epic_id: &[u8; 32]) -> DependencyGraph {
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut index: HashMap<[u8; 32], usize> = HashMap::new();

    // Primary nodes: the epic's subissue subtree, in shared work-order.
    for card in work_order(view, &Container::Card(*epic_id)) {
        index.insert(*card.id.bytes(), nodes.len());
        nodes.push(GraphNode {
            id: card.id,
            column: column_pos(view, card.id),
            ghost: false,
            progress: subtree_progress(view, card.id),
        });
    }
    let primary_count = nodes.len();

    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut seen: HashSet<(usize, usize)> = HashSet::new();

    // Iterate only the primary nodes; ghosts are pulled in on demand below and
    // never sourced for edges themselves (their own edges live off-subtree).
    for i in 0..primary_count {
        let card = view
            .card(nodes[i].id)
            .expect("primary node came from a live card");

        // Internal + upstream edges: every card that blocks this one. Read from
        // the authoritative `blocked_by`; the blocker becomes a ghost when it is
        // outside the subtree. `EdgeRef::done` already means "blocker cleared".
        for edge in &card.blocked_by {
            let from = ensure_node(&mut nodes, &mut index, view, edge.id);
            push_edge(&mut edges, &mut seen, from, i, edge.done);
        }

        // Downstream-ghost edges: this card blocks a card *outside* the subtree.
        // An in-subtree target is skipped — that same edge is read from the
        // target's `blocked_by` above, so it is never doubled.
        let source_cleared = view.card_is_done(nodes[i].id);
        for edge in &card.blocks {
            if is_primary(&index, primary_count, edge.id) {
                continue;
            }
            let to = ensure_node(&mut nodes, &mut index, view, edge.id);
            push_edge(&mut edges, &mut seen, i, to, source_cleared);
        }
    }

    DependencyGraph { nodes, edges }
}

/// Build the *collapsed* dependency graph of `epic_id`: one node per **direct**
/// subissue, each standing in for its whole subtree, rather than the flat
/// recursive node set [`dependency_graph`] produces. The graph view's default —
/// it keeps a deep epic at a single granularity and lets the user drill into a
/// node (re-run this with that card as the epic) to expand a level.
///
/// ## Nodes
/// The epic's direct subissues that are live cards, in the reducer's work-order.
/// A node whose card has its own descendants carries [`GraphNode::progress`]
/// (`Some`) — it is *expandable*; a direct child that is itself a leaf carries
/// `None` and behaves like an ordinary node. The epic card itself is the
/// container, never a node. Ghost nodes (cards outside the epic's subtree named
/// by a crossing edge) are added exactly as in the flat graph.
///
/// ## Edges
/// Every blocking edge in the epic's *full* subtree is projected onto these
/// depth-1 representatives: each endpoint maps to the direct subissue whose
/// subtree contains it. A block between two cards under the **same** representative
/// is internal to one collapsed node and dropped; a block across two
/// representatives becomes one aggregated edge. An aggregated edge is `done` only
/// when *every* underlying block it stands for is cleared — so the collapsed
/// arrow stays active while any real dependency between the two groups remains.
///
/// An unknown/leaf `epic_id` (no live card, or no subissues) yields an empty
/// graph, never a panic.
pub fn collapsed_dependency_graph(view: &BoardView, epic_id: &[u8; 32]) -> DependencyGraph {
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut index: HashMap<[u8; 32], usize> = HashMap::new();
    // Every card in the epic's subtree -> the direct-subissue node that stands in
    // for it (its depth-1 ancestor). The projection that collapses the subtree.
    let mut represent: HashMap<[u8; 32], usize> = HashMap::new();

    // Primary nodes: the epic's *direct* subissues (one level), each a live card.
    let Some(epic) = view.card(NoteId::new(*epic_id)) else {
        return DependencyGraph::default();
    };
    for sub in &epic.subissues {
        if view.card(sub.id).is_none() || index.contains_key(sub.id.bytes()) {
            continue;
        }
        let idx = nodes.len();
        index.insert(*sub.id.bytes(), idx);
        represent.insert(*sub.id.bytes(), idx);
        // Every card in this child's subtree maps up to this node (first writer
        // wins, so a subissue shared by two children stays with the earlier one).
        for desc in work_order(view, &Container::Card(*sub.id.bytes())) {
            represent.entry(*desc.id.bytes()).or_insert(idx);
        }
        nodes.push(GraphNode {
            id: sub.id,
            column: column_pos(view, sub.id),
            ghost: false,
            progress: subtree_progress(view, sub.id),
        });
    }

    // Edges, aggregated per representative pair. `edge_at` maps a `(from, to)`
    // representative pair to its slot in `edges`, so repeated underlying blocks
    // fold into one arrow whose `done` is the AND of theirs.
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut edge_at: HashMap<(usize, usize), usize> = HashMap::new();

    // Walk the epic's whole subtree (blocks live at leaf level) in work-order,
    // projecting each card's edges onto the representatives.
    for card in work_order(view, &Container::Card(*epic_id)) {
        let Some(&to) = represent.get(card.id.bytes()) else {
            continue;
        };

        // Upstream + internal blocks: cards that block this one. The blocker
        // projects to its representative, or becomes a ghost when off-subtree; a
        // same-representative block is intra-node and dropped by `add_edge`.
        for edge in &card.blocked_by {
            let from = represent
                .get(edge.id.bytes())
                .copied()
                .unwrap_or_else(|| ensure_node(&mut nodes, &mut index, view, edge.id));
            add_edge(&mut edges, &mut edge_at, from, to, edge.done);
        }

        // Downstream-ghost blocks: this card blocks one *outside* the epic's
        // subtree (an in-subtree target is read from its own `blocked_by` above).
        let source_cleared = view.card_is_done(card.id);
        for edge in &card.blocks {
            if represent.contains_key(edge.id.bytes()) {
                continue;
            }
            let ghost = ensure_node(&mut nodes, &mut index, view, edge.id);
            add_edge(&mut edges, &mut edge_at, to, ghost, source_cleared);
        }
    }

    DependencyGraph { nodes, edges }
}

/// Record a collapsed edge `from -> to`, folding a repeat of the same
/// representative pair into the existing arrow: its `done` becomes the AND of the
/// two, so an aggregated edge is cleared only when *every* underlying block is.
/// Self-edges (a block within one collapsed subtree) are dropped.
fn add_edge(
    edges: &mut Vec<GraphEdge>,
    edge_at: &mut HashMap<(usize, usize), usize>,
    from: usize,
    to: usize,
    done: bool,
) {
    if from == to {
        return;
    }
    if let Some(&pos) = edge_at.get(&(from, to)) {
        edges[pos].done &= done;
    } else {
        edge_at.insert((from, to), edges.len());
        edges.push(GraphEdge { from, to, done });
    }
}

/// Index of the node for `id`, adding it as a *ghost* if it is not already a
/// node. Existing nodes (primary or an earlier ghost) keep their index and flag.
fn ensure_node(
    nodes: &mut Vec<GraphNode>,
    index: &mut HashMap<[u8; 32], usize>,
    view: &BoardView,
    id: NoteId,
) -> usize {
    if let Some(&idx) = index.get(id.bytes()) {
        return idx;
    }
    let idx = nodes.len();
    index.insert(*id.bytes(), idx);
    nodes.push(GraphNode {
        id,
        column: column_pos(view, id),
        ghost: true,
        // Ghosts are context, not the epic's own work: no progress pill, and a
        // ghost off this board has no subtree to walk here anyway.
        progress: None,
    });
    idx
}

/// Is `id` one of the primary (subtree) nodes — the first `primary_count`
/// entries, added before any ghost?
fn is_primary(index: &HashMap<[u8; 32], usize>, primary_count: usize, id: NoteId) -> bool {
    index
        .get(id.bytes())
        .is_some_and(|&idx| idx < primary_count)
}

/// Record edge `from -> to`, skipping self-edges and any `(from, to)` already
/// seen so each relationship is a single arrow.
fn push_edge(
    edges: &mut Vec<GraphEdge>,
    seen: &mut HashSet<(usize, usize)>,
    from: usize,
    to: usize,
    done: bool,
) {
    if from == to {
        return;
    }
    if seen.insert((from, to)) {
        edges.push(GraphEdge { from, to, done });
    }
}

/// The live [`ColumnPos`] of `id` on `view`, or `None` when it is not on a live
/// column (archived here, or off-board). The position half of
/// [`crate::event::card_with_column_in_board`], without cloning the card.
fn column_pos(view: &BoardView, id: NoteId) -> Option<ColumnPos> {
    let count = view.columns.len();
    view.columns.iter().enumerate().find_map(|(index, col)| {
        col.cards
            .iter()
            .any(|c| c.id == id)
            .then_some(ColumnPos { index, count })
    })
}

/// The [`SubtreeProgress`] of the card `id`: done/total over its live subissue
/// subtree ([`work_order`]), or `None` when the card has no live descendants (a
/// leaf, which carries no progress pill). Doneness is [`BoardView::card_is_done`]
/// so the whole graph decides doneness the one way.
fn subtree_progress(view: &BoardView, id: NoteId) -> Option<SubtreeProgress> {
    let subtree = work_order(view, &Container::Card(*id.bytes()));
    if subtree.is_empty() {
        return None;
    }
    let done = subtree.iter().filter(|c| view.card_is_done(c.id)).count();
    Some(SubtreeProgress {
        done,
        total: subtree.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{ArchivedCard, CardView, ColumnView, EdgeRef, Priority, SubissueView};

    fn nid(n: u8) -> NoteId {
        NoteId::new([n; 32])
    }

    fn eref(n: u8, done: bool) -> EdgeRef {
        EdgeRef {
            id: nid(n),
            title: format!("card {n}"),
            done,
        }
    }

    fn subv(n: u8) -> SubissueView {
        SubissueView {
            id: nid(n),
            title: format!("card {n}"),
            column: Some("backlog".to_string()),
            done: false,
            archived: false,
            seq: None,
        }
    }

    /// A [`CardView`] carrying only the fields the graph model reads: id, parent,
    /// subissues (member order), and the blocking edges.
    fn card(
        n: u8,
        parent: Option<u8>,
        subissues: Vec<SubissueView>,
        blocked_by: Vec<EdgeRef>,
        blocks: Vec<EdgeRef>,
    ) -> CardView {
        CardView {
            id: nid(n),
            author: [0; 32],
            title: format!("card {n}"),
            description: String::new(),
            labels: vec![],
            priority: Priority::None,
            due: None,
            estimate: None,
            rank: "m".to_string(),
            seq: None,
            placed_at: 0,
            created_at: 0,
            updated_at: 0,
            comments: vec![],
            reviews: vec![],
            activity: vec![],
            parent: parent.map(nid),
            subissues,
            blocked_by,
            blocks,
            related: vec![],
        }
    }

    /// Assemble a three-column board (`backlog`, `doing`, `done`) placing each
    /// card into the column named by `placement` (defaulting to `backlog`).
    fn board(cards: Vec<(CardView, &str)>) -> BoardView {
        let names = ["backlog", "doing", "done"];
        let mut columns: Vec<ColumnView> = names
            .iter()
            .map(|id| ColumnView {
                id: id.to_string(),
                name: id.to_string(),
                terminal: false,
                cards: vec![],
            })
            .collect();
        for (c, col) in cards {
            let idx = names.iter().position(|n| *n == col).unwrap_or(0);
            columns[idx].cards.push(c);
        }
        BoardView {
            id: "b".to_string(),
            author: [0; 32],
            title: "b".to_string(),
            description: String::new(),
            created_at: 0,
            columns,
            archived: Vec::<ArchivedCard>::new(),
        }
    }

    fn node_id(g: &DependencyGraph, id: u8) -> usize {
        g.nodes
            .iter()
            .position(|n| n.id == nid(id))
            .unwrap_or_else(|| panic!("node {id} missing"))
    }

    /// A blocking chain A -> B -> C under epic E, with an upstream ghost X
    /// blocking A and a downstream ghost Y blocked by C. Exercises the subtree
    /// node set, both ghost directions, and edge orientation.
    #[test]
    fn chain_with_ghosts_both_sides() {
        // Epic E (id 1) owns A(2), B(3), C(4). X(8) and Y(9) sit outside.
        let epic = card(1, None, vec![subv(2), subv(3), subv(4)], vec![], vec![]);
        // A is blocked by the outside X (upstream ghost); A is cleared? no.
        let a = card(2, Some(1), vec![], vec![eref(8, false)], vec![]);
        // B is blocked by A; C is blocked by B — the internal chain.
        let b = card(3, Some(1), vec![], vec![eref(2, false)], vec![]);
        // C blocks the outside Y (downstream ghost).
        let c = card(
            4,
            Some(1),
            vec![],
            vec![eref(3, false)],
            vec![eref(9, false)],
        );
        let x = card(8, None, vec![], vec![], vec![eref(2, false)]);
        let y = card(9, None, vec![], vec![eref(4, false)], vec![]);

        let view = board(vec![
            (epic, "backlog"),
            (a, "backlog"),
            (b, "backlog"),
            (c, "backlog"),
            (x, "backlog"),
            (y, "backlog"),
        ]);
        let g = dependency_graph(&view, nid(1).bytes());

        // Primary nodes A, B, C in work-order, then ghosts X, Y on discovery.
        assert_eq!(g.nodes.len(), 5);
        assert_eq!(g.nodes[0].id, nid(2));
        assert_eq!(g.nodes[1].id, nid(3));
        assert_eq!(g.nodes[2].id, nid(4));
        assert!(!g.nodes[0].ghost && !g.nodes[1].ghost && !g.nodes[2].ghost);
        assert!(g.nodes[3].ghost && g.nodes[4].ghost, "X and Y are ghosts");
        // The epic container itself is never a node.
        assert!(g.nodes.iter().all(|n| n.id != nid(1)));

        // Edges: X->A, A->B, B->C, C->Y — blocker -> blocked, deduped.
        let want = [
            (node_id(&g, 8), node_id(&g, 2)),
            (node_id(&g, 2), node_id(&g, 3)),
            (node_id(&g, 3), node_id(&g, 4)),
            (node_id(&g, 4), node_id(&g, 9)),
        ];
        let got: HashSet<(usize, usize)> = g.edges.iter().map(|e| (e.from, e.to)).collect();
        assert_eq!(got, want.into_iter().collect::<HashSet<_>>());
        // Containment (E->A/B/C, or subissue arrows) is never an edge.
        assert_eq!(g.edges.len(), 4);
    }

    /// `EdgeRef::done` flows onto `blocked_by`-sourced edges; a downstream-ghost
    /// edge's `done` comes from the *source* card's own cleared state.
    #[test]
    fn edge_done_reflects_blocker_cleared() {
        let epic = card(1, None, vec![subv(2), subv(3)], vec![], vec![]);
        // A is done (last column); B records A as a cleared blocker.
        let a = card(2, Some(1), vec![], vec![], vec![eref(9, false)]);
        let b = card(3, Some(1), vec![], vec![eref(2, true)], vec![]);
        // Downstream ghost Y(9) blocked by A — A is in the `done` column, so the
        // A->Y edge is satisfied even though Y itself is not.
        let y = card(9, None, vec![], vec![eref(2, true)], vec![]);

        let view = board(vec![
            (epic, "backlog"),
            (a, "done"),
            (b, "backlog"),
            (y, "backlog"),
        ]);
        let g = dependency_graph(&view, nid(1).bytes());

        let edge = |from, to| {
            g.edges
                .iter()
                .find(|e| e.from == node_id(&g, from) && e.to == node_id(&g, to))
                .unwrap_or_else(|| panic!("edge {from}->{to} missing"))
        };
        // A->B: done straight from the blocked_by EdgeRef (blocker A cleared).
        assert!(edge(2, 3).done, "A cleared -> A->B satisfied");
        // A->Y: downstream ghost, done derived from A's own column (last = done).
        assert!(edge(2, 9).done, "A in last column -> A->Y satisfied");
    }

    /// A relationship between two subtree cards yields exactly one arrow even
    /// when both endpoints record it (blocked_by on one, blocks on the other).
    #[test]
    fn internal_edge_not_doubled() {
        let epic = card(1, None, vec![subv(2), subv(3)], vec![], vec![]);
        let a = card(2, Some(1), vec![], vec![], vec![eref(3, false)]);
        let b = card(3, Some(1), vec![], vec![eref(2, false)], vec![]);

        let view = board(vec![(epic, "backlog"), (a, "backlog"), (b, "backlog")]);
        let g = dependency_graph(&view, nid(1).bytes());

        assert_eq!(g.nodes.len(), 2, "no ghosts for an all-internal edge");
        assert_eq!(g.edges.len(), 1, "A->B recorded once");
        assert_eq!(g.edges[0].from, node_id(&g, 2));
        assert_eq!(g.edges[0].to, node_id(&g, 3));
    }

    /// An unknown epic id folds to an empty graph, not a panic.
    #[test]
    fn unknown_epic_is_empty() {
        let view = board(vec![]);
        let g = dependency_graph(&view, nid(7).bytes());
        assert!(g.nodes.is_empty() && g.edges.is_empty());
    }

    /// The collapsed graph nodes only the epic's *direct* subissues (each with
    /// subtree progress), projects a cross-subtree block onto its representatives,
    /// drops an intra-subtree block, and keeps cross-subtree ghosts both ways.
    #[test]
    fn collapse_projects_edges_and_ghosts() {
        // E(1) -> P(2){A(4),B(5)}, Q(3){C(6)}.
        let epic = card(1, None, vec![subv(2), subv(3)], vec![], vec![]);
        let p = card(2, Some(1), vec![subv(4), subv(5)], vec![], vec![]);
        let q = card(3, Some(1), vec![subv(6)], vec![], vec![]);
        // A(under P) is blocked by C(under Q) -> Q->P, and by ghost X(8) -> X->P.
        let a = card(
            4,
            Some(2),
            vec![],
            vec![eref(6, false), eref(8, false)],
            vec![],
        );
        // B(under P) is blocked by A(under P): intra-P, dropped.
        let b = card(5, Some(2), vec![], vec![eref(4, false)], vec![]);
        // C(under Q) blocks ghost Y(9) -> Q->Y.
        let c = card(6, Some(3), vec![], vec![], vec![eref(9, false)]);
        let x = card(8, None, vec![], vec![], vec![eref(4, false)]);
        let y = card(9, None, vec![], vec![eref(6, false)], vec![]);

        let view = board(vec![
            (epic, "backlog"),
            (p, "backlog"),
            (q, "backlog"),
            (a, "done"), // A done -> P progress 1/2
            (b, "backlog"),
            (c, "backlog"),
            (x, "backlog"),
            (y, "backlog"),
        ]);
        let g = collapsed_dependency_graph(&view, nid(1).bytes());

        // Only the direct children P, Q are real nodes; ghosts X, Y follow.
        assert_eq!(g.nodes.len(), 4);
        assert_eq!(g.nodes[0].id, nid(2));
        assert_eq!(g.nodes[1].id, nid(3));
        assert!(!g.nodes[0].ghost && !g.nodes[1].ghost);
        assert!(g.nodes[2].ghost && g.nodes[3].ghost);
        // Neither the epic nor a grandchild (A/B/C) becomes a node.
        for absent in [1u8, 4, 5, 6] {
            assert!(
                g.nodes.iter().all(|n| n.id != nid(absent)),
                "no node {absent}"
            );
        }

        // Subtree progress: P has A(done)+B -> 1/2; Q has C -> 0/1; ghosts none.
        assert_eq!(
            g.nodes[0].progress,
            Some(SubtreeProgress { done: 1, total: 2 })
        );
        assert_eq!(
            g.nodes[1].progress,
            Some(SubtreeProgress { done: 0, total: 1 })
        );
        assert_eq!(g.nodes[2].progress, None);
        assert_eq!(g.nodes[3].progress, None);

        // Projected edges: Q->P, ghost X->P, Q->ghost Y. Intra-P B<-A is gone.
        let want = [
            (node_id(&g, 3), node_id(&g, 2)),
            (node_id(&g, 8), node_id(&g, 2)),
            (node_id(&g, 3), node_id(&g, 9)),
        ];
        let got: HashSet<(usize, usize)> = g.edges.iter().map(|e| (e.from, e.to)).collect();
        assert_eq!(got, want.into_iter().collect::<HashSet<_>>());
        assert_eq!(g.edges.len(), 3);
    }

    /// Several underlying blocks between the same two subtrees fold into one
    /// arrow whose `done` is the AND of theirs — active while any block remains.
    #[test]
    fn collapse_aggregates_edge_done() {
        // E(1) -> P(2){A(4),B(5)}, Q(3){C(6),D(7)}. C blocks A, D blocks B, so
        // both underlying blocks project to a single Q->P edge.
        let epic = card(1, None, vec![subv(2), subv(3)], vec![], vec![]);
        let p = card(2, Some(1), vec![subv(4), subv(5)], vec![], vec![]);
        let q = card(3, Some(1), vec![subv(6), subv(7)], vec![], vec![]);
        // C(6) is done (cleared blocker); D(7) is not.
        let a = card(4, Some(2), vec![], vec![eref(6, true)], vec![]);
        let b = card(5, Some(2), vec![], vec![eref(7, false)], vec![]);
        let c = card(6, Some(3), vec![], vec![], vec![]);
        let d = card(7, Some(3), vec![], vec![], vec![]);

        let view = board(vec![
            (epic, "backlog"),
            (p, "backlog"),
            (q, "backlog"),
            (a, "backlog"),
            (b, "backlog"),
            (c, "done"),
            (d, "backlog"),
        ]);
        let g = collapsed_dependency_graph(&view, nid(1).bytes());

        // One aggregated Q->P edge; not done because D's block is still active.
        assert_eq!(g.edges.len(), 1);
        let edge = g.edges[0];
        assert_eq!((edge.from, edge.to), (node_id(&g, 3), node_id(&g, 2)));
        assert!(!edge.done, "aggregated edge active while any block remains");
    }

    /// A direct child that is itself a leaf collapses to an ordinary node — no
    /// progress pill — and still participates in edges.
    #[test]
    fn collapse_leaf_child_has_no_progress() {
        // E(1) -> P(2){A(4)}, L(3) leaf. L blocks P's child A -> L->P.
        let epic = card(1, None, vec![subv(2), subv(3)], vec![], vec![]);
        let p = card(2, Some(1), vec![subv(4)], vec![], vec![]);
        let l = card(3, Some(1), vec![], vec![], vec![]);
        let a = card(4, Some(2), vec![], vec![eref(3, false)], vec![]);

        let view = board(vec![
            (epic, "backlog"),
            (p, "backlog"),
            (l, "backlog"),
            (a, "backlog"),
        ]);
        let g = collapsed_dependency_graph(&view, nid(1).bytes());

        assert_eq!(g.nodes.len(), 2);
        assert_eq!(
            g.nodes[node_id(&g, 2)].progress,
            Some(SubtreeProgress { done: 0, total: 1 })
        );
        assert_eq!(
            g.nodes[node_id(&g, 3)].progress,
            None,
            "leaf child: no pill"
        );
        // L blocks A (under P) -> a single L->P edge.
        assert_eq!(g.edges.len(), 1);
        assert_eq!(
            (g.edges[0].from, g.edges[0].to),
            (node_id(&g, 3), node_id(&g, 2))
        );
    }

    /// An unknown/leaf epic collapses to an empty graph, not a panic.
    #[test]
    fn collapse_unknown_epic_is_empty() {
        let view = board(vec![]);
        let g = collapsed_dependency_graph(&view, nid(7).bytes());
        assert!(g.nodes.is_empty() && g.edges.is_empty());
    }
}
