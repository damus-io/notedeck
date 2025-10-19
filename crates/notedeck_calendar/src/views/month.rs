use crate::{days_in_month, weekday_label, CalendarApp, CalendarView};
use chrono::{Datelike, Duration, Local, NaiveDate};
use egui::scroll_area::ScrollAreaOutput;
use egui::{self, vec2, Color32, CornerRadius, FontId, ScrollArea};
use notedeck::fonts::NamedFontFamily;
use std::sync::Arc;

impl CalendarApp {
    pub(crate) fn render_month(&mut self, ui: &mut egui::Ui) -> ScrollAreaOutput<()> {
        let year = self.focus_date.year();
        let month = self.focus_date.month();
        let first_day = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month start date");
        let last_day =
            NaiveDate::from_ymd_opt(year, month, days_in_month(year, month) as u32).unwrap();

        let start_offset = first_day.weekday().num_days_from_monday() as i64;
        let grid_start = first_day - Duration::days(start_offset);
        let grid_end = grid_start + Duration::days(6 * 7 - 1);

        let today = Local::now().date_naive();
        let selected_id = self
            .selected_event
            .and_then(|idx| self.events.get(idx))
            .map(|ev| ev.id_hex.clone());
        let events_by_day = self.collect_events_by_day(grid_start, grid_end);

        #[derive(Default)]
        struct MonthCellInfo {
            date: Option<NaiveDate>,
            is_today: bool,
            rows: Vec<(usize, Arc<egui::Galley>)>,
            more: usize,
            min_height: f32,
        }

        ScrollArea::vertical()
            .id_salt("calendar-month-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let header_font = FontId::new(
                    18.0,
                    egui::FontFamily::Name(NamedFontFamily::Bold.as_str().into()),
                );
                ui.label(
                    egui::RichText::new(format!("{} {}", first_day.format("%B"), first_day.year()))
                        .font(header_font.clone()),
                );

                ui.add_space(4.0);
                ui.columns(7, |cols| {
                    for (idx, col) in cols.iter_mut().enumerate() {
                        col.label(weekday_label(idx));
                    }
                });

                ui.separator();

                for week in 0..6 {
                    let week_offset = (week as i64) * 7;
                    let mut cell_infos = Vec::with_capacity(7);
                    let mut row_min_height = 110.0f32;
                    let approx_cell_width = (ui.available_width() / 7.0).max(60.0);

                    for col_idx in 0..7 {
                        let cell_date = grid_start + Duration::days(week_offset + col_idx as i64);
                        if cell_date.month() != month {
                            cell_infos.push(MonthCellInfo {
                                min_height: 110.0,
                                ..Default::default()
                            });
                            continue;
                        }

                        let mut info = MonthCellInfo {
                            date: Some(cell_date),
                            is_today: cell_date == today,
                            min_height: 40.0,
                            ..Default::default()
                        };

                        if let Some(events) = events_by_day.get(&cell_date) {
                            let display_count = events.len().min(3);
                            ui.fonts(|fonts| {
                                for idx in events.iter().take(display_count) {
                                    let wrap_width = (approx_cell_width - 12.0).max(32.0);
                                    let (event_id, status, title) =
                                        if let Some(event) = self.events.get(*idx) {
                                            let status = self.current_user_rsvp(event);
                                            let annotated = Self::annotate_title_with_status(
                                                event.month_title(),
                                                status,
                                            )
                                            .into_owned();
                                            (event.id_hex.clone(), status, annotated)
                                        } else {
                                            continue;
                                        };

                                    let galley = self.month_title_galley(
                                        fonts, &event_id, status, &title, wrap_width,
                                    );
                                    let row_height = galley.size().y + 6.0;
                                    info.min_height += row_height;
                                    info.rows.push((*idx, galley));
                                }
                            });
                            info.more = events.len().saturating_sub(display_count);
                            if info.more > 0 {
                                info.min_height += 24.0;
                            }
                        }

                        info.min_height = info.min_height.max(110.0);
                        row_min_height = row_min_height.max(info.min_height);
                        cell_infos.push(info);
                    }

                    ui.columns(7, |cols| {
                        for (col, info) in cols.iter_mut().zip(cell_infos.iter()) {
                            col.set_min_width(110.0);
                            let mut frame =
                                egui::Frame::new().inner_margin(egui::Margin::symmetric(4, 4));
                            if info.is_today {
                                frame = frame.fill(Color32::from_rgba_unmultiplied(0, 91, 187, 18));
                            }

                            frame.show(col, |ui| {
                                ui.set_min_height(row_min_height);
                                if let Some(day) = info.date {
                                    let day_label = egui::Label::new(
                                        egui::RichText::new(format!("{}", day.day())).strong(),
                                    )
                                    .sense(egui::Sense::click());
                                    let response = ui.add(day_label);
                                    if response.clicked() {
                                        self.view = CalendarView::Day;
                                        self.focus_date = day;
                                        self.selected_event = None;
                                    }
                                    ui.add_space(4.0);

                                    for (event_idx, galley) in &info.rows {
                                        if let Some(event) = self.events.get(*event_idx) {
                                            let row_height = galley.size().y + 6.0;
                                            let item_size =
                                                egui::vec2(ui.available_width(), row_height);
                                            let (item_rect, response) = ui.allocate_exact_size(
                                                item_size,
                                                egui::Sense::click(),
                                            );

                                            let is_selected = selected_id
                                                .as_ref()
                                                .is_some_and(|id| id == &event.id_hex);
                                            let visuals = ui
                                                .style()
                                                .interact_selectable(&response, is_selected);
                                            let painter = ui.painter_at(item_rect);
                                            if visuals.bg_fill != Color32::TRANSPARENT {
                                                painter.rect_filled(
                                                    item_rect,
                                                    CornerRadius::same(4),
                                                    visuals.bg_fill,
                                                );
                                            }
                                            if visuals.bg_stroke.width > 0.0 {
                                                painter.rect_stroke(
                                                    item_rect,
                                                    CornerRadius::same(4),
                                                    visuals.bg_stroke,
                                                    egui::StrokeKind::Inside,
                                                );
                                            }

                                            painter.with_clip_rect(item_rect.shrink(1.0)).galley(
                                                item_rect.left_top() + vec2(2.0, 3.0),
                                                galley.clone(),
                                                visuals.text_color(),
                                            );

                                            let response =
                                                response.on_hover_text(event.title.as_str());
                                            if response.clicked() {
                                                self.selected_event = Some(*event_idx);
                                                self.view = CalendarView::Event;
                                                self.focus_date = day;
                                            }
                                        }
                                    }

                                    if info.more > 0 {
                                        let more_size = egui::vec2(ui.available_width(), 22.0);
                                        let (more_rect, _) =
                                            ui.allocate_exact_size(more_size, egui::Sense::hover());
                                        ui.painter_at(more_rect).text(
                                            more_rect.left_center(),
                                            egui::Align2::LEFT_CENTER,
                                            format!("+{} more", info.more),
                                            FontId::proportional(12.0),
                                            ui.visuals().weak_text_color(),
                                        );
                                    }
                                } else {
                                    ui.allocate_space(egui::vec2(
                                        ui.available_width(),
                                        row_min_height,
                                    ));
                                }
                            });
                        }
                    });

                    let next_week_start = grid_start + Duration::days(((week + 1) * 7) as i64);
                    if next_week_start.month() != month && next_week_start > last_day {
                        break;
                    }
                }
            })
    }
}
