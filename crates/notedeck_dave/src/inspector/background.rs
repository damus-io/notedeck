//! The Background card's state: the session's background tasks, wake-ups
//! and crons as its backend last reported them, the display rows built from
//! them, the last lines of each task's output file, and where each task's
//! Stop stands.

use super::LabelMemo;
use crate::messages::BackgroundEntry;
use poll_promise::Promise;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

/// How many lines of a task's output the card shows.
pub const TAIL_LINES: usize = 5;

/// How long a shown tail is trusted before it is read again.
const TAIL_REFRESH: Duration = Duration::from_secs(3);

/// The most of a file's end read to find its last lines.
const TAIL_READ_BYTES: u64 = 8 * 1024;

/// How long a stopped task may stay in the report before its Stop is
/// offered again. The CLI drops a task it stopped within a moment; one still
/// there after this refused (it had already ended, or the CLI is too old to
/// know `stop_task`).
pub const STOP_RETRY_AFTER: Duration = Duration::from_secs(10);

/// The Background card's state. See the module docs.
#[derive(Default)]
pub struct BackgroundState {
    /// Newest first, from [`DaveApiResponse::BackgroundWork`](crate::messages::DaveApiResponse::BackgroundWork).
    entries: Vec<BackgroundEntry>,
    /// Bumped each time `entries` is replaced, so rows built from them know
    /// to rebuild.
    generation: u64,
    /// One display row per entry, keyed by the generation and the second
    /// they were built in, since ages and countdowns move with the clock.
    pub rows: LabelMemo<(u64, u64), Vec<BackgroundRow>>,
    /// Output tails of local tasks.
    pub tails: OutputTails,
    /// The status bar's "Waiting · 3" pill, keyed by the count.
    pub waiting_pill: LabelMemo<u32>,
    /// Where each task's Stop stands.
    pub stops: TaskStops,
    /// The Stop control's words, built once per locale.
    pub stop_labels: LabelMemo<(), StopLabels>,
}

impl std::fmt::Debug for BackgroundState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundState")
            .field("entries", &self.entries)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl BackgroundState {
    /// Replace the entries with a new report, and drop the tails and stops
    /// of tasks no longer in it.
    pub fn set(&mut self, entries: Vec<BackgroundEntry>) {
        self.entries = entries;
        self.generation += 1;
        let entries = &self.entries;
        self.tails
            .tails
            .retain(|path, _| entries.iter().any(|e| e.output_file() == Some(path)));
        self.stops.retain_tasks(entries);
    }

    /// Newest first.
    pub fn entries(&self) -> &[BackgroundEntry] {
        &self.entries
    }

    /// Bumped each time the entries are replaced.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The entries beside the row memo and tails, so the card can build
    /// rows from the one while borrowing the others.
    pub fn parts_mut(&mut self) -> BackgroundParts<'_> {
        BackgroundParts {
            entries: &self.entries,
            generation: self.generation,
            rows: &mut self.rows,
            tails: &mut self.tails,
            stops: &mut self.stops,
            stop_labels: &mut self.stop_labels,
        }
    }
}

/// [`BackgroundState`] split for drawing: see [`BackgroundState::parts_mut`].
pub struct BackgroundParts<'a> {
    pub entries: &'a [BackgroundEntry],
    pub generation: u64,
    pub rows: &'a mut LabelMemo<(u64, u64), Vec<BackgroundRow>>,
    pub tails: &'a mut OutputTails,
    pub stops: &'a mut TaskStops,
    pub stop_labels: &'a mut LabelMemo<(), StopLabels>,
}

/// Where one task's Stop stands, as the card draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopStage {
    /// A Stop button.
    Offered,
    /// "Stop? Yes No", after a click on Stop.
    Confirming,
    /// Sent, waiting for the CLI to report the task ended.
    Stopping,
}

/// The Stop control on each background task: which task is asking "Stop?",
/// and which were sent a stop the CLI hasn't reported yet.
///
/// Stopping is never optimistic. A stopped task stays in the card, marked
/// stopping, until the backend's next report drops it; one the CLI refused
/// is offered Stop again after [`STOP_RETRY_AFTER`].
#[derive(Debug, Default)]
pub struct TaskStops {
    /// The task whose confirmation is showing. One at a time: asking on
    /// another task withdraws it.
    confirming: Option<String>,
    /// Tasks a confirmed Stop was sent for, and when.
    stopping: HashMap<String, Instant>,
}

impl TaskStops {
    /// Where task `id`'s Stop stands at `now`.
    pub fn stage(&self, id: &str, now: Instant) -> StopStage {
        if let Some(sent) = self.stopping.get(id) {
            if now.duration_since(*sent) < STOP_RETRY_AFTER {
                return StopStage::Stopping;
            }
        }
        if self.confirming.as_deref() == Some(id) {
            StopStage::Confirming
        } else {
            StopStage::Offered
        }
    }

    /// Stop was clicked on task `id`: ask to confirm.
    pub fn ask(&mut self, id: &str) {
        self.confirming = Some(id.to_string());
    }

    /// The confirmation was answered No.
    pub fn cancel(&mut self) {
        self.confirming = None;
    }

    /// The confirmation on task `id` was answered Yes, at `now`: it is
    /// stopping until the CLI reports it gone.
    pub fn confirm(&mut self, id: &str, now: Instant) {
        self.confirming = None;
        self.stopping.insert(id.to_string(), now);
    }

    /// Whether any stop is waiting on the CLI, so the card repaints to
    /// offer Stop again once one has waited too long.
    pub fn any_stopping(&self) -> bool {
        !self.stopping.is_empty()
    }

    /// Forget tasks no longer in `entries`.
    fn retain_tasks(&mut self, entries: &[BackgroundEntry]) {
        let present = |id: &str| entries.iter().any(|e| e.id == id);
        if self.confirming.as_deref().is_some_and(|id| !present(id)) {
            self.confirming = None;
        }
        self.stopping.retain(|id, _| present(id));
    }
}

/// The Stop control's words.
#[derive(Debug, Default)]
pub struct StopLabels {
    /// The button: "Stop".
    pub stop: String,
    /// The confirmation's question: "Stop?".
    pub question: String,
    /// "Yes".
    pub yes: String,
    /// "No".
    pub no: String,
    /// Shown once sent: "stopping…".
    pub stopping: String,
}

/// One entry's display strings, built when the entries or the second change.
#[derive(Debug, Default)]
pub struct BackgroundRow {
    /// The kind badge: "bash", "monitor", "wake-up", "cron", …
    pub badge: String,
    /// The command, description, wake-up reason or cron expression.
    pub title: String,
    /// The title is a command or cron expression, drawn in mono.
    pub title_is_code: bool,
    /// The age ("22m"), countdown ("in 6m 12s") or schedule ("recurring").
    pub when: String,
    /// The meta line under it: ids, usage, due time, durability.
    pub meta: String,
}

/// The last lines of task output files, read off the UI thread and
/// refreshed every few seconds while the card is drawn.
#[derive(Default)]
pub struct OutputTails {
    tails: HashMap<String, Tail>,
}

/// One file's tail.
#[derive(Default)]
struct Tail {
    /// The last lines read, joined. Empty before the first read lands.
    text: String,
    /// A read in flight.
    reading: Option<Promise<String>>,
    /// When the last read started.
    read_at: Option<Instant>,
}

impl OutputTails {
    /// The last lines of `path` as last read, starting a fresh read off the
    /// UI thread if the last one is older than a few seconds. Empty until
    /// the first read lands.
    pub fn tail(&mut self, path: &str, now: Instant) -> &str {
        if !self.tails.contains_key(path) {
            self.tails.insert(path.to_string(), Tail::default());
        }
        let tail = self.tails.get_mut(path).expect("inserted above");

        if let Some(text) = tail.reading.as_mut().and_then(|p| p.ready_mut()) {
            tail.text = std::mem::take(text);
            tail.reading = None;
        }
        let stale = tail
            .read_at
            .is_none_or(|at| now.duration_since(at) >= TAIL_REFRESH);
        if tail.reading.is_none() && stale {
            let owned = path.to_string();
            tail.reading = Some(Promise::spawn_thread("dave-output-tail", move || {
                read_tail(Path::new(&owned), TAIL_LINES)
            }));
            tail.read_at = Some(now);
        }
        &tail.text
    }

    /// How long until a shown tail should be read again, so the card can
    /// ask for a repaint then rather than every frame.
    pub fn refresh_after() -> Duration {
        TAIL_REFRESH
    }
}

/// The last `lines` lines of the file at `path`, read from at most its last
/// few KB. Empty if it can't be read.
pub fn read_tail(path: &Path, lines: usize) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_READ_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    // A read from the middle of the file starts mid-line; drop the partial.
    let text = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    let all: Vec<&str> = text
        .lines()
        .map(|l| l.rsplit('\r').next().unwrap_or(l))
        .filter(|l| !l.trim().is_empty())
        .collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{BackgroundTask, BackgroundWork};

    fn task(id: &str, output_file: Option<&str>) -> BackgroundEntry {
        BackgroundEntry {
            id: id.into(),
            started_at: 0,
            work: BackgroundWork::Task(BackgroundTask {
                output_file: output_file.map(str::to_string),
                ..Default::default()
            }),
        }
    }

    /// The tail is the last lines, skipping blanks and keeping only what a
    /// carriage return last drew on a line (a progress bar).
    #[test]
    fn reads_the_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b1.output");
        std::fs::write(
            &path,
            "one\ntwo\n\nthree\n10%\r50%\r100%\nfour\nfive\nsix\n",
        )
        .unwrap();
        assert_eq!(read_tail(&path, 3), "four\nfive\nsix");
        assert_eq!(read_tail(&path, 5), "three\n100%\nfour\nfive\nsix");
        assert_eq!(read_tail(&dir.path().join("missing"), 5), "");
    }

    /// A file bigger than the read window starts at a whole line.
    #[test]
    fn a_long_file_tails_from_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.output");
        let body: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, body).unwrap();
        assert_eq!(read_tail(&path, 2), "line 4998\nline 4999");
    }

    /// A stop shows as stopping until the task leaves the report; one the
    /// CLI refused is offered again after [`STOP_RETRY_AFTER`].
    #[test]
    fn a_stop_waits_for_the_report_and_retries_after_a_while() {
        let mut state = BackgroundState::default();
        state.set(vec![task("a", None), task("b", None)]);
        let now = Instant::now();
        let stops = &mut state.stops;
        stops.ask("a");
        assert_eq!(stops.stage("a", now), StopStage::Confirming);
        stops.ask("b");
        assert_eq!(
            stops.stage("a", now),
            StopStage::Offered,
            "one question at a time"
        );
        stops.confirm("b", now);
        assert_eq!(stops.stage("b", now), StopStage::Stopping);
        assert_eq!(
            stops.stage("b", now + STOP_RETRY_AFTER),
            StopStage::Offered,
            "refused: offer it again"
        );

        state.set(vec![task("a", None)]);
        assert!(!state.stops.any_stopping(), "b left the report");
    }

    /// A new report drops the tails of tasks it no longer carries.
    #[test]
    fn set_drops_tails_of_finished_tasks() {
        let mut state = BackgroundState::default();
        state.set(vec![task("a", Some("/tmp/a")), task("b", Some("/tmp/b"))]);
        let now = Instant::now();
        state.tails.tail("/tmp/a", now);
        state.tails.tail("/tmp/b", now);
        state.set(vec![task("b", Some("/tmp/b"))]);
        assert_eq!(state.generation(), 2);
        assert!(!state.tails.tails.contains_key("/tmp/a"));
        assert!(state.tails.tails.contains_key("/tmp/b"));
    }
}
