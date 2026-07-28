//! Apple EventKit calendar source (macOS only).
//!
//! Wraps `eventkit-rs`'s [`EventsManager`] and normalizes its [`EventItem`]s
//! into [`ExternalEvent`]s. Everything here is blocking objc2 work, so the sync
//! worker ([`crate::worker`]) drives it from a dedicated thread, never the render
//! thread. Reading calendars triggers the macOS TCC permission prompt, which
//! needs `NSCalendarsUsageDescription` in the app's Info.plist.

use chrono::{DateTime, Local, Utc};
use eventkit::{EventItem, EventsManager};

use crate::source::{CalendarSource, ExternalEvent, SourceError};

/// A [`CalendarSource`] backed by Apple EventKit.
pub struct EventKitSource {
    manager: EventsManager,
}

impl EventKitSource {
    pub fn new() -> Self {
        Self {
            manager: EventsManager::new(),
        }
    }
}

impl Default for EventKitSource {
    fn default() -> Self {
        Self::new()
    }
}

impl CalendarSource for EventKitSource {
    fn request_access(&mut self) -> Result<bool, SourceError> {
        self.manager
            .request_access()
            .map_err(|e| SourceError::Backend(e.to_string()))
    }

    fn fetch(
        &mut self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<ExternalEvent>, SourceError> {
        // EventKit speaks local time; convert our UTC window at the boundary.
        let items = self
            .manager
            .fetch_events(start.with_timezone(&Local), end.with_timezone(&Local), None)
            .map_err(|e| SourceError::Backend(e.to_string()))?;
        Ok(items.iter().map(to_external).collect())
    }
}

/// Normalize an EventKit [`EventItem`] into our platform-agnostic event. The
/// `identifier` becomes the sync key (and thus the NIP-52 `d` tag), so edits to
/// the same calendar event supersede rather than duplicate.
fn to_external(item: &EventItem) -> ExternalEvent {
    ExternalEvent {
        source_id: item.identifier.clone(),
        title: item.title.clone(),
        start: item.start_date.with_timezone(&Utc),
        end: item.end_date.with_timezone(&Utc),
        all_day: item.all_day,
        notes: item.notes.clone(),
        last_modified: item.last_modified_date.map(|d| d.with_timezone(&Utc)),
    }
}
