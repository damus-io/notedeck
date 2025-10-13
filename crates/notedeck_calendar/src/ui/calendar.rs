use crate::{Calendar, CalendarEventDisplay, EventTime, EventType, ViewMode};
use chrono::{Datelike, Local, NaiveDate};
use egui::{Color32, Frame, Margin, RichText, Sense, Vec2};
use notedeck::{AppAction, AppContext, AppResponse};
use nostrdb::NoteKey;

#[derive(Debug, Clone)]
pub enum CalendarAction {
    NextMonth,
    PrevMonth,
    NextDay,
    PrevDay,
    SelectDate(NaiveDate),
    ChangeView(ViewMode),
    SelectEvent(String),
    CreateEvent,
    SubmitEvent(EventCreationData),
    CancelEventCreation,
    RefreshEvents,
}

#[derive(Debug, Clone)]
pub struct EventCreationData {
    pub event_type: EventType,
    pub title: String,
    pub description: String,
    pub start_date: Option<NaiveDate>,
    pub start_time: Option<String>,
    pub end_date: Option<NaiveDate>,
    pub end_time: Option<String>,
    pub timezone: Option<String>,
    pub location: String,
    pub geohash: String,
    pub hashtags: String,
    pub references: String,
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
        let mut app_response = AppResponse::default();

        if self.events().is_empty() && self.calendars().is_empty() {
            self.load_events(app_ctx);
            self.load_calendars(app_ctx);
        }

        ui.vertical(|ui| {
            if let Some(msg) = self.feedback_message() {
                ui.colored_label(Color32::from_rgb(100, 200, 100), msg);
                ui.add_space(4.0);
            }

            if self.creating_event() {
                event_creation_form_ui(self, ui, &mut actions);
            } else {
                if let Some(action) = toolbar_ui(self, ui, &mut actions) {
                    app_response.action = Some(action);
                }
                
                ui.separator();
                
                match self.view_mode() {
                    ViewMode::Month => month_view_ui(self, ui, &mut actions),
                    ViewMode::Week => week_view_ui(self, ui, &mut actions),
                    ViewMode::Day => day_view_ui(self, ui, &mut actions),
                    ViewMode::List => list_view_ui(self, ui, &mut actions),
                }
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
                CalendarAction::NextDay => {
                    self.next_day(app_ctx);
                    self.load_events(app_ctx);
                }
                CalendarAction::PrevDay => {
                    self.prev_day(app_ctx);
                    self.load_events(app_ctx);
                }
                CalendarAction::SelectDate(date) => {
                    self.set_selected_date(date, app_ctx);
                    self.set_view_mode(ViewMode::Day);
                }
                CalendarAction::SelectEvent(note_key_str) => {
                    if note_key_str.is_empty() {
                        self.set_selected_event(None);
                    } else if let Ok(key) = note_key_str.parse::<u64>() {
                        self.set_selected_event(Some(NoteKey::new(key)));
                    }
                }
                CalendarAction::ChangeView(mode) => self.set_view_mode(mode),
                CalendarAction::RefreshEvents => {
                    self.load_events(app_ctx);
                    self.load_calendars(app_ctx);
                }
                CalendarAction::CreateEvent => {
                    self.clear_feedback();
                    self.start_creating_event();
                }
                CalendarAction::CancelEventCreation => {
                    self.cancel_creating_event();
                }
                CalendarAction::SubmitEvent(data) => {
                    if let Some(_note_id) = Self::create_nip52_event(app_ctx, &data) {
                        self.set_feedback("Event created successfully! It will appear once relays confirm it.".to_string());
                        self.cancel_creating_event();
                        self.load_events(app_ctx);
                    } else {
                        self.set_feedback("Failed to create event. Please check your inputs.".to_string());
                    }
                }
            }
        }

        app_response
    }
}

fn toolbar_ui(calendar: &mut Calendar, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) -> Option<AppAction> {
    let mut app_action = None;
    
    ui.horizontal(|ui| {
        if ui.button("⬅ Back").on_hover_text("Open sidebar to switch apps").clicked() {
            app_action = Some(AppAction::ToggleChrome);
        }
        
        ui.separator();

        if calendar.view_mode() == &ViewMode::Day {
            if ui.button("◀").clicked() {
                actions.push(CalendarAction::PrevDay);
            }

            if ui.button("▶").clicked() {
                actions.push(CalendarAction::NextDay);
            }

            ui.separator();

            let current_day = calendar.selected_date().format("%A, %B %d, %Y").to_string();
            ui.label(RichText::new(current_day).size(18.0).strong());
        } else {
            if ui.button("◀").clicked() {
                actions.push(CalendarAction::PrevMonth);
            }

            if ui.button("▶").clicked() {
                actions.push(CalendarAction::NextMonth);
            }

            ui.separator();

            let current_month = calendar.selected_date().format("%B %Y").to_string();
            ui.label(RichText::new(current_month).size(18.0).strong());
        }

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
    
    app_action
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
                                mini_event_ui(event, ui, actions);
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

fn mini_event_ui(event: &CalendarEventDisplay, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    let response = Frame::new()
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
    
    if response.response.interact(Sense::click()).clicked() {
        actions.push(CalendarAction::SelectEvent(event.note_key.as_u64().to_string()));
    }
    
    ui.add_space(2.0);
}

fn day_view_ui(calendar: &Calendar, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    ui.vertical(|ui| {
        ui.heading(calendar.selected_date().format("%A, %B %d, %Y").to_string());
        
        ui.separator();

        if let Some(selected_key) = calendar.selected_event() {
            if let Some(event) = calendar.events().iter().find(|e| e.note_key == selected_key) {
                event_detail_ui(event, ui, actions);
                return;
            }
        }

        let events = get_events_on_date(calendar.events(), calendar.selected_date());
        
        if events.is_empty() {
            ui.label("No events on this day");
        } else {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for event in events {
                    event_card_ui(event, ui, actions);
                }
            });
        }
    });
}

fn list_view_ui(calendar: &Calendar, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        for event in calendar.events() {
            event_card_ui(event, ui, actions);
        }
    });
}

fn event_card_ui(event: &CalendarEventDisplay, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    let response = Frame::new()
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
                        let location_text = if let Some(ref geohash) = event.geohash {
                            format!("📍 {} ({})", event.location.join(", "), geohash)
                        } else {
                            format!("📍 {}", event.location.join(", "))
                        };
                        ui.label(RichText::new(location_text).color(Color32::from_gray(160)));
                    }

                    if !event.participants.is_empty() {
                        let participant_count = event.participants.len();
                        let hosts: Vec<_> = event.participants.iter()
                            .filter(|p| p.role.as_deref() == Some("host"))
                            .collect();
                        
                        let participant_text = if !hosts.is_empty() {
                            format!("👥 {} participants ({} hosts)", participant_count, hosts.len())
                        } else {
                            format!("👥 {} participants", participant_count)
                        };
                        ui.label(RichText::new(participant_text).color(Color32::from_gray(160)));
                    }

                    if !event.hashtags.is_empty() {
                        ui.label(RichText::new(format!("🏷️ {}", event.hashtags.join(" #")))
                            .color(Color32::from_rgb(100, 150, 200)));
                    }

                    if !event.references.is_empty() {
                        ui.label(RichText::new(format!("🔗 {} references", event.references.len()))
                            .color(Color32::from_gray(160)));
                    }

                    if !event.description.is_empty() {
                        ui.label(RichText::new(&event.description)
                            .color(Color32::from_gray(140)));
                    }
                });
            });
        });
    
    if response.response.interact(Sense::click()).clicked() {
        actions.push(CalendarAction::SelectEvent(event.note_key.as_u64().to_string()));
    }
    
    ui.add_space(5.0);
}

fn event_detail_ui(event: &CalendarEventDisplay, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        if ui.button("← Back to Events").clicked() {
            actions.push(CalendarAction::SelectEvent(String::new()));
        }
        
        ui.add_space(10.0);
        
        Frame::new()
            .fill(Color32::from_gray(30))
            .inner_margin(Margin::same(15))
            .stroke(egui::Stroke::new(2.0, Color32::from_gray(80)))
            .show(ui, |ui| {
                let icon = match event.event_type {
                    EventType::DateBased => "📅",
                    EventType::TimeBased => "⏰",
                };
                ui.heading(format!("{} {}", icon, &event.title));
                
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);
                
                ui.label(RichText::new("Time").strong().size(14.0));
                let time_str = format_event_time(&event.start, &event.end);
                ui.label(RichText::new(time_str).color(Color32::from_gray(180)));
                
                ui.add_space(10.0);
                
                if !event.location.is_empty() {
                    ui.label(RichText::new("Location").strong().size(14.0));
                    for loc in &event.location {
                        ui.label(RichText::new(format!("📍 {}", loc)).color(Color32::from_gray(180)));
                    }
                    if let Some(ref geohash) = event.geohash {
                        ui.label(RichText::new(format!("Geohash: {}", geohash)).color(Color32::from_gray(160)));
                    }
                    ui.add_space(10.0);
                }
                
                if !event.description.is_empty() {
                    ui.label(RichText::new("Description").strong().size(14.0));
                    ui.label(RichText::new(&event.description).color(Color32::from_gray(180)));
                    ui.add_space(10.0);
                }
                
                if !event.participants.is_empty() {
                    ui.label(RichText::new("Participants").strong().size(14.0));
                    for participant in &event.participants {
                        let role = participant.role.as_deref().unwrap_or("participant");
                        ui.label(RichText::new(format!("👤 {} ({})", &participant.pubkey[..16], role))
                            .color(Color32::from_gray(180)));
                    }
                    ui.add_space(10.0);
                }
                
                if !event.hashtags.is_empty() {
                    ui.label(RichText::new("Tags").strong().size(14.0));
                    ui.label(RichText::new(format!("#{}", event.hashtags.join(" #")))
                        .color(Color32::from_rgb(100, 150, 200)));
                    ui.add_space(10.0);
                }
                
                if !event.references.is_empty() {
                    ui.label(RichText::new("References").strong().size(14.0));
                    for reference in &event.references {
                        ui.label(RichText::new(format!("🔗 {}", reference))
                            .color(Color32::from_gray(180)));
                    }
                    ui.add_space(10.0);
                }
                
                ui.label(RichText::new(format!("Event ID: {}", event.d_tag))
                    .size(11.0)
                    .color(Color32::from_gray(120)));
            });
    });
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

fn event_creation_form_ui(calendar: &mut Calendar, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    ui.heading("Create New Calendar Event");
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        let form = calendar.event_form_mut();

        ui.horizontal(|ui| {
            ui.label("Event Type:");
            ui.radio_value(&mut form.event_type, EventType::TimeBased, "Time-based (Kind 31923)");
            ui.radio_value(&mut form.event_type, EventType::DateBased, "Date-based (Kind 31922)");
        });

        ui.separator();

        ui.label("Title (required):");
        ui.text_edit_singleline(&mut form.title);

        ui.add_space(10.0);

        ui.label("Description (content):");
        ui.text_edit_multiline(&mut form.description);

        ui.add_space(10.0);

        ui.label("Start Date (YYYY-MM-DD):");
        ui.text_edit_singleline(&mut form.start_date);

        if matches!(form.event_type, EventType::TimeBased) {
            ui.label("Start Time (HH:MM):");
            ui.text_edit_singleline(&mut form.start_time);

            ui.label("Timezone (IANA, e.g., America/New_York, UTC):");
            ui.text_edit_singleline(&mut form.timezone);
        }

        ui.add_space(10.0);

        ui.label("End Date (YYYY-MM-DD, optional):");
        ui.text_edit_singleline(&mut form.end_date);

        if matches!(form.event_type, EventType::TimeBased) {
            ui.label("End Time (HH:MM, optional):");
            ui.text_edit_singleline(&mut form.end_time);
        }

        ui.add_space(10.0);

        ui.label("Location (optional):");
        ui.text_edit_singleline(&mut form.location);

        ui.label("Geohash (optional):");
        ui.text_edit_singleline(&mut form.geohash);

        ui.add_space(10.0);

        ui.label("Hashtags (space-separated, optional):");
        ui.text_edit_singleline(&mut form.hashtags);

        ui.label("References (comma-separated URLs, optional):");
        ui.text_edit_singleline(&mut form.references);

        ui.add_space(20.0);

        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                actions.push(CalendarAction::CancelEventCreation);
            }

            if ui.button("Create Event").clicked() {
                actions.push(CalendarAction::SubmitEvent(form_to_creation_data(&form)));
            }
        });
    });
}

fn form_to_creation_data(form: &crate::EventFormData) -> EventCreationData {
    EventCreationData {
        event_type: form.event_type.clone(),
        title: form.title.clone(),
        description: form.description.clone(),
        start_date: NaiveDate::parse_from_str(&form.start_date, "%Y-%m-%d").ok(),
        start_time: if form.start_time.is_empty() { None } else { Some(form.start_time.clone()) },
        end_date: NaiveDate::parse_from_str(&form.end_date, "%Y-%m-%d").ok(),
        end_time: if form.end_time.is_empty() { None } else { Some(form.end_time.clone()) },
        timezone: if form.timezone.is_empty() { None } else { Some(form.timezone.clone()) },
        location: form.location.clone(),
        geohash: form.geohash.clone(),
        hashtags: form.hashtags.clone(),
        references: form.references.clone(),
    }
}
