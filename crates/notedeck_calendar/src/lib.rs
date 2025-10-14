use chrono::{Datelike, Local, NaiveDate, TimeZone};
use enostr::{ClientMessage, RelayEvent, RelayMessage};
use notedeck::{AppContext, AppResponse};
use nostrdb::{Filter, Note, NoteKey, Transaction};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

pub use ui::{CalendarAction, CalendarResponse, CalendarUi};

mod ui;

pub struct Calendar {
    selected_date: NaiveDate,
    view_mode: ViewMode,
    events: Vec<CalendarEventDisplay>,
    calendars: Vec<CalendarInfo>,
    selected_calendar: Option<String>,
    creating_event: bool,
    event_form: EventFormData,
    subscribed: bool,
    feedback_message: Option<String>,
    selected_event: Option<NoteKey>,
}

#[derive(Debug, Clone, Default)]
pub struct EventFormData {
    pub event_type: EventType,
    pub title: String,
    pub description: String,
    pub start_date: String,
    pub start_time: String,
    pub end_date: String,
    pub end_time: String,
    pub timezone: String,
    pub location: String,
    pub geohash: String,
    pub hashtags: String,
    pub references: String,
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
    pub geohash: Option<String>,
    pub participants: Vec<Participant>,
    pub hashtags: Vec<String>,
    pub references: Vec<String>,
    pub description: String,
    pub d_tag: String,
    pub author_pubkey: [u8; 32],
    pub rsvps: Vec<RsvpInfo>,
}

#[derive(Debug, Clone)]
pub struct RsvpInfo {
    pub pubkey: [u8; 32],
    pub status: RsvpStatusType,
    pub note_key: NoteKey,
}

#[derive(Debug, Clone)]
pub struct Participant {
    pub pubkey: String,
    pub relay: Option<String>,
    pub role: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum EventType {
    #[default]
    TimeBased,
    DateBased,
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
        let today = Local::now().date_naive();
        Calendar {
            selected_date: today,
            view_mode: ViewMode::Month,
            events: Vec::new(),
            calendars: Vec::new(),
            selected_calendar: None,
            creating_event: false,
            event_form: EventFormData {
                start_date: today.format("%Y-%m-%d").to_string(),
                end_date: today.format("%Y-%m-%d").to_string(),
                timezone: "UTC".to_string(),
                ..Default::default()
            },
            subscribed: false,
            feedback_message: None,
            selected_event: None,
        }
    }

    pub fn creating_event(&self) -> bool {
        self.creating_event
    }

    pub fn event_form(&self) -> &EventFormData {
        &self.event_form
    }

    pub fn event_form_mut(&mut self) -> &mut EventFormData {
        &mut self.event_form
    }

    pub fn start_creating_event(&mut self) {
        self.creating_event = true;
        self.event_form = EventFormData {
            start_date: self.selected_date.format("%Y-%m-%d").to_string(),
            end_date: self.selected_date.format("%Y-%m-%d").to_string(),
            timezone: "UTC".to_string(),
            ..Default::default()
        };
    }

    pub fn cancel_creating_event(&mut self) {
        self.creating_event = false;
    }

    pub fn selected_date(&self) -> NaiveDate {
        self.selected_date
    }

    pub fn feedback_message(&self) -> Option<&String> {
        self.feedback_message.as_ref()
    }

    pub fn set_feedback(&mut self, msg: String) {
        self.feedback_message = Some(msg);
    }

    pub fn clear_feedback(&mut self) {
        self.feedback_message = None;
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

    pub fn selected_event(&self) -> Option<NoteKey> {
        self.selected_event
    }

    pub fn set_selected_event(&mut self, note_key: Option<NoteKey>) {
        self.selected_event = note_key;
    }

    pub fn set_selected_date(&mut self, date: NaiveDate, app_ctx: &mut AppContext) {
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

    pub fn next_day(&mut self, app_ctx: &mut AppContext) {
        if let Some(next) = self.selected_date.checked_add_days(chrono::Days::new(1)) {
            let month_changed = self.selected_date.month() != next.month() || self.selected_date.year() != next.year();
            self.selected_date = next;
            if month_changed {
                self.load_events(app_ctx);
            }
        }
    }

    pub fn prev_day(&mut self, app_ctx: &mut AppContext) {
        if let Some(prev) = self.selected_date.checked_sub_days(chrono::Days::new(1)) {
            let month_changed = self.selected_date.month() != prev.month() || self.selected_date.year() != prev.year();
            self.selected_date = prev;
            if month_changed {
                self.load_events(app_ctx);
            }
        }
    }

    pub fn next_week(&mut self, app_ctx: &mut AppContext) {
        if let Some(next) = self.selected_date.checked_add_days(chrono::Days::new(7)) {
            let month_changed = self.selected_date.month() != next.month() || self.selected_date.year() != next.year();
            self.selected_date = next;
            if month_changed {
                self.load_events(app_ctx);
            }
        }
    }

    pub fn prev_week(&mut self, app_ctx: &mut AppContext) {
        if let Some(prev) = self.selected_date.checked_sub_days(chrono::Days::new(7)) {
            let month_changed = self.selected_date.month() != prev.month() || self.selected_date.year() != prev.year();
            self.selected_date = prev;
            if month_changed {
                self.load_events(app_ctx);
            }
        }
    }

    pub fn load_events(&mut self, app_ctx: &mut AppContext) {
        if !self.subscribed {
            let event_filter = Filter::new()
                .kinds(vec![31922, 31923])
                .build();
            
            let rsvp_filter = Filter::new()
                .kinds(vec![31925])
                .build();
            
            let sub_id = "calendar-events".to_string();
            let msg = ClientMessage::req(sub_id, vec![event_filter, rsvp_filter]);
            app_ctx.pool.send(&msg);
            self.subscribed = true;
        }

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
                    if let Some(mut event) = Self::parse_calendar_event(&note) {
                        if Self::event_intersects_range(&event, view_start, view_end) {
                            event.rsvps = Self::load_rsvps_for_event(app_ctx, &event, &txn);
                            self.events.push(event);
                        }
                    }
                }
            }
        };
    }

    fn load_rsvps_for_event(app_ctx: &mut AppContext, event: &CalendarEventDisplay, txn: &Transaction) -> Vec<RsvpInfo> {
        let mut rsvps = Vec::new();
        
        let event_kind = match event.event_type {
            EventType::DateBased => 31922,
            EventType::TimeBased => 31923,
        };
        
        let event_author_hex = hex::encode(event.author_pubkey);
        let event_coord = format!("{}:{}:{}", event_kind, event_author_hex, event.d_tag);
        
        let rsvp_filter = Filter::new()
            .kinds(vec![31925])
            .tags([event_coord.as_str()], 'a')
            .limit(500)
            .build();
        
        if let Ok(results) = app_ctx.ndb.query(txn, &[rsvp_filter], 500) {
            for result in results {
                if let Ok(note) = app_ctx.ndb.get_note_by_key(txn, result.note_key) {
                    if let Some(rsvp_info) = Self::parse_rsvp(&note, &event_coord) {
                        rsvps.push(rsvp_info);
                    }
                }
            }
        }
        
        rsvps
    }

    fn parse_rsvp(note: &Note, event_coord: &str) -> Option<RsvpInfo> {
        let mut references_event = false;
        let mut status = None;
        
        for tag in note.tags() {
            if tag.count() < 2 {
                continue;
            }
            
            match tag.get_str(0) {
                Some("a") => {
                    if let Some(val) = tag.get_str(1) {
                        if val == event_coord {
                            references_event = true;
                        }
                    }
                }
                Some("status") => {
                    if let Some(val) = tag.get_str(1) {
                        status = match val {
                            "accepted" => Some(RsvpStatusType::Accepted),
                            "declined" => Some(RsvpStatusType::Declined),
                            "tentative" => Some(RsvpStatusType::Tentative),
                            _ => None,
                        };
                    }
                }
                _ => {}
            }
        }
        
        if references_event && status.is_some() {
            Some(RsvpInfo {
                pubkey: *note.pubkey(),
                status: status.unwrap(),
                note_key: note.key().expect("Note should have key"),
            })
        } else {
            None
        }
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

    pub fn load_calendars(&mut self, app_ctx: &mut AppContext) {
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
        let mut start_tzid: Option<String> = None;
        let mut end_tzid: Option<String> = None;
        let mut location = Vec::new();
        let mut geohash: Option<String> = None;
        let mut participants = Vec::new();
        let mut hashtags = Vec::new();
        let mut references = Vec::new();
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
                        start = Self::parse_event_time(val, kind, None);
                    }
                }
                Some("end") => {
                    if let Some(val) = tag.get_str(1) {
                        end = Self::parse_event_time(val, kind, None);
                    }
                }
                Some("start_tzid") => {
                    if let Some(val) = tag.get_str(1) {
                        start_tzid = Some(val.to_string());
                    }
                }
                Some("end_tzid") => {
                    if let Some(val) = tag.get_str(1) {
                        end_tzid = Some(val.to_string());
                    }
                }
                Some("location") => {
                    if let Some(val) = tag.get_str(1) {
                        location.push(val.to_string());
                    }
                }
                Some("g") => {
                    if let Some(val) = tag.get_str(1) {
                        geohash = Some(val.to_string());
                    }
                }
                Some("p") => {
                    if let Some(pubkey) = tag.get_str(1) {
                        participants.push(Participant {
                            pubkey: pubkey.to_string(),
                            relay: tag.get_str(2).map(|s| s.to_string()),
                            role: tag.get_str(3).map(|s| s.to_string()),
                        });
                    }
                }
                Some("t") => {
                    if let Some(val) = tag.get_str(1) {
                        hashtags.push(val.to_string());
                    }
                }
                Some("r") => {
                    if let Some(val) = tag.get_str(1) {
                        references.push(val.to_string());
                    }
                }
                _ => {}
            }
        }

        if let Some(mut s) = start {
            if let EventTime::DateTime(ts, tz_ref) = &mut s {
                *tz_ref = start_tzid.clone();
            }
            
            if let Some(EventTime::DateTime(_, end_tz_ref)) = &mut end {
                *end_tz_ref = end_tzid.or_else(|| start_tzid.clone());
            }

            Some(CalendarEventDisplay {
                note_key: note.key().expect("Note should have key"),
                event_type,
                title,
                start: s,
                end,
                location,
                geohash,
                participants,
                hashtags,
                references,
                description: note.content().to_string(),
                d_tag,
                author_pubkey: *note.pubkey(),
                rsvps: Vec::new(),
            })
        } else {
            None
        }
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

    pub fn create_nip52_event(app_ctx: &mut AppContext, data: &crate::ui::calendar::EventCreationData) -> Option<String> {
        use uuid::Uuid;
        use nostrdb::NoteBuilder;
        use enostr::ClientMessage;

        let Some(filled_keypair) = app_ctx.accounts.selected_filled() else {
            warn!("Cannot create event: No account selected");
            return None;
        };

        if data.title.is_empty() {
            warn!("Cannot create event: Title is required");
            return None;
        }

        if data.start_date.is_none() {
            warn!("Cannot create event: Start date is required");
            return None;
        }

        if data.start_time.is_none() {
            warn!("Cannot create event: Start time is required");
            return None;
        }

        let kind = match data.event_type {
            EventType::DateBased => 31922,
            EventType::TimeBased => 31923,
        };

        let d_tag = Uuid::new_v4().to_string();

        let mut builder = NoteBuilder::new()
            .kind(kind)
            .content(&data.description);

        builder = builder.start_tag().tag_str("d").tag_str(&d_tag);

        if !data.title.is_empty() {
            builder = builder.start_tag().tag_str("title").tag_str(&data.title);
        }

        if let Some(start_date) = &data.start_date {
            match data.event_type {
                EventType::DateBased => {
                    let start_str = start_date.format("%Y-%m-%d").to_string();
                    builder = builder.start_tag().tag_str("start").tag_str(&start_str);

                    if let Some(end_date) = &data.end_date {
                        let end_str = end_date.format("%Y-%m-%d").to_string();
                        builder = builder.start_tag().tag_str("end").tag_str(&end_str);
                    }
                }
                EventType::TimeBased => {
                    if let Some(start_time) = &data.start_time {
                        if let Ok(time) = chrono::NaiveTime::parse_from_str(start_time, "%H:%M") {
                            let datetime = start_date.and_time(time);
                            
                            let timestamp = if let Some(ref tz_name) = data.timezone {
                                if !tz_name.is_empty() {
                                    if let Ok(tz) = tz_name.parse::<chrono_tz::Tz>() {
                                        match tz.from_local_datetime(&datetime) {
                                            chrono::LocalResult::Single(dt) => dt.timestamp(),
                                            chrono::LocalResult::Ambiguous(dt, _) => dt.timestamp(),
                                            chrono::LocalResult::None => {
                                                datetime.and_utc().timestamp()
                                            }
                                        }
                                    } else {
                                        datetime.and_utc().timestamp()
                                    }
                                } else {
                                    datetime.and_utc().timestamp()
                                }
                            } else {
                                datetime.and_utc().timestamp()
                            };
                            
                            builder = builder.start_tag().tag_str("start").tag_str(&timestamp.to_string());

                            if let Some(tz_val) = &data.timezone {
                                if !tz_val.is_empty() {
                                    builder = builder.start_tag().tag_str("start_tzid").tag_str(tz_val);
                                }
                            }

                            if let Some(end_date) = &data.end_date {
                                if let Some(end_time) = &data.end_time {
                                    if let Ok(end_time_parsed) = chrono::NaiveTime::parse_from_str(end_time, "%H:%M") {
                                        let end_datetime = end_date.and_time(end_time_parsed);
                                        
                                        let end_timestamp = if let Some(ref tz_name) = data.timezone {
                                            if !tz_name.is_empty() {
                                                if let Ok(tz) = tz_name.parse::<chrono_tz::Tz>() {
                                                    match tz.from_local_datetime(&end_datetime) {
                                                        chrono::LocalResult::Single(dt) => dt.timestamp(),
                                                        chrono::LocalResult::Ambiguous(dt, _) => dt.timestamp(),
                                                        chrono::LocalResult::None => {
                                                            end_datetime.and_utc().timestamp()
                                                        }
                                                    }
                                                } else {
                                                    end_datetime.and_utc().timestamp()
                                                }
                                            } else {
                                                end_datetime.and_utc().timestamp()
                                            }
                                        } else {
                                            end_datetime.and_utc().timestamp()
                                        };
                                        
                                        builder = builder.start_tag().tag_str("end").tag_str(&end_timestamp.to_string());

                                        if let Some(tz_val) = &data.timezone {
                                            if !tz_val.is_empty() {
                                                builder = builder.start_tag().tag_str("end_tzid").tag_str(tz_val);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if !data.location.is_empty() {
            for loc in data.location.split(',').map(|s| s.trim()) {
                if !loc.is_empty() {
                    builder = builder.start_tag().tag_str("location").tag_str(loc);
                }
            }
        }

        if !data.geohash.is_empty() {
            builder = builder.start_tag().tag_str("g").tag_str(&data.geohash);
        }

        if !data.hashtags.is_empty() {
            for tag in data.hashtags.split_whitespace() {
                builder = builder.start_tag().tag_str("t").tag_str(tag);
            }
        }

        if !data.references.is_empty() {
            for reference in data.references.split(',').map(|s| s.trim()) {
                if !reference.is_empty() {
                    builder = builder.start_tag().tag_str("r").tag_str(reference);
                }
            }
        }

        let note = builder
            .sign(&filled_keypair.secret_key.secret_bytes())
            .build()?;

        let msg = ClientMessage::event(&note).ok()?;
        app_ctx.pool.send(&msg);

        info!("Calendar event created and sent to relays: {}", d_tag);

        Some(d_tag)
    }

    pub fn create_rsvp(app_ctx: &mut AppContext, event: &CalendarEventDisplay, status: RsvpStatusType) -> Option<String> {
        use uuid::Uuid;
        use nostrdb::NoteBuilder;
        use enostr::ClientMessage;

        let Some(filled_keypair) = app_ctx.accounts.selected_filled() else {
            warn!("Cannot create RSVP: No account selected");
            return None;
        };

        let event_kind = match event.event_type {
            EventType::DateBased => 31922,
            EventType::TimeBased => 31923,
        };

        let event_author_hex = hex::encode(event.author_pubkey);
        let event_coord = format!("{}:{}:{}", event_kind, event_author_hex, event.d_tag);

        let d_tag = Uuid::new_v4().to_string();

        let status_str = match status {
            RsvpStatusType::Accepted => "accepted",
            RsvpStatusType::Declined => "declined",
            RsvpStatusType::Tentative => "tentative",
        };

        let mut builder = NoteBuilder::new()
            .kind(31925)
            .content("");

        builder = builder.start_tag().tag_str("d").tag_str(&d_tag);
        builder = builder.start_tag().tag_str("a").tag_str(&event_coord);
        builder = builder.start_tag().tag_str("status").tag_str(status_str);

        let note = builder
            .sign(&filled_keypair.secret_key.secret_bytes())
            .build()?;

        let msg = ClientMessage::event(&note).ok()?;
        app_ctx.pool.send(&msg);

        info!("RSVP created and sent to relays: {} for event {}", status_str, event.d_tag);

        Some(d_tag)
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

fn process_relay_messages(ctx: &mut AppContext<'_>) -> bool {
    let mut received_events = false;
    
    loop {
        let ev = if let Some(ev) = ctx.pool.try_recv() {
            ev.into_owned()
        } else {
            break;
        };

        match (&ev.event).into() {
            RelayEvent::Opened => {
                info!("Calendar: relay {} opened", &ev.relay);
            }
            RelayEvent::Closed => {
                warn!("Calendar: relay {} closed", &ev.relay);
            }
            RelayEvent::Error(e) => {
                error!("Calendar: relay {} error: {}", &ev.relay, e);
            }
            RelayEvent::Other(_) => {}
            RelayEvent::Message(msg) => {
                if process_relay_message(ctx, &ev.relay, &msg) {
                    received_events = true;
                }
            }
        }
    }
    
    received_events
}

fn process_relay_message(ctx: &mut AppContext<'_>, relay_url: &str, msg: &RelayMessage) -> bool {
    match msg {
        RelayMessage::Event(subid, ev) => {
            if subid == &"calendar-events" {
                if let Err(err) = ctx.ndb.process_event_with(
                    ev,
                    nostrdb::IngestMetadata::new()
                        .client(false)
                        .relay(relay_url),
                ) {
                    error!("error processing calendar event: {}", err);
                    false
                } else {
                    info!("Calendar: received event from {}", relay_url);
                    true
                }
            } else {
                false
            }
        }
        RelayMessage::Notice(msg) => {
            info!("Notice from {}: {}", relay_url, msg);
            false
        }
        RelayMessage::OK(_cr) => {
            false
        }
        RelayMessage::Eose(sid) => {
            info!("EOSE for subscription {} from {}", sid, relay_url);
            false
        }
    }
}

impl notedeck::App for Calendar {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let received_events = process_relay_messages(ctx);
        
        // Initial load: send subscription and load events
        if !self.subscribed {
            self.load_events(ctx);
        } else if received_events {
            // Reload events when new calendar events arrive from relays
            self.load_events(ctx);
        }
        
        CalendarUi::ui(self, ctx, ui)
    }
}
