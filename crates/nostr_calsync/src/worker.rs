//! A background sync loop.
//!
//! [`spawn`] runs a [`CalendarSource`] on its own thread — the source's I/O and
//! any blocking platform calls (EventKit's objc2 work, say) stay off the caller's
//! thread. On a fixed interval, and whenever [`SyncHandle::resync_now`] is poked,
//! it pulls a rolling `[now - past, now + future]` window from the source and
//! mirrors it into nostrdb via [`sync_events`]. Dropping the [`SyncHandle`] stops
//! the thread and joins it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::Utc;
use nostrdb::Ndb;

use crate::source::CalendarSource;
use crate::sync::{NoPublish, Publisher, sync_events};

/// Builds the [`CalendarSource`] on the worker thread. Platform sources (Apple
/// EventKit, say) wrap objc2 objects that aren't `Send`, so we can't construct
/// one on the caller's thread and move it over — instead we move this `Send`
/// factory across and let it build the source where it'll live and be used.
pub type SourceFactory = Box<dyn FnOnce() -> Box<dyn CalendarSource> + Send>;

/// The rolling window to mirror and how often to refresh it.
#[derive(Clone, Debug)]
pub struct SyncConfig {
    /// How far into the past to mirror events.
    pub past: chrono::Duration,
    /// How far into the future to mirror events.
    pub future: chrono::Duration,
    /// How long to wait between automatic refreshes.
    pub interval: Duration,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            past: chrono::Duration::days(30),
            future: chrono::Duration::days(365),
            interval: Duration::from_secs(300),
        }
    }
}

/// A handle to a running sync thread. Drop it to stop and join the thread.
pub struct SyncHandle {
    stop: Arc<AtomicBool>,
    wake: mpsc::Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl SyncHandle {
    /// Ask the worker to refresh now instead of waiting for the next interval.
    /// A no-op if the worker has already exited.
    pub fn resync_now(&self) {
        let _ = self.wake.send(());
    }
}

impl Drop for SyncHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the worker so it notices the stop flag without waiting out the
        // interval; ignore the error if it's already gone.
        let _ = self.wake.send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Spawn a background thread mirroring the source built by `make_source` into
/// `ndb`, signing notes with `secret`. The source is constructed on the worker
/// thread (see [`SourceFactory`]); the thread requests calendar access once up
/// front and exits quietly if it isn't granted. See the module docs for the
/// loop's behaviour.
///
/// `publisher` fans each ingested frame out (e.g. to a relay); pass [`NoPublish`]
/// when ingesting into the same nostrdb a local relay already serves.
pub fn spawn(
    ndb: Ndb,
    secret: [u8; 32],
    make_source: SourceFactory,
    publisher: Box<dyn Publisher + Send>,
    config: SyncConfig,
) -> SyncHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let (wake, woken) = mpsc::channel();
    let join = std::thread::Builder::new()
        .name("nostr-calsync".to_string())
        .spawn({
            let stop = Arc::clone(&stop);
            move || run(ndb, secret, make_source, publisher, config, &stop, &woken)
        })
        .expect("spawn nostr-calsync thread");

    SyncHandle {
        stop,
        wake,
        join: Some(join),
    }
}

/// Convenience over [`spawn`] for the common case: the platform's default source
/// and no relay fan-out. Returns `None` if there's no real source on this
/// platform, so callers don't spin up a thread that would only ever import zero
/// events.
pub fn spawn_default(ndb: Ndb, secret: [u8; 32], config: SyncConfig) -> Option<SyncHandle> {
    let make_source = default_source()?;
    Some(spawn(ndb, secret, make_source, Box::new(NoPublish), config))
}

/// A factory for the native calendar source on this platform, if any. macOS
/// returns Apple EventKit; everywhere else returns `None` (no native calendar to
/// mirror). The source is built later, on the worker thread.
#[cfg(target_os = "macos")]
pub fn default_source() -> Option<SourceFactory> {
    Some(Box::new(
        || Box::new(crate::eventkit::EventKitSource::new()),
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn default_source() -> Option<SourceFactory> {
    None
}

fn run(
    ndb: Ndb,
    secret: [u8; 32],
    make_source: SourceFactory,
    mut publisher: Box<dyn Publisher + Send>,
    config: SyncConfig,
    stop: &AtomicBool,
    woken: &mpsc::Receiver<()>,
) {
    let mut source = make_source();
    match source.request_access() {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!("calsync: calendar access not granted; not syncing");
            return;
        }
        Err(err) => {
            tracing::error!("calsync: requesting calendar access failed: {err}");
            return;
        }
    }

    while !stop.load(Ordering::Relaxed) {
        let now = Utc::now();
        match source.fetch(now - config.past, now + config.future) {
            Ok(events) => {
                let ingested = sync_events(&ndb, &secret, &events, publisher.as_mut());
                tracing::debug!("calsync: mirrored {ingested}/{} events", events.len());
            }
            Err(err) => tracing::error!("calsync: fetch failed: {err}"),
        }

        // Sleep until the next interval, an explicit resync, or a stop.
        match woken.recv_timeout(config.interval) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{ExternalEvent, SourceError};
    use chrono::{DateTime, TimeZone};
    use enostr::FullKeypair;
    use nostrdb::{Config, Filter, Transaction};
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    /// A source that yields a single fixed event and counts its fetches.
    struct FakeSource {
        fetches: Arc<AtomicUsize>,
    }

    impl CalendarSource for FakeSource {
        fn request_access(&mut self) -> Result<bool, SourceError> {
            Ok(true)
        }

        fn fetch(
            &mut self,
            _start: DateTime<Utc>,
            _end: DateTime<Utc>,
        ) -> Result<Vec<ExternalEvent>, SourceError> {
            self.fetches.fetch_add(1, Ordering::Relaxed);
            Ok(vec![ExternalEvent {
                source_id: "fake-1".to_string(),
                title: "From the worker".to_string(),
                start: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
                end: Utc.timestamp_opt(1_700_003_600, 0).unwrap(),
                all_day: false,
                notes: None,
                last_modified: Some(Utc.timestamp_opt(1_700_000_000, 0).unwrap()),
            }])
        }
    }

    fn resolved_count(ndb: &Ndb, kind: u32) -> usize {
        let txn = Transaction::new(ndb).unwrap();
        let filter = Filter::new().kinds([kind as u64]).build();
        enostr::query_replaceable(ndb, &txn, &[filter]).len()
    }

    #[test]
    fn worker_mirrors_then_stops() {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let secret = FullKeypair::generate().secret_key.secret_bytes();
        let fetches = Arc::new(AtomicUsize::new(0));

        // Long interval so the only automatic fetch we see is the initial one.
        let fetches_for_source = Arc::clone(&fetches);
        let handle = spawn(
            ndb.clone(),
            secret,
            Box::new(move || {
                Box::new(FakeSource {
                    fetches: fetches_for_source,
                })
            }),
            Box::new(NoPublish),
            SyncConfig {
                past: chrono::Duration::days(1),
                future: chrono::Duration::days(1),
                interval: Duration::from_secs(3600),
            },
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        while resolved_count(&ndb, crate::event::KIND_TIME_BASED) < 1 {
            assert!(Instant::now() < deadline, "worker never mirrored the event");
            std::thread::sleep(Duration::from_millis(20));
        }

        // resync_now triggers another fetch without waiting out the interval.
        handle.resync_now();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fetches.load(Ordering::Relaxed) < 2 {
            assert!(Instant::now() < deadline, "resync_now never re-fetched");
            std::thread::sleep(Duration::from_millis(20));
        }

        drop(handle); // stops and joins the thread
    }

    #[test]
    fn spawn_default_is_none_off_macos() {
        // On macOS there's a real source; elsewhere there isn't. Either way the
        // call shouldn't panic, and off-mac it declines to spawn.
        #[cfg(not(target_os = "macos"))]
        assert!(default_source().is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn default_source_exists_on_macos() {
        assert!(default_source().is_some());
    }
}
