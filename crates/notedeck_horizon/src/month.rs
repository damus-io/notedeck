//! The month view: a six-week day-cell grid with truncated event chips. It
//! mirrors the sidebar mini-month's Sunday-first layout, scaled up to fill the
//! central pane and to carry a couple of event chips per day. Works at any
//! width — on a phone the cells simply hold fewer chips.

use crate::block::Block;
use crate::theme;
use crate::timeline::DayLayout;
use chrono::{DateTime, Datelike, Local, NaiveDate};
use egui::{Align2, FontId, RichText, Sense, StrokeKind, pos2, vec2};

/// Weekday header row height.
const WEEKDAY_H: f32 = 22.0;
/// Height of one event chip, and the gap under the day number before them.
const CHIP_H: f32 = 15.0;
const CHIP_TOP: f32 = 20.0;
/// Sunday-first weekday labels, matching the sidebar mini-month.
const NAMES: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

/// Draw the month grid for `focus`'s month and return the date whose cell the
/// user clicked, if any (to focus that day). `days` are the cached per-day
/// layouts for the 42 grid cells — the Sunday on/before the 1st, then 41 more —
/// so the per-day event lists are read straight from the cache, never
/// re-filtered here. `narrow` trims how many chips a cell shows.
pub(crate) fn show(
    ui: &mut egui::Ui,
    focus: DateTime<Local>,
    now: DateTime<Local>,
    blocks: &[Block],
    days: &[DayLayout],
    narrow: bool,
) -> Option<NaiveDate> {
    // The cache is built for exactly six weeks; bail rather than misindex if a
    // stale range slips through before `ensure_layout` catches up.
    if days.len() < 42 {
        return None;
    }
    let grid_start = days[0].date;
    let month = focus.date_naive();
    let today = now.date_naive();

    month_title(ui, focus);

    let avail = ui.available_size();
    let cell_w = avail.x / 7.0;
    // Fill the remaining height with six rows, but keep a sane floor so the
    // grid stays usable in a very short viewport.
    let grid_h = (avail.y - WEEKDAY_H).max(240.0);
    let row_h = grid_h / 6.0;
    let height = WEEKDAY_H + row_h * 6.0;

    let (rect, _) = ui.allocate_exact_size(vec2(avail.x, height), Sense::hover());
    let painter = ui.painter_at(rect);

    weekday_header(&painter, rect, cell_w, today);

    // A cell holds the day number plus however many chips fit; the last slot
    // becomes a "+N" overflow marker when the day has more events than that.
    let chip_room = ((row_h - CHIP_TOP - 2.0) / (CHIP_H + 2.0)).floor() as i64;
    let max_chips = chip_room.clamp(1, if narrow { 2 } else { 4 }) as usize;

    // Highlight the focused week's row (the epic's "selected week").
    let focus_row = (focus.date_naive() - grid_start).num_days() / 7;

    let mut clicked = None;
    for week in 0..6 {
        let row_top = rect.top() + WEEKDAY_H + week as f32 * row_h;
        if week as i64 == focus_row {
            let row = egui::Rect::from_min_size(pos2(rect.left(), row_top), vec2(avail.x, row_h));
            painter.rect_filled(row, 0.0, theme::SURFACE);
        }

        for dow in 0..7 {
            let idx = week * 7 + dow;
            let layout = &days[idx];
            let date = layout.date;
            let x0 = rect.left() + dow as f32 * cell_w;
            let cell = egui::Rect::from_min_size(pos2(x0, row_top), vec2(cell_w, row_h));

            painter.rect_stroke(
                cell,
                0.0,
                egui::Stroke::new(1.0, theme::GRID),
                StrokeKind::Inside,
            );

            day_number(
                &painter,
                cell,
                date,
                date.month() == month.month(),
                date == today,
            );
            day_chips(&painter, cell, blocks, layout, max_chips);

            if ui
                .interact(cell, ui.id().with(("month", idx)), Sense::click())
                .clicked()
            {
                clicked = Some(date);
            }
        }
    }

    clicked
}

/// "June 2026" heading above the grid, matching the day/week date headers.
fn month_title(ui: &mut egui::Ui, focus: DateTime<Local>) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(
            RichText::new(focus.format("%B").to_string())
                .size(24.0)
                .strong()
                .color(theme::TEXT),
        );
        ui.label(
            RichText::new(focus.format("%Y").to_string())
                .size(24.0)
                .strong()
                .color(theme::ACCENT_WARM),
        );
    });
    ui.add_space(6.0);
}

/// The Sunday-first weekday label row, today's column accented.
fn weekday_header(painter: &egui::Painter, rect: egui::Rect, cell_w: f32, today: NaiveDate) {
    for (c, name) in NAMES.iter().enumerate() {
        let x = rect.left() + c as f32 * cell_w + cell_w / 2.0;
        let is_today_col = c == today.weekday().num_days_from_sunday() as usize;
        painter.text(
            pos2(x, rect.top() + 4.0),
            Align2::CENTER_TOP,
            *name,
            FontId::proportional(11.0),
            if is_today_col {
                theme::ACCENT_BLUE
            } else {
                theme::TEXT_WEAK
            },
        );
    }
}

/// The day-of-month number, top-left of its cell: circled on today, dimmed for
/// days spilling in from the neighbouring month.
fn day_number(
    painter: &egui::Painter,
    cell: egui::Rect,
    date: NaiveDate,
    in_month: bool,
    is_today: bool,
) {
    let center = pos2(cell.left() + 14.0, cell.top() + 12.0);
    if is_today {
        painter.circle_filled(center, 11.0, theme::ACCENT_BLUE);
    }
    let color = if is_today {
        theme::BG
    } else if in_month {
        theme::TEXT
    } else {
        theme::TEXT_FAINT
    };
    painter.text(
        center,
        Align2::CENTER_CENTER,
        date.day().to_string(),
        FontId::proportional(12.0),
        color,
    );
}

/// Stacked event chips under the day number: the day's all-day bars first then
/// its timed blocks (read from the cached [`DayLayout`], so no per-frame
/// filtering), truncated to `max` with a "+N" marker when more remain.
fn day_chips(
    painter: &egui::Painter,
    cell: egui::Rect,
    blocks: &[Block],
    layout: &DayLayout,
    max: usize,
) {
    let total = layout.all_day.len() + layout.timed.len();
    if total == 0 {
        return;
    }

    // Reserve the last visible slot for an overflow marker when the day has
    // more events than fit.
    let overflow = total > max;
    let shown = if overflow { max - 1 } else { total };

    let left = cell.left() + 4.0;
    let right = cell.right() - 4.0;
    let events = layout
        .all_day
        .iter()
        .copied()
        .chain(layout.timed.iter().map(|l| l.index));

    for (row, i) in events.take(shown).enumerate() {
        let b = &blocks[i];
        let y0 = cell.top() + CHIP_TOP + row as f32 * (CHIP_H + 2.0);
        let chip = egui::Rect::from_min_max(pos2(left, y0), pos2(right, y0 + CHIP_H));
        painter.rect_filled(chip, 3.0, theme::block_fill(b.color));
        painter.with_clip_rect(chip).text(
            chip.min + vec2(4.0, CHIP_H / 2.0),
            Align2::LEFT_CENTER,
            b.title.as_str(),
            FontId::proportional(10.0),
            theme::block_text(b.color),
        );
    }

    if overflow {
        let y0 = cell.top() + CHIP_TOP + shown as f32 * (CHIP_H + 2.0);
        painter.text(
            pos2(left + 2.0, y0),
            Align2::LEFT_TOP,
            format!("+{}", total - shown),
            FontId::proportional(10.0),
            theme::TEXT_WEAK,
        );
    }
}
