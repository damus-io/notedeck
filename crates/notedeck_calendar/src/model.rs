use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use nostr::event::id::EventId;
use nostr::nips::nip01::Coordinate as Nip01Coordinate;
use nostr::nips::nip19::{Nip19Event, ToBech32};
use nostr::{Kind as NostrKind, PublicKey};
use nostrdb::Note;

use crate::TimeZoneChoice;

#[derive(Debug, Clone)]
pub enum CalendarEventTime {
    AllDay {
        start: NaiveDate,
        end: NaiveDate,
    },
    Timed {
        start_utc: NaiveDateTime,
        end_utc: Option<NaiveDateTime>,
        start_tzid: Option<String>,
        end_tzid: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct CalendarParticipant {
    pub pubkey_hex: String,
    pub role: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CalendarRsvp {
    pub id_hex: String,
    pub attendee_hex: String,
    pub status: RsvpStatus,
    pub created_at: u64,
    pub coordinate_kind: Option<u32>,
    pub coordinate_author_hex: Option<String>,
    pub coordinate_identifier: Option<String>,
    pub event_id_hex: Option<String>,
}

impl CalendarRsvp {
    pub fn is_newer_than(&self, other: &Self) -> bool {
        self.created_at > other.created_at
            || (self.created_at == other.created_at && self.id_hex > other.id_hex)
    }

    pub fn is_confirmed(&self) -> bool {
        matches!(self.status, RsvpStatus::Accepted)
    }

    pub fn matches_event(&self, event: &CalendarEvent) -> bool {
        if let Some(event_id) = &self.event_id_hex {
            if event_id == &event.id_hex {
                return true;
            }
        }

        match (
            self.coordinate_kind,
            self.coordinate_author_hex.as_deref(),
            self.coordinate_identifier.as_deref(),
            event.identifier.as_deref(),
        ) {
            (Some(kind), Some(author), Some(identifier), Some(event_identifier)) => {
                kind == event.kind
                    && author.eq_ignore_ascii_case(&event.author_hex)
                    && identifier == event_identifier
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum RsvpFeedback {
    Success(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsvpStatus {
    Accepted,
    Declined,
    Tentative,
    Unknown,
}

impl RsvpStatus {
    pub fn from_tag(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "accepted" => RsvpStatus::Accepted,
            "declined" => RsvpStatus::Declined,
            "tentative" => RsvpStatus::Tentative,
            _ => RsvpStatus::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            RsvpStatus::Accepted => "accepted",
            RsvpStatus::Declined => "declined",
            RsvpStatus::Tentative => "tentative",
            RsvpStatus::Unknown => "unknown",
        }
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            RsvpStatus::Accepted => "Accepted",
            RsvpStatus::Declined => "Declined",
            RsvpStatus::Tentative => "Tentative",
            RsvpStatus::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CalendarEvent {
    pub kind: u32,
    pub id_hex: String,
    pub identifier: Option<String>,
    pub title: String,
    pub title_month: String,
    pub title_week: String,
    pub title_day: String,
    pub summary: Option<String>,
    pub summary_preview: Option<String>,
    pub description: Option<String>,
    pub images: Vec<String>,
    pub locations: Vec<String>,
    pub hashtags: Vec<String>,
    pub references: Vec<String>,
    pub participants: Vec<CalendarParticipant>,
    pub rsvps: Vec<CalendarRsvp>,
    pub calendars: Vec<String>,
    pub time: CalendarEventTime,
    pub author_hex: String,
    pub created_at: u64,
}

impl CalendarEvent {
    pub fn start_naive(&self) -> NaiveDateTime {
        match &self.time {
            CalendarEventTime::AllDay { start, .. } => start.and_time(NaiveTime::MIN),
            CalendarEventTime::Timed { start_utc, .. } => *start_utc,
        }
    }

    pub fn duration_text(&self, timezone: &TimeZoneChoice) -> String {
        match &self.time {
            CalendarEventTime::AllDay { start, end } => {
                let start_fmt = start.format("%b %e, %Y");
                if start == end {
                    format!("{start_fmt}")
                } else {
                    let end_fmt = end.format("%b %e, %Y");
                    format!("{start_fmt} – {end_fmt}")
                }
            }
            CalendarEventTime::Timed {
                start_utc, end_utc, ..
            } => {
                let start_local = timezone.localize(start_utc);
                match end_utc {
                    Some(end) => {
                        let end_local = timezone.localize(&end);
                        if start_local.date == end_local.date
                            && start_local.abbreviation == end_local.abbreviation
                        {
                            format!(
                                "{} {} – {} {}",
                                start_local.date_text,
                                start_local.time_text,
                                end_local.time_text,
                                start_local.abbreviation
                            )
                        } else {
                            format!(
                                "{} {} ({}) – {} {} ({})",
                                start_local.date_text,
                                start_local.time_text,
                                start_local.abbreviation,
                                end_local.date_text,
                                end_local.time_text,
                                end_local.abbreviation
                            )
                        }
                    }
                    None => format!(
                        "{} {} ({})",
                        start_local.date_text, start_local.time_text, start_local.abbreviation
                    ),
                }
            }
        }
    }

    pub fn occurs_on(&self, date: NaiveDate, timezone: &TimeZoneChoice) -> bool {
        match &self.time {
            CalendarEventTime::AllDay { start, end } => date >= *start && date <= *end,
            CalendarEventTime::Timed {
                start_utc, end_utc, ..
            } => {
                let start = timezone.localize(start_utc);
                let end = end_utc
                    .map(|end| timezone.localize(&end))
                    .unwrap_or_else(|| start.clone());

                let start_date = start.date;
                let mut end_date = end.date;
                if end_date < start_date {
                    end_date = start_date;
                }

                date >= start_date && date <= end_date
            }
        }
    }

    pub fn week_title(&self) -> &str {
        &self.title_week
    }

    pub fn day_title(&self) -> &str {
        &self.title_day
    }

    pub fn month_title(&self) -> &str {
        &self.title_month
    }

    pub fn summary_preview(&self) -> Option<&str> {
        self.summary_preview.as_deref()
    }

    pub fn date_span(&self, timezone: &TimeZoneChoice) -> (NaiveDate, NaiveDate) {
        match &self.time {
            CalendarEventTime::AllDay { start, end } => (*start, *end),
            CalendarEventTime::Timed {
                start_utc, end_utc, ..
            } => {
                let start_local = timezone.localize(start_utc);
                let end_local = end_utc
                    .map(|end| timezone.localize(&end))
                    .unwrap_or_else(|| start_local.clone());

                let start_date = start_local.date;
                let mut end_date = end_local.date;
                if end_date < start_date {
                    end_date = start_date;
                }

                (start_date, end_date)
            }
        }
    }
}

pub fn parse_calendar_event(note: &Note<'_>) -> Option<CalendarEvent> {
    let note_id = note.id();
    let author = note.pubkey();
    let kind = note.kind();

    let mut title = None;
    let mut d_identifier = None;
    let mut summary = None;
    let mut images = Vec::new();
    let mut locations = Vec::new();
    let mut hashtags = Vec::new();
    let mut references = Vec::new();
    let mut participants = Vec::new();
    let mut calendars = Vec::new();
    let mut start_str: Option<String> = None;
    let mut end_str: Option<String> = None;
    let mut start_tzid: Option<String> = None;
    let mut end_tzid: Option<String> = None;

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some(name) = tag.get_str(0) else {
            continue;
        };

        match name {
            "d" => {
                if let Some(id) = tag.get_str(1) {
                    d_identifier = Some(id.to_owned());
                }
            }
            "title" => {
                if let Some(t) = tag.get_str(1) {
                    title = Some(t.to_owned());
                }
            }
            "summary" => {
                if let Some(s) = tag.get_str(1) {
                    summary = Some(s.to_owned());
                }
            }
            "image" => {
                if let Some(img) = tag.get_str(1) {
                    push_unique(&mut images, img);
                }
            }
            "location" => {
                if let Some(loc) = tag.get_str(1) {
                    locations.push(loc.to_owned());
                }
            }
            "g" => {
                if let Some(gh) = tag.get_str(1) {
                    locations.push(format!("Geohash: {gh}"));
                }
            }
            "p" => {
                let pubkey_hex = tag.get_id(1).map(|pk| hex::encode(*pk)).unwrap_or_default();
                let role = tag.get_str(3).map(|s| s.to_owned());
                participants.push(CalendarParticipant { pubkey_hex, role });
            }
            "t" => {
                if let Some(hash) = tag.get_str(1) {
                    hashtags.push(hash.to_owned());
                }
            }
            "r" => {
                if let Some(url) = tag.get_str(1) {
                    references.push(url.to_owned());
                }
            }
            "a" => {
                if let Some(reference) = tag.get_str(1) {
                    calendars.push(reference.to_owned());
                }
            }
            "start" => {
                if let Some(s) = tag.get_str(1) {
                    start_str = Some(s.to_owned());
                }
            }
            "end" => {
                if let Some(e) = tag.get_str(1) {
                    end_str = Some(e.to_owned());
                }
            }
            "start_tzid" => {
                start_tzid = tag.get_str(1).map(|s| s.to_owned());
            }
            "end_tzid" => {
                end_tzid = tag.get_str(1).map(|s| s.to_owned());
            }
            _ => {}
        }
    }

    title.as_ref()?;
    start_str.as_ref()?;

    let time = match kind {
        31922 => {
            let start_date =
                NaiveDate::parse_from_str(start_str.as_ref().unwrap(), "%Y-%m-%d").ok()?;

            let end_date = match end_str {
                Some(ref end) => {
                    let exclusive = NaiveDate::parse_from_str(end, "%Y-%m-%d").ok()?;
                    let mut inclusive = exclusive - Duration::days(1);
                    if inclusive < start_date {
                        inclusive = start_date;
                    }
                    inclusive
                }
                None => start_date,
            };

            CalendarEventTime::AllDay {
                start: start_date,
                end: end_date,
            }
        }
        31923 => {
            let start_ts: i64 = start_str.as_ref()?.parse().ok()?;
            let start_dt =
                DateTime::<Utc>::from_timestamp(start_ts, 0).map(|dt| dt.naive_utc())?;

            let end_dt = match end_str {
                Some(ref end) => {
                    let end_ts: i64 = end.parse().ok()?;
                    DateTime::<Utc>::from_timestamp(end_ts, 0).map(|dt| {
                        let naive = dt.naive_utc();
                        if naive < start_dt {
                            start_dt
                        } else {
                            naive
                        }
                    })
                }
                None => None,
            };

            CalendarEventTime::Timed {
                start_utc: start_dt,
                end_utc: end_dt,
                start_tzid,
                end_tzid,
            }
        }
        _ => return None,
    };

    let description = if note.content().is_empty() {
        None
    } else {
        Some(note.content().to_string())
    };

    if let Some(desc) = &description {
        for url in extract_image_urls(desc) {
            push_unique(&mut images, &url);
        }
    }

    let summary_preview = summary.as_ref().map(|s| wrap_title(s, 30, 3));
    let title_value = title.unwrap();
    let title_month = wrap_title(&title_value, 12, 2);
    let title_week = wrap_title(&title_value, 18, 2);
    let title_day = wrap_title(&title_value, 26, 4);
    let author_hex = hex::encode(author);

    Some(CalendarEvent {
        kind,
        id_hex: hex::encode(note_id),
        identifier: d_identifier,
        title: title_value,
        title_month,
        title_week,
        title_day,
        summary_preview,
        summary,
        description,
        images,
        locations,
        hashtags,
        references,
        participants,
        rsvps: Vec::new(),
        calendars,
        time,
        author_hex,
        created_at: note.created_at(),
    })
}

pub fn parse_calendar_rsvp(note: &Note<'_>) -> Option<CalendarRsvp> {
    let note_id = note.id();
    let attendee_hex = hex::encode(note.pubkey());

    let mut coordinate_kind = None;
    let mut coordinate_author_hex = None;
    let mut coordinate_identifier = None;
    let mut status_str = None;
    let mut event_id_hex = None;

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some(name) = tag.get_str(0) else {
            continue;
        };
        let name_lower = name.to_ascii_lowercase();

        match name_lower.as_str() {
            "a" if coordinate_kind.is_none() => {
                if let Some(value) = tag.get_str(1) {
                    if let Some((kind, author_hex, identifier)) = parse_event_coordinate(value) {
                        coordinate_kind = Some(kind);
                        coordinate_author_hex = Some(author_hex);
                        coordinate_identifier = Some(identifier);
                    }
                }
            }
            "e" if event_id_hex.is_none() => {
                if let Some(value) = tag.get_str(1) {
                    event_id_hex = Some(value.to_owned());
                }
            }
            "status" => {
                if let Some(value) = tag.get_str(1) {
                    status_str = Some(value.to_owned());
                }
            }
            "l" => {
                if tag.count() >= 3 {
                    if let Some(label) = tag.get_str(2) {
                        if label.eq_ignore_ascii_case("status") {
                            if let Some(value) = tag.get_str(1) {
                                status_str = Some(value.to_owned());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let status_value = status_str?;
    let status_enum = RsvpStatus::from_tag(status_value.trim());

    Some(CalendarRsvp {
        id_hex: hex::encode(note_id),
        attendee_hex,
        status: status_enum,
        created_at: note.created_at(),
        coordinate_kind,
        coordinate_author_hex,
        coordinate_identifier,
        event_id_hex: event_id_hex.map(|id| id.trim().to_ascii_lowercase()),
    })
}

pub fn parse_event_coordinate(value: &str) -> Option<(u32, String, String)> {
    let mut parts = value.splitn(3, ':');
    let kind_str = parts.next()?;
    let pubkey_str = parts.next()?;
    let identifier = parts.next()?.trim().to_string();

    if identifier.is_empty() {
        return None;
    }

    let kind: u32 = kind_str.parse().ok()?;
    if kind != 31922 && kind != 31923 {
        return None;
    }

    Some((kind, pubkey_str.to_ascii_lowercase(), identifier))
}

pub fn match_rsvps_for_event(event: &CalendarEvent, rsvps: &[CalendarRsvp]) -> Vec<CalendarRsvp> {
    let mut best_by_attendee: std::collections::HashMap<String, CalendarRsvp> =
        std::collections::HashMap::new();

    for rsvp in rsvps {
        if !rsvp.matches_event(event) {
            continue;
        }

        let entry = best_by_attendee
            .entry(rsvp.attendee_hex.clone())
            .or_insert_with(|| rsvp.clone());

        if rsvp.is_newer_than(entry) {
            *entry = rsvp.clone();
        }
    }

    let mut confirmed: Vec<CalendarRsvp> = best_by_attendee
        .into_values()
        .filter(|rsvp| rsvp.is_confirmed())
        .collect();

    confirmed.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.attendee_hex.cmp(&b.attendee_hex))
    });

    confirmed
}

pub fn event_nevent(event: &CalendarEvent) -> Option<String> {
    let event_id = EventId::from_hex(&event.id_hex).ok()?;
    let mut nip19 = Nip19Event::new(event_id, Vec::<String>::new());
    if let Ok(author) = PublicKey::from_hex(&event.author_hex) {
        nip19 = nip19.author(author);
    }
    let kind = u16::try_from(event.kind).ok()?;
    nip19 = nip19.kind(NostrKind::from(kind));
    nip19.to_bech32().ok()
}

pub fn event_naddr(event: &CalendarEvent) -> Option<String> {
    let identifier = event.identifier.as_ref()?;
    let author = PublicKey::from_hex(&event.author_hex).ok()?;
    let kind = u16::try_from(event.kind).ok()?;
    let mut coordinate = Nip01Coordinate::new(NostrKind::from(kind), author);
    coordinate.identifier = identifier.clone();
    coordinate.to_bech32().ok()
}

pub fn wrap_title(input: &str, max_chars_per_line: usize, max_lines: usize) -> String {
    if input.is_empty() || max_chars_per_line == 0 || max_lines == 0 {
        return String::new();
    }

    let total_limit = max_chars_per_line.saturating_mul(max_lines).max(1);
    let truncated = truncate_text(input, total_limit);
    let mut result = String::new();
    let mut line_len = 0usize;
    let mut line_count = 1usize;
    let mut chars = truncated.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\n' {
            if line_count >= max_lines {
                if !result.ends_with('…') {
                    result.push('…');
                }
                break;
            }
            result.push('\n');
            line_len = 0;
            line_count += 1;
            continue;
        }

        if line_len >= max_chars_per_line {
            if line_count >= max_lines {
                if !result.ends_with('…') {
                    result.push('…');
                }
                break;
            }
            result.push('\n');
            line_len = 0;
            line_count += 1;
        }

        result.push(ch);
        line_len += 1;

        if line_count == max_lines
            && line_len >= max_chars_per_line
            && chars.peek().is_some()
            && !result.ends_with('…')
        {
            result.pop();
            result.push('…');
            break;
        }
    }

    result
}

pub fn truncate_text(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let mut chars = input.chars();
    let mut truncated = String::with_capacity(max_chars + 1);
    for _ in 0..max_chars {
        match chars.next() {
            Some(ch) => truncated.push(ch),
            None => return truncated,
        }
    }

    if chars.next().is_some() {
        truncated.pop();
        truncated.push('…');
    }

    truncated
}

fn push_unique(items: &mut Vec<String>, value: &str) {
    if items
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(value))
    {
        return;
    }
    items.push(value.to_owned());
}

fn extract_image_urls(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|token| {
            let trimmed = token.trim_matches(|c: char| ",.;:!?()[]{}<>\"'".contains(c));
            if is_image_url(trimmed) {
                Some(trimmed.to_owned())
            } else {
                None
            }
        })
        .collect()
}

fn is_image_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return false;
    }
    let stripped = lower.split('?').next().unwrap_or(&lower);
    stripped.ends_with(".png")
        || stripped.ends_with(".jpg")
        || stripped.ends_with(".jpeg")
        || stripped.ends_with(".gif")
        || stripped.ends_with(".webp")
        || stripped.ends_with(".bmp")
        || stripped.ends_with(".svg")
}
