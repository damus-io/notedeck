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
