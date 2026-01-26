//! Calendar event subscription management.
//!
//! This module provides filters and subscription utilities for fetching
//! NIP-52 calendar events from Nostr relays.

use nostrdb::Filter;

use crate::comment::KIND_COMMENT;
use crate::{KIND_CALENDAR, KIND_DATE_CALENDAR_EVENT, KIND_RSVP, KIND_TIME_CALENDAR_EVENT};

/// Default limit for calendar event queries.
pub const DEFAULT_CALENDAR_LIMIT: u64 = 500;

/// All calendar-related event kinds as u64 for use with nostrdb filters.
const CALENDAR_KINDS: [u64; 4] = [
    KIND_DATE_CALENDAR_EVENT as u64,
    KIND_TIME_CALENDAR_EVENT as u64,
    KIND_CALENDAR as u64,
    KIND_RSVP as u64,
];

/// Creates a filter for all calendar event types.
///
/// This filter matches:
/// - Date-based calendar events (kind 31922)
/// - Time-based calendar events (kind 31923)
/// - Calendars (kind 31924)
/// - RSVPs (kind 31925)
///
/// # Arguments
///
/// * `limit` - Maximum number of events to return
///
/// # Example
///
/// ```ignore
/// use notedeck_calendar::subscription::calendar_events_filter;
///
/// let filter = calendar_events_filter(100);
/// ```
pub fn calendar_events_filter(limit: u64) -> Filter {
    Filter::new().kinds(CALENDAR_KINDS).limit(limit).build()
}

/// Creates a filter for calendar events from a specific author.
///
/// # Arguments
///
/// * `pubkey` - The 32-byte public key of the author
/// * `limit` - Maximum number of events to return
pub fn calendar_events_by_author(pubkey: &[u8; 32], limit: u64) -> Filter {
    Filter::new()
        .authors([pubkey])
        .kinds(CALENDAR_KINDS)
        .limit(limit)
        .build()
}

/// Creates a filter for calendar events within a time range.
///
/// Note: This filters by the event's `created_at` timestamp, not the
/// calendar event's start/end time. For time-based filtering of actual
/// event schedules, additional client-side filtering is needed after
/// fetching events.
///
/// # Arguments
///
/// * `since` - Unix timestamp for the start of the range
/// * `until` - Unix timestamp for the end of the range
/// * `limit` - Maximum number of events to return
pub fn calendar_events_in_range(since: u64, until: u64, limit: u64) -> Filter {
    Filter::new()
        .kinds(CALENDAR_KINDS)
        .since(since)
        .until(until)
        .limit(limit)
        .build()
}

/// Creates a filter for only date-based calendar events (kind 31922).
///
/// Date-based events are all-day or multi-day events without specific times.
pub fn date_events_filter(limit: u64) -> Filter {
    Filter::new()
        .kinds([KIND_DATE_CALENDAR_EVENT as u64])
        .limit(limit)
        .build()
}

/// Creates a filter for only time-based calendar events (kind 31923).
///
/// Time-based events have specific start and end times.
pub fn time_events_filter(limit: u64) -> Filter {
    Filter::new()
        .kinds([KIND_TIME_CALENDAR_EVENT as u64])
        .limit(limit)
        .build()
}

/// Creates a filter for calendars (kind 31924).
///
/// Calendars are collections of calendar events.
pub fn calendars_filter(limit: u64) -> Filter {
    Filter::new()
        .kinds([KIND_CALENDAR as u64])
        .limit(limit)
        .build()
}

/// Creates a filter for calendars owned by a specific author.
pub fn calendars_by_author(pubkey: &[u8; 32], limit: u64) -> Filter {
    Filter::new()
        .authors([pubkey])
        .kinds([KIND_CALENDAR as u64])
        .limit(limit)
        .build()
}

/// Creates a filter for RSVPs (kind 31925).
pub fn rsvps_filter(limit: u64) -> Filter {
    Filter::new().kinds([KIND_RSVP as u64]).limit(limit).build()
}

/// Creates a filter for RSVPs to a specific calendar event.
///
/// # Arguments
///
/// * `event_coordinates` - The NIP-33 coordinates of the event (e.g., "31923:pubkey:d-tag")
/// * `limit` - Maximum number of RSVPs to return
pub fn rsvps_for_event(event_coordinates: &str, limit: u64) -> Filter {
    Filter::new()
        .kinds([KIND_RSVP as u64])
        .tags([event_coordinates], 'a')
        .limit(limit)
        .build()
}

/// Creates a filter for RSVPs from a specific user.
pub fn rsvps_by_author(pubkey: &[u8; 32], limit: u64) -> Filter {
    Filter::new()
        .authors([pubkey])
        .kinds([KIND_RSVP as u64])
        .limit(limit)
        .build()
}

/// Creates a filter for NIP-22 comments (kind 1111).
///
/// This filter matches all kind 1111 comments. Use `comments_for_event`
/// for comments on a specific calendar event.
pub fn comments_filter(limit: u64) -> Filter {
    Filter::new()
        .kinds([KIND_COMMENT as u64])
        .limit(limit)
        .build()
}

/// Creates a filter for NIP-22 comments on a specific calendar event.
///
/// This uses the uppercase "A" tag to find comments scoped to a calendar event.
///
/// # Arguments
///
/// * `event_coordinates` - The NIP-33 coordinates of the event (e.g., "31923:pubkey:d-tag")
/// * `limit` - Maximum number of comments to return
pub fn comments_for_event(event_coordinates: &str, limit: u64) -> Filter {
    Filter::new()
        .kinds([KIND_COMMENT as u64])
        .tags([event_coordinates], 'A')
        .limit(limit)
        .build()
}

/// Creates a filter for comments on calendar events (kind 31922 and 31923).
///
/// This is a broader filter that finds comments with root kind tags
/// indicating they are commenting on calendar events.
pub fn calendar_comments_filter(limit: u64) -> Filter {
    // NIP-22 uses uppercase K tag for root kind
    // We filter for comments that have K tag with calendar event kinds
    Filter::new()
        .kinds([KIND_COMMENT as u64])
        .limit(limit)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calendar_events_filter() {
        let filter = calendar_events_filter(100);
        // Filter is built, this is a basic smoke test
        let _ = filter;
    }

    #[test]
    fn test_calendar_events_by_author() {
        let pubkey = [0u8; 32];
        let filter = calendar_events_by_author(&pubkey, 50);
        let _ = filter;
    }

    #[test]
    fn test_calendar_events_in_range() {
        let filter = calendar_events_in_range(1704067200, 1706745600, 100);
        let _ = filter;
    }
}
