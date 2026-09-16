//! Bounded waits on a nostrdb subscription, for tests whose writes commit
//! asynchronously.
//!
//! nostrdb ingests on its own writer/ingester threads, so a test that writes an
//! event and immediately queries for it races the ingest. The event-driven way to
//! close that gap is to subscribe *before* writing and await the subscription, so
//! the test advances on the writer's own notification instead of a wall-clock
//! sleep.
//!
//! The wait itself lives in nostrdb ([`SubscriptionStream::wait_for_notes`]);
//! what this module adds is the test ergonomics — a panic carrying the diagnosis
//! instead of a `Result` to unwrap, and a sync entry point for a plain `#[test]`.

use std::time::Duration;

use nostrdb::{NoteKey, SubscriptionStream};

/// Backstop for the waits in this module. A local ingest commits in
/// microseconds, so this is six orders of magnitude of headroom: it only ever
/// trips when the note is genuinely never coming (a filter that can't match what
/// the writer produced, a write that silently failed), never on a slow machine.
const INGEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Await `n` ingested notes on `stream`, summing each delivered batch.
///
/// Subscribe before the write, then call this after it: every note ingested from
/// that point on wakes the stream. `n` is the exact number of events the write
/// produces, which makes this a quiescence signal — unlike a state predicate it
/// can't be satisfied while trailing events are still in flight, and it doesn't
/// depend on which event the writer happens to emit last.
///
/// For a plain `#[test]`; from `#[tokio::test]` use [`await_notes_async`].
///
/// # Panics
///
/// As [`await_notes_async`].
pub fn await_notes(stream: &mut SubscriptionStream, n: usize) {
    // Drives the wait on a throwaway current-thread runtime, so a sync test
    // needs no runtime of its own. Nesting this inside an async context would
    // panic, which is why async callers get `await_notes_async` instead.
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("current-thread runtime")
        .block_on(await_notes_async(stream, n));
}

/// [`await_notes`] for an async test, awaiting on the caller's runtime.
///
/// # Panics
///
/// If the subscription closes early, or if fewer than `n` notes arrive within
/// [`INGEST_TIMEOUT`]. nostrdb's error carries how many notes *did* arrive,
/// which is the useful part: 0 means the subscription filter doesn't match what
/// the write produced, fewer-than-`n` means `n` is wrong.
pub async fn await_notes_async(stream: &mut SubscriptionStream, n: usize) {
    if let Err(err) = stream.wait_for_notes(n, INGEST_TIMEOUT).await {
        panic!("awaiting {n} ingested note(s): {err}");
    }
}

/// Await the next batch of ingested notes on `stream`, returning their keys, for
/// the callers that care *which* notes landed rather than how many.
///
/// # Panics
///
/// If no batch arrives within [`INGEST_TIMEOUT`], or the subscription closes
/// first.
pub async fn await_batch(stream: &mut SubscriptionStream) -> Vec<NoteKey> {
    match stream.wait_for_notes(1, INGEST_TIMEOUT).await {
        Ok(notes) => notes,
        Err(err) => panic!("awaiting the next batch of ingested notes: {err}"),
    }
}
