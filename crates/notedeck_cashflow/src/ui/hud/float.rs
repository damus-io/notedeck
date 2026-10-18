//! Floating XP: "+35 Bookkeeping XP" rising from where it was earned and
//! fading out, the way a game shows a reward as it lands.
//!
//! [`FloatingXp`] is a short queue the view keeps in its state. Each text
//! starts the first frame it is drawn, rises [`RISE`] points over
//! [`LIFETIME`] seconds, holds its color for the first half and fades out
//! over the second, then is dropped. Several land stacked one line apart.
//! Animation is frame-aware (CLAUDE.md rule 17): it asks for a frame
//! [`FRAME`] later only while a text is still on screen, so once the last
//! one has faded the view costs no frames.

use std::collections::VecDeque;
use std::time::Duration;

use egui::{Align2, FontId, Id, LayerId, Order, Pos2, Ui, vec2};

use super::paint::{ease_out, shadowed_text};
use crate::ui::palette::Palette;

/// How long a text floats, from appearing to gone.
pub const LIFETIME: f64 = 1.4;

/// How far a text rises over its life.
pub const RISE: f32 = 28.0;

/// How soon the next frame is asked for while a text floats: 60 a second.
/// Not an immediate repaint, so a test harness's `run` settles after a
/// click that floats a text rather than spinning through its whole life.
pub const FRAME: Duration = Duration::from_micros(16_667);

/// The part of its life a text holds its full color before fading.
const HOLD: f64 = 0.5;

/// The text's size.
const SIZE: f32 = 15.0;

/// How far apart texts floating at once are stacked.
const LINE: f32 = 18.0;

/// Where a floating text is at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloatPhase {
    /// Opacity, `0..=1`.
    pub alpha: f32,
    /// How far above where it started it is.
    pub rise: f32,
}

impl FloatPhase {
    /// The phase `elapsed` seconds after a text first showed, or `None`
    /// once it has faded out.
    pub fn at(elapsed: f64) -> Option<Self> {
        let elapsed = elapsed.max(0.0);
        if elapsed >= LIFETIME {
            return None;
        }
        let t = elapsed / LIFETIME;
        let alpha = if t < HOLD {
            1.0
        } else {
            (1.0 - (t - HOLD) / (1.0 - HOLD)) as f32
        };
        Some(Self {
            alpha,
            rise: RISE * ease_out(t as f32),
        })
    }
}

/// One text floating, and when it first showed.
#[derive(Debug)]
struct Floater {
    text: String,
    /// The time (egui's `input.time`) it first showed, or `None` before it
    /// has.
    since: Option<f64>,
}

/// The texts floating, oldest first.
#[derive(Debug, Default)]
pub struct FloatingXp {
    floaters: VecDeque<Floater>,
}

impl FloatingXp {
    /// Float `text` from the next [`Self::show`].
    pub fn push(&mut self, text: String) {
        self.floaters.push_back(Floater { text, since: None });
    }

    /// Whether nothing is floating.
    pub fn is_empty(&self) -> bool {
        self.floaters.is_empty()
    }

    /// The texts floating, oldest first.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.floaters.iter().map(|f| f.text.as_str())
    }

    /// Draw every text still floating, rising from `from` (the bottom
    /// left of the first), at the clock time of `ui`'s input.
    pub fn show(&mut self, ui: &Ui, palette: &Palette, from: Pos2) {
        let now = ui.input(|i| i.time);
        self.show_at(ui, palette, from, now);
    }

    /// [`Self::show`] at the clock time `now`, in seconds: drops the texts
    /// that have faded, and asks for the next frame, [`FRAME`] on, only
    /// while one hasn't.
    #[profiling::function]
    pub fn show_at(&mut self, ui: &Ui, palette: &Palette, from: Pos2, now: f64) {
        self.floaters
            .retain(|f| f.since.is_none_or(|since| now - since < LIFETIME));
        if self.floaters.is_empty() {
            return;
        }
        let painter = ui.ctx().layer_painter(LayerId::new(
            Order::Foreground,
            Id::unique("cashflow_hud_floating_xp"),
        ));
        let hud = &palette.hud;
        for (i, floater) in self.floaters.iter_mut().enumerate() {
            let since = *floater.since.get_or_insert(now);
            let Some(phase) = FloatPhase::at(now - since) else {
                continue;
            };
            let pos = from - vec2(0.0, phase.rise + i as f32 * LINE);
            shadowed_text(
                &painter,
                pos,
                Align2::LEFT_BOTTOM,
                &floater.text,
                FontId::proportional(SIZE),
                hud.title.gamma_multiply(phase.alpha),
                hud.shade.gamma_multiply(phase.alpha),
            );
        }
        ui.ctx().request_repaint_after(FRAME);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A text rises and fades over its life, and is gone after it, so
    /// nothing asks for frames once it has faded.
    #[test]
    fn rises_fades_and_ends() {
        let start = FloatPhase::at(0.0).unwrap();
        assert_eq!(start.alpha, 1.0);
        assert_eq!(start.rise, 0.0);
        let late = FloatPhase::at(LIFETIME * 0.9).unwrap();
        assert!(late.alpha < 0.3 && late.rise > RISE * 0.9);
        assert_eq!(FloatPhase::at(LIFETIME), None);
        assert_eq!(FloatPhase::at(LIFETIME * 3.0), None);
    }
}
