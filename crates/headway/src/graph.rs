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
//! [`EdgeRef`]: crate::event::EdgeRef

use std::collections::{HashMap, HashSet};

use nostrdb_net::NoteId;

use crate::event::{BoardView, ColumnPos, Container};
use crate::traversal::work_order;

/// One card in the graph: its id, live column position, and whether it is a
/// *ghost* (pulled in only because an edge crosses the epic's subtree, not one
/// of the epic's own cards).
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
        let source_cleared = is_cleared(nodes[i].column);
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

/// A card is *cleared* when it sits in its board's last (Done-style) column —
/// the same positional doneness [`crate::traversal`] and
/// [`crate::event::EdgeRef::done`] use. `None` (archived/off-board) is treated
/// as not-cleared here; the only caller is a live subtree card, which always has
/// a column.
fn is_cleared(column: Option<ColumnPos>) -> bool {
    column.is_some_and(|p| p.count > 0 && p.index + 1 == p.count)
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
}
