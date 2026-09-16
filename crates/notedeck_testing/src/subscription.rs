//! Bounded waits on a nostrdb subscription, for tests whose writes commit
//! asynchronously.
//!
//! nostrdb ingests on its own writer/ingester threads, so a test that writes an
//! event and immediately queries for it races the ingest. The event-driven way to
//! close that gap is to subscribe *before* writing and await the subscription, so
//! the test advances on the writer's own notification instead of a wall-clock
//! sleep. [`await_notes`] does that — with a deadline, so a note that never
//! arrives fails the test instead of parking the thread forever.

use std::time::Duration;

use futures_util::StreamExt;
use nostrdb::SubscriptionStream;

/// Backstop for [`await_notes`]. A local ingest commits in microseconds, so this
/// is six orders of magnitude of headroom: it only ever trips when the note is
/// genuinely never coming (a filter that can't match what the writer produced, a
/// write that silently failed), never on a slow machine.
const INGEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Await `n` ingested notes on `stream`, summing each delivered batch.
///
/// Subscribe before the write, then call this after it: every note ingested from
/// that point on wakes the stream. `n` is the exact number of events the write
/// produces, which makes this a quiescence signal — unlike a state predicate it
/// can't be satisfied while trailing events are still in flight, and it doesn't
/// depend on which event the writer happens to emit last.
///
/// Drives the stream on its own current-thread tokio runtime, so call it from a
/// plain `#[test]`, not from inside an async context.
///
/// # Panics
///
/// If the subscription closes early, or if fewer than `n` notes arrive within
/// [`INGEST_TIMEOUT`] — reporting how many did arrive, because the usual cause is
/// a subscription filter that doesn't match what the writer actually produced.
pub fn await_notes(stream: &mut SubscriptionStream, n: usize) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("current-thread runtime");

    let mut seen = 0usize;
    runtime.block_on(async {
        let wait = async {
            while seen < n {
                let batch = stream
                    .next()
                    .await
                    .expect("subscription closed while awaiting ingested notes");
                seen += batch.len();
            }
        };

        assert!(
            tokio::time::timeout(INGEST_TIMEOUT, wait).await.is_ok(),
            "timed out after {INGEST_TIMEOUT:?} awaiting {n} ingested note(s); \
             saw {seen} — does the subscription filter match what the write produced?"
        );
    });
}
