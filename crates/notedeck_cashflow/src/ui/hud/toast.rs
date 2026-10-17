//! Toasts: a gilded banner centered at the top of the window for a level-up
//! or an achievement, sliding down and fading in, holding, then fading out.
//! A medallion on its left carries the new level (or a diamond badge the
//! achievement's points), beside a small spaced gold caption and the news.
//!
//! [`Toasts`] is a queue the app keeps in its state and draws every frame;
//! one toast shows at a time. A level-up bursts around its medallion as it
//! lands ([`super::burst`]). Animation is frame-aware (CLAUDE.md rule 17):
//! it repaints every frame only while a toast is sliding, bursting or
//! fading, and for the rest of the hold it asks for one repaint when the
//! hold ends, so a toast on screen costs no frames while it sits still.

use std::collections::VecDeque;
use std::time::Duration;

use egui::{
    Align2, FontId, Frame, Order, Painter, Pos2, Rect, RichText, Stroke, StrokeKind, Ui, vec2,
};

use super::burst::{BurstPhase, paint_burst};
use super::paint::{diamond, ease_out, shadowed_text};
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

/// The medallion's radius.
const MEDALLION: f32 = 22.0;

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
    /// The small spaced gold caption: what kind of news it is.
    pub title: String,
    /// The news itself, large, under the caption.
    pub body: String,
    /// What the medallion says: the new level, or the points (`+10`).
    badge: String,
}

impl Toast {
    /// "LEVEL UP — `skill` is now level `level`", with `level` on the
    /// medallion.
    pub fn level_up(skill: &str, level: u8) -> Self {
        Self {
            kind: ToastKind::LevelUp,
            title: "LEVEL UP".to_owned(),
            body: format!("{skill} is now level {level}"),
            badge: level.to_string(),
        }
    }

    /// "ACHIEVEMENT EARNED — `name`", with its `points` on the badge.
    pub fn achievement(name: &str, points: u32) -> Self {
        Self {
            kind: ToastKind::Achievement { points },
            title: "ACHIEVEMENT EARNED".to_owned(),
            body: name.to_owned(),
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
            let elapsed = now - since;
            if let Some(phase) = ToastPhase::at(elapsed) {
                let toast = self.queue.front()?;
                let burst = burst_at(toast.kind, elapsed);
                ui.ctx().request_repaint_after(repaint_after(phase, burst));
                return Some(paint(ui, palette, toast, phase, burst));
            }
            self.queue.pop_front();
            self.shown_since = None;
        }
    }
}

/// The burst around `kind`'s medallion `elapsed` seconds into the toast:
/// a level-up's starts as it lands, at the end of the slide. An achievement
/// doesn't burst.
fn burst_at(kind: ToastKind, elapsed: f64) -> Option<BurstPhase> {
    match kind {
        ToastKind::LevelUp if elapsed >= ENTER => BurstPhase::at(elapsed - ENTER),
        ToastKind::LevelUp | ToastKind::Achievement { .. } => None,
    }
}

/// When the next repaint is needed for a toast at `phase` with `burst`
/// running: the sooner of the two.
fn repaint_after(phase: ToastPhase, burst: Option<BurstPhase>) -> Duration {
    burst.map_or(phase.repaint_after, |burst| {
        phase.repaint_after.min(burst.repaint_after)
    })
}

/// Draws `toast` at `phase`, with `burst` around its medallion, centered at
/// the top of the window.
fn paint(
    ui: &Ui,
    palette: &Palette,
    toast: &Toast,
    phase: ToastPhase,
    burst: Option<BurstPhase>,
) -> Rect {
    let hud = &palette.hud;
    let frame = Frame::new()
        .fill(hud.panel)
        .stroke(Stroke::new(
            notedeck::tokens::STROKE_MEDIUM,
            hud.title.gamma_multiply(0.85),
        ))
        .corner_radius(3)
        .inner_margin(egui::Margin {
            left: 12,
            right: 22,
            top: 10,
            bottom: 10,
        })
        .shadow(egui::Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: hud.shade,
        });

    egui::Area::new(egui::Id::unique("cashflow_hud_toast"))
        .order(Order::Foreground)
        .interactable(false)
        .anchor(Align2::CENTER_TOP, vec2(0.0, TOP_MARGIN - phase.lift))
        .show(ui.ctx(), |ui| {
            ui.multiply_opacity(phase.alpha);
            let shown = frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    medallion(ui, palette, toast, burst);
                    ui.add_space(12.0);
                    ui.vertical(|ui| text_lines(ui, palette, toast));
                });
            });
            ornaments(ui.painter(), hud, shown.response.rect);
        })
        .response
        .rect
}

/// The spaced gold caption over the news.
fn text_lines(ui: &mut Ui, palette: &Palette, toast: &Toast) {
    let hud = &palette.hud;
    ui.spacing_mut().item_spacing.y = 2.0;
    let title = RichText::new(toast.title.as_str())
        .font(FontId::new(12.0, palette.faces.bold.clone()))
        .extra_letter_spacing(1.6)
        .color(hud.title);
    ui.label(title);
    let body = RichText::new(toast.body.as_str())
        .font(FontId::new(16.0, palette.faces.medium.clone()))
        .color(hud.text);
    ui.label(body);
}

/// The medallion: a gold ring with the new level for a level-up, a
/// legendary diamond with the points for an achievement. `burst`, if any,
/// is drawn behind it.
fn medallion(ui: &mut Ui, palette: &Palette, toast: &Toast, burst: Option<BurstPhase>) {
    let hud = &palette.hud;
    let size = vec2(MEDALLION * 2.0, MEDALLION * 2.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    if let Some(burst) = burst {
        paint_burst(painter, hud, center, MEDALLION, burst);
    }
    let (color, font_size) = match toast.kind {
        ToastKind::LevelUp => {
            painter.circle(
                center,
                MEDALLION,
                hud.shade,
                Stroke::new(notedeck::tokens::STROKE_THICK, hud.title),
            );
            painter.circle_stroke(
                center,
                MEDALLION - 4.5,
                Stroke::new(1.0, hud.title.gamma_multiply(0.45)),
            );
            (hud.title, 18.0)
        }
        ToastKind::Achievement { .. } => {
            let stroke = Stroke::new(notedeck::tokens::STROKE_THICK, hud.legendary);
            diamond(painter, center, MEDALLION + 2.0, hud.shade, stroke);
            let inner = Stroke::new(1.0, hud.legendary.gamma_multiply(0.45));
            diamond(
                painter,
                center,
                MEDALLION - 3.0,
                egui::Color32::TRANSPARENT,
                inner,
            );
            (hud.legendary, 14.0)
        }
    };
    shadowed_text(
        painter,
        center,
        Align2::CENTER_CENTER,
        toast.badge.as_str(),
        FontId::new(font_size, palette.faces.bold.clone()),
        color,
        hud.shade,
    );
}

/// The banner's inner gilt rule and the diamond finials on its ends.
fn ornaments(painter: &Painter, hud: &HudPalette, rect: Rect) {
    painter.rect_stroke(
        rect.shrink(4.0),
        1.0,
        Stroke::new(1.0, hud.border.gamma_multiply(0.55)),
        StrokeKind::Inside,
    );
    let finial = |at: Pos2| diamond(painter, at, 5.0, hud.title, Stroke::NONE);
    finial(rect.left_center());
    finial(rect.right_center());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::hud::BURST;

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

    /// A level-up bursts once it has landed, repainting every frame while
    /// it does, then sleeps until the hold ends as any toast does. An
    /// achievement never bursts.
    #[test]
    fn a_level_up_bursts_as_it_lands_then_sleeps() {
        assert_eq!(burst_at(ToastKind::LevelUp, ENTER / 2.0), None);

        let landed = ENTER + 0.1;
        let burst = burst_at(ToastKind::LevelUp, landed);
        assert!(burst.is_some());
        let phase = ToastPhase::at(landed).unwrap();
        assert_eq!(repaint_after(phase, burst), Duration::ZERO);

        let settled = ENTER + BURST + 0.1;
        assert!(settled < ENTER + HOLD);
        let burst = burst_at(ToastKind::LevelUp, settled);
        assert_eq!(burst, None);
        let phase = ToastPhase::at(settled).unwrap();
        let expected = Duration::from_secs_f64(ENTER + HOLD - settled);
        assert!(repaint_after(phase, burst).abs_diff(expected) < Duration::from_micros(1));

        let achievement = ToastKind::Achievement { points: 10 };
        assert_eq!(burst_at(achievement, landed), None);
    }

    #[test]
    fn level_up_text_names_the_skill_and_level() {
        let toast = Toast::level_up("Saving", 42);
        assert_eq!(toast.body, "Saving is now level 42");
        assert_eq!(toast.badge, "42");
        let toast = Toast::achievement("Inbox zero", 10);
        assert_eq!(toast.title, "ACHIEVEMENT EARNED");
        assert_eq!(toast.body, "Inbox zero");
        assert_eq!(toast.badge, "+10");
    }
}
