//! Painting the HUD widgets share: a rule that fades out, text with a drop
//! shadow, a diamond, and the easing curve the animations use.
//!
//! None of it allocates beyond the one `String` egui lays a text run out
//! from. Gradients are drawn as a few flat rects rather than a mesh, and a
//! diamond is a rotated rect rather than a polygon: a polygon takes a fresh
//! `Vec` of points every frame, and a mesh built per frame does too. (A mesh
//! built once and kept in an `Arc` would cost nothing at rest, but fading or
//! moving it copies it, and these all fade with their widget.)

use std::fmt::Display;

use egui::epaint::RectShape;
use egui::{Align2, Color32, FontId, Painter, Pos2, Rangef, Rect, Stroke, StrokeKind, vec2};

/// How many steps a fading rule is drawn in.
const FADE_STEPS: usize = 16;

/// Which way a [`fade_rule`] fades.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fade {
    /// Full at the left, gone at the right: a rule under a title.
    Out,
    /// Gone at both ends, full in the middle: a separator.
    Both,
}

/// A one-pixel rule at `y` across `x`, in `color` fading as `fade` says.
pub(super) fn fade_rule(painter: &Painter, x: Rangef, y: f32, color: Color32, fade: Fade) {
    let step = x.span() / FADE_STEPS as f32;
    for i in 0..FADE_STEPS {
        let t = (i as f32 + 0.5) / FADE_STEPS as f32;
        let alpha = match fade {
            Fade::Out => 1.0 - t,
            Fade::Both => 1.0 - (2.0 * t - 1.0).abs(),
        };
        let left = x.min + step * i as f32;
        let rect = Rect::from_x_y_ranges(left..=left + step, y..=y + 1.0);
        painter.rect_filled(rect, 0.0, color.gamma_multiply(alpha));
    }
}

/// `text` anchored at `pos`, over a copy of itself in `shadow` one pixel down
/// and right, so it reads on a fill of any brightness. Returns where the text
/// went.
pub(super) fn shadowed_text(
    painter: &Painter,
    pos: Pos2,
    anchor: Align2,
    text: impl Display,
    font: FontId,
    color: Color32,
    shadow: Color32,
) -> Rect {
    let galley = painter.layout_no_wrap(text.to_string(), font, color);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley_with_override_text_color(rect.min + vec2(1.0, 1.0), galley.clone(), shadow);
    painter.galley(rect.min, galley, color);
    rect
}

/// A square stood on its corner, `radius` from its center to each point.
pub(super) fn diamond(painter: &Painter, center: Pos2, radius: f32, fill: Color32, stroke: Stroke) {
    let side = radius * std::f32::consts::SQRT_2;
    let rect = Rect::from_center_size(center, vec2(side, side));
    let shape = RectShape::new(rect, 0.0, fill, stroke, StrokeKind::Inside)
        .with_angle(std::f32::consts::FRAC_PI_4);
    painter.add(shape);
}

/// Cubic ease-out: fast at first, settling into place.
pub(super) fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}
