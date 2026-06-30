//! The platform-agnostic calendar source.
//!
//! A [`CalendarSource`] is anything that can hand us a batch of
//! [`ExternalEvent`]s for a time range — Apple's EventKit on macOS, a CalDAV
//! server, an `.ics` file, etc. The sync engine ([`crate::sync`]) only ever
//! speaks in terms of this trait and [`ExternalEvent`], so nothing downstream is
//! tied to a particular platform. macOS gets a real EventKit implementation
//! (see `eventkit.rs`); every other target falls back to [`NullSource`].

use chrono::{DateTime, Utc};

/// A calendar event normalized out of some external source, ready to be mirrored
/// into nostrdb as a NIP-52 event.
///
/// Times are stored in UTC. For an [`all_day`](Self::all_day) event the instants
/// are the local-midnight boundaries of the covered date(s); the date itself is
/// what gets written (NIP-52 date-based events are `YYYY-MM-DD`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalEvent {
    /// Stable identifier from the source (e.g. an EventKit `identifier`). This is
    /// the sync key: it maps to the NIP-52 `d` tag so re-importing the same event
    /// supersedes the previous note rather than duplicating it.
    pub source_id: String,
    pub title: String,
    /// Start instant (inclusive).
    pub start: DateTime<Utc>,
    /// End instant. For all-day events this is exclusive, matching NIP-52.
    pub end: DateTime<Utc>,
    /// True for date-based / all-day events (NIP-52 kind 31922).
    pub all_day: bool,
    /// Freeform notes/description, mapped to the note content.
    pub notes: Option<String>,
    /// When the source last changed this event, used to order superseding
    /// imports (becomes the note's `created_at`). `None` falls back to the start.
    pub last_modified: Option<DateTime<Utc>>,
}

/// A source of calendar events for a date range.
///
/// Implementations are expected to be cheap to construct and to do their I/O
/// (and any blocking platform calls) inside [`fetch`](Self::fetch), which the
/// sync engine drives from a background thread.
pub trait CalendarSource {
    /// Request permission to read the user's calendars. Returns `Ok(true)` when
    /// access is granted. A source that needs no permission returns `Ok(true)`.
    fn request_access(&mut self) -> Result<bool, SourceError>;

    /// Fetch every event overlapping `[start, end)`.
    fn fetch(
        &mut self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<ExternalEvent>, SourceError>;
}

/// Why a [`CalendarSource`] couldn't produce events.
#[derive(Debug)]
pub enum SourceError {
    /// The user denied (or hasn't yet granted) calendar access.
    AccessDenied,
    /// The underlying platform/library failed; carries a human-readable reason.
    Backend(String),
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::AccessDenied => write!(f, "calendar access denied"),
            SourceError::Backend(msg) => write!(f, "calendar backend error: {msg}"),
        }
    }
}

impl std::error::Error for SourceError {}

/// A [`CalendarSource`] that produces nothing. Used on every platform without a
/// native calendar integration so callers can be written once and stay
/// non-`cfg`'d: the sync simply imports zero events.
#[derive(Default)]
pub struct NullSource;

impl CalendarSource for NullSource {
    fn request_access(&mut self) -> Result<bool, SourceError> {
        Ok(false)
    }

    fn fetch(
        &mut self,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
    ) -> Result<Vec<ExternalEvent>, SourceError> {
        Ok(Vec::new())
    }
}
