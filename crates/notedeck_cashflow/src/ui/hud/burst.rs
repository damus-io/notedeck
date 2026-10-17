//! The level-up burst: a flash of gold around a medallion as it lands, two
//! rings spreading out, short rays turning slowly, and sparks flying off and
//! fading, all gone in under a second.
//!
//! Everything is a pure function of how long the burst has run, so a test
//! can pin any moment of it: [`BurstPhase::at`] takes the elapsed time from
//! the caller's clock, and each spark's angle, speed and size come from a
//! stateless hash of its index (the way microverse's effects seed their
//! pieces), not from a random generator with state. Nothing is kept between
//! frames and nothing allocates: the burst is circles, line segments and one
//! blurred rect.
//!
//! It is frame-aware (CLAUDE.md rule 17): [`BurstPhase::repaint_after`] is
//! zero only while it runs, and [`BurstPhase::at`] returns `None` once it is
//! over, so a caller goes back to sleeping.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use egui::epaint::RectShape;
use egui::{Painter, Pos2, Rect, Stroke, Vec2};

use super::paint::ease_out;
use crate::ui::palette::HudPalette;

/// How long a burst lasts, in seconds.
pub const BURST: f64 = 0.9;

/// How many rings spread out.
const RINGS: usize = 2;

/// How long after one ring the next starts, in seconds.
const RING_STAGGER: f32 = 0.12;

/// How far past the medallion's edge a ring spreads before it is gone.
const RING_SPREAD: f32 = 30.0;

/// How many rays: long and short, taking turns.
const RAYS: usize = 12;

/// The length of a long ray at its longest.
const RAY_LENGTH: f32 = 12.0;

/// How far the rays turn over the whole burst, in radians.
const RAY_TURN: f32 = 0.35;

/// How many sparks fly off.
const SPARKS: usize = 18;

/// How far past the medallion's edge the fastest spark flies.
const SPARK_TRAVEL: f32 = 44.0;

/// How quickly the glow comes up, in seconds.
const GLOW_RISE: f32 = 0.08;

/// How soft the glow's edge is, in points.
const GLOW_BLUR: f32 = 14.0;

/// Where a burst is at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BurstPhase {
    /// Seconds since the burst began, `0..BURST`.
    pub elapsed: f32,
    /// When the next repaint is needed: always zero, since a burst moves
    /// every frame until it is over.
    pub repaint_after: Duration,
}

impl BurstPhase {
    /// The phase `elapsed` seconds after the burst began (a negative time
    /// counts as its start), or `None` once it is over.
    pub fn at(elapsed: f64) -> Option<Self> {
        let elapsed = elapsed.max(0.0);
        if elapsed >= BURST {
            return None;
        }
        Some(Self {
            elapsed: elapsed as f32,
            repaint_after: Duration::ZERO,
        })
    }

    /// How far through the burst it is, `0..1`.
    fn progress(self) -> f32 {
        self.elapsed / BURST as f32
    }
}

/// One ring at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ring {
    /// Its radius past the medallion's edge.
    spread: f32,
    /// Its opacity, `0..=1`.
    alpha: f32,
    /// Its stroke width.
    width: f32,
}

/// Ring `index` at `phase`, or `None` before it starts or after it fades.
/// Each ring lives long enough that the last one ends with the burst.
fn ring(index: usize, phase: BurstPhase) -> Option<Ring> {
    let life = BURST as f32 - RING_STAGGER * (RINGS - 1) as f32;
    let local = phase.elapsed - RING_STAGGER * index as f32;
    if !(0.0..life).contains(&local) {
        return None;
    }
    let t = local / life;
    Some(Ring {
        spread: ease_out(t) * RING_SPREAD,
        alpha: (1.0 - t).powi(2),
        width: 2.5 * (1.0 - t) + 0.5,
    })
}

/// One spark at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Spark {
    /// Where it is, from the medallion's center.
    offset: Vec2,
    /// Its radius.
    radius: f32,
    /// Its opacity, `0..=1`.
    alpha: f32,
}

/// Spark `index` of [`SPARKS`] at `phase`, `base` from the center at the
/// start. Its angle, speed and size are fixed by its index alone.
fn spark(index: usize, base: f32, phase: BurstPhase) -> Spark {
    let p = phase.progress();
    let jitter = unit(hash(index as u32, 0));
    let speed = 0.55 + 0.45 * unit(hash(index as u32, 1));
    let angle = (index as f32 + 0.8 * jitter) * TAU / SPARKS as f32;
    let distance = base + SPARK_TRAVEL * speed * ease_out(p);
    Spark {
        offset: Vec2::angled(angle) * distance,
        radius: (1.0 + 1.6 * jitter) * (1.0 - 0.5 * p),
        alpha: (1.0 - p).powf(1.5),
    }
}

/// Every bit of `x` stirred into every other (microverse's `random::mix`).
fn mix(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^ (x >> 16)
}

/// Bits for the `index`th spark's `label`th property: the same pair always
/// gives the same bits.
fn hash(index: u32, label: u32) -> u32 {
    mix(index.wrapping_mul(0x9E37_79B9) ^ label.wrapping_mul(0x85EB_CA6B))
}

/// `bits` as a number in `[0, 1)`.
fn unit(bits: u32) -> f32 {
    (bits >> 8) as f32 / (1u32 << 24) as f32
}

/// The glow's opacity at `phase`: up fast, then fading with the burst.
fn glow_alpha(phase: BurstPhase) -> f32 {
    let rise = (phase.elapsed / GLOW_RISE).min(1.0);
    rise * (1.0 - phase.progress()).powi(2) * 0.7
}

/// Draws the burst at `phase` around a medallion of radius `base` at
/// `center`, in the palette's gold. Draw it before the medallion, so the
/// medallion and its number sit on top.
#[profiling::function]
pub fn paint_burst(
    painter: &Painter,
    hud: &HudPalette,
    center: Pos2,
    base: f32,
    phase: BurstPhase,
) {
    let p = phase.progress();
    let gold = hud.title;

    let glow = base + 6.0 + 6.0 * ease_out(p);
    let glow_rect = Rect::from_center_size(center, Vec2::splat(glow * 2.0));
    let corner = glow.round().min(u8::MAX as f32) as u8;
    painter.add(
        RectShape::filled(glow_rect, corner, gold.gamma_multiply(glow_alpha(phase)))
            .with_blur_width(GLOW_BLUR),
    );

    for index in 0..RINGS {
        let Some(ring) = ring(index, phase) else {
            continue;
        };
        let stroke = Stroke::new(ring.width, gold.gamma_multiply(ring.alpha));
        painter.circle_stroke(center, base + ring.spread, stroke);
    }

    let ray_alpha = 1.0 - p;
    let ray_reach = (PI * p).sin();
    let ray_start = base + 3.0 + 8.0 * ease_out(p);
    for index in 0..RAYS {
        let long = index % 2 == 0;
        let angle = index as f32 * TAU / RAYS as f32 + RAY_TURN * p;
        let dir = Vec2::angled(angle);
        let length = RAY_LENGTH * if long { 1.0 } else { 0.55 } * ray_reach;
        let from = center + dir * ray_start;
        let to = center + dir * (ray_start + length);
        let width = if long { 2.0 } else { 1.5 };
        painter.line_segment(
            [from, to],
            Stroke::new(width, gold.gamma_multiply(ray_alpha)),
        );
    }

    for index in 0..SPARKS {
        let spark = spark(index, base, phase);
        let color = if index % 2 == 0 { gold } else { hud.legendary };
        painter.circle_filled(
            center + spark.offset,
            spark.radius,
            color.gamma_multiply(spark.alpha),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A burst moves every frame while it runs and is over after [`BURST`].
    #[test]
    fn a_burst_repaints_while_it_runs_then_ends() {
        for elapsed in [0.0, 0.3, BURST - 0.01] {
            let phase = BurstPhase::at(elapsed).expect("still running");
            assert_eq!(phase.repaint_after, Duration::ZERO);
        }
        assert_eq!(BurstPhase::at(BURST), None);
        assert_eq!(BurstPhase::at(BURST + 5.0), None);
        assert_eq!(BurstPhase::at(-1.0), BurstPhase::at(0.0));
    }

    /// Every opacity stays in `0..=1` across the whole burst, and the last
    /// ring is gone by its end.
    #[test]
    fn opacities_stay_in_range() {
        for step in 0..90 {
            let phase = BurstPhase::at(step as f64 / 100.0).expect("running");
            assert!((0.0..=1.0).contains(&glow_alpha(phase)));
            for index in 0..RINGS {
                if let Some(ring) = ring(index, phase) {
                    assert!((0.0..=1.0).contains(&ring.alpha), "{ring:?}");
                }
            }
            for index in 0..SPARKS {
                let spark = spark(index, 22.0, phase);
                assert!((0.0..=1.0).contains(&spark.alpha), "{spark:?}");
            }
        }
        let last = BurstPhase::at(BURST - 1e-4).expect("running");
        assert!(ring(RINGS - 1, last).is_none_or(|r| r.alpha < 1e-3));
    }

    /// A spark is the same for the same index and moment, sparks differ from
    /// each other, and each flies outward.
    #[test]
    fn sparks_are_seeded_by_index() {
        let early = BurstPhase::at(0.1).unwrap();
        let late = BurstPhase::at(0.6).unwrap();
        assert_eq!(spark(3, 22.0, early), spark(3, 22.0, early));
        assert_ne!(spark(3, 22.0, early), spark(4, 22.0, early));
        for index in 0..SPARKS {
            let from = spark(index, 22.0, early).offset.length();
            let to = spark(index, 22.0, late).offset.length();
            assert!(to > from, "spark {index} flies outward");
        }
    }
}
