mod model;

use chrono::{
    offset::Offset, DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime,
    TimeZone, Timelike, Utc,
};
use chrono_tz::{Tz, TZ_VARIANTS};
use egui::{scroll_area::ScrollAreaOutput, vec2, Color32, CornerRadius, FontId, Stroke};
use hex::FromHex;
use nostrdb::{Filter, IngestMetadata, Note, ProfileRecord, Transaction};
use notedeck::enostr::ClientMessage;
use notedeck::filter::UnifiedSubscription;
use notedeck::media::gif::ensure_latest_texture;
use notedeck::media::{AnimationMode, ImageType};
use notedeck::{
    fonts::NamedFontFamily, get_render_state, supported_mime_hosted_at_url, App, AppAction,
    AppContext, AppResponse, MediaCacheType, TextureState,
};
use notedeck_ui::ProfilePic;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};
use std::{borrow::Cow, collections::{HashMap, HashSet}};
use tracing::warn;
use uuid::Uuid;

use crate::model::{
    event_naddr, event_nevent, match_rsvps_for_event, parse_calendar_event, parse_calendar_rsvp,
    wrap_title, CalendarEvent, CalendarEventTime, CalendarParticipant, CalendarRsvp, RsvpFeedback,
    RsvpStatus,
};

const FETCH_LIMIT: i32 = 1024;
const POLL_BATCH_SIZE: usize = 64;
const POLL_INTERVAL: StdDuration = StdDuration::from_secs(5);
const RSVP_FEEDBACK_TTL: StdDuration = StdDuration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalendarView {
    Month,
    Week,
    Day,
    Event,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TimeZoneChoice {
    Local,
    Named(Tz),
}

impl Default for TimeZoneChoice {
    fn default() -> Self {
        Self::Local
    }
}

#[derive(Clone)]
pub(crate) struct LocalizedDateTime {
    date: NaiveDate,
    date_text: String,
    time_text: String,
    abbreviation: String,
    time_of_day: NaiveTime,
}

impl TimeZoneChoice {
    fn localize(&self, utc: &NaiveDateTime) -> LocalizedDateTime {
        let dt_utc = Utc.from_utc_datetime(utc);
        match self {
            TimeZoneChoice::Local => {
                let dt_local = dt_utc.with_timezone(&Local);
                LocalizedDateTime {
                    date: dt_local.date_naive(),
                    date_text: dt_local.format("%b %e, %Y").to_string(),
                    time_text: dt_local.format("%I:%M %p").to_string(),
                    abbreviation: dt_local.format("%Z").to_string(),
                    time_of_day: dt_local.time(),
                }
            }
            TimeZoneChoice::Named(tz) => {
                let dt_named = dt_utc.with_timezone(tz);
                LocalizedDateTime {
                    date: dt_named.date_naive(),
                    date_text: dt_named.format("%b %e, %Y").to_string(),
                    time_text: dt_named.format("%I:%M %p").to_string(),
                    abbreviation: dt_named.format("%Z").to_string(),
                    time_of_day: dt_named.time(),
                }
            }
        }
    }

    fn label(&self) -> String {
        match self {
            TimeZoneChoice::Local => {
                let now = Local::now();
                let abbr = now.format("%Z").to_string();
                if abbreviation_has_letters(&abbr) {
                    format!("Local Time ({abbr})")
                } else if let Some(tz) = guess_local_timezone(now) {
                    format!("Local Time ({})", timezone_abbreviation(tz))
                } else {
                    let offset = now.offset().local_minus_utc();
                    format!("Local Time ({})", format_utc_offset(offset))
                }
            }
            TimeZoneChoice::Named(tz) => {
                format!("{} ({})", tz.name(), timezone_abbreviation(*tz))
            }
        }
    }
}

pub struct CalendarApp {
    subscription: Option<UnifiedSubscription>,
    events: Vec<CalendarEvent>,
    all_rsvps: HashMap<String, CalendarRsvp>,
    pending_rsvps: HashMap<String, CalendarRsvp>,
    month_galley_cache: HashMap<(String, u16), Arc<egui::Galley>>,
    view: CalendarView,
    focus_date: NaiveDate,
    selected_event: Option<usize>,
    last_poll: Instant,
    initialized: bool,
    timezone: TimeZoneChoice,
    timezone_filter: String,
    rsvp_feedback: Option<(Instant, RsvpFeedback)>,
    rsvp_pending: bool,
}

impl CalendarApp {
    pub fn new() -> Self {
        let today = Local::now().date_naive();
        Self {
            subscription: None,
            events: Vec::new(),
            all_rsvps: HashMap::new(),
            pending_rsvps: HashMap::new(),
            month_galley_cache: HashMap::new(),
            view: CalendarView::Month,
            focus_date: today,
            selected_event: None,
            last_poll: Instant::now(),
            initialized: false,
            timezone: TimeZoneChoice::default(),
            timezone_filter: String::new(),
            rsvp_feedback: None,
            rsvp_pending: false,
        }
    }

    fn filters() -> Vec<Filter> {
        let mut kinds = Filter::new().kinds([31922, 31923, 31925]);
        kinds = kinds.limit(FETCH_LIMIT as u64);
        vec![kinds.build()]
    }

    fn ensure_subscription(&mut self, ctx: &mut AppContext) {
        if self.subscription.is_some() {
            return;
        }

        let filters = Self::filters();

        let sub_id = match ctx.ndb.subscribe(&filters) {
            Ok(local_sub) => {
                let remote_id = Uuid::new_v4().to_string();
                ctx.pool.subscribe(remote_id.clone(), filters);
                Some(UnifiedSubscription {
                    local: local_sub,
                    remote: remote_id,
                })
            }
            Err(err) => {
                warn!("Calendar: failed to subscribe locally: {err}");
                None
            }
        };

        self.subscription = sub_id;
        self.initialized = false;
    }

    fn load_initial_events(&mut self, ctx: &mut AppContext) {
        if self.initialized {
            return;
        }
        let txn = match Transaction::new(ctx.ndb) {
            Ok(txn) => txn,
            Err(err) => {
                warn!("Calendar: failed to create transaction: {err}");
                return;
            }
        };

        let filters = Self::filters();

        let results = match ctx.ndb.query(&txn, &filters, FETCH_LIMIT) {
            Ok(results) => results,
            Err(err) => {
                warn!("Calendar: query failed: {err}");
                return;
            }
        };

        let mut events = Vec::new();
        let mut rsvps = HashMap::new();
        for result in results {
            let note = result.note;
            let kind = note.kind();
            match kind {
                31922 | 31923 => {
                    if let Some(event) = parse_calendar_event(&note) {
                        events.push(event);
                    }
                }
                31925 => {
                    if let Some(rsvp) = parse_calendar_rsvp(&note) {
                        rsvps.insert(rsvp.id_hex.clone(), rsvp);
                    }
                }
                _ => {}
            }
        }

        let mut fulfilled = Vec::new();
        for (id, pending) in self.pending_rsvps.iter() {
            if rsvps.contains_key(id) {
                fulfilled.push(id.clone());
            } else {
                rsvps.insert(id.clone(), pending.clone());
            }
        }
        for id in fulfilled {
            self.pending_rsvps.remove(&id);
        }

        self.all_rsvps = rsvps;

        for event in events.iter_mut() {
            self.populate_event_rsvps(event);
        }

        self.events = events;
        self.resort_events();
        self.initialized = true;
    }

    fn poll_for_new_notes(&mut self, ctx: &mut AppContext) {
        let Some(sub) = &self.subscription else {
            return;
        };

        if self.last_poll.elapsed() < POLL_INTERVAL {
            return;
        }

        self.last_poll = Instant::now();

        let new_keys = ctx.ndb.poll_for_notes(sub.local, POLL_BATCH_SIZE as u32);
        if new_keys.is_empty() {
            return;
        }

        let txn = match Transaction::new(ctx.ndb) {
            Ok(txn) => txn,
            Err(err) => {
                warn!("Calendar: failed to create transaction for poll: {err}");
                return;
            }
        };

        for key in new_keys {
            match ctx.ndb.get_note_by_key(&txn, key) {
                Ok(note) => self.process_note(&note),
                Err(err) => warn!("Calendar: missing note for key {:?}: {err}", key),
            }
        }

        self.resort_events();
    }

    fn process_note(&mut self, note: &Note<'_>) {
        match note.kind() {
            31922 | 31923 => {
                if let Some(mut event) = parse_calendar_event(note) {
                    self.populate_event_rsvps(&mut event);
                    self.upsert_event(event);
                }
            }
            31925 => {
                if let Some(rsvp) = parse_calendar_rsvp(note) {
                    self.apply_rsvp(rsvp);
                }
            }
            _ => {}
        }
    }

    fn upsert_event(&mut self, event: CalendarEvent) {
        let event_id = event.id_hex.clone();
        self.purge_month_cache_for(&event_id);

        if let Some(idx) = self.find_event_index(&event) {
            self.events[idx] = event;
        } else {
            self.events.push(event);
        }
    }

    fn find_event_index(&self, event: &CalendarEvent) -> Option<usize> {
        if let Some(identifier) = &event.identifier {
            self.events.iter().position(|existing| {
                existing.kind == event.kind
                    && existing
                        .identifier
                        .as_ref()
                        .map(|id| id.eq_ignore_ascii_case(identifier))
                        .unwrap_or(false)
                    && existing.author_hex.eq_ignore_ascii_case(&event.author_hex)
            })
        } else {
            self.events
                .iter()
                .position(|existing| existing.id_hex == event.id_hex)
        }
    }

    fn apply_rsvp(&mut self, rsvp: CalendarRsvp) {
        let id = rsvp.id_hex.clone();
        self.all_rsvps.insert(id.clone(), rsvp.clone());
        self.pending_rsvps.remove(&id);

        let mut updates = Vec::new();
        for (idx, event) in self.events.iter().enumerate() {
            if rsvp.matches_event(event) {
                updates.push((idx, self.relevant_rsvps_for(event)));
            }
        }

        for (idx, relevant) in updates {
            if let Some(event_mut) = self.events.get_mut(idx) {
                event_mut.rsvps = match_rsvps_for_event(event_mut, &relevant);
            }
        }
    }

    fn relevant_rsvps_for(&self, event: &CalendarEvent) -> Vec<CalendarRsvp> {
        self.all_rsvps
            .values()
            .filter(|rsvp| rsvp.matches_event(event))
            .cloned()
            .collect()
    }

    fn populate_event_rsvps(&self, event: &mut CalendarEvent) {
        let relevant = self.relevant_rsvps_for(event);
        event.rsvps = match_rsvps_for_event(event, &relevant);
    }

    fn resort_events(&mut self) {
        let selected_id = self
            .selected_event
            .and_then(|idx| self.events.get(idx).map(|ev| ev.id_hex.clone()));

        self.events
            .sort_by_key(|ev| (ev.start_naive(), ev.created_at));

        if let Some(id) = selected_id {
            self.selected_event = self.events.iter().position(|ev| ev.id_hex == id);
        } else {
            self.selected_event = None;
        }

        self.prune_month_galley_cache();
    }

    fn month_title_galley(
        &mut self,
        fonts: &egui::text::Fonts,
        event_id: &str,
        title: &str,
        width: f32,
    ) -> Arc<egui::Galley> {
        let width_key = width.round().clamp(0.0, u16::MAX as f32) as u16;
        let key = (event_id.to_owned(), width_key);

        if let Some(existing) = self.month_galley_cache.get(&key) {
            return existing.clone();
        }

        let galley = fonts.layout(
            title.to_owned(),
            FontId::proportional(12.0),
            Color32::WHITE,
            width,
        );
        self.month_galley_cache.insert(key, galley.clone());
        galley
    }

    fn prune_month_galley_cache(&mut self) {
        if self.month_galley_cache.is_empty() {
            return;
        }

        let valid_ids: HashSet<String> =
            self.events.iter().map(|event| event.id_hex.clone()).collect();
        self.month_galley_cache
            .retain(|(event_id, _), _| valid_ids.contains(event_id));
    }

    fn purge_month_cache_for(&mut self, event_id: &str) {
        if self.month_galley_cache.is_empty() {
            return;
        }

        let to_remove: Vec<(String, u16)> = self
            .month_galley_cache
            .keys()
            .filter(|(id, _)| id == event_id)
            .cloned()
            .collect();

        for key in to_remove {
            self.month_galley_cache.remove(&key);
        }
    }

    fn collect_events_by_day(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> HashMap<NaiveDate, Vec<usize>> {
        let mut map: HashMap<NaiveDate, Vec<usize>> = HashMap::new();

        for (idx, event) in self.events.iter().enumerate() {
            let (event_start, event_end) = event.date_span(&self.timezone);
            if event_end < start || event_start > end {
                continue;
            }

            let mut day = if event_start < start { start } else { event_start };
            let last = if event_end > end { end } else { event_end };

            while day <= last {
                map.entry(day).or_default().push(idx);
                day = day + Duration::days(1);
            }
        }

        map
    }

    fn scroll_drag_id(id: egui::Id) -> egui::Id {
        id.with("area")
    }

    fn prune_rsvp_feedback(&mut self) {
        if let Some((timestamp, _)) = self.rsvp_feedback {
            if timestamp.elapsed() >= RSVP_FEEDBACK_TTL {
                self.rsvp_feedback = None;
            }
        }
    }

    fn set_rsvp_feedback(&mut self, feedback: RsvpFeedback) {
        self.rsvp_feedback = Some((Instant::now(), feedback));
    }

    fn current_user_rsvp(
        &mut self,
        ctx: &mut AppContext,
        event: &CalendarEvent,
    ) -> Option<RsvpStatus> {
        let user_hex = ctx.accounts.selected_account_pubkey().hex();
        event
            .rsvps
            .iter()
            .find(|r| r.attendee_hex.eq_ignore_ascii_case(&user_hex))
            .map(|r| r.status)
    }

    fn render_rsvp_controls(
        &mut self,
        ctx: &mut AppContext,
        ui: &mut egui::Ui,
        event_idx: usize,
        event: &CalendarEvent,
    ) {
        ui.label(egui::RichText::new("RSVP").strong());

        if event.identifier.is_none() {
            ui.label("This event is missing a calendar identifier; RSVP is unavailable.");
            return;
        }

        let has_writable_account = ctx.accounts.selected_filled().is_some();
        let current_status = self.current_user_rsvp(ctx, event);

        match current_status {
            Some(status) if status != RsvpStatus::Unknown => {
                ui.label(format!("Your response: {}", status.display_label()));
            }
            _ => {
                ui.label("You have not responded yet.");
            }
        }

        if !has_writable_account {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Select an account with its private key to RSVP.",
            );
        }

        if self.rsvp_pending {
            ui.label("Sending RSVP…");
        }

        if let Some((_, feedback)) = &self.rsvp_feedback {
            match feedback {
                RsvpFeedback::Success(msg) => {
                    ui.colored_label(ui.visuals().hyperlink_color, msg);
                }
                RsvpFeedback::Error(msg) => {
                    ui.colored_label(Color32::from_rgb(220, 70, 70), msg);
                }
            }
        }

        let allow_buttons = has_writable_account && !self.rsvp_pending;

        ui.horizontal(|ui| {
            if ui
                .add_enabled(allow_buttons, egui::Button::new("Accept"))
                .clicked()
            {
                self.submit_rsvp(ctx, event_idx, event, RsvpStatus::Accepted, Some("busy"));
            }

            if ui
                .add_enabled(allow_buttons, egui::Button::new("Maybe"))
                .clicked()
            {
                self.submit_rsvp(ctx, event_idx, event, RsvpStatus::Tentative, Some("free"));
            }

            if ui
                .add_enabled(allow_buttons, egui::Button::new("Decline"))
                .clicked()
            {
                self.submit_rsvp(ctx, event_idx, event, RsvpStatus::Declined, None);
            }
        });
    }

    fn submit_rsvp(
        &mut self,
        ctx: &mut AppContext,
        event_idx: usize,
        event: &CalendarEvent,
        status: RsvpStatus,
        freebusy: Option<&str>,
    ) {
        if self.rsvp_pending {
            return;
        }

        let Some(identifier) = &event.identifier else {
            self.set_rsvp_feedback(RsvpFeedback::Error(
                "Event is missing calendar identifier; unable to RSVP.".to_string(),
            ));
            return;
        };

        let Some(filled) = ctx.accounts.selected_filled() else {
            self.set_rsvp_feedback(RsvpFeedback::Error(
                "Select an account with its private key to RSVP.".to_string(),
            ));
            return;
        };

        let account = filled.to_full();
        self.rsvp_pending = true;

        let coordinate = format!("{}:{}:{}", event.kind, event.author_hex, identifier);
        let mut builder = nostrdb::NoteBuilder::new().kind(31925).content("");

        builder = builder.start_tag().tag_str("a").tag_str(&coordinate);
        builder = builder.start_tag().tag_str("e").tag_str(&event.id_hex);

        builder = builder.start_tag().tag_str("p").tag_str(&event.author_hex);

        builder = builder
            .start_tag()
            .tag_str("status")
            .tag_str(status.as_str());
        builder = builder.start_tag().tag_str("L").tag_str("status");
        builder = builder
            .start_tag()
            .tag_str("l")
            .tag_str(status.as_str())
            .tag_str("status");

        if let Some(fb) = freebusy {
            builder = builder.start_tag().tag_str("fb").tag_str(fb);
            builder = builder.start_tag().tag_str("L").tag_str("freebusy");
            builder = builder
                .start_tag()
                .tag_str("l")
                .tag_str(fb)
                .tag_str("freebusy");
        }

        builder = builder
            .start_tag()
            .tag_str("d")
            .tag_str(&Uuid::new_v4().to_string());

        let secret_bytes = account.secret_key.secret_bytes();
        let Some(note) = builder.sign(&secret_bytes).build() else {
            self.rsvp_pending = false;
            self.set_rsvp_feedback(RsvpFeedback::Error(
                "Failed to build RSVP event.".to_string(),
            ));
            return;
        };

        let Ok(event_msg) = ClientMessage::event(&note) else {
            self.rsvp_pending = false;
            self.set_rsvp_feedback(RsvpFeedback::Error(
                "Failed to serialize RSVP event.".to_string(),
            ));
            return;
        };

        if let Ok(json) = event_msg.to_json() {
            let _ = ctx
                .ndb
                .process_event_with(&json, IngestMetadata::new().client(true));
        }

        ctx.pool.send(&event_msg);

        let attendee_hex = account.pubkey.hex();
        let new_rsvp = CalendarRsvp {
            id_hex: hex::encode(note.id()),
            attendee_hex: attendee_hex.clone(),
            status,
            created_at: note.created_at(),
            coordinate_kind: Some(event.kind),
            coordinate_author_hex: Some(event.author_hex.clone()),
            coordinate_identifier: event.identifier.clone(),
            event_id_hex: Some(event.id_hex.clone()),
        };

        self.all_rsvps
            .insert(new_rsvp.id_hex.clone(), new_rsvp.clone());

        let relevant = self
            .events
            .get(event_idx)
            .map(|event| self.relevant_rsvps_for(event))
            .unwrap_or_default();

        if let Some(event_mut) = self.events.get_mut(event_idx) {
            event_mut.rsvps = match_rsvps_for_event(event_mut, &relevant);
        }

        self.pending_rsvps
            .insert(new_rsvp.id_hex.clone(), new_rsvp);

        self.rsvp_pending = false;
        self.set_rsvp_feedback(RsvpFeedback::Success(format!(
            "{} RSVP sent",
            status.display_label()
        )));
    }

    fn events_on(&self, date: NaiveDate) -> Vec<usize> {
        self.events
            .iter()
            .enumerate()
            .filter_map(|(idx, event)| {
                if event.occurs_on(date, &self.timezone) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect()
    }

    fn view_switcher(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, CalendarView::Month, "Month");
            ui.selectable_value(&mut self.view, CalendarView::Week, "Week");
            ui.selectable_value(&mut self.view, CalendarView::Day, "Day");
            if self.selected_event.is_some() {
                ui.selectable_value(&mut self.view, CalendarView::Event, "Event");
            } else {
                let disabled_view = self.view;
                ui.add_enabled(false, egui::SelectableLabel::new(false, "Event"));
                self.view = match disabled_view {
                    CalendarView::Event => CalendarView::Day,
                    other => other,
                };
            }
        });
    }

    fn navigation_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("◀").clicked() {
                self.adjust_focus(-1);
            }
            if ui.button("Today").clicked() {
                self.focus_date = Local::now().date_naive();
            }
            if ui.button("▶").clicked() {
                self.adjust_focus(1);
            }
        });
    }

    fn timezone_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(format!("Times shown in {}", self.timezone.label()));
            ui.menu_button("Change time zone", |ui| {
                ui.set_min_width(240.0);
                ui.label("Search");
                ui.text_edit_singleline(&mut self.timezone_filter);
                ui.add_space(6.0);
                if ui
                    .selectable_label(
                        matches!(self.timezone, TimeZoneChoice::Local),
                        "Local Time",
                    )
                    .clicked()
                {
                    self.timezone = TimeZoneChoice::Local;
                    self.timezone_filter.clear();
                    ui.close_menu();
                }

                let filter = self.timezone_filter.to_lowercase();
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        let mut shown = 0usize;
                        for tz in TZ_VARIANTS.iter() {
                            if !filter.is_empty()
                                && !tz.name().to_lowercase().contains(&filter)
                            {
                                continue;
                            }

                            let abbr = timezone_abbreviation(*tz);
                            let label = format!("{} ({abbr})", tz.name());
                            let selected =
                                matches!(self.timezone, TimeZoneChoice::Named(current) if current == *tz);

                            if ui.selectable_label(selected, label).clicked() {
                                self.timezone = TimeZoneChoice::Named(*tz);
                                self.timezone_filter.clear();
                                ui.close_menu();
                            }

                            shown += 1;
                            if shown >= 200 && filter.is_empty() {
                                break;
                            }
                        }
                    });
            });
        });
        ui.add_space(8.0);
    }

    fn adjust_focus(&mut self, delta: i32) {
        match self.view {
            CalendarView::Month => {
                let mut month = self.focus_date.month() as i32 + delta;
                let mut year = self.focus_date.year();
                while month < 1 {
                    month += 12;
                    year -= 1;
                }
                while month > 12 {
                    month -= 12;
                    year += 1;
                }
                let day = self.focus_date.day().min(days_in_month(year, month as u32));
                self.focus_date = NaiveDate::from_ymd_opt(year, month as u32, day).unwrap();
            }
            CalendarView::Week => {
                self.focus_date =
                    self.focus_date + Duration::days((delta * 7).try_into().unwrap_or(0));
            }
            CalendarView::Day | CalendarView::Event => {
                self.focus_date = self.focus_date + Duration::days(delta.try_into().unwrap_or(0));
            }
        }
    }

    fn render_month(&mut self, ui: &mut egui::Ui) -> ScrollAreaOutput<()> {
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

        egui::ScrollArea::vertical()
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
                                    let (event_id, title) = if let Some(event) = self.events.get(*idx)
                                    {
                                        (event.id_hex.clone(), event.month_title().to_owned())
                                    } else {
                                        continue;
                                    };

                                    let galley = self.month_title_galley(
                                        fonts,
                                        &event_id,
                                        &title,
                                        wrap_width,
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
                                    ui.label(
                                        egui::RichText::new(format!("{}", day.day())).strong(),
                                    );
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

    fn render_week(&mut self, ui: &mut egui::Ui) -> ScrollAreaOutput<()> {
        const HOUR_HEIGHT: f32 = 42.0;
        const ALL_DAY_HEIGHT: f32 = 32.0;
        const COLUMN_WIDTH: f32 = 150.0;
        const TIME_COL_WIDTH: f32 = 64.0;

        let week_start = self.focus_date
            - Duration::days(self.focus_date.weekday().num_days_from_monday() as i64);
        let today = Local::now().date_naive();
        let selected_idx = self.selected_event;
        let total_height = ALL_DAY_HEIGHT + HOUR_HEIGHT * 24.0;

        egui::ScrollArea::both()
            .id_salt("calendar-week-scroll")
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

                    for day_offset in 0..7 {
                        let day = week_start + Duration::days(day_offset as i64);
                        let events = self.events_on(day);

                        let mut all_day_events = Vec::new();
                        let mut timed_events = Vec::new();
                        for idx in events {
                            if matches!(self.events[idx].time, CalendarEventTime::AllDay { .. }) {
                                all_day_events.push(idx);
                            } else {
                                timed_events.push(idx);
                            }
                        }

                        let (day_rect, _) = ui.allocate_exact_size(
                            vec2(COLUMN_WIDTH, total_height),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter_at(day_rect);
                        let column_id = ui.make_persistent_id(("calendar-week-column", day));
                        let column_response =
                            ui.interact(day_rect, column_id, egui::Sense::click());
                        let column_clicked = column_response.clicked();
                        let mut event_clicked = false;

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
                            format!("{} {}", weekday_label(day_offset), day.format("%m/%d")),
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
                            let max_display = 3usize;
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
                                    ui.make_persistent_id(("calendar_all_day", day, *event_idx));
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
                                let chip_clip_rect = chip_rect.shrink2(vec2(4.0, 2.0));
                                let chip_painter = painter.with_clip_rect(chip_rect.shrink(1.0));
                                let chip_color = ui.visuals().strong_text_color();
                                chip_painter.text(
                                    chip_clip_rect.left_top(),
                                    egui::Align2::LEFT_TOP,
                                    self.events[*event_idx].week_title(),
                                    FontId::proportional(12.0),
                                    chip_color,
                                );
                                if response.clicked() {
                                    event_clicked = true;
                                    self.selected_event = Some(*event_idx);
                                    self.view = CalendarView::Event;
                                    self.focus_date = day;
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
                                    egui::pos2(timeline_rect.left() + 4.0, top + 2.0),
                                    egui::pos2(timeline_rect.right() - 4.0, bottom - 2.0),
                                );

                                let id = ui.make_persistent_id(("calendar_timed", day, event_idx));
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

                                let clip_rect = event_rect.shrink2(vec2(6.0, 4.0));
                                let text_painter = painter.with_clip_rect(event_rect.shrink(1.0));
                                text_painter.text(
                                    clip_rect.left_top(),
                                    egui::Align2::LEFT_TOP,
                                    event.week_title(),
                                    FontId::proportional(13.0),
                                    ui.visuals().strong_text_color(),
                                );

                                if response.clicked() {
                                    event_clicked = true;
                                    self.selected_event = Some(event_idx);
                                    self.view = CalendarView::Event;
                                    self.focus_date = day;
                                }
                            }
                        }

                        if column_clicked && !event_clicked {
                            self.focus_date = day;
                            self.view = CalendarView::Day;
                        }
                    }
                });
            })
    }

    fn paint_timed_event_contents(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        rect: egui::Rect,
        event: &CalendarEvent,
        _time_label: Option<String>,
    ) {
        let content_rect = rect.shrink2(vec2(6.0, 4.0));
        if content_rect.height() <= 12.0 {
            return;
        }

        let max_width = content_rect.width().max(1.0);
        let mut cursor_y = content_rect.top();
        let origin_x = content_rect.left();
        let text_painter = painter.with_clip_rect(rect.shrink(1.0));

        let title_color = ui.visuals().strong_text_color();
        let title_text: Cow<'_, str> = if max_width <= 220.0 {
            Cow::Borrowed(event.day_title())
        } else {
            let chars_per_line = ((max_width / 7.0).floor() as usize).clamp(12, 96);
            let max_lines = if max_width > 360.0 { 6 } else { 4 };
            Cow::Owned(wrap_title(&event.title, chars_per_line, max_lines))
        };

        text_painter.text(
            egui::pos2(origin_x, cursor_y),
            egui::Align2::LEFT_TOP,
            title_text.as_ref(),
            FontId::proportional(13.0),
            title_color,
        );

        cursor_y += 16.0;

        let summary_text: Option<Cow<'_, str>> = if max_width <= 220.0 {
            event.summary_preview().map(Cow::Borrowed)
        } else {
            event.summary.as_deref().map(|summary| {
                let chars_per_line = ((max_width / 8.0).floor() as usize).clamp(16, 128);
                let max_lines = if max_width > 360.0 { 5 } else { 3 };
                Cow::Owned(wrap_title(summary, chars_per_line, max_lines))
            })
        };

        if let Some(summary_display) = summary_text {
            if !summary_display.is_empty() {
                text_painter.text(
                    egui::pos2(origin_x, cursor_y),
                    egui::Align2::LEFT_TOP,
                    summary_display.as_ref(),
                    FontId::proportional(11.0),
                    ui.visuals().weak_text_color(),
                );
            }
        }
    }

    fn render_day(&mut self, ui: &mut egui::Ui) -> ScrollAreaOutput<()> {
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

        egui::ScrollArea::both()
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
                            let chip_clip_rect = chip_rect.shrink2(vec2(4.0, 2.0));
                            let chip_painter = painter.with_clip_rect(chip_rect.shrink(1.0));
                            chip_painter.text(
                                chip_clip_rect.left_top(),
                                egui::Align2::LEFT_TOP,
                                self.events[*event_idx].day_title(),
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

    fn render_event(
        &mut self,
        ctx: &mut AppContext,
        ui: &mut egui::Ui,
    ) -> Option<ScrollAreaOutput<()>> {
        let Some(idx) = self.selected_event else {
            ui.label("Select an event from any calendar view to see its details.");
            return None;
        };

        self.prune_rsvp_feedback();

        let Some(event_snapshot) = self.events.get(idx).cloned() else {
            ui.label("The selected event is no longer available.");
            return None;
        };

        Some(
            egui::ScrollArea::vertical()
            .id_salt(("calendar-event", &event_snapshot.id_hex))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let event = &event_snapshot;
                ui.heading(&event.title);
                ui.label(event.duration_text(&self.timezone));
                render_author(ctx, ui, &event.author_hex);
                ui.label(format!("Times shown in {}", self.timezone.label()));
                if let Some(naddr) = event_naddr(event) {
                    ui.label(format!("Identifier (naddr): {naddr}"));
                } else if let Some(identifier) = &event.identifier {
                    ui.label(format!("Identifier: {identifier}"));
                }
                if let Some(nevent) = event_nevent(event) {
                    ui.label(format!("Event (nevent): {nevent}"));
                }

                if let CalendarEventTime::Timed {
                    start_tzid,
                    end_tzid,
                    ..
                } = &event.time
                {
                    if let Some(start_id) = start_tzid {
                        let start_label = humanize_tz_name(start_id);
                        if let Some(end_id) = end_tzid {
                            let end_label = humanize_tz_name(end_id);
                            if end_id != start_id {
                                ui.label(format!(
                                    "Original time zone: {start_label} → {end_label}"
                                ));
                            } else {
                                ui.label(format!("Original time zone: {start_label}"));
                            }
                        } else {
                            ui.label(format!("Original time zone: {start_label}"));
                        }
                    }
                }

                ui.separator();
                self.render_rsvp_controls(ctx, ui, idx, event);
                ui.separator();

                if let Some(summary) = &event.summary {
                    ui.label(summary);
                    ui.separator();
                }

                if let Some(description) = &event.description {
                    ui.label(description);
                    ui.separator();
                }

                if !event.images.is_empty() {
                    ui.label(egui::RichText::new("Images").strong());
                    for image in &event.images {
                        render_event_image(ctx, ui, image);
                        ui.add_space(6.0);
                    }
                    ui.separator();
                }

                if !event.locations.is_empty() {
                    ui.label(egui::RichText::new("Locations").strong());
                    for loc in &event.locations {
                        ui.label(loc);
                    }
                    ui.separator();
                }

                render_rsvps(ctx, ui, &event.rsvps);
                render_participants(ctx, ui, &event.participants);

                if !event.hashtags.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for tag in &event.hashtags {
                            ui.label(format!("#{tag}"));
                        }
                    });
                }

                if !event.references.is_empty() {
                    ui.separator();
                    ui.label(egui::RichText::new("Links").strong());
                    for reference in &event.references {
                        ui.hyperlink(reference);
                    }
                }

                if !event.calendars.is_empty() {
                    ui.separator();
                    ui.label(egui::RichText::new("Calendars").strong());
                    for cal in &event.calendars {
                        ui.label(cal);
                    }
                }
            }),
        )
    }

    fn timed_label_for_day(&self, event_idx: usize, day: NaiveDate) -> Option<String> {
        let event = self.events.get(event_idx)?;
        let CalendarEventTime::Timed {
            start_utc, end_utc, ..
        } = &event.time
        else {
            return None;
        };

        let start_local = self.timezone.localize(start_utc);
        let end_local = end_utc.map(|end| self.timezone.localize(&end));

        let start_label = if day == start_local.date {
            start_local.time_text.clone()
        } else {
            "00:00".to_string()
        };

        let end_label = end_local.map(|end| {
            if day == end.date {
                end.time_text.clone()
            } else {
                "24:00".to_string()
            }
        });

        match end_label {
            Some(label) => {
                if label == start_label {
                    Some(start_label)
                } else {
                    Some(format!("{start_label} – {label}"))
                }
            }
            None => Some(start_label),
        }
    }
}

impl App for CalendarApp {
    fn update(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> AppResponse {
        self.ensure_subscription(ctx);
        self.load_initial_events(ctx);
        self.poll_for_new_notes(ctx);

        let mut action = None;
        let mut drag_ids = Vec::new();
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                if ui.button("← Back to Notedeck").clicked() {
                    action = Some(AppAction::ShowColumns);
                }
            });

            ui.separator();
            self.view_switcher(ui);
            ui.add_space(8.0);
            self.navigation_bar(ui);
            ui.add_space(8.0);
            self.timezone_controls(ui);

            match self.view {
                CalendarView::Month => {
                    let output = self.render_month(ui);
                    drag_ids.push(Self::scroll_drag_id(output.id));
                }
                CalendarView::Week => {
                    let output = self.render_week(ui);
                    drag_ids.push(Self::scroll_drag_id(output.id));
                }
                CalendarView::Day => {
                    let output = self.render_day(ui);
                    drag_ids.push(Self::scroll_drag_id(output.id));
                }
                CalendarView::Event => {
                    if let Some(output) = self.render_event(ctx, ui) {
                        drag_ids.push(Self::scroll_drag_id(output.id));
                    }
                }
            }
        });

        let response = if let Some(action) = action {
            AppResponse::action(Some(action))
        } else {
            AppResponse::none()
        };

        response.drag(drag_ids)
    }
}

impl Default for CalendarApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CalendarApp {
    fn drop(&mut self) {
        // remote subscriptions are cleaned up by relay pool on drop
    }
}

fn render_event_image(ctx: &mut AppContext, ui: &mut egui::Ui, url: &str) {
    let cache_type =
        supported_mime_hosted_at_url(&mut ctx.img_cache.urls, url).unwrap_or(MediaCacheType::Image);
    let render_state = get_render_state(
        ui.ctx(),
        ctx.img_cache,
        cache_type,
        url,
        ImageType::Content(None),
    );

    match render_state.texture_state {
        TextureState::Pending => {
            let width = ui.available_width().min(420.0);
            let height = width * 0.6;
            let (rect, _) = ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(10), ui.visuals().extreme_bg_color);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Loading image…",
                FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
        }
        TextureState::Error(err) => {
            ui.colored_label(
                Color32::from_rgb(220, 70, 70),
                format!("Failed to load image: {err}"),
            );
            ui.hyperlink(url);
        }
        TextureState::Loaded(tex) => {
            let texture =
                ensure_latest_texture(ui, url, render_state.gifs, tex, AnimationMode::Reactive);
            let size = texture.size();
            let width = ui.available_width().min(420.0);
            let aspect = if size[1] == 0 {
                1.0
            } else {
                size[0] as f32 / size[1] as f32
            };
            let height = if aspect > 0.0 {
                width / aspect
            } else {
                width * 0.75
            };
            ui.add(
                egui::Image::new(&texture)
                    .fit_to_exact_size(vec2(width, height))
                    .corner_radius(CornerRadius::same(10))
                    .maintain_aspect_ratio(true),
            );
        }
    }
}

fn render_author(ctx: &mut AppContext, ui: &mut egui::Ui, author_hex: &str) {
    ui.label(egui::RichText::new("Author").strong());

    match Transaction::new(ctx.ndb) {
        Ok(txn) => {
            let profile = decode_pubkey_hex(author_hex)
                .and_then(|bytes| ctx.ndb.get_profile_by_pubkey(&txn, &bytes).ok());
            render_author_entry(ctx, ui, author_hex, profile.as_ref());
        }
        Err(err) => {
            warn!("Calendar: failed to open transaction for author: {err}");
            render_author_entry(ctx, ui, author_hex, None);
        }
    }

    ui.add_space(6.0);
}

fn render_author_entry(
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
    author_hex: &str,
    profile: Option<&ProfileRecord<'_>>,
) {
    let display_name =
        display_name_from_profile(profile).unwrap_or_else(|| short_pubkey(author_hex));

    ui.horizontal(|ui| {
        let mut avatar = ProfilePic::from_profile_or_default(ctx.img_cache, profile)
            .size(48.0)
            .border(ProfilePic::border_stroke(ui));
        let response = ui.add(&mut avatar);
        response.on_hover_text(&display_name);
        ui.label(display_name);
    });
    ui.add_space(4.0);
}

fn render_rsvps(ctx: &mut AppContext, ui: &mut egui::Ui, rsvps: &[CalendarRsvp]) {
    ui.label(egui::RichText::new("Confirmed Attendees").strong());

    if !rsvps.iter().any(|r| r.is_confirmed()) {
        ui.label("No confirmed RSVPs yet.");
        ui.separator();
        return;
    }

    match Transaction::new(ctx.ndb) {
        Ok(txn) => {
            ui.horizontal_wrapped(|ui| {
                for rsvp in rsvps.iter().filter(|r| r.is_confirmed()) {
                    let profile = decode_pubkey_hex(&rsvp.attendee_hex)
                        .and_then(|bytes| ctx.ndb.get_profile_by_pubkey(&txn, &bytes).ok());
                    let display_name = display_name_from_profile(profile.as_ref())
                        .unwrap_or_else(|| short_pubkey(&rsvp.attendee_hex));

                    ui.vertical(|ui| {
                        let mut avatar =
                            ProfilePic::from_profile_or_default(ctx.img_cache, profile.as_ref())
                                .size(40.0)
                                .border(ProfilePic::border_stroke(ui));
                        let response = ui.add(&mut avatar);
                        response.on_hover_text(&display_name);
                        ui.label(display_name);
                    });
                    ui.add_space(8.0);
                }
            });
        }
        Err(err) => {
            warn!("Calendar: failed to open transaction for RSVPs: {err}");
            for rsvp in rsvps.iter().filter(|r| r.is_confirmed()) {
                ui.label(short_pubkey(&rsvp.attendee_hex));
            }
        }
    }

    ui.separator();
}

fn render_participants(
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
    participants: &[CalendarParticipant],
) {
    if participants.is_empty() {
        return;
    }

    ui.label(egui::RichText::new("Participants").strong());

    match Transaction::new(ctx.ndb) {
        Ok(txn) => {
            ui.horizontal_wrapped(|ui| {
                for participant in participants {
                    let profile = decode_pubkey_hex(&participant.pubkey_hex)
                        .and_then(|bytes| ctx.ndb.get_profile_by_pubkey(&txn, &bytes).ok());
                    let display_name = display_name_from_profile(profile.as_ref())
                        .unwrap_or_else(|| short_pubkey(&participant.pubkey_hex));

                    ui.vertical(|ui| {
                        let mut avatar =
                            ProfilePic::from_profile_or_default(ctx.img_cache, profile.as_ref())
                                .size(40.0)
                                .border(ProfilePic::border_stroke(ui));
                        let response = ui.add(&mut avatar);
                        response.on_hover_text(&display_name);

                        if let Some(role) = &participant.role {
                            ui.label(format!("{display_name}\n{role}"));
                        } else {
                            ui.label(display_name);
                        }
                    });
                    ui.add_space(8.0);
                }
            });
        }
        Err(err) => {
            warn!("Calendar: failed to open transaction for participants: {err}");
            for participant in participants {
                ui.label(short_pubkey(&participant.pubkey_hex));
            }
        }
    }
    ui.separator();
}

fn decode_pubkey_hex(hex: &str) -> Option<[u8; 32]> {
    let bytes = Vec::<u8>::from_hex(hex).ok()?;
    bytes.try_into().ok()
}

fn display_name_from_profile(profile: Option<&ProfileRecord<'_>>) -> Option<String> {
    profile
        .and_then(|record| record.record().profile())
        .and_then(|p| p.display_name().or_else(|| p.name()))
        .map(|s| s.to_string())
}

fn short_pubkey(hex: &str) -> String {
    if hex.len() <= 12 {
        hex.to_owned()
    } else {
        format!("{}…{}", &hex[..8], &hex[hex.len() - 4..])
    }
}

fn timezone_abbreviation(tz: Tz) -> String {
    let dt = tz.from_utc_datetime(&Utc::now().naive_utc());
    let abbr = dt.format("%Z").to_string();
    if abbreviation_has_letters(&abbr) {
        abbr
    } else if let Some(code) = fallback_short_code(tz.name()) {
        code.to_string()
    } else {
        format_utc_offset(dt.offset().fix().local_minus_utc())
    }
}

fn abbreviation_has_letters(value: &str) -> bool {
    value.chars().any(|c| c.is_ascii_alphabetic())
}

fn format_utc_offset(offset_seconds: i32) -> String {
    let hours = offset_seconds / 3600;
    let minutes = (offset_seconds.abs() % 3600) / 60;
    format!("UTC{:+02}:{:02}", hours, minutes)
}

fn fallback_short_code(name: &str) -> Option<&'static str> {
    match name {
        "America/New_York" | "US/Eastern" | "EST" => Some("ET"),
        "America/Detroit" | "America/Kentucky/Louisville" | "America/Toronto" => Some("ET"),
        "America/Chicago" | "US/Central" => Some("CT"),
        "America/Indiana/Knox" | "America/Indiana/Tell_City" => Some("CT"),
        "America/Denver" | "US/Mountain" => Some("MT"),
        "America/Phoenix" => Some("MT"),
        "America/Los_Angeles" | "US/Pacific" => Some("PT"),
        "America/Anchorage" | "America/Juneau" => Some("AKT"),
        "America/Adak" => Some("HAT"),
        "Pacific/Honolulu" | "US/Hawaii" => Some("HT"),
        "America/Indiana/Indianapolis" => Some("ET"),
        "America/Boise" => Some("MT"),
        _ => None,
    }
}

fn humanize_tz_name(name: &str) -> String {
    if let Ok(tz) = name.parse::<Tz>() {
        timezone_abbreviation(tz)
    } else if let Some(code) = fallback_short_code(name) {
        code.to_string()
    } else if let Some(last) = name.rsplit('/').next() {
        last.replace('_', " ")
    } else {
        name.to_string()
    }
}

fn guess_local_timezone(now: DateTime<Local>) -> Option<Tz> {
    let offset = now.offset().local_minus_utc();
    for tz in TZ_VARIANTS.iter() {
        let dt = tz.from_utc_datetime(&now.naive_utc());
        let candidate_offset = dt.offset().fix().local_minus_utc();
        if candidate_offset == offset {
            let abbr = dt.format("%Z").to_string();
            if abbreviation_has_letters(&abbr) || fallback_short_code(tz.name()).is_some() {
                return Some(*tz);
            }
        }
    }
    None
}

fn hours_from_time(time: NaiveTime) -> f32 {
    time.hour() as f32
        + time.minute() as f32 / 60.0
        + time.second() as f32 / 3600.0
        + time.nanosecond() as f32 / 3_600_000_000_000.0
}

fn timed_range_on_day(
    event: &CalendarEvent,
    timezone: &TimeZoneChoice,
    day: NaiveDate,
) -> Option<(f32, f32)> {
    let CalendarEventTime::Timed {
        start_utc, end_utc, ..
    } = &event.time
    else {
        return None;
    };

    let start_local = timezone.localize(start_utc);
    let end_local = end_utc
        .map(|end| timezone.localize(&end))
        .unwrap_or_else(|| timezone.localize(start_utc));

    if day < start_local.date || day > end_local.date {
        return None;
    }

    let mut start_hours = if day == start_local.date {
        hours_from_time(start_local.time_of_day)
    } else {
        0.0
    };

    let mut end_hours = if day == end_local.date {
        hours_from_time(end_local.time_of_day)
    } else {
        24.0
    };

    if end_utc.is_none() && day == start_local.date {
        end_hours = (start_hours + 1.0).min(24.0);
    }

    start_hours = start_hours.clamp(0.0, 24.0);
    end_hours = end_hours.clamp(0.0, 24.0);

    if end_hours <= start_hours {
        end_hours = (start_hours + 0.5).min(24.0).max(start_hours + 0.1);
    }

    Some((start_hours, end_hours))
}

fn weekday_label(idx: usize) -> &'static str {
    match idx {
        0 => "Mon",
        1 => "Tue",
        2 => "Wed",
        3 => "Thu",
        4 => "Fri",
        5 => "Sat",
        6 => "Sun",
        _ => "",
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let next_month = if month == 12 { 1 } else { month + 1 };
    let next_year = if month == 12 { year + 1 } else { year };
    let first_next = NaiveDate::from_ymd_opt(next_year, next_month, 1).unwrap();
    let last_current = first_next - Duration::days(1);
    last_current.day()
}
