use crate::{Calendar, CalendarEventDisplay, EventTime, EventType, ViewMode};
use chrono::{Datelike, Local, NaiveDate};
use egui::{Button, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, TextEdit, Vec2};
use egui_extras::{Size, StripBuilder};
use nostrdb::NoteKey;
use notedeck::{fonts::get_font_size, AppAction, AppContext, AppResponse, NotedeckTextStyle};

#[derive(Debug, Clone)]
pub enum CalendarAction {
    NextMonth,
    PrevMonth,
    NextWeek,
    PrevWeek,
    NextDay,
    PrevDay,
    SelectDate(NaiveDate),
    ChangeView(ViewMode),
    SelectEvent(String),
    CreateEvent,
    SubmitEvent(EventCreationData),
    CancelEventCreation,
    RefreshEvents,
    SubmitRsvp(String, crate::RsvpStatusType),
}

const SECTION_SPACING: f32 = 12.0;
const CARD_CORNER_RADIUS: u8 = 8;
const TOOLBAR_HEIGHT: f32 = 44.0;
const TOOLBAR_ICON_WIDTH: f32 = 120.0;

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
            let previous_spacing = ui.spacing().item_spacing;
            ui.spacing_mut().item_spacing.y = SECTION_SPACING;

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

            ui.spacing_mut().item_spacing = previous_spacing;
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
                CalendarAction::NextWeek => {
                    self.next_week(app_ctx);
                    self.load_events(app_ctx);
                }
                CalendarAction::PrevWeek => {
                    self.prev_week(app_ctx);
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
                        self.set_feedback(
                            "Event created successfully! It will appear once relays confirm it."
                                .to_string(),
                        );
                        self.cancel_creating_event();
                        self.load_events(app_ctx);
                    } else {
                        self.set_feedback(
                            "Failed to create event. Please check your inputs.".to_string(),
                        );
                    }
                }
                CalendarAction::SubmitRsvp(event_id_str, status) => {
                    if let Ok(event_key) = event_id_str.parse::<u64>() {
                        let event_note_key = NoteKey::new(event_key);
                        if let Some(event) =
                            self.events().iter().find(|e| e.note_key == event_note_key)
                        {
                            if let Some(_rsvp_id) = Self::create_rsvp(app_ctx, event, status) {
                                self.set_feedback("RSVP submitted successfully!".to_string());
                                self.load_events(app_ctx);
                            } else {
                                self.set_feedback("Failed to submit RSVP.".to_string());
                            }
                        }
                    }
                }
            }
        }

        app_response
    }
}

fn toolbar_ui(
    calendar: &mut Calendar,
    ui: &mut egui::Ui,
    actions: &mut Vec<CalendarAction>,
) -> Option<AppAction> {
    let mut app_action = None;
    let view_label = match calendar.view_mode() {
        ViewMode::Day => calendar.selected_date().format("%A, %B %d, %Y").to_string(),
        ViewMode::Week => {
            let week_start = calendar.selected_date()
                - chrono::Duration::days(
                    calendar.selected_date().weekday().num_days_from_monday() as i64
                );
            let week_end = week_start + chrono::Duration::days(6);
            format!(
                "{} – {}",
                week_start.format("%b %d"),
                week_end.format("%b %d, %Y")
            )
        }
        ViewMode::Month => calendar.selected_date().format("%B %Y").to_string(),
        ViewMode::List => "Upcoming Events".to_string(),
    };

    let previous_spacing = ui.spacing().item_spacing;
    ui.spacing_mut().item_spacing.x = 8.0;

    StripBuilder::new(ui)
        .size(Size::exact(TOOLBAR_ICON_WIDTH))
        .size(Size::remainder())
        .size(Size::exact(TOOLBAR_ICON_WIDTH + 180.0))
        .horizontal(|mut strip| {
            strip.cell(|ui| {
                ui.set_min_height(TOOLBAR_HEIGHT);
                ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                    if toolbar_button(ui, "⬅ Back")
                        .on_hover_text("Open sidebar to switch apps")
                        .clicked()
                    {
                        app_action = Some(AppAction::ToggleChrome);
                    }

                    match calendar.view_mode() {
                        ViewMode::Day => {
                            if toolbar_icon_button(ui, "◀").clicked() {
                                actions.push(CalendarAction::PrevDay);
                            }
                            if toolbar_icon_button(ui, "▶").clicked() {
                                actions.push(CalendarAction::NextDay);
                            }
                        }
                        ViewMode::Week => {
                            if toolbar_icon_button(ui, "◀").clicked() {
                                actions.push(CalendarAction::PrevWeek);
                            }
                            if toolbar_icon_button(ui, "▶").clicked() {
                                actions.push(CalendarAction::NextWeek);
                            }
                        }
                        _ => {
                            if toolbar_icon_button(ui, "◀").clicked() {
                                actions.push(CalendarAction::PrevMonth);
                            }
                            if toolbar_icon_button(ui, "▶").clicked() {
                                actions.push(CalendarAction::NextMonth);
                            }
                        }
                    }
                });
            });

            strip.cell(|ui| {
                ui.set_min_height(TOOLBAR_HEIGHT);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(view_label.clone())
                            .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading3))
                            .strong(),
                    );
                });
            });

            strip.cell(|ui| {
                ui.set_min_height(TOOLBAR_HEIGHT);
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    if toolbar_button(ui, "➕ New Event").clicked() {
                        actions.push(CalendarAction::CreateEvent);
                    }
                    if toolbar_button(ui, "🔄 Refresh").clicked() {
                        actions.push(CalendarAction::RefreshEvents);
                    }

                    ui.add_space(8.0);

                    let current_view = calendar.view_mode().clone();
                    let view_modes = [
                        (ViewMode::List, "List"),
                        (ViewMode::Day, "Day"),
                        (ViewMode::Week, "Week"),
                        (ViewMode::Month, "Month"),
                    ];

                    for (mode, label) in view_modes {
                        let is_selected = current_view == mode;
                        let response = toolbar_toggle(ui, label, is_selected);
                        if response.clicked() && !is_selected {
                            actions.push(CalendarAction::ChangeView(mode));
                        }
                    }
                });
            });
        });

    ui.spacing_mut().item_spacing = previous_spacing;

    app_action
}

fn toolbar_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let text = RichText::new(label).size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading4));
    ui.add(
        Button::new(text)
            .min_size(Vec2::new(88.0, 32.0))
            .frame(false),
    )
}

fn toolbar_icon_button(ui: &mut egui::Ui, icon: &str) -> egui::Response {
    let text = RichText::new(icon).size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading3));
    ui.add(
        Button::new(text)
            .min_size(Vec2::new(36.0, 32.0))
            .frame(false),
    )
}

fn toolbar_toggle(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let response = ui.selectable_label(
        selected,
        RichText::new(label).size(get_font_size(ui.ctx(), &NotedeckTextStyle::Body)),
    );
    response
}

fn month_view_ui(calendar: &Calendar, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    let first_of_month = calendar.selected_date().with_day(1).unwrap();
    let days_from_monday = first_of_month.weekday().num_days_from_monday();

    let start_date = first_of_month - chrono::Duration::days(days_from_monday as i64);

    let cell_width = (ui.available_width() - SECTION_SPACING) / 7.0;
    let cell_size = Vec2::new(cell_width, 76.0);
    let header_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Heading4);
    let day_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Heading3);
    let helper_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Small);
    let visuals = ui.visuals().clone();

    ui.horizontal(|ui| {
        for day in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] {
            ui.allocate_ui(cell_size, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new(day)
                            .size(header_font)
                            .color(visuals.strong_text_color()),
                    );
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
                        .corner_radius(CornerRadius::same(CARD_CORNER_RADIUS))
                        .inner_margin(Margin::symmetric(8, 6))
                        .stroke(egui::Stroke::new(
                            1.0,
                            visuals.widgets.noninteractive.bg_stroke.color,
                        ));

                    let mut fill = visuals.panel_fill;
                    if !is_current_month {
                        fill = visuals.extreme_bg_color;
                    }
                    if is_selected {
                        fill = visuals.widgets.active.bg_fill;
                    }
                    if is_today {
                        fill = visuals.widgets.hovered.bg_fill;
                    }

                    frame = frame.fill(fill);

                    let response = frame.show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(current_date.day().to_string())
                                    .size(day_font)
                                    .color(if is_current_month {
                                        visuals.strong_text_color()
                                    } else {
                                        visuals.weak_text_color()
                                    }),
                            );

                            for (i, _event) in events_on_day.iter().take(3).enumerate() {
                                let color = match i % 3 {
                                    0 => visuals.widgets.active.bg_fill,
                                    1 => visuals.widgets.inactive.bg_fill,
                                    _ => visuals.widgets.hovered.bg_fill,
                                };

                                ui.add(
                                    egui::widgets::ProgressBar::new(1.0)
                                        .desired_width(cell_size.x - 10.0)
                                        .desired_height(4.0)
                                        .fill(color),
                                );
                            }

                            if events_on_day.len() > 3 {
                                ui.label(
                                    RichText::new(format!("+{} more", events_on_day.len() - 3))
                                        .size(helper_font)
                                        .color(visuals.weak_text_color()),
                                );
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
    let start_of_week = selected_date
        - chrono::Duration::days(selected_date.weekday().num_days_from_monday() as i64);

    let visuals = ui.visuals().clone();
    let header_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Heading4);
    let column_width = (ui.available_width() - SECTION_SPACING) / 7.0;

    ui.horizontal(|ui| {
        for day in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] {
            ui.allocate_ui(Vec2::new(column_width, 24.0), |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new(day)
                            .size(header_font)
                            .color(visuals.strong_text_color()),
                    );
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

            ui.allocate_ui(Vec2::new(column_width, 400.0), |ui| {
                let mut frame = Frame::new()
                    .corner_radius(CornerRadius::same(CARD_CORNER_RADIUS))
                    .inner_margin(Margin::symmetric(10, 8))
                    .stroke(egui::Stroke::new(
                        1.0,
                        visuals.widgets.noninteractive.bg_stroke.color,
                    ));

                if is_selected {
                    frame = frame.fill(visuals.widgets.active.bg_fill);
                } else if is_today {
                    frame = frame.fill(visuals.widgets.hovered.bg_fill);
                } else {
                    frame = frame.fill(visuals.panel_fill);
                }

                let mut event_clicked = false;
                let response = frame.show(ui, |ui| {
                    ui.vertical(|ui| {
                        let label_color = if is_today {
                            visuals.strong_text_color()
                        } else {
                            visuals.text_color()
                        };
                        let day_label_response = ui.label(
                            RichText::new(current_date.day().to_string())
                                .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading3))
                                .color(label_color),
                        );

                        ui.separator();

                        egui::ScrollArea::vertical().show(ui, |ui| {
                            let actions_before = actions.len();
                            for event in events_on_day {
                                mini_event_ui(event, ui, actions);
                            }
                            if actions.len() > actions_before {
                                for action in &actions[actions_before..] {
                                    if matches!(action, CalendarAction::SelectEvent(_)) {
                                        event_clicked = true;
                                        break;
                                    }
                                }
                            }
                        });

                        day_label_response
                    })
                    .inner
                });

                if !event_clicked && response.response.interact(Sense::click()).clicked() {
                    actions.push(CalendarAction::SelectDate(current_date));
                }
            });

            current_date += chrono::Duration::days(1);
        }
    });
}

fn mini_event_ui(
    event: &CalendarEventDisplay,
    ui: &mut egui::Ui,
    actions: &mut Vec<CalendarAction>,
) {
    let visuals = ui.visuals().clone();
    let response = Frame::new()
        .fill(visuals.widgets.inactive.bg_fill)
        .corner_radius(CornerRadius::same(CARD_CORNER_RADIUS))
        .inner_margin(Margin::symmetric(8, 6))
        .stroke(egui::Stroke::new(
            1.0,
            visuals.widgets.noninteractive.bg_stroke.color,
        ))
        .show(ui, |ui| {
            ui.label(
                RichText::new(&event.title)
                    .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading4)),
            );

            let time_str = match &event.start {
                EventTime::DateTime(ts, _) => chrono::DateTime::from_timestamp(*ts, 0)
                    .map(|dt| dt.format("%H:%M").to_string())
                    .unwrap_or_default(),
                _ => String::new(),
            };

            if !time_str.is_empty() {
                ui.label(
                    RichText::new(time_str)
                        .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Small)),
                );
            }
        });

    if response.response.interact(Sense::click()).clicked() {
        actions.push(CalendarAction::SelectEvent(
            event.note_key.as_u64().to_string(),
        ));
    }

    ui.add_space(2.0);
}

fn day_view_ui(calendar: &Calendar, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    ui.vertical(|ui| {
        let prev_spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.y = SECTION_SPACING;

        ui.label(
            RichText::new(calendar.selected_date().format("%A, %B %d, %Y").to_string())
                .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading2))
                .strong(),
        );

        ui.separator();

        if let Some(selected_key) = calendar.selected_event() {
            if let Some(event) = calendar
                .events()
                .iter()
                .find(|e| e.note_key == selected_key)
            {
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

        ui.spacing_mut().item_spacing = prev_spacing;
    });
}

fn list_view_ui(calendar: &Calendar, ui: &mut egui::Ui, actions: &mut Vec<CalendarAction>) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        let previous_spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.y = SECTION_SPACING;

        for event in calendar.events() {
            event_card_ui(event, ui, actions);
        }

        ui.spacing_mut().item_spacing = previous_spacing;
    });
}

fn event_card_ui(
    event: &CalendarEventDisplay,
    ui: &mut egui::Ui,
    actions: &mut Vec<CalendarAction>,
) {
    let visuals = ui.visuals().clone();
    let title_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Heading3);
    let body_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Body);
    let meta_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Small);

    let response = Frame::new()
        .corner_radius(CornerRadius::same(CARD_CORNER_RADIUS))
        .fill(visuals.panel_fill)
        .inner_margin(Margin::same(14))
        .stroke(egui::Stroke::new(
            1.0,
            visuals.widgets.noninteractive.bg_stroke.color,
        ))
        .show(ui, |ui| {
            ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let icon = match event.event_type {
                    EventType::DateBased => "📅",
                    EventType::TimeBased => "⏰",
                };
                ui.label(
                    RichText::new(icon).size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading2)),
                );

                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(&event.title)
                            .size(title_font)
                            .strong()
                            .color(visuals.strong_text_color()),
                    );

                    let time_str = format_event_time(&event.start, &event.end);
                    ui.label(
                        RichText::new(time_str)
                            .size(body_font)
                            .color(visuals.text_color()),
                    );

                    if !event.location.is_empty() {
                        let location_text = if let Some(ref geohash) = event.geohash {
                            format!("📍 {} ({})", event.location.join(", "), geohash)
                        } else {
                            format!("📍 {}", event.location.join(", "))
                        };
                        ui.label(
                            RichText::new(location_text)
                                .size(body_font)
                                .color(visuals.weak_text_color()),
                        );
                    }

                    if !event.participants.is_empty() {
                        let participant_count = event.participants.len();
                        let hosts: Vec<_> = event
                            .participants
                            .iter()
                            .filter(|p| p.role.as_deref() == Some("host"))
                            .collect();

                        let participant_text = if !hosts.is_empty() {
                            format!(
                                "👥 {} participants ({} hosts)",
                                participant_count,
                                hosts.len()
                            )
                        } else {
                            format!("👥 {} participants", participant_count)
                        };
                        ui.label(
                            RichText::new(participant_text)
                                .size(body_font)
                                .color(visuals.weak_text_color()),
                        );
                    }

                    if !event.hashtags.is_empty() {
                        ui.label(
                            RichText::new(format!("🏷️ {}", event.hashtags.join(" #")))
                                .size(body_font)
                                .color(visuals.strong_text_color()),
                        );
                    }

                    if !event.references.is_empty() {
                        ui.label(
                            RichText::new(format!("🔗 {} references", event.references.len()))
                                .size(body_font)
                                .color(visuals.weak_text_color()),
                        );
                    }

                    if !event.description.is_empty() {
                        ui.label(
                            RichText::new(&event.description)
                                .size(meta_font)
                                .color(visuals.weak_text_color()),
                        );
                    }
                });
            });
        });

    if response.response.interact(Sense::click()).clicked() {
        actions.push(CalendarAction::SelectEvent(
            event.note_key.as_u64().to_string(),
        ));
    }

    ui.add_space(5.0);
}

fn event_detail_ui(
    event: &CalendarEventDisplay,
    ui: &mut egui::Ui,
    actions: &mut Vec<CalendarAction>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        let previous_spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.y = SECTION_SPACING;

        if toolbar_button(ui, "← Back")
            .on_hover_text("Return to the selected day")
            .clicked()
        {
            actions.push(CalendarAction::SelectEvent(String::new()));
        }

        let visuals = ui.visuals().clone();
        let heading_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Heading2);
        let label_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Heading4);
        let body_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Body);
        let meta_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Small);

        Frame::new()
            .corner_radius(CornerRadius::same(CARD_CORNER_RADIUS))
            .fill(visuals.panel_fill)
            .inner_margin(Margin::same(18))
            .stroke(egui::Stroke::new(
                1.5,
                visuals.widgets.noninteractive.bg_stroke.color,
            ))
            .show(ui, |ui| {
                let icon = match event.event_type {
                    EventType::DateBased => "📅",
                    EventType::TimeBased => "⏰",
                };
                ui.label(
                    RichText::new(format!("{} {}", icon, &event.title))
                        .size(heading_font)
                        .strong(),
                );

                ui.add_space(SECTION_SPACING);
                ui.separator();
                ui.add_space(SECTION_SPACING);

                ui.label(
                    RichText::new("Time")
                        .strong()
                        .size(label_font)
                        .color(visuals.strong_text_color()),
                );
                let time_str = format_event_time(&event.start, &event.end);
                ui.label(
                    RichText::new(time_str)
                        .size(body_font)
                        .color(visuals.text_color()),
                );

                ui.add_space(SECTION_SPACING);

                if !event.location.is_empty() {
                    ui.label(
                        RichText::new("Location")
                            .strong()
                            .size(label_font)
                            .color(visuals.strong_text_color()),
                    );
                    for loc in &event.location {
                        ui.label(
                            RichText::new(format!("📍 {}", loc))
                                .size(body_font)
                                .color(visuals.text_color()),
                        );
                    }
                    if let Some(ref geohash) = event.geohash {
                        ui.label(
                            RichText::new(format!("Geohash: {}", geohash))
                                .size(meta_font)
                                .color(visuals.weak_text_color()),
                        );
                    }
                    ui.add_space(SECTION_SPACING);
                }

                if !event.description.is_empty() {
                    ui.label(
                        RichText::new("Description")
                            .strong()
                            .size(label_font)
                            .color(visuals.strong_text_color()),
                    );
                    ui.label(
                        RichText::new(&event.description)
                            .size(body_font)
                            .color(visuals.text_color()),
                    );
                    ui.add_space(SECTION_SPACING);
                }

                if !event.participants.is_empty() {
                    ui.label(
                        RichText::new("Participants")
                            .strong()
                            .size(label_font)
                            .color(visuals.strong_text_color()),
                    );
                    for participant in &event.participants {
                        let role = participant.role.as_deref().unwrap_or("participant");
                        ui.label(
                            RichText::new(format!("👤 {} ({})", &participant.pubkey[..16], role))
                                .size(body_font)
                                .color(visuals.text_color()),
                        );
                    }
                    ui.add_space(SECTION_SPACING);
                }

                if !event.hashtags.is_empty() {
                    ui.label(
                        RichText::new("Tags")
                            .strong()
                            .size(label_font)
                            .color(visuals.strong_text_color()),
                    );
                    ui.label(
                        RichText::new(format!("#{}", event.hashtags.join(" #")))
                            .size(body_font)
                            .color(visuals.strong_text_color()),
                    );
                    ui.add_space(SECTION_SPACING);
                }

                if !event.references.is_empty() {
                    ui.label(
                        RichText::new("References")
                            .strong()
                            .size(label_font)
                            .color(visuals.strong_text_color()),
                    );
                    for reference in &event.references {
                        ui.label(
                            RichText::new(format!("🔗 {}", reference))
                                .size(body_font)
                                .color(visuals.text_color()),
                        );
                    }
                    ui.add_space(SECTION_SPACING);
                }

                let accepted_rsvps: Vec<_> = event
                    .rsvps
                    .iter()
                    .filter(|r| matches!(r.status, crate::RsvpStatusType::Accepted))
                    .collect();

                if !accepted_rsvps.is_empty() {
                    ui.label(
                        RichText::new("Confirmed Attendees")
                            .strong()
                            .size(label_font)
                            .color(visuals.strong_text_color()),
                    );
                    ui.horizontal_wrapped(|ui| {
                        for rsvp in accepted_rsvps {
                            ui.label(
                                RichText::new(format!("✓ {}", hex::encode(&rsvp.pubkey[..8])))
                                    .size(body_font)
                                    .color(Color32::from_rgb(100, 200, 100)),
                            );
                        }
                    });
                    ui.add_space(SECTION_SPACING);
                }

                ui.separator();
                ui.add_space(SECTION_SPACING);

                ui.label(
                    RichText::new("RSVP to this event:")
                        .strong()
                        .size(label_font)
                        .color(visuals.strong_text_color()),
                );
                ui.horizontal(|ui| {
                    if ui.button("✓ Accept").clicked() {
                        actions.push(CalendarAction::SubmitRsvp(
                            event.note_key.as_u64().to_string(),
                            crate::RsvpStatusType::Accepted,
                        ));
                    }

                    if ui.button("? Tentative").clicked() {
                        actions.push(CalendarAction::SubmitRsvp(
                            event.note_key.as_u64().to_string(),
                            crate::RsvpStatusType::Tentative,
                        ));
                    }

                    if ui.button("✗ Decline").clicked() {
                        actions.push(CalendarAction::SubmitRsvp(
                            event.note_key.as_u64().to_string(),
                            crate::RsvpStatusType::Declined,
                        ));
                    }
                });

                ui.add_space(SECTION_SPACING);

                ui.label(
                    RichText::new(format!("Event ID: {}", event.d_tag))
                        .size(meta_font)
                        .color(visuals.weak_text_color()),
                );
            });

        ui.spacing_mut().item_spacing = previous_spacing;
    });
}

fn get_events_on_date(
    events: &[CalendarEventDisplay],
    date: NaiveDate,
) -> Vec<&CalendarEventDisplay> {
    events
        .iter()
        .filter(|event| match &event.start {
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
        })
        .collect()
}

fn format_event_time(start: &EventTime, end: &Option<EventTime>) -> String {
    match start {
        EventTime::Date(date) => {
            if let Some(EventTime::Date(end_date)) = end {
                format!(
                    "{} - {}",
                    date.format("%Y-%m-%d"),
                    end_date.format("%Y-%m-%d")
                )
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
                format!(
                    "{} - {} ({})",
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

fn event_creation_form_ui(
    calendar: &mut Calendar,
    ui: &mut egui::Ui,
    actions: &mut Vec<CalendarAction>,
) {
    ui.label(
        RichText::new("Create New Calendar Event")
            .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading2))
            .strong(),
    );
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        let form = calendar.event_form_mut();
        let previous_spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.y = SECTION_SPACING;
        let body_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Body);
        let hint_font = get_font_size(ui.ctx(), &NotedeckTextStyle::Small);

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Event Type:")
                    .size(body_font)
                    .color(ui.visuals().strong_text_color()),
            );
            ui.radio_value(
                &mut form.event_type,
                EventType::TimeBased,
                "Time-based (Kind 31923)",
            );
            ui.radio_value(
                &mut form.event_type,
                EventType::DateBased,
                "Date-based (Kind 31922)",
            );
        });

        ui.separator();

        ui.label(
            RichText::new("Title (required):")
                .size(body_font)
                .color(ui.visuals().strong_text_color()),
        );
        ui.add(
            TextEdit::singleline(&mut form.title)
                .hint_text("Summit planning session")
                .desired_width(f32::INFINITY),
        );

        ui.add_space(10.0);

        ui.label(
            RichText::new("Description (content):")
                .size(body_font)
                .color(ui.visuals().strong_text_color()),
        );
        ui.add(
            TextEdit::multiline(&mut form.description)
                .hint_text("Agenda, links, or summary for participants")
                .desired_width(f32::INFINITY)
                .desired_rows(4),
        );

        ui.add_space(10.0);

        ui.label(
            RichText::new("Start Date (YYYY-MM-DD):")
                .size(body_font)
                .color(ui.visuals().strong_text_color()),
        );
        ui.add(
            TextEdit::singleline(&mut form.start_date)
                .hint_text("2025-01-31")
                .desired_width(f32::INFINITY),
        );

        ui.label(
            RichText::new("Start Time (HH:MM, required):")
                .size(body_font)
                .color(ui.visuals().strong_text_color()),
        );
        ui.add(
            TextEdit::singleline(&mut form.start_time)
                .hint_text("14:30")
                .desired_width(f32::INFINITY),
        );

        ui.label(
            RichText::new("Timezone (IANA, e.g., America/New_York, UTC):")
                .size(body_font)
                .color(ui.visuals().strong_text_color()),
        );
        ui.add(
            TextEdit::singleline(&mut form.timezone)
                .hint_text("UTC")
                .desired_width(f32::INFINITY),
        );

        ui.add_space(10.0);

        ui.label(
            RichText::new("End Date (YYYY-MM-DD, optional):")
                .size(body_font)
                .color(ui.visuals().strong_text_color()),
        );
        ui.add(
            TextEdit::singleline(&mut form.end_date)
                .hint_text("2025-02-01")
                .desired_width(f32::INFINITY),
        );

        if matches!(form.event_type, EventType::TimeBased) {
            ui.label(
                RichText::new("End Time (HH:MM, optional):")
                    .size(body_font)
                    .color(ui.visuals().strong_text_color()),
            );
            ui.add(
                TextEdit::singleline(&mut form.end_time)
                    .hint_text("16:00")
                    .desired_width(f32::INFINITY),
            );
        }

        ui.add_space(10.0);

        ui.label(
            RichText::new("Location (optional):")
                .size(body_font)
                .color(ui.visuals().strong_text_color()),
        );
        ui.add(
            TextEdit::singleline(&mut form.location)
                .hint_text("River conference room / livestream link")
                .desired_width(f32::INFINITY),
        );

        ui.label(
            RichText::new("Geohash (optional):")
                .size(body_font)
                .color(ui.visuals().strong_text_color()),
        );
        ui.add(
            TextEdit::singleline(&mut form.geohash)
                .hint_text("9q8yyk8v")
                .desired_width(f32::INFINITY),
        );

        ui.add_space(10.0);

        ui.label(
            RichText::new("Hashtags (space-separated, optional):")
                .size(body_font)
                .color(ui.visuals().strong_text_color()),
        );
        ui.add(
            TextEdit::singleline(&mut form.hashtags)
                .hint_text("#nostr #meetup")
                .desired_width(f32::INFINITY),
        );

        ui.label(
            RichText::new("References (comma-separated URLs, optional):")
                .size(body_font)
                .color(ui.visuals().strong_text_color()),
        );
        ui.add(
            TextEdit::singleline(&mut form.references)
                .hint_text("https://example.com/info, https://join.link")
                .desired_width(f32::INFINITY),
        );

        ui.add_space(20.0);

        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                actions.push(CalendarAction::CancelEventCreation);
            }

            if ui.button("Create Event").clicked() {
                actions.push(CalendarAction::SubmitEvent(form_to_creation_data(&form)));
            }
        });

        ui.label(
            RichText::new("Fields marked optional will be omitted if left blank.")
                .size(hint_font)
                .color(ui.visuals().weak_text_color()),
        );

        ui.spacing_mut().item_spacing = previous_spacing;
    });
}

fn form_to_creation_data(form: &crate::EventFormData) -> EventCreationData {
    EventCreationData {
        event_type: form.event_type.clone(),
        title: form.title.clone(),
        description: form.description.clone(),
        start_date: NaiveDate::parse_from_str(&form.start_date, "%Y-%m-%d").ok(),
        start_time: if form.start_time.is_empty() {
            None
        } else {
            Some(form.start_time.clone())
        },
        end_date: NaiveDate::parse_from_str(&form.end_date, "%Y-%m-%d").ok(),
        end_time: if form.end_time.is_empty() {
            None
        } else {
            Some(form.end_time.clone())
        },
        timezone: if form.timezone.is_empty() {
            None
        } else {
            Some(form.timezone.clone())
        },
        location: form.location.clone(),
        geohash: form.geohash.clone(),
        hashtags: form.hashtags.clone(),
        references: form.references.clone(),
    }
}
