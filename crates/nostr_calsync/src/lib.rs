//! Mirror external calendars into nostrdb as NIP-52 events.
//!
//! `nostr_calsync` is a small, platform-agnostic bridge: a [`CalendarSource`]
//! produces normalized [`ExternalEvent`]s, and [`sync_events`] writes them into a
//! local nostrdb as signed NIP-52 calendar notes (kinds `31922`/`31923`). Apps
//! that already read those notes — Horizon, for one — pick the events up with no
//! changes of their own.
//!
//! The sync is one-way (source → nostrdb) and idempotent: every imported note is
//! addressable on a `d` tag derived from the source id, so re-importing
//! supersedes rather than duplicates. macOS supplies a real Apple EventKit
//! source; other platforms use [`NullSource`].

mod event;
mod source;
mod sync;

pub use event::{
    KIND_DATE_BASED, KIND_TIME_BASED, SOURCE_TAG, build_calendar_event, created_at, d_tag,
};
pub use source::{CalendarSource, ExternalEvent, NullSource, SourceError};
pub use sync::{NoPublish, Publisher, sync_events};
