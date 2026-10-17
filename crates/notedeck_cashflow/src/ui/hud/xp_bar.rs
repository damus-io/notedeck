//! The XP bar: a full-width bar cut into [`XP_BAR_SEGMENTS`] segments, filled
//! with the accent color up to the skill's progress through its level, with
//! any rested bonus drawn past it as a lighter overlay.

use egui::{Rect, Response, Sense, Stroke, Ui, pos2, vec2};

use super::{Grouped, Rarity, XpProgress, show_tooltip};
use crate::ui::palette::Palette;

/// How many segments the bar is cut into.
pub const XP_BAR_SEGMENTS: usize = 20;

/// The bar's height.
pub const XP_BAR_HEIGHT: f32 = 14.0;

/// Draws `progress` for the skill `name` as a full-width XP bar, with a HUD
/// tooltip on hover: the level, XP toward the next one, this month's gain
/// and any rested bonus.
#[profiling::function]
pub fn xp_bar(ui: &mut Ui, palette: &Palette, name: &str, progress: &XpProgress) -> Response {
    let size = vec2(ui.available_width(), XP_BAR_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ProgressIndicator, true, name));

    if ui.is_rect_visible(rect) {
        paint(ui, palette, rect, progress);
    }

    show_tooltip(&response, palette, |t| {
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

    response
}

/// Paints the track, the earned and rested fills, and the segment dividers.
fn paint(ui: &Ui, palette: &Palette, rect: Rect, progress: &XpProgress) {
    let hud = &palette.hud;
    let painter = ui.painter_at(rect);
    let radius = 3.0;
    painter.rect_filled(rect, radius, hud.track);

    let earned = rect.width() * progress.fraction();
    let rested = rect.width() * progress.rested_fraction();
    if earned > 0.0 {
        let fill = Rect::from_min_max(rect.min, pos2(rect.left() + earned, rect.bottom()));
        painter.rect_filled(fill, radius, hud.xp_fill);
    }
    if rested > 0.0 {
        let left = rect.left() + earned;
        let overlay =
            Rect::from_min_max(pos2(left, rect.top()), pos2(left + rested, rect.bottom()));
        painter.rect_filled(overlay, 0.0, hud.xp_rested.gamma_multiply(0.6));
    }

    // Dividers in the track's color cut the fill into segments.
    let step = rect.width() / XP_BAR_SEGMENTS as f32;
    let divider = Stroke::new(1.0, hud.panel);
    for i in 1..XP_BAR_SEGMENTS {
        let x = (rect.left() + step * i as f32).round() + 0.5;
        painter.vline(x, rect.y_range(), divider);
    }

    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(notedeck::tokens::STROKE_THIN, hud.border),
        egui::StrokeKind::Inside,
    );
}
