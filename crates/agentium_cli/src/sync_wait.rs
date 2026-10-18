//! The CLI's bounded waits on the engine's sync, drawn as a progress line.
//!
//! [`Engine::wait_for_sync`] can take seconds against a big PNS corpus, and a
//! reachable-but-silent relay can hold it until the caller's deadline. Both
//! used to be a silent pause followed by output that may be stale. [`wait`]
//! runs the same bounded wait as a [`ProgressLine`] phase fed by the session's
//! [`BackfillStatus`] watch, then says on stderr when the wait expired or the
//! backfill gave up, so a reader (or an agent capturing stderr) knows the
//! output came from the cache.

use std::future::Future;
use std::io::{IsTerminal, Write};
use std::time::Duration;

use agentium_core::Engine;
use cli_term::progress::{ProgressLine, REDRAW_EVERY};
use nostrdb_net::relay::sync::BackfillStatus;
use tokio::sync::watch;

/// One bounded wait on the engine's sync: what to call it and what to say when
/// it ends badly.
pub(crate) struct SyncWait {
    /// The phase label, which also prefixes the end-of-wait messages.
    pub label: &'static str,
    /// How long to wait before giving up on the backfill.
    pub max: Duration,
    /// What the command does instead once `max` expires, e.g. "showing cached
    /// sessions"; follows "still running after {max}s, ".
    pub expired: &'static str,
    /// What to say when a backfill that started during this wait gave up.
    /// `None` leaves it unsaid, for a wait whose failure an earlier one reports.
    pub failed: Option<&'static str>,
}

/// How a [`SyncWait`] ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SyncOutcome {
    /// The backfills in flight settled. `failed` when one of them that started
    /// during the wait gave up rather than completing.
    Settled { fetched: u64, failed: bool },
    /// `max` expired with the backfill still running.
    Expired,
}

/// Wait for `engine`'s sync as described by `wait`, drawing progress on stderr
/// and reporting an expired or failed wait there. The line is gone by the time
/// this returns, so the caller can print or start the pager straight after.
pub(crate) async fn wait(engine: &Engine, wait: &SyncWait) -> SyncOutcome {
    let tty = std::io::stderr().is_terminal();
    let outcome = {
        let mut line = ProgressLine::new(tty);
        // An embedded engine has no session to watch: a receiver whose sender
        // is already gone never reports a change.
        let status = engine
            .backfill_status()
            .unwrap_or_else(|| watch::channel(BackfillStatus::default()).1);
        settle(&mut line, tty, wait, engine.wait_for_sync(), status).await
    };
    if let Some(message) = message(wait, &outcome) {
        eprintln!("{message}");
    }
    outcome
}

/// Run `settled` as a phase on `line`, bounded by `wait.max`, updating the
/// count from `status` as it changes. Counts are relative to `status` when the
/// wait starts, since the session's are cumulative over its life.
async fn settle<W: Write>(
    line: &mut ProgressLine<W>,
    tty: bool,
    wait: &SyncWait,
    settled: impl Future,
    mut status: watch::Receiver<BackfillStatus>,
) -> SyncOutcome {
    let base = status.borrow_and_update().clone();
    line.phase(wait.label);

    let deadline = tokio::time::sleep(wait.max);
    // The line only redraws when told to; tick it on a tty so the spinner
    // keeps turning through a stretch with no events (a silent relay).
    let mut tick = tokio::time::interval(REDRAW_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tokio::pin!(deadline, settled);

    loop {
        tokio::select! {
            _ = &mut settled => break,
            Ok(()) = status.changed() => {
                let now = status.borrow_and_update().clone();
                let fetched = now.fetched.saturating_sub(base.fetched);
                let need = now.need.saturating_sub(base.need);
                line.set(fetched, (need > 0).then_some(need.max(fetched)));
            }
            _ = tick.tick(), if tty => line.add(0),
            _ = &mut deadline => {
                line.clear();
                return SyncOutcome::Expired;
            }
        }
    }

    let end = status.borrow().clone();
    let fetched = end.fetched.saturating_sub(base.fetched);
    let failed = end.failed && !base.failed;
    if failed {
        line.clear();
    } else {
        line.finish(&format!("{fetched} fetched"));
    }
    SyncOutcome::Settled { fetched, failed }
}

/// What to say on stderr once `wait` ended as `outcome`: nothing for a sync
/// that settled cleanly (its progress line already summarised it).
fn message(wait: &SyncWait, outcome: &SyncOutcome) -> Option<String> {
    match outcome {
        SyncOutcome::Settled { failed: false, .. } => None,
        SyncOutcome::Settled { failed: true, .. } => wait
            .failed
            .map(|failed| format!("{}: {failed}", wait.label)),
        SyncOutcome::Expired => Some(format!(
            "{}: still running after {}s, {}",
            wait.label,
            wait.max.as_secs(),
            wait.expired
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAIT: SyncWait = SyncWait {
        label: "syncing sessions",
        max: Duration::from_secs(6),
        expired: "showing cached sessions",
        failed: Some("failed, showing cached sessions"),
    };

    /// A wait whose backfill never settles ends at the deadline, and says so.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_sync_expires_and_says_so() {
        let (_tx, rx) = watch::channel(BackfillStatus::default());
        let mut buf = Vec::new();
        let outcome = {
            let mut line = ProgressLine::with_writer(&mut buf, false);
            settle(&mut line, false, &WAIT, std::future::pending::<()>(), rx).await
        };
        assert_eq!(outcome, SyncOutcome::Expired);
        assert_eq!(
            message(&WAIT, &outcome).as_deref(),
            Some("syncing sessions: still running after 6s, showing cached sessions")
        );
        assert!(buf.is_empty(), "{:?}", String::from_utf8_lossy(&buf));
    }

    /// Counts are taken relative to the status when the wait started, and a
    /// clean settle has nothing extra to say.
    #[tokio::test(start_paused = true)]
    async fn a_settled_sync_counts_from_the_start_of_the_wait() {
        let (tx, rx) = watch::channel(BackfillStatus {
            fetched: 100,
            need: 100,
            ..Default::default()
        });
        let settled = async {
            tx.send_modify(|s| {
                s.fetched += 40;
                s.need += 50;
            });
        };
        let mut line = ProgressLine::with_writer(Vec::new(), false);
        let outcome = settle(&mut line, false, &WAIT, settled, rx).await;
        assert_eq!(
            outcome,
            SyncOutcome::Settled {
                fetched: 40,
                failed: false
            }
        );
        assert_eq!(message(&WAIT, &outcome), None);
    }

    /// A backfill that gave up during the wait is reported; one that had
    /// already failed before it is an earlier wait's to report.
    #[tokio::test(start_paused = true)]
    async fn only_a_failure_during_the_wait_is_reported() {
        let (tx, rx) = watch::channel(BackfillStatus::default());
        let settled = async { tx.send_modify(|s| s.failed = true) };
        let mut line = ProgressLine::with_writer(Vec::new(), false);
        let outcome = settle(&mut line, false, &WAIT, settled, rx).await;
        assert_eq!(
            message(&WAIT, &outcome).as_deref(),
            Some("syncing sessions: failed, showing cached sessions")
        );

        let (_tx, rx) = watch::channel(BackfillStatus {
            failed: true,
            ..Default::default()
        });
        let outcome = settle(&mut line, false, &WAIT, async {}, rx).await;
        assert_eq!(
            outcome,
            SyncOutcome::Settled {
                fetched: 0,
                failed: false
            }
        );
    }

    /// A wait with no failure message (the publish flush) stays quiet when
    /// the backfill gave up.
    #[test]
    fn a_wait_without_a_failure_message_stays_quiet() {
        let flush = SyncWait {
            failed: None,
            ..WAIT
        };
        let outcome = SyncOutcome::Settled {
            fetched: 0,
            failed: true,
        };
        assert_eq!(message(&flush, &outcome), None);
    }
}
