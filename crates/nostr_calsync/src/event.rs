//! Building NIP-52 calendar events from [`ExternalEvent`]s.
//!
//! NIP-52 defines two addressable calendar kinds, which Horizon already reads:
//! date-based / all-day (`31922`, dates as `YYYY-MM-DD`) and time-based
//! (`31923`, unix-second timestamps). We map each [`ExternalEvent`] onto one of
//! them, keyed by a `d` tag derived from the source id so a re-import supersedes
//! the old note instead of duplicating it.

use chrono::{Local, TimeZone};
use nostrdb::{NoteBuildOptions, NoteBuilder};

use crate::source::ExternalEvent;

/// NIP-52 date-based (all-day) calendar event.
pub const KIND_DATE_BASED: u32 = 31922;
/// NIP-52 time-based calendar event.
pub const KIND_TIME_BASED: u32 = 31923;

/// Tag value marking a note as mirrored from an external source. Stamped on
/// every imported note as `["source", SOURCE_TAG]` so a future two-way sync can
/// recognise its own imports and avoid echoing them back to the source.
pub const SOURCE_TAG: &str = "eventkit";

/// Build the NIP-52 `d`-tag id for an imported event. Namespacing by source
/// keeps these from colliding with `d` tags minted by other tools, and makes the
/// imported set easy to identify.
pub fn d_tag(source_id: &str) -> String {
    format!("{SOURCE_TAG}:{source_id}")
}

/// The `created_at` (unix seconds) to stamp on the note for `ev`: its
/// last-modified time when known, otherwise its start. Determines which import
/// wins when the same `d` tag is seen twice, so it must move forward as the
/// source event changes.
pub fn created_at(ev: &ExternalEvent) -> u64 {
    ev.last_modified.unwrap_or(ev.start).timestamp().max(0) as u64
}

/// Build the NIP-52 note for `ev`. The caller signs and ingests it (see
/// [`crate::sync`]); `created_at` is set here from [`created_at`] so superseding
/// works without the caller having to thread it through.
pub fn build_calendar_event(ev: &ExternalEvent) -> NoteBuilder<'static> {
    let kind = if ev.all_day {
        KIND_DATE_BASED
    } else {
        KIND_TIME_BASED
    };

    let content = ev.notes.clone().unwrap_or_default();
    let mut b = NoteBuilder::new()
        .content(&content)
        .kind(kind)
        .created_at(created_at(ev))
        .options(NoteBuildOptions::default())
        .start_tag()
        .tag_str("d")
        .tag_str(&d_tag(&ev.source_id))
        .start_tag()
        .tag_str("title")
        .tag_str(&ev.title);

    let (start, end) = if ev.all_day {
        (fmt_date(&ev.start), fmt_date(&ev.end))
    } else {
        (
            ev.start.timestamp().to_string(),
            ev.end.timestamp().to_string(),
        )
    };

    b = b
        .start_tag()
        .tag_str("start")
        .tag_str(&start)
        .start_tag()
        .tag_str("end")
        .tag_str(&end)
        .start_tag()
        .tag_str("source")
        .tag_str(SOURCE_TAG);

    b
}

/// Format an instant as the local-date `YYYY-MM-DD` NIP-52 wants for date-based
/// events. All-day boundaries come through as local midnight, so the local date
/// is the calendar date the user sees.
fn fmt_date(dt: &chrono::DateTime<chrono::Utc>) -> String {
    Local
        .from_utc_datetime(&dt.naive_utc())
        .date_naive()
        .format("%Y-%m-%d")
        .to_string()
}
