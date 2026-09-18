//! Shared, egui-only edge/arrow rendering for graph-style views.
//!
//! Notebook's canvas and the headway dependency-graph view both draw directed
//! edges between boxes as an "Obsidian-style" cubic bezier that ends in a filled
//! triangular arrowhead touching the target box. This module owns that geometry
//! and drawing, expressed purely in egui terms (rects, [`Side`]s, colors), so it
//! carries no jsoncanvas or notebook data — callers map their own edge model
//! onto [`Side`] and plain [`Rect`]s.

use egui::{epaint::CubicBezierShape, vec2, Color32, Pos2, Rect, Shape, Stroke};
use std::ops::Neg;

/// How close (pixels) the pointer must be to an edge's curve to count as
/// hovering it — a threshold callers can reuse for edge-hover affordances (e.g.
/// revealing a midpoint delete handle). Pair it with [`dist_to_polyline`].
pub const EDGE_HOVER_DIST: f32 = 8.0;
/// Stroke width of an edge's curve, in pixels. Exposed so callers can build the
/// [`Stroke`] they pass to [`draw_edge`] and match any companion lines.
pub const EDGE_STROKE: f32 = 2.0;
/// Length of an edge's arrowhead, tip to base. Shared so the curve can end flush
/// against the arrow's base rather than poking through its tip.
const ARROW_LEN: f32 = 11.0;
/// Width of an edge's arrowhead base.
const ARROW_WIDTH: f32 = 9.0;
/// How hard an edge's curve bows out from its anchors — the tangent handles are
/// pulled this fraction of the anchor-to-anchor distance. ¼-ish feels "Obsidian".
const EDGE_BEND: f32 = 0.28;

/// One of a box's four sides — where an edge anchors. A portable, `Copy`
/// stand-in for a caller's own side type (e.g. jsoncanvas's `Side`), keeping
/// this module free of any edge-model dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Top,
    Left,
    Right,
    Bottom,
}

/// The point at the centre of `rect`'s given side.
pub fn side_point(side: Side, rect: Rect) -> Pos2 {
    match side {
        Side::Top => rect.center_top(),
        Side::Left => rect.left_center(),
        Side::Right => rect.right_center(),
        Side::Bottom => rect.center_bottom(),
    }
}

/// A unit vector pointing outward from the given side.
pub fn side_tangent(side: Side) -> egui::Vec2 {
    match side {
        Side::Top => vec2(0.0, -1.0),
        Side::Bottom => vec2(0.0, 1.0),
        Side::Left => vec2(-1.0, 0.0),
        Side::Right => vec2(1.0, 0.0),
    }
}

/// The cubic-bezier control points of an edge plus where its arrowhead tip
/// touches the target node. Pulled out of [`draw_edge`] so the geometry — in
/// particular that the curve ends on the arrow's base centre, aligned with the
/// arrow axis — can be unit-tested without a live frame.
struct EdgeCurve {
    /// Bezier control points: start, two tangent handles, end.
    points: [Pos2; 4],
    /// Where the arrowhead's tip sits, on the target node's side.
    to_anchor: Pos2,
}

/// Compute an edge's curve from the two node rects and the sides it anchors to.
///
/// The curve ends on the arrow's *base centre* (a hair inside it so no seam
/// shows), not at the box edge: the arrow's tip touches the box at `to_anchor`
/// and its base sits [`ARROW_LEN`] out along the side's outward normal, so ending
/// the curve there makes the line flow straight into the arrow instead of poking
/// out through its tip. The end tangent runs along that same axis for the same
/// reason.
fn edge_curve(from_rect: Rect, from_side: Side, to_rect: Rect, to_side: Side) -> EdgeCurve {
    let p0 = side_point(from_side, from_rect);
    let to_anchor = side_point(to_side, to_rect);
    let p3 = to_anchor + side_tangent(to_side) * (ARROW_LEN - 0.5);

    // How far to pull the tangent handles out from each anchor.
    let d = (p3 - p0).length() * EDGE_BEND;
    let c1 = p0 + side_tangent(from_side) * d;
    let c2 = p3 - side_tangent(to_side).neg() * d;

    EdgeCurve {
        points: [p0, c1, c2, p3],
        to_anchor,
    }
}

/// What [`draw_edge`] leaves behind for a caller's own interaction handling: the
/// curve midpoint (e.g. to anchor a delete handle) and the flattened curve as a
/// polyline (for hover/hit tests via [`dist_to_polyline`]).
pub struct DrawnEdge {
    /// The curve's midpoint, sampled at t = 0.5.
    pub mid: Pos2,
    /// The flattened curve as a polyline.
    pub polyline: Vec<Pos2>,
}

/// Draw a directed edge between two boxes: a cubic-bezier curve from
/// `from_rect`'s `from_side` to `to_rect`'s `to_side`, ending in a filled
/// triangular arrowhead whose tip touches the target box. `color` fills the
/// arrowhead and (for an open curve) is nominal; `stroke` draws the line.
/// Returns the curve's midpoint and flattened polyline for the caller.
pub fn draw_edge(
    painter: &egui::Painter,
    from_rect: Rect,
    from_side: Side,
    to_rect: Rect,
    to_side: Side,
    color: Color32,
    stroke: Stroke,
) -> DrawnEdge {
    let EdgeCurve { points, to_anchor } = edge_curve(from_rect, from_side, to_rect, to_side);
    let bezier = CubicBezierShape::from_points_stroke(points, false, color, stroke);

    // The curve midpoint and flattened polyline, captured before the shape is
    // moved into the painter (used by the caller for a midpoint handle and
    // edge-hover test).
    let mid = bezier.sample(0.5);
    // Explicit tolerance: the default derives from the curve's horizontal span,
    // which is zero for a vertical edge and trips a "tolerance must be positive"
    // assert. Half a pixel is plenty fine for a hover-distance polyline.
    let polyline = bezier.flatten(Some(0.5));
    painter.add(Shape::CubicBezier(bezier));
    draw_arrow(painter, to_side, to_anchor, color);

    DrawnEdge { mid, polyline }
}

/// Shortest distance from `p` to a polyline (a flattened curve), e.g. the
/// [`DrawnEdge::polyline`] returned by [`draw_edge`].
pub fn dist_to_polyline(points: &[Pos2], p: Pos2) -> f32 {
    points
        .windows(2)
        .map(|w| dist_to_segment(p, w[0], w[1]))
        .fold(f32::INFINITY, f32::min)
}

/// Shortest distance from `p` to the line segment `a`–`b`.
fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    let t = if len_sq <= f32::EPSILON {
        0.0
    } else {
        ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0)
    };
    (p - (a + ab * t)).length()
}

/// Draw a filled triangular arrowhead whose tip sits at `point` on `side`.
///
/// * `painter` – the egui `Painter` to draw into
/// * `side`  – which edge of the box we're attaching to
/// * `point` – the exact spot on that edge the arrow's tip should touch
/// * `fill`  – colour to fill the arrow with (usually the edge's colour)
fn draw_arrow(painter: &egui::Painter, side: Side, point: Pos2, fill: Color32) {
    let verts = arrow_verts(side, point);
    painter.add(Shape::convex_polygon(
        verts.to_vec(),
        fill,
        Stroke::new(1.0_f32, fill), // outline; matches the fill so it reads as solid
    ));
}

/// The three vertices of an edge's arrowhead: `verts[0]` is the tip (at `point`,
/// on the node's side), `verts[1]`/`verts[2]` are the base corners — [`ARROW_LEN`]
/// out from the tip along the side's outward normal and [`ARROW_WIDTH`] apart.
/// Their midpoint is the base centre, where the edge's curve should terminate.
fn arrow_verts(side: Side, point: Pos2) -> [Pos2; 3] {
    let len = ARROW_LEN; // distance from tip to base
    let half = ARROW_WIDTH * 0.5; // half the base width
    match side {
        Side::Top => [
            point,                                    // tip
            Pos2::new(point.x - half, point.y - len), // base‑left (above)
            Pos2::new(point.x + half, point.y - len), // base‑right (above)
        ],
        Side::Bottom => [
            point,
            Pos2::new(point.x + half, point.y + len), // below
            Pos2::new(point.x - half, point.y + len),
        ],
        Side::Left => [
            point,
            Pos2::new(point.x - len, point.y + half), // left
            Pos2::new(point.x - len, point.y - half),
        ],
        Side::Right => [
            point,
            Pos2::new(point.x + len, point.y - half), // right
            Pos2::new(point.x + len, point.y + half),
        ],
    }
}

/// Layered (Sugiyama-style) auto-layout for a directed acyclic graph, computing
/// a [`Rect`] per node from the graph's blocking edges alone.
///
/// Notebook's canvas positions every node by hand; a dependency graph has no
/// hand-placed coordinates, so this module derives them. It is pure geometry —
/// plain node indices and `(from, to)` edge pairs in, a `Vec<Rect>` aligned to
/// the input node order out — carrying no headway or jsoncanvas data, so a
/// caller maps its own node model (e.g. `NoteId`s) onto `0..node_count` and
/// reads its rects back by the same index.
///
/// The three classic phases:
/// 1. **Rank** by longest path over the edges ([`rank_nodes`]): roots — nodes
///    nothing in the set points at — sit at rank 0, everything else one past its
///    deepest predecessor.
/// 2. **Order** within each rank by a barycenter heuristic to reduce edge
///    crossings.
/// 3. **Place**: map `(rank, order)` to an `x`/`y` [`Rect`] with a configurable
///    node size and inter-node / inter-rank gaps.
pub mod layout {
    use egui::{Pos2, Rect, Vec2};

    /// Node size and spacing for [`layered_layout`]. Ranks stack down the `y`
    /// axis (roots at the top); nodes within a rank spread along `x`.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct LayoutConfig {
        /// Size of every node's box.
        pub node_size: Vec2,
        /// Horizontal gap between adjacent nodes in the same rank.
        pub node_gap: f32,
        /// Vertical gap between one rank and the next.
        pub rank_gap: f32,
        /// Top-left of the laid-out area; the whole graph is offset by this.
        pub origin: Pos2,
    }

    impl Default for LayoutConfig {
        fn default() -> Self {
            LayoutConfig {
                node_size: Vec2::new(220.0, 96.0),
                node_gap: 32.0,
                rank_gap: 64.0,
                origin: Pos2::ZERO,
            }
        }
    }

    /// How many barycenter ordering sweeps to run. Crossing-reduction converges
    /// fast; a handful of down/up passes is plenty and keeps the layout stable.
    const ORDER_SWEEPS: usize = 4;

    /// Assign every node a rank by longest path over the edges, where an edge
    /// `(a, b)` means "a blocks b" so `a` is upstream of `b`. Roots — nodes with
    /// no in-set predecessor — get rank 0; every other node gets one more than
    /// its deepest predecessor's rank. Returns a rank per node, indexed `0..n`.
    ///
    /// The write path forbids block cycles within a board
    /// (`store::would_block_cycle`), but a cross-board mutual edge could still
    /// slip in, so this guards defensively: an edge that closes a cycle (a
    /// back-edge into a node still being resolved) contributes nothing to the
    /// rank, mirroring the visited-set discipline of the store's DFS traversal.
    /// The layout is therefore always finite and never loops.
    pub fn rank_nodes(node_count: usize, edges: &[(usize, usize)]) -> Vec<u32> {
        let preds = predecessors(node_count, edges);
        let mut rank = vec![None; node_count];
        let mut resolving = vec![false; node_count];
        for v in 0..node_count {
            rank_of(v, &preds, &mut rank, &mut resolving);
        }
        // Every node is resolved to `Some` by the loop above.
        rank.into_iter().map(|r| r.unwrap_or(0)).collect()
    }

    /// Longest-path rank of `v`, memoized into `rank`. `resolving[u]` marks a
    /// node on the current DFS stack; a predecessor that is still resolving is a
    /// back-edge (a cycle) and is skipped so the recursion always terminates.
    fn rank_of(
        v: usize,
        preds: &[Vec<usize>],
        rank: &mut [Option<u32>],
        resolving: &mut [bool],
    ) -> u32 {
        if let Some(r) = rank[v] {
            return r;
        }
        resolving[v] = true;
        let mut best = 0;
        for &u in &preds[v] {
            if resolving[u] {
                continue; // back-edge: ignore so a cycle can't loop
            }
            best = best.max(rank_of(u, preds, rank, resolving) + 1);
        }
        resolving[v] = false;
        rank[v] = Some(best);
        best
    }

    /// Predecessor adjacency: `preds[b]` lists every `a` with an edge `a -> b`.
    fn predecessors(node_count: usize, edges: &[(usize, usize)]) -> Vec<Vec<usize>> {
        let mut preds = vec![Vec::new(); node_count];
        for &(a, b) in edges {
            if a < node_count && b < node_count && a != b {
                preds[b].push(a);
            }
        }
        preds
    }

    /// Lay a directed graph out in layers and return a [`Rect`] per node, indexed
    /// to match the `0..node_count` node ids. `edges` are `(from, to)` pairs where
    /// `from` blocks `to`; out-of-range indices and self-edges are ignored.
    ///
    /// Ranks stack downward (roots on top); within each rank nodes are ordered by
    /// a barycenter heuristic to reduce crossings and then centered horizontally
    /// so narrower ranks sit under the middle of wider ones. Determinism: equal
    /// barycenters preserve input order (stable sort), so the same graph always
    /// yields the same rects.
    pub fn layered_layout(
        node_count: usize,
        edges: &[(usize, usize)],
        cfg: &LayoutConfig,
    ) -> Vec<Rect> {
        if node_count == 0 {
            return Vec::new();
        }

        let ranks = rank_nodes(node_count, edges);
        let preds = predecessors(node_count, edges);
        let succs = successors(node_count, edges);

        // Group node ids by rank, in input order to start.
        let max_rank = ranks.iter().copied().max().unwrap_or(0) as usize;
        let mut rows: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
        for (v, &r) in ranks.iter().enumerate() {
            rows[r as usize].push(v);
        }

        order_rows(&mut rows, &preds, &succs);
        place(&rows, node_count, cfg)
    }

    /// Successor adjacency: `succs[a]` lists every `b` with an edge `a -> b`.
    fn successors(node_count: usize, edges: &[(usize, usize)]) -> Vec<Vec<usize>> {
        let mut succs = vec![Vec::new(); node_count];
        for &(a, b) in edges {
            if a < node_count && b < node_count && a != b {
                succs[a].push(b);
            }
        }
        succs
    }

    /// Reorder each rank's nodes to reduce edge crossings via barycenter sweeps:
    /// down passes order a rank by the mean position of its predecessors in the
    /// rank above, up passes by its successors in the rank below. A node with no
    /// neighbour in the reference rank keeps its slot (stable sort on the current
    /// position), so ordering stays deterministic.
    fn order_rows(rows: &mut [Vec<usize>], preds: &[Vec<usize>], succs: &[Vec<usize>]) {
        let node_count = preds.len();
        for _ in 0..ORDER_SWEEPS {
            for r in 1..rows.len() {
                let pos = positions(rows, node_count);
                sort_by_barycenter(&mut rows[r], preds, &pos);
            }
            for r in (0..rows.len().saturating_sub(1)).rev() {
                let pos = positions(rows, node_count);
                sort_by_barycenter(&mut rows[r], succs, &pos);
            }
        }
    }

    /// Position of each node within its own rank (its index in the row). Nodes in
    /// no row (unreachable in practice) map to 0.
    fn positions(rows: &[Vec<usize>], node_count: usize) -> Vec<f32> {
        let mut pos = vec![0.0; node_count];
        for row in rows {
            for (i, &v) in row.iter().enumerate() {
                pos[v] = i as f32;
            }
        }
        pos
    }

    /// Stably sort a rank by each node's barycenter — the mean position of its
    /// neighbours (`neigh[v]`) in the adjacent rank. Nodes with no neighbour keep
    /// their current relative order.
    fn sort_by_barycenter(row: &mut [usize], neigh: &[Vec<usize>], pos: &[f32]) {
        // Snapshot each node's current slot so a node with no neighbour sorts on
        // where it already is, leaving it put.
        let current: Vec<f32> = (0..row.len()).map(|i| i as f32).collect();
        let key = |v: usize, fallback: f32| -> f32 {
            let ns = &neigh[v];
            if ns.is_empty() {
                fallback
            } else {
                ns.iter().map(|&u| pos[u]).sum::<f32>() / ns.len() as f32
            }
        };
        let mut keyed: Vec<(f32, usize)> = row
            .iter()
            .enumerate()
            .map(|(i, &v)| (key(v, current[i]), v))
            .collect();
        // Stable sort keeps equal barycenters in their prior order → deterministic.
        keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (slot, (_, v)) in keyed.into_iter().enumerate() {
            row[slot] = v;
        }
    }

    /// Turn ranked, ordered rows into a rect per node. Each rank is a horizontal
    /// row; rows are centered against the widest rank so the graph is balanced.
    fn place(rows: &[Vec<usize>], node_count: usize, cfg: &LayoutConfig) -> Vec<Rect> {
        let step_x = cfg.node_size.x + cfg.node_gap;
        let step_y = cfg.node_size.y + cfg.rank_gap;
        let widest = rows.iter().map(|r| r.len()).max().unwrap_or(0);

        // A default rect for any node that somehow lands in no row; it never
        // happens for `0..node_count`, but keeps the returned Vec fully populated.
        let mut rects = vec![Rect::from_min_size(cfg.origin, cfg.node_size); node_count];
        for (r, row) in rows.iter().enumerate() {
            // Center this row under the widest one.
            let offset = (widest - row.len()) as f32 * 0.5 * step_x;
            let y = cfg.origin.y + r as f32 * step_y;
            for (i, &v) in row.iter().enumerate() {
                let x = cfg.origin.x + offset + i as f32 * step_x;
                rects[v] = Rect::from_min_size(Pos2::new(x, y), cfg.node_size);
            }
        }
        rects
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A blocking chain a -> b -> c -> d ranks as four descending layers.
        #[test]
        fn chain_ranks_descend() {
            let ranks = rank_nodes(4, &[(0, 1), (1, 2), (2, 3)]);
            assert_eq!(ranks, vec![0, 1, 2, 3]);
        }

        /// Fork/join: 0 blocks both 1 and 2, which both block 3 (the diamond).
        /// 0 is the sole root (rank 0), 1 and 2 share rank 1, and 3 sits below
        /// both at rank 2 by longest path.
        #[test]
        fn diamond_ranks_by_longest_path() {
            let ranks = rank_nodes(4, &[(0, 1), (0, 2), (1, 3), (2, 3)]);
            assert_eq!(ranks[0], 0);
            assert_eq!(ranks[1], 1);
            assert_eq!(ranks[2], 1);
            assert_eq!(ranks[3], 2);
        }

        /// Longest path, not shortest: 0 -> 3 directly and 0 -> 1 -> 2 -> 3 both
        /// reach 3, and 3 must rank past the *longest* of the two (3), not the
        /// short hop (1).
        #[test]
        fn rank_takes_longest_path() {
            let ranks = rank_nodes(4, &[(0, 3), (0, 1), (1, 2), (2, 3)]);
            assert_eq!(ranks, vec![0, 1, 2, 3]);
        }

        /// A defensive cross-board mutual edge (0 <-> 1) must not loop; ranking
        /// stays finite and every node still gets a rank.
        #[test]
        fn cycle_is_safe() {
            let ranks = rank_nodes(2, &[(0, 1), (1, 0)]);
            assert_eq!(ranks.len(), 2);
            // Whichever way the back-edge is broken, both nodes are ranked and
            // the result is small and finite.
            assert!(ranks.iter().all(|&r| r < 2));
        }

        /// Disconnected nodes are all roots at rank 0.
        #[test]
        fn isolated_nodes_are_roots() {
            let ranks = rank_nodes(3, &[]);
            assert_eq!(ranks, vec![0, 0, 0]);
        }

        /// The chain places each node in its own rank, one full step below the
        /// last, with no two rects overlapping.
        #[test]
        fn chain_layout_stacks_without_overlap() {
            let cfg = LayoutConfig::default();
            let rects = layered_layout(4, &[(0, 1), (1, 2), (2, 3)], &cfg);
            assert_eq!(rects.len(), 4);

            let step_y = cfg.node_size.y + cfg.rank_gap;
            for w in rects.windows(2) {
                assert!(
                    (w[1].min.y - w[0].min.y - step_y).abs() < 0.01,
                    "each rank should sit one step below the previous"
                );
            }
            assert_no_overlap(&rects);
        }

        /// A wider graph still produces strictly non-overlapping boxes, including
        /// the two siblings that share a rank.
        #[test]
        fn diamond_layout_no_overlap() {
            let cfg = LayoutConfig::default();
            let rects = layered_layout(4, &[(0, 1), (0, 2), (1, 3), (2, 3)], &cfg);
            // Siblings 1 and 2 share a rank (same y) but different x.
            assert!((rects[1].min.y - rects[2].min.y).abs() < 0.01);
            assert!((rects[1].min.x - rects[2].min.x).abs() > 0.01);
            assert_no_overlap(&rects);
        }

        /// Same graph, same rects — the barycenter ordering is deterministic.
        #[test]
        fn layout_is_deterministic() {
            let cfg = LayoutConfig::default();
            let edges = [(0, 1), (0, 2), (1, 3), (2, 4), (3, 5), (4, 5)];
            let a = layered_layout(6, &edges, &cfg);
            let b = layered_layout(6, &edges, &cfg);
            assert_eq!(a, b);
        }

        /// An empty graph lays out to nothing.
        #[test]
        fn empty_graph() {
            assert!(layered_layout(0, &[], &LayoutConfig::default()).is_empty());
        }

        /// No pair of rects overlaps (touching edges are allowed).
        fn assert_no_overlap(rects: &[Rect]) {
            for i in 0..rects.len() {
                for j in (i + 1)..rects.len() {
                    let a = rects[i];
                    let b = rects[j];
                    let disjoint = a.max.x <= b.min.x + 0.01
                        || b.max.x <= a.min.x + 0.01
                        || a.max.y <= b.min.y + 0.01
                        || b.max.y <= a.min.y + 0.01;
                    assert!(disjoint, "rects {i} {a:?} and {j} {b:?} overlap");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    /// The arrowhead bug that "looked broken": the curve's end didn't meet the
    /// centre of the arrow's base, so the line poked out past the tip and the
    /// head sat crooked on the line. Guard the geometry the renderer actually
    /// uses — [`edge_curve`] (the line) and [`arrow_verts`] (the triangle) — for
    /// every side an arrow can attach to: the line must terminate on the base
    /// centre and approach it straight along the arrow's axis.
    #[test]
    fn arrowhead_base_centre_lines_up_with_curve() {
        // Source box fixed; target box placed so the arrow side genuinely faces
        // it, mirroring how edges are actually drawn.
        let from_rect = Rect::from_min_size(Pos2::new(0.0, 0.0), vec2(120.0, 80.0));
        let cases = [
            (Side::Right, Pos2::new(400.0, 20.0)),
            (Side::Left, Pos2::new(-400.0, 20.0)),
            (Side::Bottom, Pos2::new(20.0, 400.0)),
            (Side::Top, Pos2::new(20.0, -400.0)),
        ];

        for (to_side, to_min) in cases {
            let to_rect = Rect::from_min_size(to_min, vec2(120.0, 80.0));
            let curve = edge_curve(from_rect, Side::Right, to_rect, to_side);
            let verts = arrow_verts(to_side, curve.to_anchor);

            let base_centre = verts[1] + (verts[2] - verts[1]) * 0.5;
            let line_end = curve.points[3];

            // The line ends on the base centre (within the half-pixel inset that
            // hides the seam) — not short of it and not poking through the tip.
            let gap = (base_centre - line_end).length();
            assert!(
                gap <= 0.75,
                "{to_side:?}: line end {line_end:?} not on arrow base centre \
                 {base_centre:?} (gap {gap})"
            );

            // The line flows straight into the arrow: its incoming direction at
            // the end runs along the arrow's axis (base centre -> tip), so the
            // head reads as a continuation of the line rather than crooked.
            let tip = verts[0];
            let axis = (tip - base_centre).normalized();
            let end_dir = (line_end - curve.points[2]).normalized();
            let dot = axis.dot(end_dir);
            assert!(
                dot > 0.99,
                "{to_side:?}: arrow axis {axis:?} not aligned with curve end \
                 direction {end_dir:?} (dot {dot})"
            );
        }
    }

    /// Render a real edge (its actual bezier line plus arrowhead) through
    /// [`draw_edge`] in a live frame, exercising the full paint path the geometry
    /// test stops short of. Each case places the target on the facing side; the
    /// vertical cases (same x-centre) are deliberate — a vertical edge has zero
    /// horizontal span, which trips the curve-flattening tolerance unless it's
    /// set explicitly. A clean run means the edge draws without panicking and
    /// flattens to a usable polyline.
    #[test]
    fn draw_edge_renders_line_and_arrow() {
        // (from_side, to_side, target offset from the source). Bottom/Top share
        // the source's x-centre, so those edges are exactly vertical.
        let cases = [
            (Side::Right, Side::Left, vec2(400.0, 0.0)),
            (Side::Left, Side::Right, vec2(-400.0, 0.0)),
            (Side::Bottom, Side::Top, vec2(0.0, 400.0)),
            (Side::Top, Side::Bottom, vec2(0.0, -400.0)),
        ];

        for (from_side, to_side, offset) in cases {
            let from_rect = Rect::from_min_size(Pos2::new(0.0, 0.0), vec2(120.0, 80.0));
            let to_rect = Rect::from_min_size(Pos2::new(0.0, 0.0) + offset, vec2(120.0, 80.0));

            let mut harness = Harness::new_ui(|ui| {
                let color = ui.visuals().noninteractive().bg_stroke.color;
                let stroke = Stroke::new(EDGE_STROKE, color);
                let drawn = draw_edge(
                    ui.painter(),
                    from_rect,
                    from_side,
                    to_rect,
                    to_side,
                    color,
                    stroke,
                );
                assert!(
                    !drawn.polyline.is_empty(),
                    "{from_side:?}->{to_side:?}: edge should flatten to a polyline"
                );
            });
            harness.run();
        }
    }
}
