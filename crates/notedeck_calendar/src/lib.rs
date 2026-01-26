//! NIP-52 Calendar Events for Notedeck
//!
//! This crate provides data structures, parsing utilities, and a calendar application
//! for NIP-52 calendar events. It integrates with the Notedeck platform.
//!
//! # Overview
//!
//! NIP-52 defines four event kinds:
//! - **Date-based calendar events** (kind 31922): All-day or multi-day events
//! - **Time-based calendar events** (kind 31923): Events with specific start/end times
//! - **Calendars** (kind 31924): Collections of calendar events
//! - **RSVPs** (kind 31925): Attendance responses to calendar events
//!
//! # Example
//!
//! ```ignore
//! use notedeck_calendar::{CalendarEvent, Calendar, Rsvp, CalendarApp};
//! use nostrdb::{Ndb, Transaction};
//!
//! // Parse a calendar event from a nostrdb Note
//! let event = CalendarEvent::from_note(&note);
//!
//! // Create the calendar app
//! let app = CalendarApp::new();
//! ```

mod calendar;
pub mod comment;
mod event;
mod parse;
mod rsvp;
pub mod subscription;
pub mod timezone;

pub use calendar::{Calendar, EventRef};
pub use comment::{CachedComment, Comment, KIND_COMMENT};
pub use event::{CalendarEvent, CalendarTime, Participant};
pub use parse::{
    get_a_tag_value, get_all_a_tags, get_all_tag_values, get_e_tag, get_participants,
    get_tag_value, parse_a_tag, parse_participant_tag,
};
pub use rsvp::{FreeBusy, ParseFreeBusyError, ParseRsvpStatusError, Rsvp, RsvpStatus};

/// Kind number for date-based calendar events (NIP-52)
pub const KIND_DATE_CALENDAR_EVENT: u32 = 31922;

/// Kind number for time-based calendar events (NIP-52)
pub const KIND_TIME_CALENDAR_EVENT: u32 = 31923;

/// Kind number for calendars (NIP-52)
pub const KIND_CALENDAR: u32 = 31924;

/// Kind number for calendar event RSVPs (NIP-52)
pub const KIND_RSVP: u32 = 31925;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kind_constants() {
        assert_eq!(KIND_DATE_CALENDAR_EVENT, 31922);
        assert_eq!(KIND_TIME_CALENDAR_EVENT, 31923);
        assert_eq!(KIND_CALENDAR, 31924);
        assert_eq!(KIND_RSVP, 31925);
    }
}
