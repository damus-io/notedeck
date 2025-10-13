use crate::{Calendar, CalendarEventDisplay, EventTime, EventType, ViewMode};
use chrono::{Datelike, Local, NaiveDate};
use egui::{Color32, Frame, Margin, RichText, Sense, Vec2};
use notedeck::{AppContext, AppResponse};

#[derive(Debug, Clone)]
pub enum CalendarAction {
    NextMonth,
    PrevMonth,
    SelectDate(NaiveDate),
    ChangeView(ViewMode),
    SelectEvent(String),
    CreateEvent,
    RefreshEvents,
}

#[derive(Debug, Clone)]
pub enum CalendarResponse {
    Action(CalendarAction),
}

pub trait CalendarUi {
    fn ui(&mut self, app_ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse;
}

impl CalendarUi for Calendar {
    fn ui(&mut self, app_ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let mut actions = Vec::new();

        if self.events().is_empty() && self.calendars().is_empty() {
            self.load_events(app_ctx);
            self.load_calendars(app_ctx);
        }

        ui.vertical(|ui| {
            toolbar_ui(self, ui, &mut actions);
            
            ui.separator();
            
            match self.view_mode() {
                ViewMode::Month => month_view_ui(self, ui, &mut actions),
                ViewMode::Week => week_view_ui(self, ui, &mut actions),
                ViewMode::Day => day_view_ui(self, ui, &mut actions),
                ViewMode::List => list_view_ui(self, ui, &mut actions),
            }
        });

        for action in actions {
            match action {
                CalendarAction::NextMonth => {
                    self.next_month();
                    self.load_events(app_ctx);
                }
                CalendarAction::PrevMonth => {
                    self.prev_month();
                    self.load_events(app_ctx);
                }
                CalendarAction::SelectDate(date) => self.set_selected_date(date, app_ctx),
                CalendarAction::ChangeView(mode) => self.set_view_mode(mode),
                CalendarAction::RefreshEvents => {
                    self.load_events(app_ctx);
                    self.load_calendars(app_ctx);
                }
                _ => {}
            }
        }

        AppResponse::default()
    }
}

fn toolbar_ui(calendar: &mut Calendar, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    ui.horizontal(|ui| {
        if ui.button("◀").clicked() {
            actions.push(CalendarAction::PrevMonth);
        }

        if ui.button("▶").clicked() {
            actions.push(CalendarAction::NextMonth);
        }

        ui.separator();

        let current_month = calendar.selected_date().format("%B %Y").to_string();
        ui.label(RichText::new(current_month).size(18.0).strong());

        ui.separator();

        let current_view = calendar.view_mode();
        
        if ui.selectable_label(matches!(current_view, ViewMode::Month), "Month").clicked() {
            actions.push(CalendarAction::ChangeView(ViewMode::Month));
        }

        if ui.selectable_label(matches!(current_view, ViewMode::Week), "Week").clicked() {
            actions.push(CalendarAction::ChangeView(ViewMode::Week));
        }

        if ui.selectable_label(matches!(current_view, ViewMode::Day), "Day").clicked() {
            actions.push(CalendarAction::ChangeView(ViewMode::Day));
        }

        if ui.selectable_label(matches!(current_view, ViewMode::List), "List").clicked() {
            actions.push(CalendarAction::ChangeView(ViewMode::List));
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("🔄 Refresh").clicked() {
                actions.push(CalendarAction::RefreshEvents);
            }

            if ui.button("➕ New Event").clicked() {
                actions.push(CalendarAction::CreateEvent);
            }
        });
    });
}

fn month_view_ui(calendar: &Calendar, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    let first_of_month = calendar.selected_date().with_day(1).unwrap();
    let days_from_monday = first_of_month.weekday().num_days_from_monday();
    
    let start_date = first_of_month - chrono::Duration::days(days_from_monday as i64);
    
    let cell_size = Vec2::new(
        (ui.available_width() - 20.0) / 7.0,
        60.0,
    );

    ui.horizontal(|ui| {
        for day in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] {
            ui.allocate_ui(cell_size, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(day).strong());
                });
            });
        }
    });

    ui.separator();

    let mut current_date = start_date;
    for _week in 0..6 {
        ui.horizontal(|ui| {
            for _day in 0..7 {
                let is_current_month = current_date.month() == calendar.selected_date().month();
                let is_today = current_date == Local::now().date_naive();
                let is_selected = current_date == calendar.selected_date();

                let events_on_day = get_events_on_date(calendar.events(), current_date);

                ui.allocate_ui(cell_size, |ui| {
                    let mut frame = Frame::new()
                        .inner_margin(Margin::same(2))
                        .stroke(egui::Stroke::new(1.0, Color32::from_gray(60)));

                    if is_today {
                        frame = frame.fill(Color32::from_rgb(50, 50, 80));
                    } else if is_selected {
                        frame = frame.fill(Color32::from_rgb(40, 60, 80));
                    } else if !is_current_month {
                        frame = frame.fill(Color32::from_gray(20));
                    }

                    let response = frame.show(ui, |ui| {
                        ui.vertical(|ui| {
                            let mut text = RichText::new(current_date.day().to_string());
                            if !is_current_month {
                                text = text.color(Color32::from_gray(100));
                            }
                            ui.label(text);

                            for (i, _event) in events_on_day.iter().take(3).enumerate() {
                                let color = match i % 3 {
                                    0 => Color32::from_rgb(100, 150, 255),
                                    1 => Color32::from_rgb(150, 100, 255),
                                    _ => Color32::from_rgb(255, 150, 100),
                                };
                                
                                ui.add(
                                    egui::widgets::ProgressBar::new(1.0)
                                        .desired_width(cell_size.x - 10.0)
                                        .desired_height(4.0)
                                        .fill(color)
                                );
                            }

                            if events_on_day.len() > 3 {
                                ui.label(RichText::new(format!("+{} more", events_on_day.len() - 3))
                                    .size(10.0)
                                    .color(Color32::from_gray(150)));
                            }
                        });
                    });

                    if response.response.interact(Sense::click()).clicked() {
                        actions.push(CalendarAction::SelectDate(current_date));
                    }
                });

                current_date += chrono::Duration::days(1);
            }
        });
    }
}

fn week_view_ui(calendar: &Calendar, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    let selected_date = calendar.selected_date();
    let start_of_week = selected_date - chrono::Duration::days(selected_date.weekday().num_days_from_monday() as i64);
    
    ui.horizontal(|ui| {
        for day in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] {
            ui.allocate_ui(Vec2::new((ui.available_width() - 20.0) / 7.0, 20.0), |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(day).strong());
                });
            });
        }
    });

    ui.separator();

    let mut current_date = start_of_week;
    ui.horizontal(|ui| {
        for _day in 0..7 {
            let is_selected = current_date == selected_date;
            let is_today = current_date == Local::now().date_naive();
            let events_on_day = get_events_on_date(calendar.events(), current_date);
            
            ui.allocate_ui(Vec2::new((ui.available_width() - 20.0) / 7.0, 400.0), |ui| {
                let mut frame = Frame::new()
                    .inner_margin(Margin::same(4))
                    .stroke(egui::Stroke::new(1.0, Color32::from_gray(60)));

                if is_today {
                    frame = frame.fill(Color32::from_rgb(50, 50, 80));
                } else if is_selected {
                    frame = frame.fill(Color32::from_rgb(40, 60, 80));
                }

                let response = frame.show(ui, |ui| {
                    ui.vertical(|ui| {
                        let mut text = RichText::new(current_date.day().to_string()).size(16.0);
                        if is_today {
                            text = text.color(Color32::from_rgb(150, 200, 255));
                        }
                        ui.label(text);

                        ui.separator();

                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for event in events_on_day {
                                mini_event_ui(event, ui);
                            }
                        });
                    });
                });

                if response.response.interact(Sense::click()).clicked() {
                    actions.push(CalendarAction::SelectDate(current_date));
                }
            });

            current_date += chrono::Duration::days(1);
        }
    });
}

fn mini_event_ui(event: &CalendarEventDisplay, ui: &mut egui::Ui) {
    Frame::new()
        .fill(Color32::from_gray(40))
        .inner_margin(Margin::same(4))
        .stroke(egui::Stroke::new(1.0, Color32::from_gray(80)))
        .show(ui, |ui| {
            ui.label(RichText::new(&event.title).size(12.0));
            
            let time_str = match &event.start {
                EventTime::DateTime(ts, _) => {
                    chrono::DateTime::from_timestamp(*ts, 0)
                        .map(|dt| dt.format("%H:%M").to_string())
                        .unwrap_or_default()
                }
                _ => String::new(),
            };
            
            if !time_str.is_empty() {
                ui.label(RichText::new(time_str).size(10.0).color(Color32::from_gray(150)));
            }
        });
    
    ui.add_space(2.0);
}

fn day_view_ui(calendar: &Calendar, ui: &mut egui::Ui, _actions: &mut Vec<CalendarAction>) {
    ui.vertical(|ui| {
        ui.heading(calendar.selected_date().format("%A, %B %d, %Y").to_string());
        
        ui.separator();

        let events = get_events_on_date(calendar.events(), calendar.selected_date());
        
        if events.is_empty() {
            ui.label("No events on this day");
        } else {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for event in events {
                    event_card_ui(event, ui);
                }
            });
        }
    });
}

fn list_view_ui(calendar: &Calendar, ui: &mut egui::Ui, _actions: &mut Vec<CalendarAction>) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        for event in calendar.events() {
            event_card_ui(event, ui);
        }
    });
}

fn event_card_ui(event: &CalendarEventDisplay, ui: &mut egui::Ui) {
    Frame::new()
        .fill(Color32::from_gray(30))
        .inner_margin(Margin::same(10))
        .stroke(egui::Stroke::new(1.0, Color32::from_gray(60)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let icon = match event.event_type {
                    EventType::DateBased => "📅",
                    EventType::TimeBased => "⏰",
                };
                ui.label(RichText::new(icon).size(20.0));

                ui.vertical(|ui| {
                    ui.label(RichText::new(&event.title).size(16.0).strong());
                    
                    let time_str = format_event_time(&event.start, &event.end);
                    ui.label(RichText::new(time_str).color(Color32::from_gray(180)));

                    if !event.location.is_empty() {
                        ui.label(RichText::new(format!("📍 {}", event.location.join(", ")))
                            .color(Color32::from_gray(160)));
                    }

                    if !event.description.is_empty() {
                        ui.label(RichText::new(&event.description)
                            .color(Color32::from_gray(140)));
                    }
                });
            });
        });
    
    ui.add_space(5.0);
}

fn get_events_on_date(events: &[CalendarEventDisplay], date: NaiveDate) -> Vec<&CalendarEventDisplay> {
    events.iter()
        .filter(|event| {
            match &event.start {
                EventTime::Date(event_date) => {
                    if let Some(EventTime::Date(end_date)) = &event.end {
                        date >= *event_date && date < *end_date
                    } else {
                        date == *event_date
                    }
                }
                EventTime::DateTime(timestamp, _) => {
                    let event_date = chrono::DateTime::from_timestamp(*timestamp, 0)
                        .map(|dt| dt.date_naive())
                        .unwrap_or(date);
                    
                    if let Some(EventTime::DateTime(end_ts, _)) = &event.end {
                        let end_date = chrono::DateTime::from_timestamp(*end_ts, 0)
                            .map(|dt| dt.date_naive())
                            .unwrap_or(date);
                        date >= event_date && date <= end_date
                    } else {
                        date == event_date
                    }
                }
            }
        })
        .collect()
}

fn format_event_time(start: &EventTime, end: &Option<EventTime>) -> String {
    match start {
        EventTime::Date(date) => {
            if let Some(EventTime::Date(end_date)) = end {
                format!("{} - {}", date.format("%Y-%m-%d"), end_date.format("%Y-%m-%d"))
            } else {
                date.format("%Y-%m-%d").to_string()
            }
        }
        EventTime::DateTime(timestamp, tz) => {
            let dt = chrono::DateTime::from_timestamp(*timestamp, 0)
                .unwrap_or_else(|| Local::now().into());
            
            let tz_str = tz.as_deref().unwrap_or("UTC");
            
            if let Some(EventTime::DateTime(end_ts, _)) = end {
                let end_dt = chrono::DateTime::from_timestamp(*end_ts, 0)
                    .unwrap_or_else(|| Local::now().into());
                format!("{} - {} ({})", 
                    dt.format("%Y-%m-%d %H:%M"),
                    end_dt.format("%H:%M"),
                    tz_str
                )
            } else {
                format!("{} ({})", dt.format("%Y-%m-%d %H:%M"), tz_str)
            }
        }
    }
}
