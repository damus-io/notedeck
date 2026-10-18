//! A live progress line on stderr for the CLIs' slow relay phases.
//!
//! `headway` and `agentium` can spend seconds in a negentropy reconcile before
//! printing anything. [`ProgressLine`] makes that wait legible without
//! changing what the program prints:
//!
//! - **stderr is a tty**: one line redrawn in place, at most every
//!   [`REDRAW_EVERY`], showing a spinner, the phase label and either a
//!   determinate bar (`[#####-----] 412/1000`) when the total is known or a
//!   bare count when it isn't. The line is erased on [`ProgressLine::finish`],
//!   [`ProgressLine::clear`] and drop, so nothing bleeds into stdout or the
//!   pager.
//! - **stderr is not a tty** (agents, tests, pipes): no `\r`, no escapes. A
//!   phase that ran longer than [`SLOW_PHASE`] leaves one summary line
//!   (`pulling key-shares: 412 fetched in 3.2s`); a fast phase leaves nothing,
//!   so captured stderr is unchanged in the common case.
//!
//! The line never writes words of its own beyond the label and summary the
//! caller passes, so callers control everything a test might match on.
//!
//! Std only: each CLI adapts its sync library's progress events onto this.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::{Stderr, Write};
use std::time::{Duration, Instant};

/// A non-tty phase that runs longer than this prints a summary line on
/// [`ProgressLine::finish`]; a shorter one prints nothing.
pub const SLOW_PHASE: Duration = Duration::from_secs(1);

/// The tty line redraws at most this often (phase changes and finishes always
/// draw), so a reconcile reporting thousands of events doesn't flood stderr.
pub const REDRAW_EVERY: Duration = Duration::from_millis(100);

/// Width of the determinate bar's `[####----]` body, in cells.
const BAR_WIDTH: u64 = 20;

/// Spinner frames, advanced once per tty draw.
const SPINNER: [char; 4] = ['|', '/', '-', '\\'];

/// Erase the current terminal line and return to its start.
const ERASE_LINE: &str = "\r\x1b[2K";

/// One progress line on a writer (stderr by default); see the module docs for
/// the tty and non-tty behaviour.
pub struct ProgressLine<W: Write = Stderr> {
    out: W,
    tty: bool,
    /// Visible cells the tty line may use (`COLUMNS`, else 80).
    width: usize,
    /// The open phase's label; `None` between phases.
    label: Option<Cow<'static, str>>,
    done: u64,
    total: Option<u64>,
    started: Instant,
    /// When the tty line last drew; `None` forces the next draw.
    last_draw: Option<Instant>,
    /// Whether the tty line currently shows something that needs erasing.
    drawn: bool,
    frame: usize,
    /// Reused buffer for the tty line, so a redraw doesn't allocate.
    line: String,
}

impl ProgressLine {
    /// A progress line on stderr. `tty` is the caller's
    /// `std::io::stderr().is_terminal()`.
    pub fn new(tty: bool) -> Self {
        Self::with_writer(std::io::stderr(), tty)
    }
}

impl<W: Write> ProgressLine<W> {
    /// A progress line on `out`, drawn as a tty line when `tty`.
    pub fn with_writer(out: W, tty: bool) -> Self {
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|c| c.trim().parse::<usize>().ok())
            .filter(|&w| w > 0)
            .unwrap_or(80);
        ProgressLine {
            out,
            tty,
            width,
            label: None,
            done: 0,
            total: None,
            started: Instant::now(),
            last_draw: None,
            drawn: false,
            frame: 0,
            line: String::new(),
        }
    }

    /// Start a phase named `label`, finishing any open one first (with its
    /// count as the summary). The count resets to zero with no total.
    pub fn phase(&mut self, label: impl Into<Cow<'static, str>>) {
        if self.label.is_some() {
            let summary = self.count_summary();
            self.finish(&summary);
        }
        self.label = Some(label.into());
        self.done = 0;
        self.total = None;
        self.started = Instant::now();
        self.last_draw = None;
        self.draw();
    }

    /// Set the phase's count to `done`, out of `total` when it is known.
    pub fn set(&mut self, done: u64, total: Option<u64>) {
        self.done = done;
        self.total = total;
        self.draw();
    }

    /// Add `n` to the phase's count.
    pub fn add(&mut self, n: u64) {
        self.done = self.done.saturating_add(n);
        self.draw();
    }

    /// Close the phase. On a tty this erases the line; otherwise a phase that
    /// ran longer than [`SLOW_PHASE`] writes `"{label}: {summary} in {secs}s"`.
    pub fn finish(&mut self, summary: &str) {
        let Some(label) = self.label.take() else {
            return;
        };
        if self.tty {
            self.clear();
            return;
        }
        let elapsed = self.started.elapsed();
        if elapsed <= SLOW_PHASE {
            return;
        }
        let _ = writeln!(
            self.out,
            "{label}: {summary} in {:.1}s",
            elapsed.as_secs_f64()
        );
        let _ = self.out.flush();
    }

    /// Erase the tty line now, e.g. right before printing to stdout or
    /// starting the pager. The phase stays open; its next update redraws.
    pub fn clear(&mut self) {
        if !self.drawn {
            return;
        }
        let _ = self.out.write_all(ERASE_LINE.as_bytes());
        let _ = self.out.flush();
        self.drawn = false;
        self.last_draw = None;
    }

    /// The phase's count as a summary, for a phase closed implicitly.
    fn count_summary(&self) -> String {
        match self.total {
            Some(total) => format!("{}/{total}", self.done),
            None => self.done.to_string(),
        }
    }

    /// Redraw the tty line, unless it drew less than [`REDRAW_EVERY`] ago.
    fn draw(&mut self) {
        if !self.tty {
            return;
        }
        let Some(label) = self.label.as_deref() else {
            return;
        };
        let now = Instant::now();
        if self
            .last_draw
            .is_some_and(|last| now.duration_since(last) < REDRAW_EVERY)
        {
            return;
        }
        self.last_draw = Some(now);

        let spinner = SPINNER[self.frame % SPINNER.len()];
        self.frame = self.frame.wrapping_add(1);
        self.line.clear();
        let _ = write!(self.line, "{spinner} {label} ");
        match self.total {
            Some(total) => {
                // A zero total has nothing left to do: draw it full.
                let filled = (self.done.min(total).saturating_mul(BAR_WIDTH))
                    .checked_div(total)
                    .unwrap_or(BAR_WIDTH);
                self.line.push('[');
                (0..BAR_WIDTH).for_each(|i| self.line.push(if i < filled { '#' } else { '-' }));
                let _ = write!(self.line, "] {}/{total}", self.done);
            }
            None => {
                let _ = write!(self.line, "{}", self.done);
            }
        }
        // Leave the last column free so the line never wraps.
        let max = self.width.saturating_sub(1);
        if let Some((cut, _)) = self.line.char_indices().nth(max) {
            self.line.truncate(cut);
        }

        let _ = self.out.write_all(ERASE_LINE.as_bytes());
        let _ = self.out.write_all(self.line.as_bytes());
        let _ = self.out.flush();
        self.drawn = true;
    }

    /// Pretend the open phase started (and the line last drew) `by` earlier.
    #[cfg(test)]
    fn backdate(&mut self, by: Duration) {
        self.started -= by;
        if let Some(last) = self.last_draw.as_mut() {
            *last -= by;
        }
    }
}

impl<W: Write> Drop for ProgressLine<W> {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(buf: &[u8]) -> &str {
        std::str::from_utf8(buf).unwrap()
    }

    #[test]
    fn not_tty_fast_phase_writes_nothing() {
        let mut buf = Vec::new();
        {
            let mut line = ProgressLine::with_writer(&mut buf, false);
            line.phase("pulling key-shares");
            line.set(412, Some(1000));
            line.add(3);
            line.finish("415 fetched");
        }
        assert!(buf.is_empty(), "{:?}", text(&buf));
    }

    #[test]
    fn not_tty_slow_phase_writes_one_plain_line() {
        let mut buf = Vec::new();
        {
            let mut line = ProgressLine::with_writer(&mut buf, false);
            line.phase("pulling key-shares");
            line.set(412, None);
            line.backdate(Duration::from_millis(3200));
            line.finish("412 fetched");
        }
        let out = text(&buf);
        assert!(
            out.starts_with("pulling key-shares: 412 fetched in 3."),
            "{out:?}"
        );
        assert!(out.ends_with("s\n"), "{out:?}");
        assert_eq!(out.matches('\n').count(), 1, "{out:?}");
        assert!(!out.contains('\x1b') && !out.contains('\r'), "{out:?}");
    }

    #[test]
    fn not_tty_new_phase_summarises_the_open_slow_one() {
        let mut buf = Vec::new();
        {
            let mut line = ProgressLine::with_writer(&mut buf, false);
            line.phase("pulling cards");
            line.set(7, Some(9));
            line.backdate(Duration::from_secs(2));
            line.phase("pulling comments");
        }
        let out = text(&buf);
        assert!(out.starts_with("pulling cards: 7/9 in 2."), "{out:?}");
        assert!(!out.contains("pulling comments"), "{out:?}");
    }

    #[test]
    fn tty_draws_a_bar_throttles_and_erases_on_finish() {
        let mut buf = Vec::new();
        {
            let mut line = ProgressLine::with_writer(&mut buf, true);
            line.phase("pulling key-shares");
            line.backdate(REDRAW_EVERY * 2);
            line.set(412, Some(1000));
            line.set(413, Some(1000));
            line.finish("413 fetched");
        }
        let out = text(&buf);
        assert!(
            out.contains("pulling key-shares [########------------] 412/1000"),
            "{out:?}"
        );
        assert!(
            !out.contains("413/1000"),
            "throttled redraw leaked: {out:?}"
        );
        assert!(out.ends_with(ERASE_LINE), "{out:?}");
        assert!(
            !out.contains("413 fetched"),
            "tty finish wrote a summary: {out:?}"
        );
    }

    #[test]
    fn tty_without_total_shows_a_count() {
        let mut buf = Vec::new();
        {
            let mut line = ProgressLine::with_writer(&mut buf, true);
            line.phase("backfill");
            line.backdate(REDRAW_EVERY * 2);
            line.add(41);
            line.clear();
        }
        let out = text(&buf);
        assert!(out.contains(" backfill 41"), "{out:?}");
        assert!(!out.contains("] "), "{out:?}");
    }

    #[test]
    fn drop_clears_the_tty_line() {
        let mut buf = Vec::new();
        {
            let mut line = ProgressLine::with_writer(&mut buf, true);
            line.phase("pulling cards");
        }
        let out = text(&buf);
        assert!(out.contains("pulling cards"), "{out:?}");
        assert!(out.ends_with(ERASE_LINE), "{out:?}");
    }

    #[test]
    fn tty_line_is_truncated_to_the_width() {
        let mut buf = Vec::new();
        {
            let mut line = ProgressLine::with_writer(&mut buf, true);
            line.width = 10;
            line.phase("a very long phase label");
        }
        let out = text(&buf);
        let drawn = out.split(ERASE_LINE).nth(1).unwrap();
        assert_eq!(drawn.chars().count(), 9, "{out:?}");
    }
}
