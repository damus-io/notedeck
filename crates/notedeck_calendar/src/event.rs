//! Calendar event data structures for NIP-52.
//!
//! This module defines the `CalendarEvent` struct which represents both
//! date-based (kind 31922) and time-based (kind 31923) calendar events.

use chrono::NaiveDate;
use nostrdb::Note;
use tracing::warn;

pub use crate::parse::Participant;
use crate::parse::{get_all_tag_values, get_participants, get_tag_value};
use crate::{KIND_DATE_CALENDAR_EVENT, KIND_TIME_CALENDAR_EVENT};

/// Represents a calendar event time, which can be either a date (for all-day events)
/// or a Unix timestamp with optional timezone (for time-based events).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarTime {
    /// A date without time, used for all-day or multi-day events (kind 31922).
    /// The date is in ISO 8601 format (YYYY-MM-DD).
    Date(NaiveDate),

    /// A Unix timestamp in seconds with optional IANA timezone identifier.
    /// Used for time-based events (kind 31923).
    Timestamp {
        /// Unix timestamp in seconds.
        timestamp: u64,
        /// Optional IANA timezone identifier (e.g., "America/New_York").
        timezone: Option<String>,
    },
}

impl CalendarTime {
    /// Creates a new date-based calendar time.
    pub fn date(date: NaiveDate) -> Self {
        Self::Date(date)
    }

    /// Creates a new timestamp-based calendar time.
    pub fn timestamp(ts: u64, tz: Option<String>) -> Self {
        Self::Timestamp {
            timestamp: ts,
            timezone: tz,
        }
    }

    /// Returns the Unix timestamp if this is a timestamp-based time.
    pub fn as_timestamp(&self) -> Option<u64> {
        match self {
            Self::Timestamp { timestamp, .. } => Some(*timestamp),
            Self::Date(_) => None,
        }
    }

    /// Returns the date if this is a date-based time.
    pub fn as_date(&self) -> Option<NaiveDate> {
        match self {
            Self::Date(date) => Some(*date),
            Self::Timestamp { .. } => None,
        }
    }

    /// Returns the timezone if this is a timestamp-based time with a timezone.
    pub fn timezone(&self) -> Option<&str> {
        match self {
            Self::Timestamp { timezone, .. } => timezone.as_deref(),
            Self::Date(_) => None,
        }
    }
}

/// A NIP-52 calendar event.
///
/// This struct represents both date-based (kind 31922) and time-based (kind 31923)
/// calendar events. The type is determined by the `kind` field and the `start` field.
#[derive(Debug, Clone)]
pub struct CalendarEvent {
    /// Unique identifier for this event (d-tag).
    pub d_tag: String,

    /// Title of the calendar event (required).
    pub title: String,

    /// Brief description/summary of the event.
    pub summary: Option<String>,

    /// Full description of the event (from content field).
    pub content: String,

    /// URL of an image for the event.
    pub image: Option<String>,

    /// Physical or virtual locations for the event.
    pub locations: Vec<String>,

    /// Geohash for searchable physical location.
    pub geohash: Option<String>,

    /// Participants in the event with their pubkeys, relay hints, and roles.
    pub participants: Vec<Participant>,

    /// Hashtags categorizing the event.
    pub hashtags: Vec<String>,

    /// References to web pages, documents, video calls, etc.
    pub references: Vec<String>,

    /// References to calendars this event is part of (a-tags to kind 31924).
    pub calendar_refs: Vec<String>,

    /// Start time of the event.
    pub start: CalendarTime,

    /// End time of the event (exclusive).
    pub end: Option<CalendarTime>,

    /// Timezone for the start time (only for time-based events).
    pub start_tzid: Option<String>,

    /// Timezone for the end time (only for time-based events).
    pub end_tzid: Option<String>,

    /// The event kind (31922 for date-based, 31923 for time-based).
    pub kind: u32,

    /// Public key of the event creator.
    pub pubkey: [u8; 32],

    /// Unix timestamp when the event was created.
    pub created_at: u64,
}

impl CalendarEvent {
    /// Parses a `CalendarEvent` from a nostrdb `Note`.
    ///
    /// Returns `None` if the note is not a valid calendar event (wrong kind
    /// or missing required fields).
    ///
    /// # Arguments
    ///
    /// * `note` - The nostrdb Note to parse
    ///
    /// # Returns
    ///
    /// A `CalendarEvent` if parsing succeeds, or `None` if the note is not
    /// a valid calendar event.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use notedeck_calendar::CalendarEvent;
    ///
    /// if let Some(event) = CalendarEvent::from_note(&note) {
    ///     println!("Event: {}", event.title);
    /// }
    /// ```
    pub fn from_note(note: &Note<'_>) -> Option<Self> {
        let kind = note.kind();

        // Verify this is a calendar event kind
        if kind != KIND_DATE_CALENDAR_EVENT && kind != KIND_TIME_CALENDAR_EVENT {
            return None;
        }

        // Required: d-tag
        let d_tag = get_tag_value(note, "d")?;

        // Required: title (with fallback to deprecated "name" tag)
        let title = get_tag_value(note, "title").or_else(|| get_tag_value(note, "name"))?;

        // Required: start time
        let start_str = get_tag_value(note, "start")?;
        let start_tzid = get_tag_value(note, "start_tzid").map(|s| s.to_string());
        let end_tzid = get_tag_value(note, "end_tzid").map(|s| s.to_string());

        let start = parse_calendar_time(start_str, kind, start_tzid.as_deref())?;

        // Optional: end time
        let end = get_tag_value(note, "end").and_then(|end_str| {
            // For end_tzid, use start_tzid as fallback if not specified
            let tz = end_tzid.as_deref().or(start_tzid.as_deref());
            parse_calendar_time(end_str, kind, tz)
        });

        // Optional fields
        let summary = get_tag_value(note, "summary").map(|s| s.to_string());
        let image = get_tag_value(note, "image").map(|s| s.to_string());
        let geohash = get_tag_value(note, "g").map(|s| s.to_string());

        // Collect repeated tags
        let locations: Vec<String> = get_all_tag_values(note, "location")
            .into_iter()
            .map(|s| s.to_string())
            .collect();

        let hashtags: Vec<String> = get_all_tag_values(note, "t")
            .into_iter()
            .map(|s| s.to_string())
            .collect();

        let references: Vec<String> = get_all_tag_values(note, "r")
            .into_iter()
            .map(|s| s.to_string())
            .collect();

        let calendar_refs: Vec<String> = get_all_tag_values(note, "a")
            .into_iter()
            .filter(|a| a.starts_with("31924:"))
            .map(|s| s.to_string())
            .collect();

        // Parse participants
        let participants = get_participants(note);

        Some(CalendarEvent {
            d_tag: d_tag.to_string(),
            title: title.to_string(),
            summary,
            content: note.content().to_string(),
            image,
            locations,
            geohash,
            participants,
            hashtags,
            references,
            calendar_refs,
            start,
            end,
            start_tzid,
            end_tzid,
            kind,
            pubkey: *note.pubkey(),
            created_at: note.created_at(),
        })
    }

    /// Returns true if this is a date-based (all-day) event.
    pub fn is_date_based(&self) -> bool {
        self.kind == KIND_DATE_CALENDAR_EVENT
    }

    /// Returns true if this is a time-based event.
    pub fn is_time_based(&self) -> bool {
        self.kind == KIND_TIME_CALENDAR_EVENT
    }

    /// Returns the NIP-33 addressable event coordinates for this event.
    ///
    /// Format: `<kind>:<pubkey_hex>:<d-tag>`
    pub fn coordinates(&self) -> String {
        format!("{}:{}:{}", self.kind, hex::encode(self.pubkey), self.d_tag)
    }
}

/// Parses a calendar time string based on the event kind.
fn parse_calendar_time(value: &str, kind: u32, timezone: Option<&str>) -> Option<CalendarTime> {
    if kind == KIND_DATE_CALENDAR_EVENT {
        // Parse as ISO 8601 date (YYYY-MM-DD)
        match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
            Ok(date) => Some(CalendarTime::Date(date)),
            Err(e) => {
                warn!("Failed to parse date '{}': {}", value, e);
                None
            }
        }
    } else {
        // Parse as Unix timestamp
        match value.parse::<u64>() {
            Ok(ts) => Some(CalendarTime::Timestamp {
                timestamp: ts,
                timezone: timezone.map(|s| s.to_string()),
            }),
            Err(e) => {
                warn!("Failed to parse timestamp '{}': {}", value, e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calendar_time_date() {
        let date = NaiveDate::from_ymd_opt(2024, 12, 25).unwrap();
        let ct = CalendarTime::date(date);

        assert_eq!(ct.as_date(), Some(date));
        assert_eq!(ct.as_timestamp(), None);
        assert_eq!(ct.timezone(), None);
    }

    #[test]
    fn test_calendar_time_timestamp() {
        let ct = CalendarTime::timestamp(1703500800, Some("America/New_York".to_string()));

        assert_eq!(ct.as_timestamp(), Some(1703500800));
        assert_eq!(ct.as_date(), None);
        assert_eq!(ct.timezone(), Some("America/New_York"));
    }

    #[test]
    fn test_calendar_time_timestamp_no_tz() {
        let ct = CalendarTime::timestamp(1703500800, None);

        assert_eq!(ct.as_timestamp(), Some(1703500800));
        assert_eq!(ct.timezone(), None);
    }

    #[test]
    fn test_parse_calendar_time_date() {
        let result = parse_calendar_time("2024-12-25", KIND_DATE_CALENDAR_EVENT, None);
        assert!(result.is_some());

        let ct = result.unwrap();
        let expected_date = NaiveDate::from_ymd_opt(2024, 12, 25).unwrap();
        assert_eq!(ct.as_date(), Some(expected_date));
    }

    #[test]
    fn test_parse_calendar_time_timestamp() {
        let result = parse_calendar_time("1703500800", KIND_TIME_CALENDAR_EVENT, Some("UTC"));
        assert!(result.is_some());

        let ct = result.unwrap();
        assert_eq!(ct.as_timestamp(), Some(1703500800));
        assert_eq!(ct.timezone(), Some("UTC"));
    }

    #[test]
    fn test_parse_calendar_time_invalid() {
        // Invalid date format
        assert!(parse_calendar_time("not-a-date", KIND_DATE_CALENDAR_EVENT, None).is_none());

        // Invalid timestamp
        assert!(parse_calendar_time("not-a-number", KIND_TIME_CALENDAR_EVENT, None).is_none());
    }
}
