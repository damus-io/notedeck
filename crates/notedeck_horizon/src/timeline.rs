//! Painter-drawn timeline grids for the day and week views.

use crate::block::Block;
use chrono::{DateTime, Local};
use egui::{Align2, Color32, FontId, Stroke, pos2, vec2};

/// Height of one hour row, in points.
const HOUR_H: f32 = 48.0;
/// Width of the left gutter that holds the hour labels.
const GUTTER_W: f32 = 52.0;
/// Height of the weekday header strip in the week view.
const WEEK_HEADER_H: f32 = 28.0;

/// The warm accent used for the "now" line — a horizon sunrise orange.
const ACCENT: Color32 = Color32::from_rgb(0xF9, 0x73, 0x16);

fn grid_stroke(ui: &egui::Ui) -> Stroke {
    let c = ui.visuals().widgets.noninteractive.bg_stroke.color;
    Stroke::new(1.0, c.gamma_multiply(0.6))
}

/// Draw a single day's 24-hour timeline.
pub(crate) fn day(ui: &mut egui::Ui, focus: DateTime<Local>, blocks: &[Block]) {
    let now = Local::now();
    let is_today = focus.date_naive() == now.date_naive();

    let width = ui.available_width();
    let height = HOUR_H * 24.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);

    let stroke = grid_stroke(ui);
    let label_color = ui.visuals().weak_text_color();
    let grid_left = rect.left() + GUTTER_W;

    // Hour rows + labels.
    for h in 0..=24 {
        let y = rect.top() + h as f32 * HOUR_H;
        painter.line_segment([pos2(grid_left, y), pos2(rect.right(), y)], stroke);
        if h < 24 {
            painter.text(
                pos2(grid_left - 8.0, y + 2.0),
                Align2::RIGHT_TOP,
                format!("{h:02}:00"),
                FontId::proportional(11.0),
                label_color,
            );
        }
    }

    // Gutter separator.
    painter.line_segment(
        [pos2(grid_left, rect.top()), pos2(grid_left, rect.bottom())],
        stroke,
    );

    // Time blocks for this day.
    let day_blocks: Vec<&Block> = blocks
        .iter()
        .filter(|b| b.start.date_naive() == focus.date_naive())
        .collect();
    draw_blocks(
        &painter,
        grid_left,
        rect.right(),
        rect.top(),
        height,
        &day_blocks,
    );

    // "Now" indicator.
    if is_today {
        now_line(&painter, grid_left, rect.right(), rect.top(), height, now);
    }
}

/// Draw a 7-day week timeline with a shared hour grid.
pub(crate) fn week(ui: &mut egui::Ui, focus: DateTime<Local>, blocks: &[Block]) {
    let now = Local::now();
    let monday = crate::start_of_week(focus);

    let width = ui.available_width();
    let height = WEEK_HEADER_H + HOUR_H * 24.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);

    let stroke = grid_stroke(ui);
    let label_color = ui.visuals().weak_text_color();
    let text_color = ui.visuals().text_color();
    let grid_left = rect.left() + GUTTER_W;
    let grid_top = rect.top() + WEEK_HEADER_H;
    let col_w = (rect.right() - grid_left) / 7.0;

    // Weekday headers.
    for d in 0..7 {
        let day = monday + chrono::Duration::days(d);
        let is_today = day.date_naive() == now.date_naive();
        let x = grid_left + d as f32 * col_w;
        painter.text(
            pos2(x + col_w / 2.0, rect.top() + 4.0),
            Align2::CENTER_TOP,
            day.format("%a %-d").to_string(),
            FontId::proportional(12.0),
            if is_today { ACCENT } else { text_color },
        );
    }

    // Hour rows + labels.
    for h in 0..=24 {
        let y = grid_top + h as f32 * HOUR_H;
        painter.line_segment([pos2(grid_left, y), pos2(rect.right(), y)], stroke);
        if h < 24 {
            painter.text(
                pos2(grid_left - 8.0, y + 2.0),
                Align2::RIGHT_TOP,
                format!("{h:02}:00"),
                FontId::proportional(11.0),
                label_color,
            );
        }
    }

    // Vertical day separators (including the gutter edge).
    for d in 0..=7 {
        let x = grid_left + d as f32 * col_w;
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], stroke);
    }

    // Time blocks, laid out within each day's column.
    for d in 0..7 {
        let day = monday + chrono::Duration::days(d);
        let x0 = grid_left + d as f32 * col_w;
        let day_blocks: Vec<&Block> = blocks
            .iter()
            .filter(|b| b.start.date_naive() == day.date_naive())
            .collect();
        draw_blocks(
            &painter,
            x0,
            x0 + col_w,
            grid_top,
            HOUR_H * 24.0,
            &day_blocks,
        );
    }

    // "Now" indicator, confined to today's column if it's in this week.
    let days_in = (now.date_naive() - monday.date_naive()).num_days();
    if (0..7).contains(&days_in) {
        let x0 = grid_left + days_in as f32 * col_w;
        now_line(&painter, x0, x0 + col_w, grid_top, HOUR_H * 24.0, now);
    }
}

/// Lay out and draw `blocks` within the column `[left, right]`, mapping each
/// block's start/end time to a vertical offset in `[top, top + height]`.
/// Overlapping blocks are split into side-by-side lanes.
fn draw_blocks(
    painter: &egui::Painter,
    left: f32,
    right: f32,
    top: f32,
    height: f32,
    blocks: &[&Block],
) {
    if blocks.is_empty() {
        return;
    }

    let lanes = crate::block::layout(blocks);
    let avail = right - left;

    for (b, (col, cols)) in blocks.iter().zip(lanes) {
        let y0 = top + crate::day_fraction(b.start) * height;
        // Keep a minimum height so very short blocks stay legible.
        let y1 = (top + crate::day_fraction(b.end) * height).max(y0 + 16.0);

        let lane_w = avail / cols as f32;
        let x0 = left + col as f32 * lane_w;
        let rect =
            egui::Rect::from_min_max(pos2(x0 + 1.0, y0 + 1.0), pos2(x0 + lane_w - 1.0, y1 - 1.0));

        painter.rect_filled(rect, 4.0, b.color);
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, b.color.gamma_multiply(0.55)),
            egui::StrokeKind::Inside,
        );

        // Title, clipped to the block so it never spills into neighbors.
        painter.with_clip_rect(rect).text(
            rect.min + vec2(7.0, 3.0),
            Align2::LEFT_TOP,
            b.title.as_str(),
            FontId::proportional(11.0),
            Color32::WHITE,
        );
    }
}

/// Draw the horizontal "now" line with a dot at its left edge.
fn now_line(
    painter: &egui::Painter,
    left: f32,
    right: f32,
    top: f32,
    height: f32,
    now: DateTime<Local>,
) {
    let y = top + crate::day_fraction(now) * height;
    painter.line_segment([pos2(left, y), pos2(right, y)], Stroke::new(2.0, ACCENT));
    painter.circle_filled(pos2(left, y), 4.0, ACCENT);
}
