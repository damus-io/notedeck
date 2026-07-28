//! The sync engine: turn [`ExternalEvent`]s into signed NIP-52 notes ingested
//! into a local nostrdb.
//!
//! This is one-way for now — external source into nostrdb. Because each note is
//! addressable on a `d` tag derived from the source id (see [`crate::event`]),
//! re-running the sync is idempotent: unchanged events ingest as byte-identical
//! notes (no-ops), and changed events supersede their predecessor by
//! `created_at`. Downstream readers (e.g. Horizon) just see the notes appear.

use enostr::NoteId;
use nostrdb::{IngestMetadata, Ndb};

use crate::event::build_calendar_event;
use crate::source::ExternalEvent;

/// A sink for events that have been ingested locally and should also be fanned
/// out — typically published to a relay. Mirrors the seam headway uses so the
/// egui app (which ingests into the same nostrdb its embedded relay serves) can
/// pass [`NoPublish`] while other callers forward each frame to a relay.
pub trait Publisher {
    /// Called once per ingested event with its `["EVENT", {...}]` JSON frame.
    fn publish(&mut self, event_frame: &str);
}

/// A [`Publisher`] that drops everything: local ingest only, no fan-out.
pub struct NoPublish;

impl Publisher for NoPublish {
    fn publish(&mut self, _event_frame: &str) {}
}

/// Mirror `events` into `ndb` as signed NIP-52 notes, handing each ingested
/// frame to `publisher`. Returns the number of notes successfully ingested.
///
/// Signing happens here, so `secret` is the account the imported events are
/// attributed to. Re-importing is safe and cheap; see the module docs.
pub fn sync_events(
    ndb: &Ndb,
    secret: &[u8; 32],
    events: &[ExternalEvent],
    publisher: &mut dyn Publisher,
) -> usize {
    let mut ingested = 0;
    for ev in events {
        if ingest(ndb, ev, secret, publisher).is_some() {
            ingested += 1;
        }
    }
    ingested
}

/// Build, sign and ingest a single event's NIP-52 note, then publish its frame.
/// Returns the note id, or `None` if building/ingesting failed.
fn ingest(
    ndb: &Ndb,
    ev: &ExternalEvent,
    secret: &[u8; 32],
    publisher: &mut dyn Publisher,
) -> Option<NoteId> {
    let note = build_calendar_event(ev).sign(secret).build()?;
    let id = NoteId::new(*note.id());
    let json = enostr::ClientMessage::event(&note).ok()?.to_json().ok()?;
    if let Err(err) = ndb.process_event_with(&json, IngestMetadata::new().client(true)) {
        tracing::error!("calsync: failed to ingest {}: {err}", ev.source_id);
        return None;
    }
    publisher.publish(&json);
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{KIND_DATE_BASED, KIND_TIME_BASED, d_tag};
    use chrono::{TimeZone, Utc};
    use enostr::FullKeypair;
    use nostrdb::{Config, Filter, Ndb, Transaction};
    use std::time::{Duration, Instant};

    struct TestNdb {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
    }

    impl TestNdb {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            Self {
                ndb,
                _dir: dir,
                kp: FullKeypair::generate(),
            }
        }

        fn secret(&self) -> [u8; 32] {
            self.kp.secret_key.secret_bytes()
        }

        fn sync(&self, events: &[ExternalEvent]) -> usize {
            sync_events(&self.ndb, &self.secret(), events, &mut NoPublish)
        }

        /// Poll until the *resolved* calendar notes of `kind` satisfy `pred`,
        /// returning them. Ingest is async, so we retry up to a deadline.
        ///
        /// "Resolved" means deduped by `d` tag keeping the highest `created_at` —
        /// nostrdb keeps every revision of an addressable event, so a reader
        /// resolves the effective state itself (see `query_replaceable` in
        /// notedeck_dave). This mirrors what Horizon must do to show edits.
        fn wait_notes<F>(&self, kind: u32, pred: F) -> Vec<MirroredNote>
        where
            F: Fn(&[MirroredNote]) -> bool,
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let notes = self.resolved_notes(kind);
                if pred(&notes) {
                    return notes;
                }
                assert!(Instant::now() < deadline, "calendar notes never satisfied");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        fn resolved_notes(&self, kind: u32) -> Vec<MirroredNote> {
            let txn = Transaction::new(&self.ndb).unwrap();
            let filter = Filter::new().kinds([kind as u64]).build();
            // The shared resolver hands back one note per `d` tag (latest wins) —
            // exactly what a reader like Horizon will use.
            enostr::query_replaceable(&self.ndb, &txn, &[filter])
                .into_iter()
                .filter_map(|key| self.ndb.get_note_by_key(&txn, key).ok())
                .map(|note| MirroredNote {
                    d: tag(&note, "d"),
                    title: tag(&note, "title"),
                    start: tag(&note, "start"),
                    end: tag(&note, "end"),
                    source: tag(&note, "source"),
                })
                .collect()
        }
    }

    #[derive(Debug)]
    struct MirroredNote {
        d: Option<String>,
        title: Option<String>,
        start: Option<String>,
        end: Option<String>,
        source: Option<String>,
    }

    fn tag(note: &nostrdb::Note, name: &str) -> Option<String> {
        note.tags().iter().find_map(|t| {
            (t.count() >= 2 && t.get_str(0) == Some(name))
                .then(|| t.get_str(1).map(|s| s.to_string()))?
        })
    }

    fn event(source_id: &str, title: &str, start: i64, end: i64, modified: i64) -> ExternalEvent {
        ExternalEvent {
            source_id: source_id.to_string(),
            title: title.to_string(),
            start: Utc.timestamp_opt(start, 0).unwrap(),
            end: Utc.timestamp_opt(end, 0).unwrap(),
            all_day: false,
            notes: None,
            last_modified: Some(Utc.timestamp_opt(modified, 0).unwrap()),
        }
    }

    #[test]
    fn mirrors_time_based_event() {
        let t = TestNdb::new();
        let ev = event(
            "ABC-123",
            "Standup",
            1_700_000_000,
            1_700_003_600,
            1_700_000_000,
        );
        assert_eq!(t.sync(std::slice::from_ref(&ev)), 1);

        let notes = t.wait_notes(KIND_TIME_BASED, |n| n.len() == 1);
        let note = &notes[0];
        assert_eq!(note.d.as_deref(), Some(d_tag("ABC-123").as_str()));
        assert_eq!(note.title.as_deref(), Some("Standup"));
        assert_eq!(note.start.as_deref(), Some("1700000000"));
        assert_eq!(note.end.as_deref(), Some("1700003600"));
        assert_eq!(note.source.as_deref(), Some("eventkit"));
    }

    #[test]
    fn mirrors_all_day_event_as_date_based() {
        let t = TestNdb::new();
        // 2023-11-15 .. 2023-11-16 (exclusive) at UTC midnight.
        let mut ev = event(
            "DAY-1",
            "Holiday",
            1_700_006_400,
            1_700_092_800,
            1_700_006_400,
        );
        ev.all_day = true;
        assert_eq!(t.sync(std::slice::from_ref(&ev)), 1);

        let notes = t.wait_notes(KIND_DATE_BASED, |n| n.len() == 1);
        let note = &notes[0];
        assert_eq!(note.d.as_deref(), Some(d_tag("DAY-1").as_str()));
        // Date-based start/end are YYYY-MM-DD, not unix seconds.
        assert!(
            note.start.as_deref().is_some_and(|s| s.contains('-')),
            "all-day start should be a date: {:?}",
            note.start
        );
    }

    #[test]
    fn reimport_is_idempotent_then_supersedes() {
        let t = TestNdb::new();
        let ev = event("X", "v1", 1_700_000_000, 1_700_003_600, 1_700_000_000);

        // Two identical imports collapse to one addressable note.
        t.sync(std::slice::from_ref(&ev));
        t.sync(std::slice::from_ref(&ev));
        let notes = t.wait_notes(KIND_TIME_BASED, |n| n.len() == 1);
        assert_eq!(notes[0].title.as_deref(), Some("v1"));

        // A changed event with a newer last_modified supersedes: still one
        // resolved note per d tag, now carrying the new title.
        let ev2 = event("X", "v2", 1_700_000_000, 1_700_003_600, 1_700_000_100);
        t.sync(std::slice::from_ref(&ev2));
        let notes = t.wait_notes(KIND_TIME_BASED, |n| {
            n.len() == 1 && n[0].title.as_deref() == Some("v2")
        });
        assert_eq!(notes.len(), 1, "supersede resolves to a single note");
    }

    #[test]
    fn null_source_yields_nothing() {
        use crate::source::{CalendarSource, NullSource};
        let mut src = NullSource;
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        assert_eq!(src.fetch(now, now).unwrap().len(), 0);
        assert_eq!(src.request_access().unwrap(), false);
    }
}
