//! The sync's live progress line: adapts `nostrdb_net`'s [`SyncEvent`]s onto a
//! [`cli_term::progress::ProgressLine`], so each relay leg in `run()` shows what
//! it is doing instead of hanging silently.
//!
//! One [`SyncProgress`] lives across the whole sync block and is shared by
//! reference into every leg. Each leg opens its own phase; the adapter tallies
//! the events of the open phase and closes it with a worded summary
//! ("412 fetched", "3 pushed", "nothing new").

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::io::{IsTerminal, Stderr, Write};

use cli_term::progress::ProgressLine;
use nostrdb_net::relay::sync::{Progress, SyncEvent};

/// What the open phase has reported so far.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Tally {
    /// Ids the phase's reconciles said to fetch, summed across windows.
    need: u64,
    /// Events that landed.
    fetched: u64,
    /// Local events pushed to the relay.
    pushed: u64,
}

impl Tally {
    /// The phase's summary for the non-tty slow-phase line.
    fn summary(&self) -> String {
        match (self.fetched, self.pushed) {
            (0, 0) => "nothing new".to_owned(),
            (fetched, 0) => format!("{fetched} fetched"),
            (0, pushed) => format!("{pushed} pushed"),
            (fetched, pushed) => format!("{fetched} fetched, {pushed} pushed"),
        }
    }

    /// The line's total: the need count once a reconcile has reported a
    /// non-empty one, else none (a spinner and count rather than a bar that
    /// starts full at `0/0`).
    fn total(&self) -> Option<u64> {
        (self.need > 0).then_some(self.need)
    }
}

/// The sync's progress line plus the open phase's [`Tally`]; see the module
/// docs. Interior mutability, because the fork's [`Progress`] observer takes
/// `&self`, and headway's legs are `!Send` anyway (they hold nostrdb filters),
/// so a `RefCell` on the current task is fine.
pub(crate) struct SyncProgress<W: Write = Stderr> {
    line: RefCell<ProgressLine<W>>,
    tally: Cell<Tally>,
}

impl SyncProgress {
    /// A progress line on stderr, drawn live when stderr is a terminal.
    pub(crate) fn stderr() -> Self {
        Self::with_line(ProgressLine::new(std::io::stderr().is_terminal()))
    }
}

impl<W: Write> SyncProgress<W> {
    /// Wrap an existing [`ProgressLine`].
    fn with_line(line: ProgressLine<W>) -> Self {
        SyncProgress {
            line: RefCell::new(line),
            tally: Cell::new(Tally::default()),
        }
    }

    /// Close any open phase with its summary and start one named `label`.
    pub(crate) fn phase(&self, label: impl Into<Cow<'static, str>>) {
        self.finish();
        self.line.borrow_mut().phase(label);
    }

    /// Close the open phase with its tallied summary (see [`Tally::summary`]).
    /// A no-op when no phase is open.
    pub(crate) fn finish(&self) {
        let summary = self.tally.take().summary();
        self.line.borrow_mut().finish(&summary);
    }

    /// Close the open phase with `summary`, for a phase that reports no sync
    /// events (a local peel walk).
    pub(crate) fn finish_with(&self, summary: &str) {
        self.tally.take();
        self.line.borrow_mut().finish(summary);
    }

    /// Erase the tty line before writing anything else to stderr, so the line
    /// never splices into a message. The phase stays open.
    pub(crate) fn clear(&self) {
        self.line.borrow_mut().clear();
    }
}

impl<W: Write> Progress for SyncProgress<W> {
    fn event(&self, ev: SyncEvent) {
        let mut tally = self.tally.get();
        let mut line = self.line.borrow_mut();
        match ev {
            SyncEvent::Need { count } => tally.need += count as u64,
            SyncEvent::Fetched { count } => tally.fetched += count as u64,
            SyncEvent::Pushed { count } => {
                tally.pushed += count as u64;
                // The fork prints "flushed N local event(s)" right after this
                // event; erase the line first so the message lands on its own.
                line.clear();
            }
            // Each round trip ticks the spinner. A window's `created_at`
            // coverage isn't drawn: the bisection does the (empty) future half
            // first, so as a fraction it jumps to ~50% and then crawls, and
            // every window reports a `Need` that already gives the bar a total.
            SyncEvent::Round | SyncEvent::Window { .. } | SyncEvent::Split => {}
        }
        self.tally.set(tally);
        line.set(tally.fetched, tally.total());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_words() {
        let t = |fetched, pushed| {
            Tally {
                need: 0,
                fetched,
                pushed,
            }
            .summary()
        };
        assert_eq!(t(0, 0), "nothing new");
        assert_eq!(t(412, 0), "412 fetched");
        assert_eq!(t(0, 3), "3 pushed");
        assert_eq!(t(412, 3), "412 fetched, 3 pushed");
    }

    /// Needs accumulate across windows and fetches count against them; a phase
    /// resets the tally.
    #[test]
    fn events_tally_per_phase() {
        let progress = SyncProgress::with_line(ProgressLine::with_writer(Vec::new(), false));
        progress.phase("syncing boards");
        progress.event(SyncEvent::Need { count: 0 });
        assert_eq!(
            progress.tally.get().total(),
            None,
            "an empty need draws no bar"
        );
        progress.event(SyncEvent::Need { count: 5 });
        progress.event(SyncEvent::Need { count: 2 });
        progress.event(SyncEvent::Fetched { count: 4 });
        progress.event(SyncEvent::Pushed { count: 1 });
        assert_eq!(
            progress.tally.get(),
            Tally {
                need: 7,
                fetched: 4,
                pushed: 1
            }
        );
        progress.phase("pulling key-shares");
        assert_eq!(progress.tally.get(), Tally::default());
    }

    /// Off a tty, a fast phase writes nothing at all: no bar, no summary.
    #[test]
    fn not_tty_fast_phases_are_silent() {
        let mut buf = Vec::new();
        {
            let progress = SyncProgress::with_line(ProgressLine::with_writer(&mut buf, false));
            progress.phase("syncing boards");
            progress.event(SyncEvent::Round);
            progress.event(SyncEvent::Need { count: 3 });
            progress.event(SyncEvent::Fetched { count: 3 });
            progress.clear();
            progress.phase("peeling key-shares");
            progress.finish_with("done");
        }
        assert!(buf.is_empty(), "{:?}", String::from_utf8_lossy(&buf));
    }
}
