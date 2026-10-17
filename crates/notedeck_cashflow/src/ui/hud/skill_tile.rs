//! The skill tile: one cell of a skills grid, with the skill's icon on the
//! left and its level as `current/99` on the right, over a hairline of
//! progress through the level.

use egui::{Align2, FontId, Rect, Response, Sense, Stroke, Ui, pos2, vec2};

use super::{Grouped, MAX_LEVEL, Rarity, XpProgress, show_tooltip};
use crate::ui::palette::Palette;

/// A tile's size. A skills grid is rows of these.
pub const SKILL_TILE_SIZE: egui::Vec2 = egui::Vec2::new(104.0, 44.0);

/// Draws the skill `name` as a tile showing `icon` (a glyph, such as an emoji
/// or a `notedeck_ui` icon character) and its level, with a HUD tooltip on
/// hover: XP, XP to the next level, and this month's gain.
#[profiling::function]
pub fn skill_tile(
    ui: &mut Ui,
    palette: &Palette,
    icon: &str,
    name: &str,
    progress: &XpProgress,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(SKILL_TILE_SIZE, Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Group, true, name));

    if ui.is_rect_visible(rect) {
        paint(ui, palette, rect, icon, progress, response.hovered());
    }

    show_tooltip(&response, palette, |t| {
        t.title(name);
        t.value(
            "Level",
            format_args!("{}/{MAX_LEVEL}", progress.level),
            Rarity::Common,
        );
        t.value("XP", Grouped(progress.xp), Rarity::Common);
        if progress.is_max() {
            t.note("Maximum level");
        } else {
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
    });

    response
}

/// Paints the tile's panel, icon, level and progress hairline.
fn paint(ui: &Ui, palette: &Palette, rect: Rect, icon: &str, progress: &XpProgress, hovered: bool) {
    let hud = &palette.hud;
    let painter = ui.painter_at(rect);
    let radius = notedeck::tokens::RADIUS_SM;
    let border = if hovered { hud.title } else { hud.border };
    painter.rect_filled(rect, radius, hud.panel);
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(notedeck::tokens::STROKE_THIN, border),
        egui::StrokeKind::Inside,
    );

    let inset = 10.0;
    painter.text(
        pos2(rect.left() + inset, rect.center().y - 1.0),
        Align2::LEFT_CENTER,
        icon,
        FontId::proportional(20.0),
        hud.text,
    );
    painter.text(
        pos2(rect.right() - inset, rect.center().y - 1.0),
        Align2::RIGHT_CENTER,
        format_args!("{}/{MAX_LEVEL}", progress.level),
        FontId::proportional(15.0),
        hud.title,
    );

    // A hairline of progress through the level along the bottom edge.
    let track = Rect::from_min_size(
        pos2(rect.left() + inset, rect.bottom() - 6.0),
        vec2(rect.width() - 2.0 * inset, 2.0),
    );
    painter.rect_filled(track, 1.0, hud.track);
    let filled = track.width() * progress.fraction();
    if filled > 0.0 {
        let fill = Rect::from_min_size(track.min, vec2(filled, track.height()));
        painter.rect_filled(fill, 1.0, hud.xp_fill);
    }
}
