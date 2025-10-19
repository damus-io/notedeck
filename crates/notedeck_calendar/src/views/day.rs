use crate::{model::CalendarEventTime, timed_range_on_day, CalendarApp, CalendarView};
use chrono::Local;
use egui::scroll_area::ScrollAreaOutput;
use egui::{self, vec2, Color32, CornerRadius, FontId, ScrollArea, Stroke};

impl CalendarApp {
    pub(crate) fn render_day(&mut self, ui: &mut egui::Ui) -> ScrollAreaOutput<()> {
        const HOUR_HEIGHT: f32 = 42.0;
        const ALL_DAY_HEIGHT: f32 = 32.0;
        const TIME_COL_WIDTH: f32 = 64.0;
        const COLUMN_MIN_WIDTH: f32 = 220.0;

        let day = self.focus_date;
        let today = Local::now().date_naive();
        let header = if day == today {
            format!("Today – {} ({})", day.format("%A"), day)
        } else {
            format!("{} ({})", day.format("%A"), day)
        };
        ui.heading(header);

        let events = self.events_on(day);
        if events.is_empty() {
            ui.label("No events found for this day.");
        }

        let mut all_day_events = Vec::new();
        let mut timed_events = Vec::new();
        for idx in events {
            if matches!(self.events[idx].time, CalendarEventTime::AllDay { .. }) {
                all_day_events.push(idx);
            } else {
                timed_events.push(idx);
            }
        }

        let total_height = ALL_DAY_HEIGHT + HOUR_HEIGHT * 24.0;
        let selected_idx = self.selected_event;

        ScrollArea::both()
            .id_salt("calendar-day-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (time_rect, _) = ui.allocate_exact_size(
                        vec2(TIME_COL_WIDTH, total_height),
                        egui::Sense::hover(),
                    );
                    let time_painter = ui.painter_at(time_rect);
                    time_painter.rect_filled(
                        time_rect,
                        CornerRadius::same(6),
                        ui.visuals().extreme_bg_color,
                    );
                    for hour in 0..24 {
                        let y = time_rect.top() + ALL_DAY_HEIGHT + hour as f32 * HOUR_HEIGHT;
                        time_painter.text(
                            egui::pos2(time_rect.left() + 6.0, y + 4.0),
                            egui::Align2::LEFT_TOP,
                            format!("{:02}:00", hour),
                            FontId::proportional(12.0),
                            ui.visuals().weak_text_color(),
                        );
                        let stroke = Stroke::new(0.75, ui.visuals().weak_text_color());
                        time_painter.line_segment(
                            [
                                egui::pos2(time_rect.right() - 8.0, y),
                                egui::pos2(time_rect.right(), y),
                            ],
                            stroke,
                        );
                    }

                    let column_width = ui.available_width().max(COLUMN_MIN_WIDTH);
                    let (day_rect, _) = ui.allocate_exact_size(
                        vec2(column_width, total_height),
                        egui::Sense::hover(),
                    );
                    let painter = ui.painter_at(day_rect);

                    if day == today {
                        painter.rect_filled(
                            day_rect,
                            CornerRadius::same(6),
                            Color32::from_rgba_unmultiplied(0, 91, 187, 18),
                        );
                    }

                    let header_rect = egui::Rect::from_min_max(
                        day_rect.left_top(),
                        egui::pos2(day_rect.right(), day_rect.top() + 24.0),
                    );
                    painter.text(
                        header_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        format!("{} {}", day.format("%A"), day.format("%m/%d")),
                        FontId::proportional(14.0),
                        ui.visuals().strong_text_color(),
                    );

                    let all_day_rect = egui::Rect::from_min_max(
                        egui::pos2(day_rect.left(), day_rect.top() + 24.0),
                        egui::pos2(day_rect.right(), day_rect.top() + ALL_DAY_HEIGHT),
                    );
                    let timeline_rect = egui::Rect::from_min_max(
                        egui::pos2(day_rect.left(), all_day_rect.bottom()),
                        day_rect.right_bottom(),
                    );

                    let grid_stroke = Stroke::new(0.5, ui.visuals().weak_text_color());
                    for hour in 0..=24 {
                        let y = timeline_rect.top() + hour as f32 * HOUR_HEIGHT;
                        painter.line_segment(
                            [
                                egui::pos2(timeline_rect.left(), y),
                                egui::pos2(timeline_rect.right(), y),
                            ],
                            grid_stroke,
                        );
                    }

                    if !all_day_events.is_empty() {
                        let mut y = all_day_rect.top() + 4.0;
                        let chip_height = 20.0;
                        let max_display = 5usize;
                        for (display_idx, event_idx) in all_day_events.iter().enumerate() {
                            if display_idx >= max_display {
                                let more = all_day_events.len() - max_display;
                                painter.text(
                                    egui::pos2(all_day_rect.left() + 6.0, y),
                                    egui::Align2::LEFT_TOP,
                                    format!("+{} more", more),
                                    FontId::proportional(12.0),
                                    ui.visuals().weak_text_color(),
                                );
                                break;
                            }

                            let chip_rect = egui::Rect::from_min_max(
                                egui::pos2(all_day_rect.left() + 6.0, y),
                                egui::pos2(all_day_rect.right() - 6.0, y + chip_height),
                            );
                            let id =
                                ui.make_persistent_id(("calendar_day_all_day", day, *event_idx));
                            let response = ui.interact(chip_rect, id, egui::Sense::click());
                            let is_selected = selected_idx == Some(*event_idx);
                            let fill = if is_selected {
                                ui.visuals().selection.bg_fill
                            } else {
                                ui.visuals().extreme_bg_color
                            };
                            let stroke = if is_selected {
                                ui.visuals().selection.stroke
                            } else {
                                Stroke::new(1.0, ui.visuals().weak_text_color())
                            };
                            painter.rect_filled(chip_rect, CornerRadius::same(6), fill);
                            painter.rect_stroke(
                                chip_rect,
                                CornerRadius::same(6),
                                stroke,
                                egui::StrokeKind::Inside,
                            );
                            let event = &self.events[*event_idx];
                            let status = self.current_user_rsvp(event);
                            let annotated =
                                Self::annotate_title_with_status(event.day_title(), status);
                            let chip_clip_rect = chip_rect.shrink2(vec2(4.0, 2.0));
                            let chip_painter = painter.with_clip_rect(chip_rect.shrink(1.0));
                            chip_painter.text(
                                chip_clip_rect.left_top(),
                                egui::Align2::LEFT_TOP,
                                annotated.as_ref(),
                                FontId::proportional(12.0),
                                ui.visuals().strong_text_color(),
                            );
                            if response.clicked() {
                                self.selected_event = Some(*event_idx);
                                self.view = CalendarView::Event;
                            }
                            y += chip_height + 4.0;
                        }
                    }

                    for &event_idx in &timed_events {
                        let event = &self.events[event_idx];
                        if let Some((start_hour, end_hour)) =
                            timed_range_on_day(event, &self.timezone, day)
                        {
                            let top = timeline_rect.top() + start_hour * HOUR_HEIGHT;
                            let bottom = timeline_rect.top() + end_hour * HOUR_HEIGHT;
                            let event_rect = egui::Rect::from_min_max(
                                egui::pos2(timeline_rect.left() + 6.0, top + 2.0),
                                egui::pos2(timeline_rect.right() - 6.0, bottom - 2.0),
                            );

                            let id = ui.make_persistent_id(("calendar_day_timed", day, event_idx));
                            let response = ui.interact(event_rect, id, egui::Sense::click());

                            let is_selected = selected_idx == Some(event_idx);
                            let fill = if is_selected {
                                ui.visuals().selection.bg_fill
                            } else {
                                ui.visuals().extreme_bg_color
                            };
                            let stroke = if is_selected {
                                ui.visuals().selection.stroke
                            } else {
                                Stroke::new(1.0, ui.visuals().weak_text_color())
                            };
                            painter.rect_filled(event_rect, CornerRadius::same(6), fill);
                            painter.rect_stroke(
                                event_rect,
                                CornerRadius::same(6),
                                stroke,
                                egui::StrokeKind::Inside,
                            );

                            let time_label = self.timed_label_for_day(event_idx, day);
                            self.paint_timed_event_contents(
                                ui, &painter, event_rect, event, time_label,
                            );

                            if response.clicked() {
                                self.selected_event = Some(event_idx);
                                self.view = CalendarView::Event;
                            }
                        }
                    }

                    if all_day_events.is_empty() && timed_events.is_empty() {
                        painter.text(
                            timeline_rect.left_top() + vec2(6.0, 6.0),
                            egui::Align2::LEFT_TOP,
                            "No events scheduled.",
                            FontId::proportional(12.0),
                            ui.visuals().weak_text_color(),
                        );
                    }
                });
            })
    }
}
