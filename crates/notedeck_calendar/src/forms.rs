//! Form state and note building functions for calendar write operations.
//!
//! This module provides:
//! - Form state structs for calendars and events
//! - Functions to build signed Nostr notes for publishing

use nostrdb::{Note, NoteBuilder};

use crate::rsvp::{FreeBusy, RsvpStatus};
use crate::{KIND_CALENDAR, KIND_DATE_CALENDAR_EVENT, KIND_RSVP, KIND_TIME_CALENDAR_EVENT};

/// State for creating/editing a calendar (kind 31924).
#[derive(Debug, Clone, Default)]
pub struct CalendarFormState {
    /// Calendar title (required).
    pub title: String,
    /// Calendar description.
    pub description: String,
    /// Unique identifier (d-tag). Auto-generated UUID for new calendars.
    pub d_tag: String,
    /// Whether this is an edit of an existing calendar.
    pub is_editing: bool,
}

impl CalendarFormState {
    /// Create a new empty form state with a generated d-tag.
    pub fn new() -> Self {
        Self {
            d_tag: uuid::Uuid::new_v4().to_string(),
            ..Default::default()
        }
    }

    /// Create form state for editing an existing calendar.
    pub fn for_edit(title: String, description: String, d_tag: String) -> Self {
        Self {
            title,
            description,
            d_tag,
            is_editing: true,
        }
    }

    /// Check if the form has valid data for submission.
    pub fn is_valid(&self) -> bool {
        !self.title.trim().is_empty() && !self.d_tag.trim().is_empty()
    }

    /// Clear the form state.
    pub fn clear(&mut self) {
        self.title.clear();
        self.description.clear();
        self.d_tag = uuid::Uuid::new_v4().to_string();
        self.is_editing = false;
    }
}

/// State for creating/editing a calendar event (kind 31922 or 31923).
#[derive(Debug, Clone, Default)]
pub struct EventFormState {
    /// Event title (required).
    pub title: String,
    /// Short summary.
    pub summary: String,
    /// Full description/content.
    pub content: String,
    /// Location address.
    pub location: String,
    /// Image URL for the event.
    pub image_url: String,
    /// Start date in "YYYY-MM-DD" format.
    pub start_date: String,
    /// Start time in "HH:MM" format (time-based events only).
    pub start_time: String,
    /// End date in "YYYY-MM-DD" format.
    pub end_date: String,
    /// End time in "HH:MM" format (time-based events only).
    pub end_time: String,
    /// Timezone identifier (e.g., "America/New_York").
    pub timezone: String,
    /// Whether this is an all-day event (kind 31922) vs timed (kind 31923).
    pub is_all_day: bool,
    /// Unique identifier (d-tag).
    pub d_tag: String,
    /// Whether this is an edit of an existing event.
    pub is_editing: bool,
    /// Comma-separated hashtags.
    pub hashtags: String,
}

impl EventFormState {
    /// Create a new empty form state with a generated d-tag.
    pub fn new() -> Self {
        Self {
            d_tag: uuid::Uuid::new_v4().to_string(),
            is_all_day: true, // Default to all-day events
            ..Default::default()
        }
    }

    /// Create a new form state with a pre-filled date.
    pub fn with_date(date: chrono::NaiveDate) -> Self {
        Self {
            start_date: date.format("%Y-%m-%d").to_string(),
            end_date: date.format("%Y-%m-%d").to_string(),
            d_tag: uuid::Uuid::new_v4().to_string(),
            is_all_day: true,
            ..Default::default()
        }
    }

    /// Check if the form has valid data for submission.
    pub fn is_valid(&self) -> bool {
        !self.title.trim().is_empty()
            && !self.d_tag.trim().is_empty()
            && !self.start_date.trim().is_empty()
    }

    /// Clear the form state.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Get the event kind based on whether it's all-day or timed.
    pub fn kind(&self) -> u32 {
        if self.is_all_day {
            KIND_DATE_CALENDAR_EVENT
        } else {
            KIND_TIME_CALENDAR_EVENT
        }
    }
}

/// Build a calendar note (kind 31924).
///
/// # Arguments
/// * `state` - The form state with calendar data
/// * `seckey` - The 32-byte secret key for signing
///
/// # Returns
/// A signed Note ready for publishing, or an error message.
pub fn build_calendar_note(
    state: &CalendarFormState,
    seckey: &[u8; 32],
) -> Result<Note<'static>, String> {
    if !state.is_valid() {
        return Err("Calendar title and d-tag are required".to_string());
    }

    let mut builder = NoteBuilder::new()
        .kind(KIND_CALENDAR)
        .content(&state.description);

    // Required: d-tag (unique identifier)
    builder = builder.start_tag().tag_str("d").tag_str(&state.d_tag);

    // Required: title
    builder = builder.start_tag().tag_str("title").tag_str(&state.title);

    builder
        .sign(seckey)
        .build()
        .ok_or_else(|| "Failed to build calendar note".to_string())
}

/// Build a calendar event note (kind 31922 for date-based, 31923 for time-based).
///
/// # Arguments
/// * `state` - The form state with event data
/// * `seckey` - The 32-byte secret key for signing
///
/// # Returns
/// A signed Note ready for publishing, or an error message.
pub fn build_event_note(
    state: &EventFormState,
    seckey: &[u8; 32],
) -> Result<Note<'static>, String> {
    if !state.is_valid() {
        return Err("Event title, d-tag, and start date are required".to_string());
    }

    let kind = state.kind();
    let mut builder = NoteBuilder::new().kind(kind).content(&state.content);

    // Required: d-tag
    builder = builder.start_tag().tag_str("d").tag_str(&state.d_tag);

    // Required: title
    builder = builder.start_tag().tag_str("title").tag_str(&state.title);

    // Required: start
    if state.is_all_day {
        // Date-based: start is "YYYY-MM-DD"
        builder = builder
            .start_tag()
            .tag_str("start")
            .tag_str(&state.start_date);
    } else {
        // Time-based: start is Unix timestamp
        let timestamp =
            parse_datetime_to_timestamp(&state.start_date, &state.start_time, &state.timezone)?;
        builder = builder
            .start_tag()
            .tag_str("start")
            .tag_str(&timestamp.to_string());
        if !state.timezone.is_empty() {
            builder = builder
                .start_tag()
                .tag_str("start_tzid")
                .tag_str(&state.timezone);
        }
    }

    // Optional: end
    if !state.end_date.is_empty() {
        if state.is_all_day {
            builder = builder.start_tag().tag_str("end").tag_str(&state.end_date);
        } else if !state.end_time.is_empty() {
            let timestamp =
                parse_datetime_to_timestamp(&state.end_date, &state.end_time, &state.timezone)?;
            builder = builder
                .start_tag()
                .tag_str("end")
                .tag_str(&timestamp.to_string());
            if !state.timezone.is_empty() {
                builder = builder
                    .start_tag()
                    .tag_str("end_tzid")
                    .tag_str(&state.timezone);
            }
        }
    }

    // Optional: summary
    if !state.summary.is_empty() {
        builder = builder
            .start_tag()
            .tag_str("summary")
            .tag_str(&state.summary);
    }

    // Optional: location
    if !state.location.is_empty() {
        builder = builder
            .start_tag()
            .tag_str("location")
            .tag_str(&state.location);
    }

    // Optional: image
    if !state.image_url.is_empty() {
        builder = builder
            .start_tag()
            .tag_str("image")
            .tag_str(&state.image_url);
    }

    // Optional: hashtags (t-tags)
    for tag in state
        .hashtags
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let tag = tag.trim_start_matches('#').to_lowercase();
        builder = builder.start_tag().tag_str("t").tag_str(&tag);
    }

    builder
        .sign(seckey)
        .build()
        .ok_or_else(|| "Failed to build event note".to_string())
}

/// Build an RSVP note (kind 31925).
///
/// # Arguments
/// * `event_coordinates` - The NIP-33 coordinates of the event (e.g., "31923:pubkey:d-tag")
/// * `status` - The RSVP status (Accepted, Declined, Tentative)
/// * `free_busy` - Optional free/busy indicator
/// * `seckey` - The 32-byte secret key for signing
///
/// # Returns
/// A signed Note ready for publishing, or an error message.
pub fn build_rsvp_note(
    event_coordinates: &str,
    status: RsvpStatus,
    free_busy: Option<FreeBusy>,
    seckey: &[u8; 32],
) -> Result<Note<'static>, String> {
    if event_coordinates.is_empty() {
        return Err("Event coordinates are required".to_string());
    }

    // Use event coordinates as d-tag to ensure one RSVP per event per user
    let d_tag = event_coordinates;

    let mut builder = NoteBuilder::new().kind(KIND_RSVP).content(""); // Content can be optional comment

    // Required: d-tag (unique per event for this user)
    builder = builder.start_tag().tag_str("d").tag_str(d_tag);

    // Required: a-tag (reference to the event)
    builder = builder.start_tag().tag_str("a").tag_str(event_coordinates);

    // Required: status
    builder = builder
        .start_tag()
        .tag_str("status")
        .tag_str(status.as_str());

    // Optional: free/busy
    if let Some(fb) = free_busy {
        builder = builder.start_tag().tag_str("fb").tag_str(fb.as_str());
    }

    builder
        .sign(seckey)
        .build()
        .ok_or_else(|| "Failed to build RSVP note".to_string())
}

/// Parse a date and time string into a Unix timestamp.
fn parse_datetime_to_timestamp(date: &str, time: &str, timezone: &str) -> Result<u64, String> {
    use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
    use chrono_tz::Tz;

    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|e| format!("Invalid date format: {}", e))?;

    let time = if time.is_empty() {
        NaiveTime::from_hms_opt(0, 0, 0).unwrap()
    } else {
        NaiveTime::parse_from_str(time, "%H:%M")
            .map_err(|e| format!("Invalid time format: {}", e))?
    };

    let naive_dt = NaiveDateTime::new(date, time);

    let timestamp = if timezone.is_empty() {
        // Use local timezone
        chrono::Local
            .from_local_datetime(&naive_dt)
            .single()
            .ok_or_else(|| "Ambiguous local time".to_string())?
            .timestamp() as u64
    } else {
        // Parse timezone
        let tz: Tz = timezone
            .parse()
            .map_err(|_| format!("Invalid timezone: {}", timezone))?;
        tz.from_local_datetime(&naive_dt)
            .single()
            .ok_or_else(|| "Ambiguous time in timezone".to_string())?
            .timestamp() as u64
    };

    Ok(timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calendar_form_validation() {
        let mut form = CalendarFormState::new();
        assert!(!form.is_valid()); // Empty title

        form.title = "My Calendar".to_string();
        assert!(form.is_valid());
    }

    #[test]
    fn test_event_form_validation() {
        let mut form = EventFormState::new();
        assert!(!form.is_valid()); // Empty title and date

        form.title = "My Event".to_string();
        assert!(!form.is_valid()); // Still missing date

        form.start_date = "2026-01-26".to_string();
        assert!(form.is_valid());
    }

    #[test]
    fn test_event_form_kind() {
        let mut form = EventFormState::new();
        form.is_all_day = true;
        assert_eq!(form.kind(), KIND_DATE_CALENDAR_EVENT);

        form.is_all_day = false;
        assert_eq!(form.kind(), KIND_TIME_CALENDAR_EVENT);
    }
}
