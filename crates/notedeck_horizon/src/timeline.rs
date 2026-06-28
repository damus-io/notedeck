//! The center pane: a styled day (and week) timeline.

use crate::block::Block;
use crate::theme;
use chrono::{DateTime, Local};
use egui::{Align2, FontId, RichText, Sense, Stroke, StrokeKind, pos2, vec2};

/// Height of one hour row, in points.
const HOUR_H: f32 = 56.0;
/// Width of the hour-label gutter on each side of the grid.
const GUTTER_W: f32 = 58.0;
/// Height of one all-day event bar.
const ALLDAY_ROW_H: f32 = 22.0;
/// Height of the weekday header strip in the week view.
const WEEK_HEADER_H: f32 = 28.0;

fn grid_stroke() -> Stroke {
    Stroke::new(1.0, theme::GRID)
}

fn hour_label(h: u32) -> String {
    match h {
        0 => "12 AM".into(),
        12 => "noon".into(),
        h if h < 12 => format!("{h} AM"),
        h => format!("{} PM", h - 12),
    }
}

/// Everything the day view needs to render a single day: the focused date, the
/// materialized blocks, the current selection/cursor, the zoom level, and the
/// per-frame keyboard scroll signals.
pub(crate) struct DayView<'a> {
    pub focus: DateTime<Local>,
    pub blocks: &'a [Block],
    pub selected: Option<usize>,
    pub cursor: DateTime<Local>,
    /// Pixels per hour (zoom level).
    pub hour_height: f32,
    /// If set, scroll the cursor block to this edge of the viewport this frame.
    pub scroll_to_cursor: Option<egui::Align>,
    /// Vertical points to nudge the scroll by this frame (Ctrl-d/Ctrl-u).
    pub scroll_delta: f32,
}

/// Draw the whole center pane for the day view: big date header, the all-day
/// row, then the scrollable hour grid. Returns the block index that was clicked
/// this frame, if any.
pub(crate) fn center_day(ui: &mut egui::Ui, dv: &DayView) -> Option<usize> {
    let DayView {
        focus,
        blocks,
        selected,
        cursor,
        hour_height,
        scroll_to_cursor,
        scroll_delta,
    } = *dv;

    let mut clicked = None;
    let date = focus.date_naive();

    date_header(ui, focus);
    ui.add_space(6.0);

    // All-day bars across the top.
    let all_day: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.all_day && b.covers(date))
        .map(|(i, _)| i)
        .collect();
    if let Some(i) = allday_row(ui, blocks, &all_day, selected) {
        clicked = Some(i);
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Ctrl-d/Ctrl-u scroll the grid without moving the cursor.
            if scroll_delta != 0.0 {
                ui.scroll_with_delta(vec2(0.0, -scroll_delta));
            }

            let width = ui.available_width();
            let height = hour_height * 24.0;
            let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
            let painter = ui.painter_at(rect);

            let grid_left = rect.left() + GUTTER_W;
            let grid_right = rect.right() - GUTTER_W;

            // Hour rows + labels on both edges.
            for h in 0..=24 {
                let y = rect.top() + h as f32 * hour_height;
                painter.line_segment([pos2(grid_left, y), pos2(grid_right, y)], grid_stroke());
                if h < 24 {
                    let label = hour_label(h);
                    painter.text(
                        pos2(grid_left - 10.0, y - 1.0),
                        Align2::RIGHT_TOP,
                        &label,
                        FontId::proportional(11.0),
                        theme::TEXT_WEAK,
                    );
                    painter.text(
                        pos2(grid_right + 10.0, y - 1.0),
                        Align2::LEFT_TOP,
                        &label,
                        FontId::proportional(11.0),
                        theme::TEXT_WEAK,
                    );
                }
            }

            // Timed blocks for this day.
            let day_blocks: Vec<usize> = blocks
                .iter()
                .enumerate()
                .filter(|(_, b)| !b.all_day && b.covers(date))
                .map(|(i, _)| i)
                .collect();
            if let Some(i) = draw_blocks(
                ui,
                &painter,
                grid_left,
                grid_right,
                rect.top(),
                height,
                blocks,
                &day_blocks,
                selected,
            ) {
                clicked = Some(i);
            }

            // Keyboard selection cursor: a translucent block at the cursor
            // time, drawn only when no event is selected (a selected event
            // highlights itself instead). Mirrors viscal's `draw_selection`.
            if cursor.date_naive() == date {
                let y0 = rect.top() + crate::day_fraction(cursor) * height;
                let cur_end = cursor + chrono::Duration::minutes(crate::SELECTION_MINUTES);
                let y1 = (rect.top() + crate::day_fraction(cur_end) * height)
                    .max(y0 + 14.0)
                    .min(rect.bottom());
                let sel = egui::Rect::from_min_max(pos2(grid_left, y0), pos2(grid_right, y1));

                if selected.is_none() {
                    painter.rect_filled(sel, 4.0, theme::cursor_fill());
                    painter.rect_stroke(
                        sel,
                        4.0,
                        Stroke::new(1.0, theme::cursor_stroke()),
                        StrokeKind::Inside,
                    );
                    painter.text(
                        pos2(grid_left - 10.0, y0),
                        Align2::RIGHT_TOP,
                        cursor.format("%-I:%M %p").to_string(),
                        FontId::proportional(11.0),
                        theme::TEXT,
                    );
                }

                if let Some(align) = scroll_to_cursor {
                    ui.scroll_to_rect(sel, Some(align));
                }
            }

            // "Now" indicator with the time on both edges.
            let now = Local::now();
            if date == now.date_naive() {
                now_line(&painter, grid_left, grid_right, rect.top(), height, now);
            }
        });

    clicked
}

/// The large colored date title, e.g. "Saturday June 27, 2026".
fn date_header(ui: &mut egui::Ui, focus: DateTime<Local>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(GUTTER_W - 6.0);
        let size = 26.0;
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(
            RichText::new(focus.format("%A").to_string())
                .size(size)
                .strong()
                .color(theme::ACCENT_BLUE),
        );
        ui.label(
            RichText::new(focus.format("%B %-d,").to_string())
                .size(size)
                .strong()
                .color(theme::TEXT),
        );
        ui.label(
            RichText::new(focus.format("%Y").to_string())
                .size(size)
                .strong()
                .color(theme::ACCENT_WARM),
        );
    });
}

/// Draw the stacked all-day bars (or nothing if there are none).
fn allday_row(
    ui: &mut egui::Ui,
    blocks: &[Block],
    indices: &[usize],
    selected: Option<usize>,
) -> Option<usize> {
    if indices.is_empty() {
        return None;
    }

    let mut clicked = None;
    let rows = indices.len() as f32;
    let height = rows * (ALLDAY_ROW_H + 2.0) + 6.0;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter_at(rect);

    let left = rect.left() + GUTTER_W;
    let right = rect.right() - GUTTER_W;
    painter.text(
        pos2(left - 10.0, rect.top() + 4.0),
        Align2::RIGHT_TOP,
        "all-day",
        FontId::proportional(11.0),
        theme::TEXT_WEAK,
    );
    painter.text(
        pos2(right + 10.0, rect.top() + 4.0),
        Align2::LEFT_TOP,
        "all-day",
        FontId::proportional(11.0),
        theme::TEXT_WEAK,
    );

    for (row, &i) in indices.iter().enumerate() {
        let b = &blocks[i];
        let y0 = rect.top() + 3.0 + row as f32 * (ALLDAY_ROW_H + 2.0);
        let bar = egui::Rect::from_min_max(pos2(left, y0), pos2(right, y0 + ALLDAY_ROW_H));
        let is_sel = selected == Some(i);

        let resp = ui.interact(bar, ui.id().with(("allday", i)), Sense::click());
        let (fill, text) = if is_sel {
            (theme::SELECTED_FILL, theme::SELECTED_TEXT)
        } else {
            (theme::block_fill(b.color), theme::block_text(b.color))
        };
        painter.rect_filled(bar, 4.0, fill);
        painter.with_clip_rect(bar).text(
            bar.min + vec2(8.0, ALLDAY_ROW_H / 2.0),
            Align2::LEFT_CENTER,
            b.title.as_str(),
            FontId::proportional(12.0),
            text,
        );
        if resp.clicked() {
            clicked = Some(i);
        }
    }

    clicked
}

/// Lay out and draw timed `blocks` (given by global index) within the column
/// `[left, right]`. Returns the index of any block clicked this frame.
#[allow(clippy::too_many_arguments)]
fn draw_blocks(
    ui: &egui::Ui,
    painter: &egui::Painter,
    left: f32,
    right: f32,
    top: f32,
    height: f32,
    all_blocks: &[Block],
    indices: &[usize],
    selected: Option<usize>,
) -> Option<usize> {
    if indices.is_empty() {
        return None;
    }

    let refs: Vec<&Block> = indices.iter().map(|&i| &all_blocks[i]).collect();
    let lanes = crate::block::layout(&refs);
    let avail = right - left;
    let mut clicked = None;

    for (&i, (col, cols)) in indices.iter().zip(lanes) {
        let b = &all_blocks[i];
        let y0 = top + crate::day_fraction(b.start) * height;
        // Keep a minimum height so very short blocks stay legible.
        let y1 = (top + crate::day_fraction(b.end) * height).max(y0 + 20.0);

        let lane_w = avail / cols as f32;
        let x0 = left + col as f32 * lane_w;
        let rect = egui::Rect::from_min_max(pos2(x0 + 1.0, y0 + 1.0), pos2(x0 + lane_w - 1.0, y1));

        let is_sel = selected == Some(i);
        let resp = ui.interact(rect, ui.id().with(("block", i)), Sense::click());

        let (fill, accent, text) = if is_sel {
            (
                theme::SELECTED_FILL,
                theme::SELECTED_TEXT,
                theme::SELECTED_TEXT,
            )
        } else {
            (
                theme::block_fill(b.color),
                b.color,
                theme::block_text(b.color),
            )
        };

        painter.rect_filled(rect, 5.0, fill);
        // Saturated left accent bar.
        let bar = egui::Rect::from_min_max(rect.min, pos2(rect.left() + 3.0, rect.bottom()));
        painter.rect_filled(bar, 0.0, accent);
        if is_sel {
            painter.rect_stroke(
                rect,
                5.0,
                Stroke::new(1.5, theme::ACCENT_BLUE),
                StrokeKind::Inside,
            );
        }

        // Time + title, clipped to the block.
        let clip = painter.with_clip_rect(rect);
        let pad = rect.min + vec2(9.0, 4.0);
        clip.text(
            pad,
            Align2::LEFT_TOP,
            b.start.format("%-I:%M %p").to_string(),
            FontId::proportional(11.0),
            text,
        );
        if y1 - y0 > 30.0 {
            clip.text(
                pad + vec2(0.0, 15.0),
                Align2::LEFT_TOP,
                b.title.as_str(),
                FontId::proportional(12.0),
                text,
            );
        }

        if resp.clicked() {
            clicked = Some(i);
        }
    }

    clicked
}

/// Draw the horizontal "now" line with a dot and the time on each edge.
fn now_line(
    painter: &egui::Painter,
    left: f32,
    right: f32,
    top: f32,
    height: f32,
    now: DateTime<Local>,
) {
    let y = top + crate::day_fraction(now) * height;
    painter.line_segment(
        [pos2(left, y), pos2(right, y)],
        Stroke::new(2.0, theme::NOW),
    );
    painter.circle_filled(pos2(left, y), 4.0, theme::NOW);
    let label = now.format("%-I:%M %p").to_string();
    painter.text(
        pos2(left - 10.0, y),
        Align2::RIGHT_CENTER,
        &label,
        FontId::proportional(11.0),
        theme::NOW,
    );
    painter.text(
        pos2(right + 10.0, y),
        Align2::LEFT_CENTER,
        &label,
        FontId::proportional(11.0),
        theme::NOW,
    );
}

/// Draw a 7-day week timeline with a shared hour grid.
pub(crate) fn week(ui: &mut egui::Ui, focus: DateTime<Local>, blocks: &[Block]) {
    let now = Local::now();
    let monday = crate::start_of_week(focus);

    let width = ui.available_width();
    let height = WEEK_HEADER_H + HOUR_H * 24.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter_at(rect);

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
            if is_today {
                theme::ACCENT_BLUE
            } else {
                theme::TEXT
            },
        );
    }

    // Hour rows + labels.
    for h in 0..=24 {
        let y = grid_top + h as f32 * HOUR_H;
        painter.line_segment([pos2(grid_left, y), pos2(rect.right(), y)], grid_stroke());
        if h < 24 {
            painter.text(
                pos2(grid_left - 10.0, y - 1.0),
                Align2::RIGHT_TOP,
                hour_label(h),
                FontId::proportional(11.0),
                theme::TEXT_WEAK,
            );
        }
    }

    // Vertical day separators.
    for d in 0..=7 {
        let x = grid_left + d as f32 * col_w;
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], grid_stroke());
    }

    // Timed blocks within each day's column.
    for d in 0..7 {
        let day = (monday + chrono::Duration::days(d)).date_naive();
        let x0 = grid_left + d as f32 * col_w;
        let day_blocks: Vec<usize> = blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| !b.all_day && b.covers(day))
            .map(|(i, _)| i)
            .collect();
        draw_blocks(
            ui,
            &painter,
            x0,
            x0 + col_w,
            grid_top,
            HOUR_H * 24.0,
            blocks,
            &day_blocks,
            None,
        );
    }

    // "Now" indicator, confined to today's column if it's in this week.
    let days_in = (now.date_naive() - monday.date_naive()).num_days();
    if (0..7).contains(&days_in) {
        let x0 = grid_left + days_in as f32 * col_w;
        now_line(&painter, x0, x0 + col_w, grid_top, HOUR_H * 24.0, now);
    }
}
