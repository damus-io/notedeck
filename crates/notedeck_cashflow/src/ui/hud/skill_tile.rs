//! The skill tile: one cell of a skills grid. The skill's icon sits in a
//! framed slot on the left, its level is set on a diagonal on the right —
//! the current level large, `99` small beneath — and a hairline along the
//! bottom shows progress through the level. A hovered tile lights its edge
//! gold, and a maxed skill turns legendary orange.
//!
//! [`locked_skill_tile`] is a skill not started yet: its icon dimmed and a
//! padlock where the level would be.

use egui::{
    Align2, CornerRadius, FontId, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    pos2, vec2,
};

use super::paint::shadowed_text;
use super::{Grouped, MAX_LEVEL, Rarity, XpProgress, show_tooltip};
use crate::ui::palette::{HudPalette, Palette};

/// A tile's size. A skills grid is rows of these.
pub const SKILL_TILE_SIZE: egui::Vec2 = egui::Vec2::new(112.0, 48.0);

/// The icon slot's side.
const SLOT: f32 = 34.0;

/// The gap between the tile's edge and its slot.
const INSET: f32 = 6.0;

/// How far a locked tile's icon is faded.
const LOCKED_ICON_OPACITY: f32 = 0.28;

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
        let painter = ui.painter_at(rect);
        let hud = &palette.hud;
        let hovered = response.hovered();
        panel(&painter, hud, rect, hovered);
        let slot = icon_slot(&painter, hud, rect, icon, 1.0);
        level(&painter, palette, rect, slot, progress);
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

/// Draws the skill `name` as a locked tile: `icon` dimmed and a padlock in
/// place of a level, with a tooltip saying how to `unlock` it.
#[profiling::function]
pub fn locked_skill_tile(
    ui: &mut Ui,
    palette: &Palette,
    icon: &str,
    name: &str,
    unlock: &str,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(SKILL_TILE_SIZE, Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Group, false, name));

    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let hud = &palette.hud;
        panel(&painter, hud, rect, false);
        let slot = icon_slot(&painter, hud, rect, icon, LOCKED_ICON_OPACITY);
        let area = level_area(rect, slot);
        padlock(&painter, area.center(), hud.text_muted);
        track(&painter, hud, rect);
    }

    show_tooltip(&response, palette, |t| {
        t.title(name);
        t.line("Locked", Rarity::Common);
        t.note(unlock);
    });

    response
}

/// The tile's glass and edge, washed and edged gold when `hovered`.
fn panel(painter: &Painter, hud: &HudPalette, rect: Rect, hovered: bool) {
    let radius = 3.0;
    painter.rect_filled(rect, radius, hud.panel);
    let edge = if hovered {
        painter.rect_filled(rect, radius, hud.title.gamma_multiply(0.08));
        hud.title
    } else {
        hud.border
    };
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(notedeck::tokens::STROKE_THIN, edge),
        StrokeKind::Inside,
    );
}

/// The framed slot on the tile's left with `icon` in it at `opacity`.
/// Returns the slot.
fn icon_slot(painter: &Painter, hud: &HudPalette, rect: Rect, icon: &str, opacity: f32) -> Rect {
    let slot = Rect::from_min_size(rect.min + vec2(INSET, INSET), vec2(SLOT, SLOT));
    painter.rect_filled(slot, 2.0, hud.shade);
    painter.rect_stroke(
        slot,
        2.0,
        Stroke::new(
            notedeck::tokens::STROKE_THIN,
            hud.border.gamma_multiply(0.6),
        ),
        StrokeKind::Inside,
    );
    let mut icon_painter = painter.clone();
    icon_painter.multiply_opacity(opacity);
    icon_painter.text(
        slot.center(),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(20.0),
        hud.text,
    );
    slot
}

/// Where the level goes: right of the slot, clear of the progress hairline.
fn level_area(rect: Rect, slot: Rect) -> Rect {
    Rect::from_min_max(
        pos2(slot.right() + INSET, rect.top() + 5.0),
        pos2(rect.right() - 7.0, rect.bottom() - 7.0),
    )
}

/// The level on a diagonal — current level top left, a slash, `99` bottom
/// right — and the progress hairline under it.
fn level(painter: &Painter, palette: &Palette, rect: Rect, slot: Rect, progress: &XpProgress) {
    let hud = &palette.hud;
    let maxed = progress.is_max();
    let (current, max) = if maxed {
        (hud.legendary, hud.legendary)
    } else {
        (hud.title, hud.text_muted)
    };
    let area = level_area(rect, slot);

    shadowed_text(
        painter,
        area.left_top() + vec2(2.0, 0.0),
        Align2::LEFT_TOP,
        progress.level,
        FontId::new(17.0, palette.faces.bold.clone()),
        current,
        hud.shade,
    );
    let mid = area.center().x;
    painter.line_segment(
        [
            pos2(mid + 5.0, area.top() + 6.0),
            pos2(mid - 3.0, area.bottom() - 2.0),
        ],
        Stroke::new(1.0, hud.text_muted.gamma_multiply(0.6)),
    );
    shadowed_text(
        painter,
        area.right_bottom(),
        Align2::RIGHT_BOTTOM,
        MAX_LEVEL,
        FontId::new(12.0, palette.faces.medium.clone()),
        max,
        hud.shade,
    );

    let track = track(painter, hud, rect);
    let filled = track.width() * progress.fraction();
    if filled > 0.0 {
        let fill = Rect::from_min_size(track.min, vec2(filled, track.height()));
        let color = if maxed { hud.legendary } else { hud.xp_fill };
        painter.rect_filled(fill, 1.0, color);
    }
}

/// The empty progress hairline along the tile's bottom edge. Returns it.
fn track(painter: &Painter, hud: &HudPalette, rect: Rect) -> Rect {
    let track = Rect::from_min_size(
        pos2(rect.left() + INSET, rect.bottom() - 4.0),
        vec2(rect.width() - 2.0 * INSET, 2.0),
    );
    painter.rect_filled(track, 1.0, hud.track);
    track
}

/// A padlock centered on `center`: a rounded shackle over a solid body.
fn padlock(painter: &Painter, center: Pos2, color: egui::Color32) {
    let shackle = Rect::from_center_size(center + vec2(0.0, -3.0), vec2(8.0, 10.0));
    let top_round = CornerRadius {
        nw: 4,
        ne: 4,
        sw: 0,
        se: 0,
    };
    painter.rect_stroke(
        shackle,
        top_round,
        Stroke::new(1.8, color),
        StrokeKind::Middle,
    );
    let body = Rect::from_center_size(center + vec2(0.0, 3.0), vec2(12.0, 9.0));
    painter.rect_filled(body, 1.5, color);
}
