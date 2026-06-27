//! The left sidebar: a mini month calendar above a scrolling agenda list.

use crate::block::Block;
use crate::theme;
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate};
use egui::{Align2, FontId, RichText, Sense, pos2, vec2};

/// Outcome of drawing the sidebar for one frame.
#[derive(Default)]
pub(crate) struct SidebarAction {
    /// A day the user picked (mini-month cell or agenda row) to focus.
    pub focus: Option<NaiveDate>,
    /// A block the user clicked in the agenda.
    pub selected: Option<usize>,
    /// Step the displayed mini-month by this many months.
    pub month_step: i32,
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    focus: DateTime<Local>,
    month: NaiveDate,
    blocks: &[Block],
    selected: Option<usize>,
) -> SidebarAction {
    let mut action = SidebarAction::default();

    ui.add_space(8.0);
    month_header(ui, month, &mut action);
    ui.add_space(6.0);
    mini_month(ui, month, focus.date_naive(), blocks, &mut action);
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            agenda(ui, focus.date_naive(), blocks, selected, &mut action);
        });

    action
}

fn month_header(ui: &mut egui::Ui, month: NaiveDate, action: &mut SidebarAction) {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.label(
            RichText::new(month.format("%B").to_string())
                .size(22.0)
                .strong()
                .color(theme::TEXT),
        );
        ui.label(
            RichText::new(month.format("%Y").to_string())
                .size(22.0)
                .strong()
                .color(theme::ACCENT_WARM),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if chevron(ui, "›") {
                action.month_step += 1;
            }
            if chevron(ui, "‹") {
                action.month_step -= 1;
            }
        });
    });
}

fn chevron(ui: &mut egui::Ui, glyph: &str) -> bool {
    ui.add(egui::Button::new(
        RichText::new(glyph).size(16.0).color(theme::TEXT_WEAK),
    ))
    .clicked()
}

/// Painter-drawn month grid: weekday headers, day numbers, per-day event dots,
/// today's cell circled. Clicking a cell focuses that day.
fn mini_month(
    ui: &mut egui::Ui,
    month: NaiveDate,
    focused: NaiveDate,
    blocks: &[Block],
    action: &mut SidebarAction,
) {
    const WEEKDAY_H: f32 = 18.0;
    const CELL_H: f32 = 38.0;

    let today = Local::now().date_naive();
    let width = ui.available_width();
    let height = WEEKDAY_H + CELL_H * 6.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    let cell_w = width / 7.0;

    // Weekday header row (Sunday-first to match the reference layout).
    const NAMES: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
    for (c, name) in NAMES.iter().enumerate() {
        let x = rect.left() + c as f32 * cell_w + cell_w / 2.0;
        let is_today_col = c == today.weekday().num_days_from_sunday() as usize;
        painter.text(
            pos2(x, rect.top() + 2.0),
            Align2::CENTER_TOP,
            *name,
            FontId::proportional(10.0),
            if is_today_col {
                theme::ACCENT_BLUE
            } else {
                theme::TEXT_WEAK
            },
        );
    }

    // First cell is the Sunday on or before the 1st of the month.
    let first = month.with_day(1).unwrap();
    let back = first.weekday().num_days_from_sunday() as i64;
    let grid_start = first - Duration::days(back);

    for week in 0..6 {
        for dow in 0..7 {
            let date = grid_start + Duration::days(week * 7 + dow);
            let cx = rect.left() + dow as f32 * cell_w + cell_w / 2.0;
            let cy = rect.top() + WEEKDAY_H + week as f32 * CELL_H;
            let cell = egui::Rect::from_min_size(
                pos2(rect.left() + dow as f32 * cell_w, cy),
                vec2(cell_w, CELL_H),
            );

            let in_month = date.month() == month.month();
            let is_today = date == today;
            let is_focused = date == focused;

            // Selection / today highlight circle behind the number.
            let num_center = pos2(cx, cy + 12.0);
            if is_today {
                painter.circle_filled(num_center, 12.0, theme::ACCENT_BLUE);
            } else if is_focused {
                painter.circle_stroke(num_center, 12.0, egui::Stroke::new(1.0, theme::TEXT_WEAK));
            }

            let color = if is_today {
                theme::BG
            } else if in_month {
                theme::TEXT
            } else {
                theme::TEXT_FAINT
            };
            painter.text(
                num_center,
                Align2::CENTER_CENTER,
                date.day().to_string(),
                FontId::proportional(12.0),
                color,
            );

            // Up to four event color dots beneath the number.
            let mut colors: Vec<egui::Color32> = blocks
                .iter()
                .filter(|b| b.covers(date))
                .map(|b| b.color)
                .collect();
            colors.dedup();
            colors.truncate(4);
            let n = colors.len();
            if n > 0 {
                let gap = 6.0;
                let mut dx = cx - (n as f32 - 1.0) * gap / 2.0;
                for col in colors {
                    painter.circle_filled(pos2(dx, cy + 27.0), 1.8, col);
                    dx += gap;
                }
            }

            if ui
                .interact(cell, ui.id().with(("mm", week, dow)), Sense::click())
                .clicked()
            {
                action.focus = Some(date);
            }
        }
    }
}

/// The day-grouped agenda list below the mini month.
fn agenda(
    ui: &mut egui::Ui,
    start: NaiveDate,
    blocks: &[Block],
    selected: Option<usize>,
    action: &mut SidebarAction,
) {
    // Show the next 10 days that actually have events.
    let mut shown = 0;
    let mut offset = 0;
    while shown < 10 && offset < 60 {
        let date = start + Duration::days(offset);
        offset += 1;

        let mut day_blocks: Vec<usize> = blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.covers(date))
            .map(|(i, _)| i)
            .collect();
        if day_blocks.is_empty() {
            continue;
        }
        shown += 1;
        // All-day first, then timed by start.
        day_blocks.sort_by(|&a, &b| {
            blocks[b]
                .all_day
                .cmp(&blocks[a].all_day)
                .then(blocks[a].start.cmp(&blocks[b].start))
        });

        agenda_day_header(ui, date, start);

        for i in day_blocks {
            agenda_row(ui, blocks, i, selected == Some(i), action);
        }
        ui.add_space(10.0);
    }

    if shown == 0 {
        ui.add_space(8.0);
        ui.label(RichText::new("No upcoming events").color(theme::TEXT_WEAK));
    }
}

fn agenda_day_header(ui: &mut egui::Ui, date: NaiveDate, start: NaiveDate) {
    let days = (date - start).num_days();
    let label = match days {
        0 => "TODAY".to_string(),
        1 => "TOMORROW".to_string(),
        _ => date.format("%A").to_string().to_uppercase(),
    };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .size(12.0)
                .strong()
                .color(theme::ACCENT_BLUE),
        );
        ui.label(
            RichText::new(date.format("%Y-%m-%d").to_string())
                .size(12.0)
                .color(theme::TEXT_WEAK),
        );
    });
    ui.add_space(4.0);
}

fn agenda_row(
    ui: &mut egui::Ui,
    blocks: &[Block],
    i: usize,
    is_sel: bool,
    action: &mut SidebarAction,
) {
    let b = &blocks[i];
    let resp = if b.all_day {
        // All-day events render as a filled colored pill.
        let (fill, text) = if is_sel {
            (theme::SELECTED_FILL, theme::SELECTED_TEXT)
        } else {
            (theme::block_fill(b.color), theme::block_text(b.color))
        };
        egui::Frame::new()
            .fill(fill)
            .corner_radius(5.0)
            .inner_margin(egui::Margin::symmetric(7, 3))
            .show(ui, |ui| {
                ui.label(RichText::new(&b.title).size(12.0).color(text));
            })
            .response
    } else {
        // Timed events: color dot + time range on one line, title below.
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 4.0, b.color);
            ui.vertical(|ui| {
                let range = format!(
                    "{} – {}",
                    b.start.format("%-I:%M"),
                    b.end.format("%-I:%M %p")
                );
                ui.label(RichText::new(range).size(12.0).color(theme::TEXT_WEAK));
                let color = if is_sel {
                    theme::ACCENT_BLUE
                } else {
                    theme::TEXT
                };
                ui.label(RichText::new(&b.title).size(13.0).color(color));
            });
        })
        .response
    };

    if resp.interact(Sense::click()).clicked() {
        action.selected = Some(i);
        action.focus = Some(b.start.date_naive());
    }
    ui.add_space(6.0);
}

/// First day of the month `dt` falls in, as a local `NaiveDate`.
pub(crate) fn month_of(dt: DateTime<Local>) -> NaiveDate {
    dt.date_naive().with_day(1).unwrap()
}

/// Step a first-of-month date forward/backward by `n` months.
pub(crate) fn step_month(month: NaiveDate, n: i32) -> NaiveDate {
    let mut y = month.year();
    let mut m = month.month() as i32 - 1 + n;
    y += m.div_euclid(12);
    m = m.rem_euclid(12);
    NaiveDate::from_ymd_opt(y, m as u32 + 1, 1).unwrap_or(month)
}
