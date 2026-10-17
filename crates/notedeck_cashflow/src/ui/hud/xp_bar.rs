//! The XP bar: a full-width bar cut into [`XP_BAR_SEGMENTS`] notched
//! segments, filled with the accent color up to the skill's progress through
//! its level, with any rested bonus striped past it, and the level and
//! percentage printed on it. [`xp_bar_slim`] is the same progress as a thin
//! unlabelled line, for a character sheet's rows.

use egui::{Align2, FontId, Painter, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::paint::shadowed_text;
use super::{Grouped, Rarity, XpProgress, show_tooltip};
use crate::ui::palette::{HudPalette, Palette};

/// How many segments the bar is cut into.
pub const XP_BAR_SEGMENTS: usize = 20;

/// The bar's height: room for its label.
pub const XP_BAR_HEIGHT: f32 = 18.0;

/// The slim bar's height.
pub const XP_BAR_SLIM_HEIGHT: f32 = 6.0;

/// The gap between the rested bonus's stripes.
const STRIPE_PITCH: f32 = 5.0;

/// Draws `progress` for the skill `name` as a full-width XP bar labelled with
/// its level and percentage, with a HUD tooltip on hover: XP toward the next
/// level, this month's gain and any rested bonus.
#[profiling::function]
pub fn xp_bar(ui: &mut Ui, palette: &Palette, name: &str, progress: &XpProgress) -> Response {
    let size = vec2(ui.available_width(), XP_BAR_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ProgressIndicator, true, name));

    if ui.is_rect_visible(rect) {
        paint(ui, palette, rect, progress);
    }

    xp_tooltip(&response, palette, name, progress);
    response
}

/// [`xp_bar`] as a thin line with no notches or label, for a row of skills on
/// a character sheet. Its tooltip is the full bar's.
#[profiling::function]
pub fn xp_bar_slim(ui: &mut Ui, palette: &Palette, name: &str, progress: &XpProgress) -> Response {
    let size = vec2(ui.available_width(), XP_BAR_SLIM_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ProgressIndicator, true, name));

    if ui.is_rect_visible(rect) {
        let hud = &palette.hud;
        let painter = ui.painter_at(rect.expand(1.0));
        painter.rect_filled(rect, 1.0, hud.track);
        fill(&painter, hud, rect, progress.fraction());
        painter.rect_stroke(
            rect,
            1.0,
            Stroke::new(
                notedeck::tokens::STROKE_THIN,
                hud.border.gamma_multiply(0.7),
            ),
            StrokeKind::Outside,
        );
    }

    xp_tooltip(&response, palette, name, progress);
    response
}

/// The tooltip both bars show: the level, XP toward the next one, this
/// month's gain and any rested bonus.
fn xp_tooltip(response: &Response, palette: &Palette, name: &str, progress: &XpProgress) {
    show_tooltip(response, palette, |t| {
        t.title(format_args!("{name} — level {}", progress.level));
        if progress.is_max() {
            t.value("XP", Grouped(progress.xp), Rarity::Legendary);
            t.note("Maximum level");
        } else {
            t.value(
                "XP",
                format_args!(
                    "{} / {}",
                    Grouped(progress.xp),
                    Grouped(progress.next_level_xp)
                ),
                Rarity::Common,
            );
            t.value(
                "To next level",
                Grouped(progress.remaining()),
                Rarity::Common,
            );
        }
        t.separator();
        t.value(
            "This month",
            format_args!("+{}", Grouped(progress.month_xp)),
            Rarity::Uncommon,
        );
        if progress.rested_xp > 0 {
            t.value(
                "Rested",
                format_args!("+{}", Grouped(progress.rested_xp)),
                Rarity::Rare,
            );
        }
    });
}

/// Paints the track, the earned fill, the striped rested bonus, the notches,
/// the gilt edge and the label.
fn paint(ui: &Ui, palette: &Palette, rect: Rect, progress: &XpProgress) {
    let hud = &palette.hud;
    // The gilt edge is drawn just outside the bar.
    let painter = ui.painter_at(rect.expand(1.0));
    let radius = 2.0;
    painter.rect_filled(rect, radius, hud.track);
    // The track's top lip, shading it as if sunk into the panel.
    let lip = Rect::from_min_size(rect.min, vec2(rect.width(), 2.0));
    painter.rect_filled(lip, 0.0, hud.shade);

    let earned = fill(&painter, hud, rect, progress.fraction());
    let rested = rect.width() * progress.rested_fraction();
    if rested > 0.0 {
        let bonus = Rect::from_min_max(
            pos2(earned, rect.top()),
            pos2(earned + rested, rect.bottom()),
        );
        stripes(&painter, hud, bonus);
    }

    // Notches cut the bar into segments, each with a sheen on its right.
    let step = rect.width() / XP_BAR_SEGMENTS as f32;
    let notch = Stroke::new(1.0, hud.shade);
    let sheen = Stroke::new(1.0, hud.highlight.gamma_multiply(0.2));
    for i in 1..XP_BAR_SEGMENTS {
        let x = (rect.left() + step * i as f32).round() + 0.5;
        painter.vline(x, rect.y_range(), notch);
        painter.vline(x + 1.0, rect.top() + 1.0..=rect.bottom() - 1.0, sheen);
    }

    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(notedeck::tokens::STROKE_THIN, hud.border),
        StrokeKind::Outside,
    );

    let percent = (progress.fraction() * 100.0).floor() as u32;
    let font = FontId::new(11.5, palette.faces.medium.clone());
    let center = rect.center();
    if progress.is_max() {
        let label = format_args!("Level {}  ·  Max", progress.level);
        shadowed_text(
            &painter,
            center,
            Align2::CENTER_CENTER,
            label,
            font,
            hud.text,
            hud.shade,
        );
    } else {
        let label = format_args!("Level {}  ·  {percent}%", progress.level);
        shadowed_text(
            &painter,
            center,
            Align2::CENTER_CENTER,
            label,
            font,
            hud.text,
            hud.shade,
        );
    }
}

/// Fills `rect` with the earned XP up to `fraction`, lit from above: the
/// accent with a sheen over its top band and shade over its bottom one, and
/// a one-pixel highlight along the top of a full-height bar.
/// Returns where the fill ends.
fn fill(painter: &Painter, hud: &HudPalette, rect: Rect, fraction: f32) -> f32 {
    let right = rect.left() + rect.width() * fraction;
    if fraction <= 0.0 {
        return right;
    }
    let x = rect.left()..=right;
    let h = rect.height();
    let full = Rect::from_x_y_ranges(x.clone(), rect.y_range());
    painter.rect_filled(full, 1.0, hud.xp_fill);
    let top = Rect::from_x_y_ranges(x.clone(), rect.top()..=rect.top() + h * 0.4);
    painter.rect_filled(top, 0.0, hud.highlight.gamma_multiply(0.4));
    let bottom = Rect::from_x_y_ranges(x.clone(), rect.bottom() - h * 0.3..=rect.bottom());
    painter.rect_filled(bottom, 0.0, hud.shade.gamma_multiply(0.45));
    if h > XP_BAR_SLIM_HEIGHT {
        painter.hline(x, rect.top() + 1.5, Stroke::new(1.0, hud.highlight));
    }
    right
}

/// Diagonal stripes of [`HudPalette::xp_rested`] across `rect`, over a faint
/// wash of it: distinct from the solid fill on a dark page and a light one.
fn stripes(painter: &Painter, hud: &HudPalette, rect: Rect) {
    painter.rect_filled(rect, 0.0, hud.xp_rested.gamma_multiply(0.18));
    let clipped = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let stroke = Stroke::new(2.0, hud.xp_rested.gamma_multiply(0.55));
    let mut x = rect.left() - rect.height();
    while x < rect.right() {
        clipped.line_segment(
            [pos2(x, rect.bottom()), pos2(x + rect.height(), rect.top())],
            stroke,
        );
        x += STRIPE_PITCH;
    }
}
