//! Toasts: a banner centered at the top of the window for a level-up or an
//! achievement, sliding down and fading in, holding, then fading out.
//!
//! [`Toasts`] is a queue the app keeps in its state and draws every frame;
//! one toast shows at a time. Animation is frame-aware (CLAUDE.md rule 17):
//! it repaints every frame only while a toast is sliding or fading, and
//! during the hold it asks for one repaint when the hold ends, so a toast on
//! screen costs no frames while it sits still.

use std::collections::VecDeque;
use std::time::Duration;

use egui::{Align2, Color32, Frame, Order, Rect, RichText, Stroke, Ui, vec2};

use crate::ui::palette::{HudPalette, Palette};

/// How long a toast takes to slide in.
const ENTER: f64 = 0.3;

/// How long a toast stays still on screen.
const HOLD: f64 = 4.0;

/// How long a toast takes to fade out.
const EXIT: f64 = 0.6;

/// How far above its resting place a toast starts its slide.
const SLIDE: f32 = 24.0;

/// The gap between the top of the window and a resting toast.
const TOP_MARGIN: f32 = 24.0;

/// What a toast announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// A skill went up a level.
    LevelUp,
    /// An achievement was earned, worth `points`.
    Achievement {
        /// Achievement points it is worth.
        points: u32,
    },
}

/// One toast: its kind and its text, formatted once when it is made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// What it announces.
    pub kind: ToastKind,
    /// The gold first line.
    pub title: String,
    /// The line under it.
    pub body: String,
    /// The achievement points badge (`+10`), empty for a level-up.
    badge: String,
}

impl Toast {
    /// "You've advanced a `skill` level! You are now level `level`."
    pub fn level_up(skill: &str, level: u8) -> Self {
        Self {
            kind: ToastKind::LevelUp,
            title: "Level up!".to_owned(),
            body: format!("You've advanced a {skill} level! You are now level {level}."),
            badge: String::new(),
        }
    }

    /// "Achievement earned: `name`", with a badge for its `points`.
    pub fn achievement(name: &str, points: u32) -> Self {
        Self {
            kind: ToastKind::Achievement { points },
            title: format!("Achievement earned: {name}"),
            body: format!("{points} achievement points"),
            badge: format!("+{points}"),
        }
    }
}

/// Where a toast is in its animation at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToastPhase {
    /// Opacity, `0..=1`.
    pub alpha: f32,
    /// How far above its resting place it is drawn.
    pub lift: f32,
    /// When the next repaint is needed: zero while it moves, the rest of the
    /// hold while it sits still.
    pub repaint_after: Duration,
}

impl ToastPhase {
    /// The phase `elapsed` seconds after a toast first showed, or `None` once
    /// it has finished.
    pub fn at(elapsed: f64) -> Option<Self> {
        let elapsed = elapsed.max(0.0);
        if elapsed < ENTER {
            let t = ease_out((elapsed / ENTER) as f32);
            return Some(Self {
                alpha: t,
                lift: SLIDE * (1.0 - t),
                repaint_after: Duration::ZERO,
            });
        }
        if elapsed < ENTER + HOLD {
            return Some(Self {
                alpha: 1.0,
                lift: 0.0,
                repaint_after: Duration::from_secs_f64(ENTER + HOLD - elapsed),
            });
        }
        if elapsed < ENTER + HOLD + EXIT {
            let t = ((elapsed - ENTER - HOLD) / EXIT) as f32;
            return Some(Self {
                alpha: 1.0 - t,
                lift: 0.0,
                repaint_after: Duration::ZERO,
            });
        }
        None
    }
}

/// Cubic ease-out: fast at first, settling into place.
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// The queue of toasts waiting to show, oldest first, and when the front one
/// started showing.
#[derive(Debug, Default)]
pub struct Toasts {
    queue: VecDeque<Toast>,
    /// The time (egui's `input.time`) the front toast first showed, or `None`
    /// if it has not shown yet.
    shown_since: Option<f64>,
}

impl Toasts {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues `toast` behind any already waiting.
    pub fn push(&mut self, toast: Toast) {
        self.queue.push_back(toast);
    }

    /// The toast showing (or about to show), if any.
    pub fn current(&self) -> Option<&Toast> {
        self.queue.front()
    }

    /// Draws the front toast, if any, at the clock time of `ui`'s input.
    /// Returns where it was drawn.
    pub fn show(&mut self, ui: &Ui, palette: &Palette) -> Option<Rect> {
        let now = ui.input(|i| i.time);
        self.show_at(ui, palette, now)
    }

    /// [`Self::show`] at the clock time `now`, in seconds: retires a toast
    /// whose time is up, starts the next, and schedules the next repaint.
    #[profiling::function]
    pub fn show_at(&mut self, ui: &Ui, palette: &Palette, now: f64) -> Option<Rect> {
        loop {
            self.queue.front()?;
            let since = *self.shown_since.get_or_insert(now);
            if let Some(phase) = ToastPhase::at(now - since) {
                ui.ctx().request_repaint_after(phase.repaint_after);
                let toast = self.queue.front()?;
                return Some(paint(ui, &palette.hud, toast, phase));
            }
            self.queue.pop_front();
            self.shown_since = None;
        }
    }
}

/// Draws `toast` at `phase`, centered at the top of the window.
fn paint(ui: &Ui, hud: &HudPalette, toast: &Toast, phase: ToastPhase) -> Rect {
    let frame = Frame::new()
        .fill(hud.panel)
        .stroke(Stroke::new(
            notedeck::tokens::STROKE_THIN,
            hud.title.gamma_multiply(0.7),
        ))
        .corner_radius(notedeck::tokens::RADIUS_MD)
        .inner_margin(egui::Margin::symmetric(18, 10))
        .shadow(egui::Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(120),
        });

    egui::Area::new(egui::Id::unique("cashflow_hud_toast"))
        .order(Order::Foreground)
        .interactable(false)
        .anchor(Align2::CENTER_TOP, vec2(0.0, TOP_MARGIN - phase.lift))
        .show(ui.ctx(), |ui| {
            ui.multiply_opacity(phase.alpha);
            frame.show(ui, |ui| match toast.kind {
                ToastKind::LevelUp => text_lines(ui, hud, toast),
                ToastKind::Achievement { .. } => {
                    ui.horizontal(|ui| {
                        badge(ui, hud, &toast.badge);
                        ui.add_space(6.0);
                        ui.vertical(|ui| text_lines(ui, hud, toast));
                    });
                }
            });
        })
        .response
        .rect
}

/// The toast's gold title over its body.
fn text_lines(ui: &mut Ui, hud: &HudPalette, toast: &Toast) {
    ui.spacing_mut().item_spacing.y = 2.0;
    let title = RichText::new(toast.title.as_str())
        .strong()
        .size(17.0)
        .color(hud.title);
    ui.label(title);
    ui.label(RichText::new(toast.body.as_str()).color(hud.text));
}

/// A round points badge, like a medal.
fn badge(ui: &mut Ui, hud: &HudPalette, text: &str) {
    let diameter = 38.0;
    let (rect, _) = ui.allocate_exact_size(vec2(diameter, diameter), egui::Sense::hover());
    let painter = ui.painter();
    painter.circle(
        rect.center(),
        diameter / 2.0,
        hud.legendary.gamma_multiply(0.25),
        Stroke::new(notedeck::tokens::STROKE_MEDIUM, hud.legendary),
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(14.0),
        hud.legendary,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toast_slides_in_holds_then_fades_out() {
        let start = ToastPhase::at(0.0).unwrap();
        assert_eq!(start.alpha, 0.0);
        assert_eq!(start.lift, SLIDE);
        assert_eq!(start.repaint_after, Duration::ZERO);

        let holding = ToastPhase::at(ENTER + 1.0).unwrap();
        assert_eq!(holding.alpha, 1.0);
        assert_eq!(holding.lift, 0.0);

        let fading = ToastPhase::at(ENTER + HOLD + EXIT / 2.0).unwrap();
        assert!((fading.alpha - 0.5).abs() < 1e-4);
        assert_eq!(fading.repaint_after, Duration::ZERO);

        assert_eq!(ToastPhase::at(ENTER + HOLD + EXIT), None);
    }

    /// While a toast sits still it asks for exactly one repaint, when its
    /// hold ends, instead of one every frame.
    #[test]
    fn a_holding_toast_sleeps_until_its_hold_ends() {
        let holding = ToastPhase::at(ENTER + 1.0).unwrap();
        let expected = Duration::from_secs_f64(HOLD - 1.0);
        assert!(holding.repaint_after.abs_diff(expected) < Duration::from_micros(1));
    }

    #[test]
    fn level_up_text_names_the_skill_and_level() {
        let toast = Toast::level_up("Saving", 42);
        assert_eq!(
            toast.body,
            "You've advanced a Saving level! You are now level 42."
        );
        let toast = Toast::achievement("Inbox zero", 10);
        assert_eq!(toast.title, "Achievement earned: Inbox zero");
        assert_eq!(toast.badge, "+10");
    }
}
