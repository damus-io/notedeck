use chrono::{Datelike, Local, NaiveDate, TimeZone};
use notedeck::{AppContext, AppResponse};
use nostrdb::{Filter, Note, NoteKey, Transaction};
use serde::{Deserialize, Serialize};

pub use ui::{CalendarAction, CalendarResponse, CalendarUi};

mod ui;

pub struct Calendar {
    selected_date: NaiveDate,
    view_mode: ViewMode,
    events: Vec<CalendarEventDisplay>,
    calendars: Vec<CalendarInfo>,
    selected_calendar: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewMode {
    Month,
    Week,
    Day,
    List,
}

#[derive(Debug, Clone)]
pub struct CalendarEventDisplay {
    pub note_key: NoteKey,
    pub event_type: EventType,
    pub title: String,
    pub start: EventTime,
    pub end: Option<EventTime>,
    pub location: Vec<String>,
    pub participants: Vec<String>,
    pub description: String,
    pub d_tag: String,
}

#[derive(Debug, Clone)]
pub enum EventType {
    DateBased,
    TimeBased,
}

#[derive(Debug, Clone)]
pub enum EventTime {
    Date(NaiveDate),
    DateTime(i64, Option<String>),
}

#[derive(Debug, Clone)]
pub struct CalendarInfo {
    pub note_key: NoteKey,
    pub title: String,
    pub description: String,
    pub d_tag: String,
    pub event_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RsvpStatus {
    pub event_id: String,
    pub status: RsvpStatusType,
    pub free_busy: Option<FreeBusyStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RsvpStatusType {
    Accepted,
    Declined,
    Tentative,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FreeBusyStatus {
    Free,
    Busy,
}

impl Calendar {
    pub fn new() -> Self {
        Calendar {
            selected_date: Local::now().date_naive(),
            view_mode: ViewMode::Month,
            events: Vec::new(),
            calendars: Vec::new(),
            selected_calendar: None,
        }
    }

    pub fn selected_date(&self) -> NaiveDate {
        self.selected_date
    }

    pub fn view_mode(&self) -> &ViewMode {
        &self.view_mode
    }

    pub fn events(&self) -> &[CalendarEventDisplay] {
        &self.events
    }

    pub fn calendars(&self) -> &[CalendarInfo] {
        &self.calendars
    }

    pub fn set_selected_date(&mut self, date: NaiveDate, app_ctx: &AppContext) {
        let month_changed = self.selected_date.month() != date.month() || self.selected_date.year() != date.year();
        self.selected_date = date;
        if month_changed {
            self.load_events(app_ctx);
        }
    }

    pub fn set_view_mode(&mut self, mode: ViewMode) {
        self.view_mode = mode;
    }

    pub fn next_month(&mut self) {
        if let Some(next) = self.selected_date.checked_add_months(chrono::Months::new(1)) {
            self.selected_date = next;
        }
    }

    pub fn prev_month(&mut self) {
        if let Some(prev) = self.selected_date.checked_sub_months(chrono::Months::new(1)) {
            self.selected_date = prev;
        }
    }

    pub fn load_events(&mut self, app_ctx: &AppContext) {
        let start_of_month = self.selected_date
            .with_day(1)
            .expect("Failed to get start of month");
        
        let end_of_month = if let Some(next_month) = start_of_month.checked_add_months(chrono::Months::new(1)) {
            next_month
        } else {
            start_of_month
        };

        let days_from_monday = start_of_month.weekday().num_days_from_monday();
        let expanded_start = start_of_month - chrono::Duration::days(days_from_monday as i64);
        let expanded_end = end_of_month + chrono::Duration::days(7);

        let view_start = Local.from_local_datetime(
            &expanded_start.and_hms_opt(0, 0, 0).unwrap()
        ).unwrap().timestamp();
        
        let view_end = Local.from_local_datetime(
            &expanded_end.and_hms_opt(23, 59, 59).unwrap()
        ).unwrap().timestamp();

        let filter = Filter::new()
            .kinds(vec![31922, 31923])
            .limit(5000)
            .build();

        let txn = Transaction::new(app_ctx.ndb).expect("Failed to create transaction");
        
        if let Ok(results) = app_ctx.ndb.query(&txn, &[filter], 5000) {
            self.events.clear();
            
            for result in results {
                if let Ok(note) = app_ctx.ndb.get_note_by_key(&txn, result.note_key) {
                    if let Some(event) = Self::parse_calendar_event(&note) {
                        if Self::event_intersects_range(&event, view_start, view_end) {
                            self.events.push(event);
                        }
                    }
                }
            }
        };
    }

    fn event_intersects_range(event: &CalendarEventDisplay, view_start: i64, view_end: i64) -> bool {
        match &event.start {
            EventTime::Date(start_date) => {
                let event_start = Local.from_local_datetime(
                    &start_date.and_hms_opt(0, 0, 0).unwrap()
                ).unwrap().timestamp();
                
                let event_end = if let Some(EventTime::Date(end_date)) = &event.end {
                    Local.from_local_datetime(
                        &end_date.and_hms_opt(23, 59, 59).unwrap()
                    ).unwrap().timestamp()
                } else {
                    event_start + 86400
                };
                
                event_start <= view_end && event_end >= view_start
            }
            EventTime::DateTime(start_ts, _) => {
                let event_end = if let Some(EventTime::DateTime(end_ts, _)) = &event.end {
                    *end_ts
                } else {
                    start_ts + 3600
                };
                
                *start_ts <= view_end && event_end >= view_start
            }
        }
    }

    pub fn load_calendars(&mut self, app_ctx: &AppContext) {
        let filter = Filter::new()
            .kinds(vec![31924])
            .limit(100)
            .build();

        let txn = Transaction::new(app_ctx.ndb).expect("Failed to create transaction");

        if let Ok(results) = app_ctx.ndb.query(&txn, &[filter], 100) {
            self.calendars.clear();
            
            for result in results {
                if let Ok(note) = app_ctx.ndb.get_note_by_key(&txn, result.note_key) {
                    if let Some(calendar) = Self::parse_calendar(&note) {
                        self.calendars.push(calendar);
                    }
                }
            }
        };
    }

    fn parse_calendar_event(note: &Note) -> Option<CalendarEventDisplay> {
        let kind = note.kind();
        let event_type = match kind {
            31922 => EventType::DateBased,
            31923 => EventType::TimeBased,
            _ => return None,
        };

        let mut title = String::new();
        let mut start: Option<EventTime> = None;
        let mut end: Option<EventTime> = None;
        let mut location = Vec::new();
        let mut participants = Vec::new();
        let mut d_tag = String::new();

        for tag in note.tags() {
            if tag.count() < 2 {
                continue;
            }

            match tag.get_str(0) {
                Some("d") => {
                    if let Some(val) = tag.get_str(1) {
                        d_tag = val.to_string();
                    }
                }
                Some("title") => {
                    if let Some(val) = tag.get_str(1) {
                        title = val.to_string();
                    }
                }
                Some("start") => {
                    if let Some(val) = tag.get_str(1) {
                        start = Self::parse_event_time(val, kind, tag.get_str(2));
                    }
                }
                Some("end") => {
                    if let Some(val) = tag.get_str(1) {
                        end = Self::parse_event_time(val, kind, tag.get_str(2));
                    }
                }
                Some("location") => {
                    if let Some(val) = tag.get_str(1) {
                        location.push(val.to_string());
                    }
                }
                Some("p") => {
                    if let Some(val) = tag.get_str(1) {
                        participants.push(val.to_string());
                    }
                }
                _ => {}
            }
        }

        start.map(|s| CalendarEventDisplay {
            note_key: note.key().expect("Note should have key"),
            event_type,
            title,
            start: s,
            end,
            location,
            participants,
            description: note.content().to_string(),
            d_tag,
        })
    }

    fn parse_event_time(time_str: &str, kind: u32, tz_str: Option<&str>) -> Option<EventTime> {
        if kind == 31922 {
            NaiveDate::parse_from_str(time_str, "%Y-%m-%d")
                .ok()
                .map(EventTime::Date)
        } else {
            time_str.parse::<i64>()
                .ok()
                .map(|ts| EventTime::DateTime(ts, tz_str.map(|s| s.to_string())))
        }
    }

    fn parse_calendar(note: &Note) -> Option<CalendarInfo> {
        let mut title = String::new();
        let mut d_tag = String::new();
        let mut event_count = 0;

        for tag in note.tags() {
            if tag.count() < 2 {
                continue;
            }

            match tag.get_str(0) {
                Some("d") => {
                    if let Some(val) = tag.get_str(1) {
                        d_tag = val.to_string();
                    }
                }
                Some("title") => {
                    if let Some(val) = tag.get_str(1) {
                        title = val.to_string();
                    }
                }
                Some("a") => {
                    event_count += 1;
                }
                _ => {}
            }
        }

        if title.is_empty() {
            return None;
        }

        Some(CalendarInfo {
            note_key: note.key().expect("Note should have key"),
            title,
            description: note.content().to_string(),
            d_tag,
            event_count,
        })
    }
}

impl Default for Calendar {
    fn default() -> Self {
        Self::new()
    }
}

impl notedeck::App for Calendar {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        CalendarUi::ui(self, ctx, ui)
    }
}
