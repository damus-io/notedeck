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

/// Format a span length the way viscal's `format_time_duration` does:
/// `1h30m`, `2h`, `45m`, `20s`. Negative durations clamp to zero.
fn format_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (_, 0) => format!("{h}h"),
        (0, _) => format!("{m}m"),
        (_, _) => format!("{h}h{m}m"),
    }
}

/// The "until next" span: from `now` to the start of the next upcoming block,
/// returned only when `now` is free (inside no timed block) and the gap is
/// longer than five minutes. Mirrors viscal's `draw_ephemeral_event`, which
/// shows how much unbooked time is left before the next commitment.
fn next_gap(
    blocks: &[Block],
    timed: &[LaidBlock],
    now: DateTime<Local>,
) -> Option<(DateTime<Local>, DateTime<Local>)> {
    // Occupied right now → there's no free gap to advertise.
    if timed
        .iter()
        .any(|l| blocks[l.index].start <= now && now < blocks[l.index].end)
    {
        return None;
    }
    let next_start = timed
        .iter()
        .map(|l| blocks[l.index].start)
        .filter(|&s| s > now)
        .min()?;
    (next_start - now >= chrono::Duration::minutes(5)).then_some((now, next_start))
}

/// The lower "summary" line viscal draws under an event's title: its time span
/// and length, plus how long you've been in it and how long is left when the
/// event is happening right now. Rendered on the selected and ongoing blocks.
fn block_summary(b: &Block, now: DateTime<Local>) -> String {
    let span = format!(
        "{}–{}  {}",
        b.start.format("%-I:%M %p"),
        b.end.format("%-I:%M %p"),
        format_duration(b.end - b.start),
    );
    if b.start <= now && now < b.end {
        return format!(
            "{span}   {} in · {} left",
            format_duration(now - b.start),
            format_duration(b.end - now),
        );
    }
    span
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
    /// Reference "now" (wall clock, or pinned in tests) for the now-line.
    pub now: DateTime<Local>,
    pub blocks: &'a [Block],
    /// This day's cached all-day/timed layout (built off the per-frame path).
    pub layout: &'a DayLayout,
    pub selected: Option<usize>,
    pub cursor: DateTime<Local>,
    /// Pixels per hour (zoom level).
    pub hour_height: f32,
    /// If set, scroll the cursor block to this edge of the viewport this frame.
    pub scroll_to_cursor: Option<egui::Align>,
    /// Vertical points to nudge the scroll by this frame (Ctrl-d/Ctrl-u).
    pub scroll_delta: f32,
}

/// What the day view reports back after a frame: any block clicked, and how an
/// in-progress inline edit ended (if one was active).
pub(crate) struct DayResponse {
    pub clicked: Option<usize>,
    pub edit: Option<crate::EditOutcome>,
}

/// Draw the whole center pane for the day view: big date header, the all-day
/// row, then the scrollable hour grid. When `editing` is set, a `TextEdit` is
/// overlaid on the selected block and its outcome reported in the response.
pub(crate) fn center_day(
    ui: &mut egui::Ui,
    dv: &DayView,
    editing: Option<&mut crate::EditState>,
) -> DayResponse {
    let DayView {
        focus,
        now,
        blocks,
        layout,
        selected,
        cursor,
        hour_height,
        scroll_to_cursor,
        scroll_delta,
    } = *dv;

    let mut clicked = None;
    let mut edit = None;
    let date = layout.date;

    date_header(ui, focus);
    ui.add_space(6.0);

    // All-day bars across the top.
    if let Some(i) = allday_row(ui, blocks, &layout.all_day, selected) {
        clicked = Some(i);
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Ctrl-d/Ctrl-u scroll the grid without moving the cursor.
            if scroll_delta != 0.0 {
                ui.scroll_with_delta(vec2(0.0, -scroll_delta));
            }
            let mut editing = editing;

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

            // Timed blocks for this day, from the cached layout.
            let drawn = draw_blocks(
                ui,
                &painter,
                grid_left,
                grid_right,
                rect.top(),
                height,
                blocks,
                &layout.timed,
                selected,
                BlockDraw::Day {
                    now,
                    editing: editing.is_some(),
                },
            );
            if let Some(i) = drawn.clicked {
                clicked = Some(i);
            }

            // "Until next" ephemeral block: translucent free-time strip from now
            // to the next event, labelled with how long is left (viscal's
            // `draw_ephemeral_event`). Only on today, and only when free.
            if date == now.date_naive()
                && let Some((st, et)) = next_gap(blocks, &layout.timed, now)
            {
                let y0 = rect.top() + crate::day_fraction(st) * height;
                let y1 = rect.top() + crate::day_fraction(et) * height;
                let gap = egui::Rect::from_min_max(pos2(grid_left, y0), pos2(grid_right, y1));
                painter.rect_filled(gap, 4.0, theme::ephemeral_fill());
                painter.rect_stroke(
                    gap,
                    4.0,
                    Stroke::new(1.0, theme::ephemeral_stroke()),
                    StrokeKind::Inside,
                );
                painter.with_clip_rect(gap).text(
                    gap.min + vec2(9.0, 4.0),
                    Align2::LEFT_TOP,
                    format!("Until next · {}", format_duration(et - st)),
                    FontId::proportional(11.0),
                    theme::TEXT_WEAK,
                );
            }

            // Overlay the inline title editor on the selected block, if editing.
            if let (Some(state), Some(edit_rect)) = (editing.as_mut(), drawn.selected_rect) {
                edit = place_editor(ui, edit_rect, state);
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
                    // Start time at the top edge, end time at the bottom — the
                    // selection's span read straight off the gutter.
                    painter.text(
                        pos2(grid_left - 10.0, y0),
                        Align2::RIGHT_TOP,
                        cursor.format("%-I:%M %p").to_string(),
                        FontId::proportional(11.0),
                        theme::TEXT,
                    );
                    painter.text(
                        pos2(grid_left - 10.0, y1),
                        Align2::RIGHT_BOTTOM,
                        cur_end.format("%-I:%M %p").to_string(),
                        FontId::proportional(11.0),
                        theme::TEXT_WEAK,
                    );
                }

                if let Some(align) = scroll_to_cursor {
                    ui.scroll_to_rect(sel, Some(align));
                }
            }

            // "Now" indicator with the time on both edges.
            if date == now.date_naive() {
                now_line(&painter, grid_left, grid_right, rect.top(), height, now);
            }
        });

    DayResponse { clicked, edit }
}

/// Render the inline title `TextEdit` over the selected block's `rect`, grabbing
/// focus on entry. Returns `Some` once the field loses focus: `Cancel` on
/// Escape, `Commit` otherwise (Return or a click away). To keep the keystroke
/// that opened the editor from landing in the buffer, the text is held at its
/// initial value until the field actually takes focus.
fn place_editor(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    state: &mut crate::EditState,
) -> Option<crate::EditOutcome> {
    let resp = ui.put(
        rect,
        egui::TextEdit::singleline(&mut state.buf)
            .frame(false)
            .margin(vec2(9.0, 4.0))
            .text_color(theme::SELECTED_TEXT)
            .font(FontId::proportional(12.0)),
    );

    if !state.focused {
        resp.request_focus();
        state.buf = state.initial.clone();
        state.focused = resp.has_focus();
    }

    if resp.lost_focus() {
        let escaped = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));
        return Some(if escaped {
            crate::EditOutcome::Cancel
        } else {
            crate::EditOutcome::Commit
        });
    }

    None
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

/// The outcome of drawing a day's timed blocks: any block clicked this frame,
/// and the on-screen rect of the selected block (for the inline edit overlay).
struct DrawResult {
    clicked: Option<usize>,
    selected_rect: Option<egui::Rect>,
}

/// How a day's blocks are rendered. `Week` is the plain, read-only rendering
/// used by the multi-day view; `Day` adds the interactive detail — the live
/// "now" time that drives the summary and in/out labels, and whether the
/// selected block is currently being inline-edited (its title is left to the
/// overlaid `TextEdit`).
#[derive(Clone, Copy)]
enum BlockDraw {
    Week,
    Day { now: DateTime<Local>, editing: bool },
}

/// One timed block placed within its day column: its global index into the
/// block list plus the lane assignment [`block::layout`] gave it (`col` of
/// `cols` side-by-side lanes). Precomputed so the per-frame draw is a plain
/// read rather than a re-layout.
#[derive(Clone, Copy)]
pub(crate) struct LaidBlock {
    pub index: usize,
    pub col: usize,
    pub cols: usize,
}

/// The blocks visible on one calendar date, split into the all-day row and the
/// laid-out timed column. Built by [`Self::build`] only when the underlying
/// blocks change or a new date scrolls into view — never inside a `*_ui` frame.
pub(crate) struct DayLayout {
    pub date: chrono::NaiveDate,
    pub all_day: Vec<usize>,
    pub timed: Vec<LaidBlock>,
}

impl DayLayout {
    /// Filter `blocks` down to those covering `date` and assign the timed ones
    /// their side-by-side lanes. Allocates, so callers must cache the result and
    /// rebuild it only when the inputs change.
    pub(crate) fn build(blocks: &[Block], date: chrono::NaiveDate) -> Self {
        let all_day = blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.all_day && b.covers(date))
            .map(|(i, _)| i)
            .collect();
        let timed_idx: Vec<usize> = blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| !b.all_day && b.covers(date))
            .map(|(i, _)| i)
            .collect();
        let refs: Vec<&Block> = timed_idx.iter().map(|&i| &blocks[i]).collect();
        let timed = timed_idx
            .iter()
            .zip(crate::block::layout(&refs))
            .map(|(&index, (col, cols))| LaidBlock { index, col, cols })
            .collect();
        Self {
            date,
            all_day,
            timed,
        }
    }
}

/// Draw the pre-laid-out timed `blocks` within the column `[left, right]`. The
/// lane assignment comes from the cached [`DayLayout`], so nothing is allocated
/// or re-laid-out here on the per-frame path.
#[allow(clippy::too_many_arguments)]
fn draw_blocks(
    ui: &egui::Ui,
    painter: &egui::Painter,
    left: f32,
    right: f32,
    top: f32,
    height: f32,
    all_blocks: &[Block],
    timed: &[LaidBlock],
    selected: Option<usize>,
    draw: BlockDraw,
) -> DrawResult {
    let avail = right - left;
    let mut clicked = None;
    let mut selected_rect = None;

    for &LaidBlock {
        index: i,
        col,
        cols,
    } in timed
    {
        let b = &all_blocks[i];
        let y0 = top + crate::day_fraction(b.start) * height;
        // Keep a minimum height so very short blocks stay legible.
        let y1 = (top + crate::day_fraction(b.end) * height).max(y0 + 20.0);

        let lane_w = avail / cols as f32;
        let x0 = left + col as f32 * lane_w;
        let rect = egui::Rect::from_min_max(pos2(x0 + 1.0, y0 + 1.0), pos2(x0 + lane_w - 1.0, y1));

        let is_sel = selected == Some(i);
        let tall = y1 - y0 > 30.0;
        let resp = ui.interact(rect, ui.id().with(("block", i)), Sense::click());

        if is_sel {
            // Where the inline editor sits: the title line when there's room,
            // else the whole block. Kept below the time label so it stays legible.
            let editor_top = if tall { rect.top() + 15.0 } else { rect.top() };
            selected_rect = Some(egui::Rect::from_min_max(
                pos2(rect.left(), editor_top),
                pos2(rect.right(), y1),
            ));
        }

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
        // Skip the painted title on the block being edited — the overlaid
        // `TextEdit` renders the live buffer there instead.
        let editing = matches!(draw, BlockDraw::Day { editing: true, .. });
        if tall && !(is_sel && editing) {
            clip.text(
                pad + vec2(0.0, 15.0),
                Align2::LEFT_TOP,
                b.title.as_str(),
                FontId::proportional(12.0),
                text,
            );
        }

        // Summary line (span · duration [+ in/out]) under the title, for the
        // selected block and whichever block is happening right now. Only in
        // the detailed day view, and only when there's a third line of room.
        if let BlockDraw::Day { now, .. } = draw {
            let ongoing = b.start <= now && now < b.end;
            if y1 - y0 > 48.0 && (is_sel || ongoing) {
                clip.text(
                    pad + vec2(0.0, 31.0),
                    Align2::LEFT_TOP,
                    block_summary(b, now),
                    FontId::proportional(10.0),
                    text.gamma_multiply(0.8),
                );
            }
        }

        if resp.clicked() {
            clicked = Some(i);
        }
    }

    DrawResult {
        clicked,
        selected_rect,
    }
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

/// Draw a multi-day week timeline with a shared hour grid, one column per
/// entry in `days` (the cached per-day layouts, in ascending date order). The
/// full week passes seven; a narrow phone passes three (see `WEEK_DAYS_NARROW`)
/// so the columns stay legible. Each column's date, header and now-line come
/// straight off its [`DayLayout`], so the count is however many are handed in.
pub(crate) fn week(ui: &mut egui::Ui, now: DateTime<Local>, blocks: &[Block], days: &[DayLayout]) {
    let cols = days.len().max(1);

    let width = ui.available_width();
    let height = WEEK_HEADER_H + HOUR_H * 24.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter_at(rect);

    let grid_left = rect.left() + GUTTER_W;
    let grid_top = rect.top() + WEEK_HEADER_H;
    let col_w = (rect.right() - grid_left) / cols as f32;

    // Weekday headers, from each column's own date.
    for (d, layout) in days.iter().enumerate() {
        let is_today = layout.date == now.date_naive();
        let x = grid_left + d as f32 * col_w;
        painter.text(
            pos2(x + col_w / 2.0, rect.top() + 4.0),
            Align2::CENTER_TOP,
            layout.date.format("%a %-d").to_string(),
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
    for d in 0..=cols {
        let x = grid_left + d as f32 * col_w;
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], grid_stroke());
    }

    // Timed blocks within each day's column, from the cached per-day layouts.
    for (d, layout) in days.iter().enumerate() {
        let x0 = grid_left + d as f32 * col_w;
        draw_blocks(
            ui,
            &painter,
            x0,
            x0 + col_w,
            grid_top,
            HOUR_H * 24.0,
            blocks,
            &layout.timed,
            None,
            BlockDraw::Week,
        );
    }

    // "Now" indicator, confined to today's column when today is on screen.
    if let Some(d) = days.iter().position(|l| l.date == now.date_naive()) {
        let x0 = grid_left + d as f32 * col_w;
        now_line(&painter, x0, x0 + col_w, grid_top, HOUR_H * 24.0, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 6, 25, h, m, 0).unwrap()
    }

    fn timed(uid: &str, s: (u32, u32), e: (u32, u32)) -> Block {
        Block::timed([0; 32], uid, "x", at(s.0, s.1), at(e.0, e.1))
    }

    /// The laid-out timed blocks for the sample day, as the cache would hold
    /// them, so `next_gap` can be exercised on real [`LaidBlock`]s.
    fn timed_layout(blocks: &[Block]) -> Vec<LaidBlock> {
        DayLayout::build(blocks, at(0, 0).date_naive()).timed
    }

    #[test]
    fn duration_formatting_matches_viscal() {
        assert_eq!(format_duration(Duration::seconds(20)), "20s");
        assert_eq!(format_duration(Duration::minutes(45)), "45m");
        assert_eq!(format_duration(Duration::hours(2)), "2h");
        assert_eq!(format_duration(Duration::minutes(90)), "1h30m");
        // Negative spans clamp to zero rather than printing a sign.
        assert_eq!(format_duration(Duration::minutes(-5)), "0s");
    }

    #[test]
    fn next_gap_spans_now_to_the_next_event() {
        let blocks = vec![timed("a", (9, 0), (10, 0)), timed("b", (14, 0), (15, 0))];
        let timed = timed_layout(&blocks);
        // Free at noon: the gap runs to b's 2pm start.
        let gap = next_gap(&blocks, &timed, at(12, 0));
        assert_eq!(gap, Some((at(12, 0), at(14, 0))));
    }

    #[test]
    fn next_gap_is_none_while_occupied() {
        let blocks = vec![timed("a", (9, 0), (10, 0)), timed("b", (14, 0), (15, 0))];
        let timed = timed_layout(&blocks);
        // Inside event a → no free gap to advertise.
        assert_eq!(next_gap(&blocks, &timed, at(9, 30)), None);
    }

    #[test]
    fn next_gap_ignores_tiny_gaps_and_past_events() {
        let blocks = vec![timed("a", (9, 0), (10, 0)), timed("b", (14, 0), (15, 0))];
        let timed = timed_layout(&blocks);
        // Under five minutes to the next event → suppressed.
        assert_eq!(next_gap(&blocks, &timed, at(13, 57)), None);
        // Nothing upcoming after the last event → nothing to show.
        assert_eq!(next_gap(&blocks, &timed, at(16, 0)), None);
    }
}
