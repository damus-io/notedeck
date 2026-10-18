//! Toasts: a gilded banner centered at the top of the window for a level-up
//! or an achievement, sliding down and fading in, holding, then fading out.
//! A medallion on its left carries the new level (or a diamond badge the
//! achievement's points), beside a small spaced gold caption and the news.
//!
//! [`Toasts`] is a queue the app keeps in its state and draws every frame;
//! one toast shows at a time. A level-up bursts around its medallion as it
//! lands ([`super::burst`]). Given the host's 3D renderer, a level-up's
//! medallion is a gilt coin ([`super::coin`]) that flips as the toast slides
//! in and settles face-on as the burst ends; without one it is drawn flat.
//! Animation is frame-aware (CLAUDE.md rule 17): it repaints every frame
//! only while a toast is sliding, bursting or fading, and for the rest of
//! the hold it asks for one repaint when the hold ends, so a toast on screen
//! costs no frames while it sits still.

use std::collections::VecDeque;
use std::time::Duration;

use egui::{
    Align2, FontId, Frame, Order, Painter, Pos2, Rect, RichText, Stroke, StrokeKind, TextureId, Ui,
    vec2,
};
use notedeck::Renderer3d;
use renderbud::PixelSize;

use super::BURST;
use super::burst::{BurstPhase, paint_burst};
use super::coin::Coin;
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

/// The side of the 3D coin's texture, in points. The view's fitted camera
/// frames the coin's bounding box with room to spare, so the texture is
/// this much wider than the coin for the coin to fill the medallion.
const COIN_SIDE: f32 = MEDALLION * 2.0 * 1.7;

/// How many texture pixels the coin is rendered at per screen pixel. Its
/// offscreen view has no multisampling, so it is drawn larger and egui's
/// linear sampling smooths the silhouette as it scales it down.
const COIN_SUPERSAMPLE: f32 = 2.0;

/// How long the coin spins: through the slide and the burst. Those already
/// repaint every frame, so the spin costs no repaints of its own.
const SPIN: f64 = ENTER + BURST;

/// How many times the coin turns over while it spins.
const SPIN_TURNS: f32 = 2.0;

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

    /// "ACHIEVEMENTS UNLOCKED — You unlocked `count` achievements from
    /// your history!": the Adventure's first opening, summing up what the
    /// books had already earned rather than toasting each. `points` is
    /// their total.
    pub fn history(count: usize, points: u32) -> Self {
        let noun = if count == 1 {
            "achievement"
        } else {
            "achievements"
        };
        Self {
            kind: ToastKind::Achievement { points },
            title: "ACHIEVEMENTS UNLOCKED".to_owned(),
            body: format!("You unlocked {count} {noun} from your history!"),
            badge: format!("+{points}"),
        }
    }

    /// "ACHIEVEMENTS UNLOCKED — You unlocked `count` achievements": several
    /// earned at once, in one toast rather than a queue of them.
    pub fn achievements(count: usize, points: u32) -> Self {
        Self {
            kind: ToastKind::Achievement { points },
            title: "ACHIEVEMENTS UNLOCKED".to_owned(),
            body: format!("You unlocked {count} achievements"),
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

/// The queue of toasts waiting to show, oldest first, when the front one
/// started showing, and the level-up coin, built the first time a level-up
/// shows with a renderer and kept for the next.
#[derive(Debug, Default)]
pub struct Toasts {
    queue: VecDeque<Toast>,
    /// The time (egui's `input.time`) the front toast first showed, or `None`
    /// if it has not shown yet.
    shown_since: Option<f64>,
    /// The 3D medallion a level-up shows, once built.
    coin: Coin,
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

    /// Every toast waiting, the one showing first.
    pub fn queued(&self) -> impl Iterator<Item = &Toast> {
        self.queue.iter()
    }

    /// The toast showing (or about to show), if any.
    pub fn current(&self) -> Option<&Toast> {
        self.queue.front()
    }

    /// Whether the level-up coin is built, so a level-up draws it rather
    /// than the flat medallion.
    pub fn coin_ready(&self) -> bool {
        self.coin.is_ready()
    }

    /// Draws the front toast, if any, at the clock time of `ui`'s input,
    /// a level-up's medallion in 3D through `renderer` when there is one.
    /// Returns where it was drawn.
    pub fn show(
        &mut self,
        ui: &Ui,
        palette: &Palette,
        renderer: Option<&Renderer3d>,
    ) -> Option<Rect> {
        let now = ui.input(|i| i.time);
        self.show_at(ui, palette, renderer, now)
    }

    /// [`Self::show`] at the clock time `now`, in seconds: retires a toast
    /// whose time is up, starts the next, and schedules the next repaint.
    #[profiling::function]
    pub fn show_at(
        &mut self,
        ui: &Ui,
        palette: &Palette,
        renderer: Option<&Renderer3d>,
        now: f64,
    ) -> Option<Rect> {
        loop {
            self.queue.front()?;
            let since = *self.shown_since.get_or_insert(now);
            let elapsed = now - since;
            if let Some(phase) = ToastPhase::at(elapsed) {
                let toast = self.queue.front()?;
                let burst = burst_at(toast.kind, elapsed);
                let coin = match toast.kind {
                    ToastKind::LevelUp => {
                        let pixels = coin_pixels(ui.ctx().pixels_per_point());
                        let coin = self.coin.poll(renderer, palette.hud.gilt, pixels, ui.ctx());
                        coin.map(|coin| {
                            let yaw = coin_yaw(elapsed);
                            coin.turn(yaw);
                            CoinFace {
                                texture: coin.texture(),
                                yaw,
                            }
                        })
                    }
                    ToastKind::Achievement { .. } => None,
                };
                ui.ctx().request_repaint_after(repaint_after(phase, burst));
                let medallion = Medallion { burst, coin };
                return Some(paint(ui, palette, toast, phase, medallion));
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

/// The coin's texture size at `pixels_per_point`.
fn coin_pixels(pixels_per_point: f32) -> PixelSize {
    let side = (COIN_SIDE * pixels_per_point * COIN_SUPERSAMPLE).round() as u32;
    PixelSize::new(side, side)
}

/// The coin's turn `elapsed` seconds into the toast: [`SPIN_TURNS`] whole
/// turns, fast at first and slowing to rest face-on at [`SPIN`], and
/// exactly zero from then on, so a resting coin is never rendered again.
fn coin_yaw(elapsed: f64) -> f32 {
    let t = (elapsed / SPIN) as f32;
    SPIN_TURNS * std::f32::consts::TAU * (1.0 - ease_out(t))
}

/// When the next repaint is needed for a toast at `phase` with `burst`
/// running: the sooner of the two.
fn repaint_after(phase: ToastPhase, burst: Option<BurstPhase>) -> Duration {
    burst.map_or(phase.repaint_after, |burst| {
        phase.repaint_after.min(burst.repaint_after)
    })
}

/// The 3D coin as one frame draws it: its texture, turned to `yaw`.
#[derive(Debug, Clone, Copy)]
struct CoinFace {
    texture: TextureId,
    yaw: f32,
}

/// What a frame draws in and around a toast's medallion.
#[derive(Debug, Clone, Copy)]
struct Medallion {
    /// The level-up burst behind it, while it runs.
    burst: Option<BurstPhase>,
    /// The 3D coin, when there is one: drawn in place of the flat ring.
    coin: Option<CoinFace>,
}

/// Draws `toast` at `phase` with its `medallion`, centered at the top of the
/// window.
fn paint(
    ui: &Ui,
    palette: &Palette,
    toast: &Toast,
    phase: ToastPhase,
    medallion: Medallion,
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
                    medallion_ui(ui, palette, toast, medallion);
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

/// The medallion: a gold ring (or the 3D coin) with the new level for a
/// level-up, a legendary diamond with the points for an achievement. Its
/// burst, if any, is drawn behind it.
fn medallion_ui(ui: &mut Ui, palette: &Palette, toast: &Toast, medallion: Medallion) {
    let hud = &palette.hud;
    let size = vec2(MEDALLION * 2.0, MEDALLION * 2.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    if let Some(burst) = medallion.burst {
        paint_burst(painter, hud, center, MEDALLION, burst);
    }
    let (color, font_size) = match (toast.kind, medallion.coin) {
        (ToastKind::LevelUp, Some(coin)) => {
            let side = vec2(COIN_SIDE, COIN_SIDE);
            let uv = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
            let at = Rect::from_center_size(center, side);
            painter.image(coin.texture, at, uv, egui::Color32::WHITE);
            (hud.text, 18.0)
        }
        (ToastKind::LevelUp, None) => {
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
        (ToastKind::Achievement { .. }, _) => {
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
    // On the coin the level is struck on the face: it fades as the coin
    // turns edge-on and comes back on the far face.
    let face = medallion.coin.map_or(1.0, |coin| coin.yaw.cos().abs());
    shadowed_text(
        painter,
        center,
        Align2::CENTER_CENTER,
        toast.badge.as_str(),
        FontId::new(font_size, palette.faces.bold.clone()),
        color.gamma_multiply(face),
        hud.shade.gamma_multiply(face),
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

    /// The coin turns while it spins and rests at exactly zero once the
    /// spin is over, so a resting coin needs no render. Every spinning
    /// frame of a level-up already repaints (it is sliding or bursting),
    /// so the spin asks for no frames of its own.
    #[test]
    fn the_coin_spins_while_the_toast_repaints_then_rests_face_on() {
        assert_eq!(coin_yaw(0.0), SPIN_TURNS * std::f32::consts::TAU);
        assert!(coin_yaw(SPIN / 2.0) > 0.0);
        assert_eq!(coin_yaw(SPIN), 0.0);
        assert_eq!(coin_yaw(SPIN + 1.0), 0.0);

        let mut t = 0.0;
        while t < SPIN {
            let phase = ToastPhase::at(t).unwrap();
            let burst = burst_at(ToastKind::LevelUp, t);
            assert_eq!(repaint_after(phase, burst), Duration::ZERO, "at {t}");
            t += 1.0 / 120.0;
        }
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
