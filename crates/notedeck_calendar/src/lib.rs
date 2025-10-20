mod model;
mod views;

use chrono::{
    offset::{LocalResult, Offset},
    DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike,
    Utc,
};
use chrono_tz::{Tz, TZ_VARIANTS};
use egui::{scroll_area::ScrollAreaOutput, vec2, Color32, CornerRadius, FontId, Key};
use hex::FromHex;
use iana_time_zone::get_timezone;
use nostr::nips::nip19::{FromBech32, Nip19};
use nostr::nips::nip44::{self, Version as Nip44Version};
use nostr::nips::nip59::RANGE_RANDOM_TIMESTAMP_TWEAK;
use nostr::{
    Event as NostrEvent, JsonUtil, Keys as NostrKeys, PublicKey as NostrPublicKey, UnsignedEvent,
};
use nostrdb::{Filter, IngestMetadata, Note, ProfileRecord, Transaction};
use notedeck::enostr::{ClientMessage, FullKeypair};
use notedeck::filter::UnifiedSubscription;
use notedeck::media::gif::ensure_latest_texture;
use notedeck::media::{AnimationMode, ImageType};
use notedeck::{
    get_render_state, supported_mime_hosted_at_url, App, AppAction, AppContext, AppResponse,
    MediaCacheType, TextureState, WebOfTrustBuilder,
};
use notedeck_ui::{
    app_images::{copy_to_clipboard_dark_image, copy_to_clipboard_image},
    info_icon, AnimationHelper, IosSwitch, ProfilePic,
};
use rand::Rng;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};
use tracing::warn;
use urlencoding::encode;
use uuid::Uuid;

use model::{
    event_naddr, event_nevent, match_rsvps_for_event, parse_calendar_event, parse_calendar_rsvp,
    wrap_title, CalendarEvent, CalendarEventTime, CalendarParticipant, CalendarRsvp, RsvpFeedback,
    RsvpStatus,
};

const FETCH_LIMIT: i32 = 1024;
const POLL_BATCH_SIZE: usize = 64;
const POLL_INTERVAL: StdDuration = StdDuration::from_secs(5);
const EVENT_CREATION_FEEDBACK_TTL: StdDuration = StdDuration::from_secs(10);
const WOT_CACHE_TTL: StdDuration = StdDuration::from_secs(60);
const DEFAULT_WOT_DEPTH: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DraftEventType {
    AllDay,
    Timed,
}

impl DraftEventType {
    fn as_kind(&self) -> u32 {
        match self {
            DraftEventType::AllDay => 31922,
            DraftEventType::Timed => 31923,
        }
    }
}

#[derive(Debug, Clone)]
struct CalendarEventDraft {
    event_type: DraftEventType,
    identifier: String,
    title: String,
    summary: String,
    description: String,
    locations_text: String,
    images_text: String,
    hashtags_text: String,
    references_text: String,
    calendars_text: String,
    participants: Vec<(String, Option<String>)>,
    participant_input: String,
    start_date: String,
    end_date: String,
    start_time: String,
    end_time: String,
    include_end: bool,
    start_tzid: String,
    end_tzid: String,
    is_private: bool,
}

impl CalendarEventDraft {
    fn with_kind(event_type: DraftEventType) -> Self {
        let today = Local::now().date_naive();
        let now = Local::now().time();
        let default_time = format!("{:02}:{:02}", now.hour(), now.minute());
        let guessed = default_timezone_name();

        CalendarEventDraft {
            event_type,
            identifier: Self::new_identifier(),
            title: String::new(),
            summary: String::new(),
            description: String::new(),
            locations_text: String::new(),
            images_text: String::new(),
            hashtags_text: String::new(),
            references_text: String::new(),
            calendars_text: String::new(),
            participants: Vec::new(),
            participant_input: String::new(),
            start_date: today.format("%Y-%m-%d").to_string(),
            end_date: String::new(),
            start_time: default_time.clone(),
            end_time: default_time,
            include_end: false,
            start_tzid: guessed.clone(),
            end_tzid: guessed,
            is_private: false,
        }
    }

    fn new() -> Self {
        Self::with_kind(DraftEventType::Timed)
    }

    fn reset_preserving_type(&mut self) {
        let event_type = self.event_type;
        let is_private = self.is_private;
        *self = Self::with_kind(event_type);
        self.is_private = is_private;
    }

    fn new_identifier() -> String {
        Uuid::new_v4().simple().to_string()
    }

    fn parsed_locations(&self) -> Vec<String> {
        self.locations_text
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(|line| line.to_owned())
            .collect()
    }

    fn parsed_images(&self) -> Vec<String> {
        self.images_text
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(|line| line.to_owned())
            .collect()
    }

    fn parsed_hashtags(&self) -> Vec<String> {
        self.hashtags_text
            .split_whitespace()
            .map(|tag| tag.trim_matches('#').to_ascii_lowercase())
            .filter(|tag| !tag.is_empty())
            .collect()
    }

    fn parsed_references(&self) -> Vec<String> {
        self.references_text
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(|line| line.to_owned())
            .collect()
    }

    fn parsed_calendars(&self) -> Vec<String> {
        self.calendars_text
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(|line| line.to_owned())
            .collect()
    }

    fn parsed_participants(&self) -> Vec<(String, Option<String>)> {
        self.participants.clone()
    }

    fn parse_participant_lines(value: &str) -> Result<Vec<(String, Option<String>)>, String> {
        let mut participants = Vec::new();
        for (idx, line) in value.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let mut parts = trimmed.splitn(2, ',');
            let identifier = parts.next().unwrap().trim();
            if identifier.is_empty() {
                return Err(format!(
                    "Participant entry on line {} is missing an identifier.",
                    idx + 1
                ));
            }

            let pubkey_hex = Self::parse_participant_identifier(identifier).map_err(|err| {
                format!(
                    "Participant entry on line {} could not be parsed: {err}",
                    idx + 1
                )
            })?;

            let role = parts
                .next()
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty());

            participants.push((pubkey_hex, role));
        }

        Ok(participants)
    }

    fn absorb_participant_input(&mut self) {
        if self.participant_input.trim().is_empty() {
            return;
        }

        if let Ok(entries) = Self::parse_participant_lines(&self.participant_input) {
            for (hex, role) in entries {
                if !self
                    .participants
                    .iter()
                    .any(|(existing, _)| existing.eq_ignore_ascii_case(&hex))
                {
                    self.participants.push((hex, role));
                }
            }
            self.participant_input.clear();
        }
    }

    fn parse_participant_identifier(value: &str) -> Result<String, String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("Identifier is empty.".to_string());
        }

        if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(trimmed.to_ascii_lowercase());
        }

        if let Ok(pk) = NostrPublicKey::parse(trimmed) {
            return Ok(pk.to_hex());
        }

        let without_prefix = trimmed.strip_prefix("nostr:").unwrap_or(trimmed);

        if without_prefix.starts_with("nprofile") {
            match Nip19::from_bech32(without_prefix) {
                Ok(Nip19::Profile(profile)) => return Ok(profile.public_key.to_hex()),
                Ok(_) => {
                    return Err("Identifier decoded to an unexpected NIP-19 variant.".to_string())
                }
                Err(err) => {
                    return Err(format!("Invalid nprofile identifier: {err}"));
                }
            }
        }

        if without_prefix.contains('@') {
            return Self::resolve_nip05_identifier(without_prefix);
        }

        Err("Identifier must be a hex pubkey, npub, nprofile, or NIP-05 address.".to_string())
    }

    fn resolve_nip05_identifier(value: &str) -> Result<String, String> {
        let trimmed = value.trim_start_matches('@');
        let mut parts = trimmed.split('@');
        let raw_name = parts
            .next()
            .ok_or_else(|| "NIP-05 identifier is missing a username.".to_string())?;
        let domain = parts
            .next()
            .ok_or_else(|| "NIP-05 identifier is missing a domain.".to_string())?;
        if parts.next().is_some() {
            return Err("NIP-05 identifier contains extra '@' characters.".to_string());
        }

        if domain.trim().is_empty() {
            return Err("NIP-05 identifier is missing a domain.".to_string());
        }

        let name = if raw_name.trim().is_empty() {
            "_"
        } else {
            raw_name.trim()
        };

        let normalized_name = name.to_ascii_lowercase();
        let normalized_domain = domain.trim().to_ascii_lowercase();

        let url = format!(
            "https://{}/.well-known/nostr.json?name={}",
            normalized_domain,
            encode(&normalized_name)
        );

        let response = ureq::get(&url)
            .call()
            .map_err(|err| format!("Failed to resolve NIP-05 '{value}': {err}"))?;

        if !(200..=299).contains(&response.status()) {
            return Err(format!(
                "Failed to resolve NIP-05 '{value}': HTTP {}",
                response.status()
            ));
        }

        let json: Value = response
            .into_json()
            .map_err(|err| format!("Failed to decode NIP-05 response: {err}"))?;

        let names = json
            .get("names")
            .and_then(Value::as_object)
            .ok_or_else(|| "NIP-05 response missing a 'names' section.".to_string())?;

        if let Some(mapped) = names.get(&normalized_name).and_then(Value::as_str) {
            return Self::validate_pubkey_hex(mapped, value);
        }

        Err(format!(
            "NIP-05 identifier '{value}' was not found on {normalized_domain}."
        ))
    }

    fn validate_pubkey_hex(value: &str, context: &str) -> Result<String, String> {
        let trimmed = value.trim();
        if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            Ok(trimmed.to_ascii_lowercase())
        } else {
            Err(format!(
                "Identifier '{context}' resolved to a non-hex public key."
            ))
        }
    }

    fn parse_required_date(value: &str, field: &str) -> Result<NaiveDate, String> {
        NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
            .map_err(|_| format!("{field} must use YYYY-MM-DD format"))
    }

    fn parse_optional_date(value: &str, field: &str) -> Result<Option<NaiveDate>, String> {
        if value.trim().is_empty() {
            Ok(None)
        } else {
            Self::parse_required_date(value, field).map(Some)
        }
    }

    fn parse_required_time(value: &str, field: &str) -> Result<NaiveTime, String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(format!("{field} is required"));
        }

        NaiveTime::parse_from_str(trimmed, "%H:%M")
            .or_else(|_| NaiveTime::parse_from_str(trimmed, "%H:%M:%S"))
            .map_err(|_| format!("{field} must use HH:MM or HH:MM:SS format"))
    }
}

#[derive(Debug, Clone)]
enum EventCreationFeedback {
    Success(String),
    Error(String),
}

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
    rsvp_feedback: Option<(String, RsvpFeedback)>,
    rsvp_pending: bool,
    creating_event: bool,
    creation_feedback: Option<(Instant, EventCreationFeedback)>,
    creation_pending: bool,
    event_draft: CalendarEventDraft,
    wot_only: bool,
    wot_cache: Option<WebOfTrustCache>,
    user_pubkey_hex: String,
}

struct WebOfTrustCache {
    trusted_hex: HashSet<String>,
    source_timestamp: Option<u64>,
    computed_at: Instant,
    root_hex: String,
}

impl WebOfTrustCache {
    fn contains(&self, hex: &str) -> bool {
        self.trusted_hex.contains(hex)
    }
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
            creating_event: false,
            creation_feedback: None,
            creation_pending: false,
            event_draft: CalendarEventDraft::new(),
            wot_only: true,
            wot_cache: None,
            user_pubkey_hex: String::new(),
        }
    }

    fn filters() -> Vec<Filter> {
        let mut kinds = Filter::new().kinds([31922, 31923, 31925]);
        kinds = kinds.limit(FETCH_LIMIT as u64);
        vec![kinds.build()]
    }

    fn ensure_wot_cache(&mut self, ctx: &mut AppContext) {
        if !self.wot_only {
            self.wot_cache = None;
            return;
        }

        let root_pk = ctx.accounts.selected_account_pubkey().clone();
        let root_hex = hex::encode(root_pk.bytes());
        let snapshot = ctx.accounts.get_selected_account().data.contacts.snapshot();
        let snapshot_timestamp = snapshot.as_ref().map(|snap| snap.timestamp);

        let needs_refresh = match &self.wot_cache {
            Some(cache) => {
                cache.root_hex != root_hex
                    || cache.source_timestamp != snapshot_timestamp
                    || cache.computed_at.elapsed() >= WOT_CACHE_TTL
            }
            None => true,
        };

        if !needs_refresh {
            return;
        }

        let txn = match Transaction::new(ctx.ndb) {
            Ok(txn) => txn,
            Err(err) => {
                warn!("Calendar: failed to open transaction for web-of-trust cache: {err}");
                let mut trusted = HashSet::new();
                trusted.insert(root_hex.clone());
                self.wot_cache = Some(WebOfTrustCache {
                    trusted_hex: trusted,
                    source_timestamp: snapshot_timestamp,
                    computed_at: Instant::now(),
                    root_hex,
                });
                return;
            }
        };

        let mut builder = WebOfTrustBuilder::new(ctx.ndb, &txn, root_pk);
        builder = builder.max_depth(DEFAULT_WOT_DEPTH).include_self(true);

        if let Some(snapshot) = snapshot {
            builder = builder.with_seed_contacts(snapshot.contacts.clone());
        }

        let mut trusted_hex = builder.build().to_hex_set();
        if !trusted_hex.contains(&root_hex) {
            trusted_hex.insert(root_hex.clone());
        }

        self.wot_cache = Some(WebOfTrustCache {
            trusted_hex,
            source_timestamp: snapshot_timestamp,
            computed_at: Instant::now(),
            root_hex,
        });
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
        let mut purge_ids = Vec::new();
        for (idx, event) in self.events.iter().enumerate() {
            if rsvp.matches_event(event) {
                updates.push((idx, self.relevant_rsvps_for(event)));
            }
        }

        for (idx, relevant) in updates {
            if let Some(event_mut) = self.events.get_mut(idx) {
                event_mut.rsvps = match_rsvps_for_event(event_mut, &relevant);
                purge_ids.push(event_mut.id_hex.clone());
            }
        }

        for id in purge_ids {
            self.purge_month_cache_for(&id);
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
        status: Option<RsvpStatus>,
        title: &str,
        width: f32,
    ) -> Arc<egui::Galley> {
        let width_key = width.round().clamp(0.0, u16::MAX as f32) as u16;
        let cache_id = format!("{}:{}", event_id, Self::status_cache_suffix(status));
        let key = (cache_id.clone(), width_key);

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

        let valid_ids: HashSet<String> = self
            .events
            .iter()
            .map(|event| event.id_hex.clone())
            .collect();
        self.month_galley_cache
            .retain(|(cache_id, _), _| valid_ids.iter().any(|valid| cache_id.starts_with(valid)));
    }

    fn purge_month_cache_for(&mut self, event_id: &str) {
        if self.month_galley_cache.is_empty() {
            return;
        }

        let to_remove: Vec<(String, u16)> = self
            .month_galley_cache
            .keys()
            .filter(|(cache_id, _)| cache_id.starts_with(event_id))
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
            if !self.is_event_visible(event) {
                continue;
            }

            let (event_start, event_end) = event.date_span(&self.timezone);
            if event_end < start || event_start > end {
                continue;
            }

            let mut day = if event_start < start {
                start
            } else {
                event_start
            };
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

    fn prune_creation_feedback(&mut self) {
        if let Some((timestamp, _)) = self.creation_feedback {
            if timestamp.elapsed() >= EVENT_CREATION_FEEDBACK_TTL {
                self.creation_feedback = None;
            }
        }
    }

    fn set_rsvp_feedback(&mut self, event_id: String, feedback: RsvpFeedback) {
        self.rsvp_feedback = Some((event_id, feedback));
    }

    fn set_creation_feedback(&mut self, feedback: EventCreationFeedback) {
        self.creation_feedback = Some((Instant::now(), feedback));
    }

    fn render_event_creation_window(&mut self, ctx: &mut AppContext, egui_ctx: &egui::Context) {
        self.prune_creation_feedback();
        if !self.creating_event {
            return;
        }

        let mut open = true;
        egui::Window::new("Create Calendar Event")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .show(egui_ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(500.0)
                    .show(ui, |ui| {
                        self.render_event_creation_contents(ctx, ui);
                    });
            });

        if !open {
            self.creating_event = false;
        }
    }

    fn render_event_creation_contents(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) {
        let has_writable_account = ctx.accounts.selected_filled().is_some();

        if !has_writable_account {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Select an account with its private key to publish events.",
            );
            ui.add_space(6.0);
        }

        if self.creation_pending {
            ui.label("Publishing event…");
        }

        if let Some((_, feedback)) = &self.creation_feedback {
            match feedback {
                EventCreationFeedback::Success(msg) => {
                    ui.colored_label(ui.visuals().hyperlink_color, msg);
                }
                EventCreationFeedback::Error(msg) => {
                    ui.colored_label(Color32::from_rgb(220, 70, 70), msg);
                }
            }
        }

        ui.separator();

        ui.label("Fields marked with * are required.");
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("Event type*");
            ui.selectable_value(
                &mut self.event_draft.event_type,
                DraftEventType::Timed,
                "Timed",
            );
            ui.selectable_value(
                &mut self.event_draft.event_type,
                DraftEventType::AllDay,
                "All-day",
            );
        });

        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("Visibility");
            let toggle_response = ui.add(IosSwitch::new(&mut self.event_draft.is_private));
            if toggle_response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }

            let visibility_text = if self.event_draft.is_private {
                "Private event"
            } else {
                "Public event"
            };

            ui.add_space(8.0);
            ui.label(visibility_text);

            let tooltip_text = if self.event_draft.is_private {
                "Private events are only visible to you, and the invited participants."
            } else {
                "Public events are visible to anyone."
            };

            let info = egui::Label::new(
                egui::RichText::new("i")
                    .strong()
                    .color(ui.visuals().weak_text_color()),
            )
            .sense(egui::Sense::click());

            let response = ui.add(info);
            if response.hovered() || response.clicked() {
                egui::show_tooltip_at_pointer(
                    ui.ctx(),
                    ui.layer_id(),
                    response.id,
                    |ui: &mut egui::Ui| {
                        ui.label(tooltip_text);
                    },
                );
            }
        });

        ui.add_space(6.0);

        ui.label("Title*");
        ui.text_edit_singleline(&mut self.event_draft.title);

        ui.add_space(6.0);

        ui.label("Summary");
        ui.text_edit_singleline(&mut self.event_draft.summary);

        ui.add_space(6.0);

        ui.label("Description");
        ui.text_edit_multiline(&mut self.event_draft.description)
            .on_hover_text("Free-form description for the event body");

        ui.add_space(12.0);

        match self.event_draft.event_type {
            DraftEventType::AllDay => {
                ui.label("Start date (YYYY-MM-DD)*");
                ui.text_edit_singleline(&mut self.event_draft.start_date);

                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.event_draft.include_end, "Specify end date");
                    if self.event_draft.include_end {
                        ui.label("(inclusive)");
                    }
                });

                if self.event_draft.include_end {
                    ui.label("End date (YYYY-MM-DD)");
                    ui.text_edit_singleline(&mut self.event_draft.end_date);
                } else {
                    self.event_draft.end_date.clear();
                }
            }
            DraftEventType::Timed => {
                ui.label("Start date (YYYY-MM-DD)*");
                ui.text_edit_singleline(&mut self.event_draft.start_date);
                ui.add_space(4.0);
                ui.label("Start time (HH:MM)*");
                ui.text_edit_singleline(&mut self.event_draft.start_time);

                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.event_draft.include_end, "Specify end time");
                    if self.event_draft.include_end {
                        ui.label("(end is exclusive)");
                    }
                });

                if self.event_draft.include_end {
                    ui.label("End date (YYYY-MM-DD, blank = same day)");
                    ui.text_edit_singleline(&mut self.event_draft.end_date);
                    ui.add_space(4.0);
                    ui.label("End time (HH:MM)*");
                    ui.text_edit_singleline(&mut self.event_draft.end_time);
                } else {
                    self.event_draft.end_date.clear();
                }

                ui.add_space(6.0);

                ui.label("Start time zone (IANA identifier)");
                ui.text_edit_singleline(&mut self.event_draft.start_tzid);
                ui.add_space(4.0);
                ui.label("End time zone (optional, overrides start)");
                ui.text_edit_singleline(&mut self.event_draft.end_tzid);
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);

        ui.label("Locations (one per line)");
        ui.text_edit_multiline(&mut self.event_draft.locations_text);

        ui.add_space(6.0);
        ui.label("Image URLs (one per line)");
        ui.text_edit_multiline(&mut self.event_draft.images_text);

        ui.add_space(6.0);
        ui.label("Hashtags (space separated, without #)");
        ui.text_edit_singleline(&mut self.event_draft.hashtags_text);

        ui.add_space(6.0);
        ui.label("References / links (one per line)");
        ui.text_edit_multiline(&mut self.event_draft.references_text);

        ui.add_space(6.0);
        ui.label("Calendars to request (enter full 'a' coordinate per line)");
        ui.text_edit_multiline(&mut self.event_draft.calendars_text);

        ui.add_space(6.0);
        ui.label("Participants (npub / nprofile / NIP-05 / hex[,role] per line)");
        let parsed_participants = self.event_draft.parsed_participants();
        let txn = Transaction::new(ctx.ndb).ok();
        ui.add_space(6.0);

        let mut removal: Option<usize> = None;
        let mut pending_absorb = false;

        ui.horizontal_wrapped(|ui| {
            for (idx, (hex, role)) in parsed_participants.iter().enumerate() {
                let (profile, name) =
                    if let (Some(bytes), Some(txn)) = (decode_pubkey_hex(hex), txn.as_ref()) {
                        ctx.unknown_ids.add_pubkey_if_missing(ctx.ndb, txn, &bytes);
                        let profile = ctx.ndb.get_profile_by_pubkey(txn, &bytes).ok();
                        let display = display_name_from_profile(profile.as_ref())
                            .unwrap_or_else(|| short_pubkey(hex));
                        (profile, display)
                    } else {
                        (None, short_pubkey(hex))
                    };

                let mut display = name;
                if let Some(role) = role {
                    display = format!("{display} ({role})");
                }

                ui.group(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            let mut avatar = ProfilePic::from_profile_or_default(
                                ctx.img_cache,
                                profile.as_ref(),
                            )
                            .size(36.0)
                            .border(ProfilePic::border_stroke(ui));
                            let response = ui.add(&mut avatar);
                            response.on_hover_text(display.clone());

                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(display.clone())
                                    .size(13.0)
                                    .color(ui.visuals().text_color()),
                            );
                        });
                        ui.add_space(4.0);
                        if ui.add(egui::Button::new("Remove").small()).clicked() {
                            removal = Some(idx);
                        }
                    });
                });
                ui.add_space(8.0);
            }

            let input_response = ui.add(
                egui::TextEdit::singleline(&mut self.event_draft.participant_input)
                    .hint_text("Add participant")
                    .desired_width(220.0),
            );

            if input_response.changed() && self.event_draft.participant_input.contains('\n') {
                pending_absorb = true;
            }

            if input_response.lost_focus()
                && ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Tab))
            {
                pending_absorb = true;
            }
        });

        if pending_absorb && !self.event_draft.participant_input.trim().is_empty() {
            if !self.event_draft.participant_input.ends_with('\n') {
                self.event_draft.participant_input.push('\n');
            }
            self.event_draft.absorb_participant_input();
        }

        if let Some(idx) = removal {
            self.event_draft.participants.remove(idx);
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                self.creating_event = false;
            }

            let can_publish = has_writable_account && !self.creation_pending;
            if ui
                .add_enabled(can_publish, egui::Button::new("Publish event"))
                .clicked()
            {
                self.submit_event_creation(ctx);
            }
        });
    }

    fn submit_event_creation(&mut self, ctx: &mut AppContext) {
        if self.creation_pending {
            return;
        }

        if self.event_draft.identifier.trim().is_empty() {
            self.set_creation_feedback(EventCreationFeedback::Error(
                "Identifier is required.".to_string(),
            ));
            return;
        }

        if self.event_draft.title.trim().is_empty() {
            self.set_creation_feedback(EventCreationFeedback::Error(
                "Title is required.".to_string(),
            ));
            return;
        }

        let Some(filled) = ctx.accounts.selected_filled() else {
            self.set_creation_feedback(EventCreationFeedback::Error(
                "Select an account with its private key to publish events.".to_string(),
            ));
            return;
        };

        let account = filled.to_full();
        self.creation_pending = true;

        self.event_draft.absorb_participant_input();

        match self.build_calendar_event_note(&self.event_draft, &account) {
            Ok((note, mut event)) => {
                self.populate_event_rsvps(&mut event);
                let new_event_id = event.id_hex.clone();
                let focus_date = event.date_span(&self.timezone).0;

                let event_msg = match ClientMessage::event(&note) {
                    Ok(msg) => msg,
                    Err(_) => {
                        self.creation_pending = false;
                        self.set_creation_feedback(EventCreationFeedback::Error(
                            "Failed to serialize calendar event.".to_string(),
                        ));
                        return;
                    }
                };

                if let Ok(json) = event_msg.to_json() {
                    let _ = ctx
                        .ndb
                        .process_event_with(&json, IngestMetadata::new().client(true));
                }

                let private_count = if self.event_draft.is_private {
                    match self.publish_private_event(ctx, &account, &note, &event) {
                        Ok(count) => Some(count),
                        Err(err) => {
                            self.creation_pending = false;
                            self.set_creation_feedback(EventCreationFeedback::Error(err));
                            return;
                        }
                    }
                } else {
                    ctx.pool.send(&event_msg);
                    None
                };

                self.upsert_event(event);
                self.resort_events();
                if let Some(idx) = self.events.iter().position(|ev| ev.id_hex == new_event_id) {
                    self.selected_event = Some(idx);
                    self.view = CalendarView::Event;
                }
                self.focus_date = focus_date;

                self.creation_pending = false;
                self.creating_event = false;
                self.event_draft.reset_preserving_type();

                let success_msg = match private_count {
                    Some(0) => {
                        "Private calendar event prepared, but no recipients resolved.".to_string()
                    }
                    Some(1) => "Private calendar event gift wrapped for 1 recipient.".to_string(),
                    Some(count) => format!(
                        "Private calendar event gift wrapped for {} recipients.",
                        count
                    ),
                    None => "Calendar event published.".to_string(),
                };

                self.set_creation_feedback(EventCreationFeedback::Success(success_msg));
            }
            Err(err) => {
                self.creation_pending = false;
                self.set_creation_feedback(EventCreationFeedback::Error(err));
            }
        }
    }

    fn build_calendar_event_note(
        &self,
        draft: &CalendarEventDraft,
        account: &FullKeypair,
    ) -> Result<(nostrdb::Note<'static>, CalendarEvent), String> {
        let identifier = draft.identifier.trim();
        if identifier.is_empty() {
            return Err("Identifier is required.".to_string());
        }

        let title = draft.title.trim();
        if title.is_empty() {
            return Err("Title is required.".to_string());
        }

        let summary = draft.summary.trim();
        let content_owned = draft.description.clone();
        let mut builder = nostrdb::NoteBuilder::new()
            .kind(draft.event_type.as_kind())
            .content(content_owned.as_str());

        builder = builder.start_tag().tag_str("d").tag_str(identifier);
        builder = builder.start_tag().tag_str("title").tag_str(title);

        if !summary.is_empty() {
            builder = builder.start_tag().tag_str("summary").tag_str(summary);
        }

        for loc in draft.parsed_locations() {
            builder = builder.start_tag().tag_str("location").tag_str(&loc);
        }

        for image in draft.parsed_images() {
            builder = builder.start_tag().tag_str("image").tag_str(&image);
        }

        for hashtag in draft.parsed_hashtags() {
            builder = builder.start_tag().tag_str("t").tag_str(&hashtag);
        }

        for reference in draft.parsed_references() {
            builder = builder.start_tag().tag_str("r").tag_str(&reference);
        }

        for calendar in draft.parsed_calendars() {
            builder = builder.start_tag().tag_str("a").tag_str(&calendar);
        }

        for (pubkey, role) in draft.parsed_participants() {
            let mut tag_builder = builder.start_tag().tag_str("p").tag_str(&pubkey);
            if let Some(role_value) = role {
                tag_builder = tag_builder.tag_str("").tag_str(&role_value);
            }
            builder = tag_builder;
        }

        match draft.event_type {
            DraftEventType::AllDay => {
                let start_date =
                    CalendarEventDraft::parse_required_date(&draft.start_date, "Start date")?;

                let mut end_date = if draft.include_end {
                    CalendarEventDraft::parse_optional_date(&draft.end_date, "End date")?
                        .unwrap_or(start_date)
                } else {
                    start_date
                };

                if end_date < start_date {
                    return Err("End date cannot be before start date.".to_string());
                }

                builder = builder
                    .start_tag()
                    .tag_str("start")
                    .tag_str(&start_date.format("%Y-%m-%d").to_string());

                if end_date > start_date {
                    end_date = end_date + Duration::days(1);
                    builder = builder
                        .start_tag()
                        .tag_str("end")
                        .tag_str(&end_date.format("%Y-%m-%d").to_string());
                }
            }
            DraftEventType::Timed => {
                let start_date =
                    CalendarEventDraft::parse_required_date(&draft.start_date, "Start date")?;
                let start_time =
                    CalendarEventDraft::parse_required_time(&draft.start_time, "Start time")?;
                let start_naive = start_date.and_time(start_time);
                let start_tz_trimmed = draft.start_tzid.trim();

                let (start_ts, start_tz_tag) =
                    resolve_timestamp(start_naive, start_tz_trimmed, "Start time")?;
                builder = builder
                    .start_tag()
                    .tag_str("start")
                    .tag_str(&start_ts.to_string());

                if let Some(tz_value) = start_tz_tag {
                    if !tz_value.is_empty() {
                        builder = builder.start_tag().tag_str("start_tzid").tag_str(&tz_value);
                    }
                }

                if draft.include_end {
                    let end_time =
                        CalendarEventDraft::parse_required_time(&draft.end_time, "End time")?;

                    let end_date = if draft.end_date.trim().is_empty() {
                        start_date
                    } else {
                        CalendarEventDraft::parse_required_date(&draft.end_date, "End date")?
                    };

                    let end_naive = end_date.and_time(end_time);
                    let end_tz_trimmed = draft.end_tzid.trim();
                    let tz_for_end = if end_tz_trimmed.is_empty() {
                        start_tz_trimmed
                    } else {
                        end_tz_trimmed
                    };
                    let (end_ts, end_tz_tag) =
                        resolve_timestamp(end_naive, tz_for_end, "End time")?;

                    if end_ts < start_ts {
                        return Err("End time must be after start time.".to_string());
                    }

                    builder = builder
                        .start_tag()
                        .tag_str("end")
                        .tag_str(&end_ts.to_string());

                    if !end_tz_trimmed.is_empty() {
                        if let Some(tz_value) = end_tz_tag {
                            builder = builder.start_tag().tag_str("end_tzid").tag_str(&tz_value);
                        }
                    }
                }
            }
        }

        let secret_bytes = account.secret_key.secret_bytes();
        let Some(note) = builder.sign(&secret_bytes).build() else {
            return Err("Failed to build calendar event.".to_string());
        };

        let Some(event) = parse_calendar_event(&note) else {
            return Err("Failed to parse calendar event after building.".to_string());
        };

        Ok((note, event))
    }

    fn private_recipients(event: &CalendarEvent, account: &FullKeypair) -> Vec<String> {
        let mut recipients: HashSet<String> = HashSet::new();
        recipients.insert(account.pubkey.hex().to_ascii_lowercase());

        for participant in &event.participants {
            if participant.pubkey_hex.len() == 64 {
                recipients.insert(participant.pubkey_hex.to_ascii_lowercase());
            }
        }

        recipients.into_iter().collect()
    }

    fn random_backdated_timestamp() -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut rng = rand::rng();
        let range = RANGE_RANDOM_TIMESTAMP_TWEAK;
        let tweak = if range.start >= range.end {
            0
        } else {
            rng.random_range(range)
        };

        now.saturating_sub(tweak)
    }

    fn publish_private_event(
        &self,
        ctx: &mut AppContext,
        account: &FullKeypair,
        note: &nostrdb::Note<'static>,
        event: &CalendarEvent,
    ) -> Result<usize, String> {
        let recipients = Self::private_recipients(event, account);
        if recipients.is_empty() {
            return Err("No recipients resolved for private event.".to_string());
        }

        let note_json = note
            .json()
            .map_err(|err| format!("Failed to serialize calendar event: {err}"))?;
        let event_for_rumor = NostrEvent::from_json(note_json)
            .map_err(|err| format!("Failed to parse calendar event: {err}"))?;
        let rumor_json = UnsignedEvent::from(event_for_rumor).as_json();

        let account_secret_bytes = account.secret_key.secret_bytes();
        let mut wrapped_count = 0usize;

        for recipient_hex in recipients {
            let recipient_pk = NostrPublicKey::from_hex(&recipient_hex)
                .map_err(|_| format!("Invalid recipient pubkey: {}", recipient_hex))?;

            let encrypted_rumor = nip44::encrypt(
                &account.secret_key,
                &recipient_pk,
                &rumor_json,
                Nip44Version::default(),
            )
            .map_err(|err| format!("Failed to encrypt rumor for {}: {err}", recipient_hex))?;

            let mut seal_builder = nostrdb::NoteBuilder::new()
                .kind(13)
                .content(&encrypted_rumor)
                .created_at(Self::random_backdated_timestamp());
            seal_builder = seal_builder.sign(&account_secret_bytes);
            let Some(seal_note) = seal_builder.build() else {
                return Err("Failed to build seal event.".to_string());
            };

            let seal_json = seal_note
                .json()
                .map_err(|err| format!("Failed to serialize seal event: {err}"))?;
            let keys = NostrKeys::generate();
            let encrypted_seal = nip44::encrypt(
                keys.secret_key(),
                &recipient_pk,
                &seal_json,
                Nip44Version::default(),
            )
            .map_err(|err| format!("Failed to encrypt gift wrap for {}: {err}", recipient_hex))?;

            let mut gift_builder = nostrdb::NoteBuilder::new()
                .kind(1059)
                .content(&encrypted_seal)
                .created_at(Self::random_backdated_timestamp());
            gift_builder = gift_builder
                .start_tag()
                .tag_str("p")
                .tag_str(&recipient_hex);

            let ephemeral_secret_bytes = keys.secret_key().secret_bytes();
            gift_builder = gift_builder.sign(&ephemeral_secret_bytes);
            let Some(gift_note) = gift_builder.build() else {
                return Err("Failed to build gift wrap event.".to_string());
            };

            let gift_msg = ClientMessage::event(&gift_note)
                .map_err(|err| format!("Failed to serialize gift wrap event: {err}"))?;

            if let Ok(json) = gift_msg.to_json() {
                let _ = ctx
                    .ndb
                    .process_event_with(&json, IngestMetadata::new().client(true));
            }

            ctx.pool.send(&gift_msg);

            wrapped_count += 1;
        }

        Ok(wrapped_count)
    }

    fn current_user_rsvp(&self, event: &CalendarEvent) -> Option<RsvpStatus> {
        if self.user_pubkey_hex.is_empty() {
            return None;
        }

        event
            .rsvps
            .iter()
            .find(|r| r.attendee_hex.eq_ignore_ascii_case(&self.user_pubkey_hex))
            .map(|r| r.status)
    }

    fn status_label(status: Option<RsvpStatus>) -> Option<&'static str> {
        match status {
            Some(RsvpStatus::Accepted) => Some("Accepted"),
            Some(RsvpStatus::Declined) => Some("Declined"),
            Some(RsvpStatus::Tentative) => Some("Maybe"),
            _ => None,
        }
    }

    fn annotate_title_with_status<'a>(base: &'a str, status: Option<RsvpStatus>) -> Cow<'a, str> {
        if let Some(label) = Self::status_label(status) {
            Cow::Owned(format!("{base} · {label}"))
        } else {
            Cow::Borrowed(base)
        }
    }

    fn status_cache_suffix(status: Option<RsvpStatus>) -> &'static str {
        match status {
            Some(RsvpStatus::Accepted) => "acc",
            Some(RsvpStatus::Declined) => "dec",
            Some(RsvpStatus::Tentative) => "tent",
            Some(RsvpStatus::Unknown) => "unk",
            None => "none",
        }
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
        let current_status = self.current_user_rsvp(event);

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

        if let Some((feedback_event_id, feedback)) = &self.rsvp_feedback {
            if feedback_event_id == &event.id_hex {
                match feedback {
                    RsvpFeedback::Success(msg) => {
                        ui.colored_label(ui.visuals().hyperlink_color, msg);
                    }
                    RsvpFeedback::Error(msg) => {
                        ui.colored_label(Color32::from_rgb(220, 70, 70), msg);
                    }
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
            self.set_rsvp_feedback(
                event.id_hex.clone(),
                RsvpFeedback::Error(
                    "Event is missing calendar identifier; unable to RSVP.".to_string(),
                ),
            );
            return;
        };

        let Some(filled) = ctx.accounts.selected_filled() else {
            self.set_rsvp_feedback(
                event.id_hex.clone(),
                RsvpFeedback::Error("Select an account with its private key to RSVP.".to_string()),
            );
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
            self.set_rsvp_feedback(
                event.id_hex.clone(),
                RsvpFeedback::Error("Failed to build RSVP event.".to_string()),
            );
            return;
        };

        let Ok(event_msg) = ClientMessage::event(&note) else {
            self.rsvp_pending = false;
            self.set_rsvp_feedback(
                event.id_hex.clone(),
                RsvpFeedback::Error("Failed to serialize RSVP event.".to_string()),
            );
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

        self.pending_rsvps.insert(new_rsvp.id_hex.clone(), new_rsvp);

        self.rsvp_pending = false;
        self.set_rsvp_feedback(
            event.id_hex.clone(),
            RsvpFeedback::Success(format!("{} RSVP sent", status.display_label())),
        );
    }

    fn is_event_visible(&self, event: &CalendarEvent) -> bool {
        if !self.wot_only {
            return true;
        }

        self.wot_cache
            .as_ref()
            .map(|cache| cache.contains(&event.author_hex))
            .unwrap_or(true)
    }

    fn ensure_selected_event_visible(&mut self) {
        if let Some(idx) = self.selected_event {
            let visible = self
                .events
                .get(idx)
                .map(|event| self.is_event_visible(event))
                .unwrap_or(false);

            if !visible {
                self.selected_event = None;
                if matches!(self.view, CalendarView::Event) {
                    self.view = CalendarView::Day;
                }
            }
        }
    }

    fn events_on(&self, date: NaiveDate) -> Vec<usize> {
        self.events
            .iter()
            .enumerate()
            .filter_map(|(idx, event)| {
                if self.is_event_visible(event) && event.occurs_on(date, &self.timezone) {
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

        let status = self.current_user_rsvp(event);
        let title_color = ui.visuals().strong_text_color();
        let base_title: Cow<'_, str> = if max_width <= 220.0 {
            Cow::Borrowed(event.day_title())
        } else {
            let chars_per_line = ((max_width / 7.0).floor() as usize).clamp(12, 96);
            let max_lines = if max_width > 360.0 { 6 } else { 4 };
            Cow::Owned(wrap_title(&event.title, chars_per_line, max_lines))
        };

        let title_text: Cow<'_, str> = if let Some(label) = Self::status_label(status) {
            Cow::Owned(format!("{} · {}", base_title, label))
        } else {
            base_title
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

    fn render_event(
        &mut self,
        ctx: &mut AppContext,
        ui: &mut egui::Ui,
    ) -> Option<ScrollAreaOutput<()>> {
        let Some(idx) = self.selected_event else {
            ui.label("Select an event from any calendar view to see its details.");
            return None;
        };

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
                        self.copy_identifier_row(
                            ctx,
                            ui,
                            "Identifier (naddr):",
                            &naddr,
                            &event_snapshot.id_hex,
                            "naddr",
                        );
                    } else if let Some(identifier) = &event.identifier {
                        ui.label(format!("Identifier: {identifier}"));
                    }
                    if let Some(nevent) = event_nevent(event) {
                        self.copy_identifier_row(
                            ctx,
                            ui,
                            "Event (nevent):",
                            &nevent,
                            &event_snapshot.id_hex,
                            "nevent",
                        );
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

    fn copy_identifier_row(
        &self,
        ctx: &mut AppContext,
        ui: &mut egui::Ui,
        label: &str,
        value: &str,
        event_id: &str,
        suffix: &str,
    ) {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.monospace(truncated_identifier(value));
            ui.add_space(4.0);

            let copy_img = if ui.visuals().dark_mode {
                copy_to_clipboard_image()
            } else {
                copy_to_clipboard_dark_image()
            };

            let animation_id = format!("copy-{suffix}-{event_id}");
            let helper = AnimationHelper::new(ui, animation_id, vec2(16.0, 16.0));
            copy_img.paint_at(ui, helper.scaled_rect());

            if helper.take_animation_response().clicked() {
                let _ = ctx.clipboard.set_text(value.to_owned());
            }
        });
    }
}

impl App for CalendarApp {
    fn update(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> AppResponse {
        let new_user_hex = hex::encode(ctx.accounts.selected_account_pubkey_bytes());
        if self.user_pubkey_hex != new_user_hex {
            self.user_pubkey_hex = new_user_hex;
            self.wot_cache = None;
        }

        self.ensure_subscription(ctx);
        self.load_initial_events(ctx);
        self.poll_for_new_notes(ctx);
        self.prune_creation_feedback();
        self.ensure_wot_cache(ctx);
        self.ensure_selected_event_visible();

        let mut action = None;
        let mut drag_ids = Vec::new();
        let mut open_creation_requested = false;
        let mut wot_changed = false;
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                if ui.button("← Back to Notedeck").clicked() {
                    action = Some(AppAction::ShowColumns);
                }
            });

            ui.separator();
            self.view_switcher(ui);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("New Event").clicked() {
                    open_creation_requested = true;
                }

                if self.creation_pending {
                    ui.label("Publishing event…");
                } else if let Some((_, feedback)) = &self.creation_feedback {
                    match feedback {
                        EventCreationFeedback::Success(message) => {
                            ui.colored_label(ui.visuals().hyperlink_color, message);
                        }
                        EventCreationFeedback::Error(message) => {
                            ui.colored_label(Color32::from_rgb(220, 70, 70), message);
                        }
                    }
                }
            });
            ui.add_space(6.0);
            self.navigation_bar(ui);
            ui.add_space(8.0);
            self.timezone_controls(ui);
            ui.horizontal(|ui| {
                let toggle_response = ui.add(IosSwitch::new(&mut self.wot_only));
                if toggle_response.changed() {
                    self.wot_cache = None;
                    wot_changed = true;
                }

                let state_label = if self.wot_only {
                    "Friends-of-friends"
                } else {
                    "Nostr calendar firehose"
                };
                ui.add_space(6.0);
                ui.label(state_label);
                ui.add_space(4.0);
                let tooltip = if self.wot_only {
                    "Friends-of-friends: Limit events to authors you follow and their followers."
                } else {
                    "Display all calendar events from your relay list."
                };
                info_icon(ui, tooltip);
            });
            ui.add_space(8.0);

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

        if wot_changed {
            self.ensure_wot_cache(ctx);
            self.ensure_selected_event_visible();
        }

        if open_creation_requested {
            if !self.creating_event {
                self.event_draft.reset_preserving_type();
            }
            self.creating_event = true;
        }

        self.render_event_creation_window(ctx, ui.ctx());

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

fn resolve_timestamp(
    naive: NaiveDateTime,
    tzid: &str,
    label: &str,
) -> Result<(i64, Option<String>), String> {
    let trimmed = tzid.trim();
    if trimmed.is_empty() {
        return Ok((Utc.from_utc_datetime(&naive).timestamp(), None));
    }

    let tz: Tz = trimmed
        .parse()
        .map_err(|_| format!("{label} has unknown time zone '{trimmed}'."))?;

    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Ok((
            dt.with_timezone(&Utc).timestamp(),
            Some(trimmed.to_string()),
        )),
        LocalResult::Ambiguous(first, second) => {
            let chosen = if first <= second { first } else { second };
            Ok((
                chosen.with_timezone(&Utc).timestamp(),
                Some(trimmed.to_string()),
            ))
        }
        LocalResult::None => Err(format!(
            "{label} {naive} does not exist in time zone {trimmed} due to an offset transition.",
        )),
    }
}

fn truncated_identifier(value: &str) -> String {
    if value.len() <= 16 {
        return value.to_owned();
    }

    let prefix = &value[..8];
    let suffix = &value[value.len().saturating_sub(8)..];
    format!("{prefix}…{suffix}")
}

fn default_timezone_name() -> String {
    if let Ok(name) = get_timezone() {
        if !name.trim().is_empty() {
            return name;
        }
    }

    guess_local_timezone(Local::now())
        .map(|tz| tz.name().to_string())
        .unwrap_or_else(|| "UTC".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::nips::nip19::{Nip19, Nip19Profile, ToBech32};

    #[test]
    fn timed_events_use_kind_31923() {
        let app = CalendarApp::new();
        let account = FullKeypair::generate();
        let mut draft = CalendarEventDraft::with_kind(DraftEventType::Timed);
        draft.title = "Meeting".to_string();
        draft.description = "Discuss roadmap".to_string();

        let (note, event) = app
            .build_calendar_event_note(&draft, &account)
            .expect("should build timed event");

        assert_eq!(note.kind(), DraftEventType::Timed.as_kind());
        assert_eq!(event.kind, DraftEventType::Timed.as_kind());
    }

    #[test]
    fn all_day_events_use_kind_31922() {
        let app = CalendarApp::new();
        let account = FullKeypair::generate();
        let mut draft = CalendarEventDraft::with_kind(DraftEventType::AllDay);
        draft.title = "Holiday".to_string();
        draft.description = "Out of office".to_string();
        draft.include_end = true;
        draft.end_date = draft.start_date.clone();

        let (note, event) = app
            .build_calendar_event_note(&draft, &account)
            .expect("should build all-day event");

        assert_eq!(note.kind(), DraftEventType::AllDay.as_kind());
        assert_eq!(event.kind, DraftEventType::AllDay.as_kind());
    }

    #[test]
    fn draft_defaults_use_local_timezone() {
        let draft = CalendarEventDraft::new();
        let expected = default_timezone_name();
        assert_eq!(draft.start_tzid, expected);
        assert_eq!(draft.end_tzid, expected);
    }

    #[test]
    fn participants_accept_extended_formats() {
        let keys = NostrKeys::generate();
        let expected_hex = keys.public_key().to_hex();

        let npub = keys
            .public_key()
            .to_bech32()
            .expect("npub encoding should succeed");

        let mut draft = CalendarEventDraft::new();
        draft.participant_input = format!("{npub},speaker");
        draft.absorb_participant_input();
        let parsed = draft.parsed_participants();
        assert_eq!(
            parsed,
            vec![(expected_hex.clone(), Some("speaker".to_string()))]
        );

        let profile = Nip19Profile::new(keys.public_key(), Vec::<&str>::new())
            .expect("should construct profile");
        let nprofile = Nip19::Profile(profile)
            .to_bech32()
            .expect("nprofile encoding should succeed");

        draft.participant_input = nprofile;
        draft.absorb_participant_input();
        let parsed_profile = draft.parsed_participants();
        assert_eq!(parsed_profile[1].0, expected_hex);
        assert!(parsed_profile[1].1.is_none());
    }
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
