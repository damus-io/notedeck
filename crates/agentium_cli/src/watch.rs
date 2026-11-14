//! `agentium watch` — a live dashboard of every session: one row per session,
//! sessions waiting on the user first, redrawn as the session corpus changes.
//!
//! The table is agentium-core's incremental session fold
//! ([`SessionReducer`]), the same one Dave's realtime cache drives: seeded once
//! from the cache, then advanced by the note keys an
//! [`Engine::watch_activity`] subscription delivers. That subscription spans
//! kind-31988 state revisions *and* kind-1988 messages, so a session that
//! streams output without republishing its status still reads as recently
//! active — and the "last active" column is the fold's memoized timestamp, never
//! a per-row ndb query.
//!
//! `--jsonl` swaps the dashboard for a stream: one JSON line per session whose
//! status changed, for a script to act on (e.g. notify when one is done).

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::time::Duration;

use agentium_core::Engine;
use agentium_core::cli::watch::{
    TICK, clip, render_frame, select, statuses, transitions, updated_line,
};
use agentium_core::session_events::AI_CONVERSATION_KIND;
use agentium_core::session_events::AI_SESSION_STATE_KIND;
use agentium_core::session_fold::{SessionReducer, fold_sessions, reduce_delta};
use cli_term::ColorWhen;
use nostrdb::{Filter, Ndb, NoteKey, Transaction};
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::list::{ListFilters, ListScope};
use crate::term::{ScreenGuard, now_secs, stdout_width};

/// How often `watch --jsonl` re-reads the cache for session-state revisions
/// other agentium runs wrote, which its subscription never hears.
const POLL: Duration = Duration::from_secs(1);

/// How far back, in seconds, each poll looks. A revision that reaches the
/// cache later than this after it was made (a host offline that long) is missed.
const WINDOW: u64 = 600;

/// The most revisions one poll reads: far above what [`WINDOW`] holds.
const POLL_LIMIT: i32 = 5000;

/// `watch`'s own flags.
pub(crate) struct WatchOpts {
    /// `--once`: print one frame and exit, rather than following.
    pub(crate) once: bool,
    /// `--color`: whether to ANSI-color the dashboard.
    pub(crate) color: ColorWhen,
    /// `--jsonl`: print a JSON line per status change instead of the dashboard.
    pub(crate) jsonl: bool,
}

impl WatchOpts {
    /// `watch`'s flags, refusing `--once` with `--jsonl`: one prints a single
    /// frame, the other a stream of changes and never a frame.
    pub(crate) fn new(once: bool, color: ColorWhen, jsonl: bool) -> Result<WatchOpts> {
        if once && jsonl {
            return Err("watch: --once and --jsonl don't mix; --jsonl only streams changes".into());
        }
        Ok(WatchOpts { once, color, jsonl })
    }
}

/// `agentium watch` — draw the session dashboard, then redraw it on every
/// change until Ctrl-C.
///
/// On a terminal it takes over the alternate screen and redraws in place; into
/// a pipe it prints each changed frame in full, separated by a blank line.
/// `filters` and `scope` select rows exactly as they do for `list`.
///
/// The subscription is opened *before* the seed fold so an event landing in
/// between is delivered rather than lost; folding it twice is harmless, since
/// the fold is idempotent. A key the subscription names but a fresh read can't
/// see yet is kept and retried on the next wake (see [`reduce_delta`]).
pub(crate) async fn cmd_watch(
    engine: &Engine,
    author: &Pubkey,
    filters: &ListFilters,
    scope: ListScope,
    opts: &WatchOpts,
) -> Result<()> {
    let ndb = engine.ndb();
    let mut watch = engine.watch_activity()?;
    let mut fold = {
        let txn = Transaction::new(ndb)?;
        fold_sessions(ndb, &txn, author).ok_or("could not read sessions from the cache")?
    };

    if opts.jsonl {
        return stream_transitions(engine, watch, fold, author, filters, scope).await;
    }

    let tty = std::io::stdout().is_terminal();
    let color = opts.color.enabled(tty);
    let width = terminal_width();
    let frame = |fold: &SessionReducer| {
        let rows = select(fold, filters, scope);
        render_frame(&rows, now_secs(), color, width)
    };

    if opts.once {
        print!("{}", frame(&fold));
        return Ok(());
    }

    // Held for the loop's lifetime: dropping it (on Ctrl-C, or an error
    // returning early) restores the cursor and the user's screen.
    let screen = tty.then(ScreenGuard::enter);
    let mut shown: Option<String> = None;
    let mut deferred: Vec<NoteKey> = Vec::new();
    let mut tick = tokio::time::interval(TICK);
    tick.tick().await; // the first tick is immediate; the loop draws anyway.

    loop {
        // Redraw only when the frame actually changed, so a pipe doesn't fill
        // with identical frames from a re-delivered note.
        let next = frame(&fold);
        if shown.as_ref() != Some(&next) {
            show(screen.as_ref(), shown.is_some(), &next, width);
            shown = Some(next);
        }

        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            keys = watch.next_keys() => {
                let Some(keys) = keys else {
                    break; // the subscription ended: the database is gone.
                };
                deferred.extend(keys);
                let txn = Transaction::new(ndb)?;
                deferred = reduce_delta(&mut fold, ndb, &txn, &deferred);
            }
            _ = tick.tick() => {}
        }
    }

    Ok(())
}

/// `watch --jsonl`: print one [`Transition`](agentium_core::cli::watch::Transition) line for each session whose status
/// changes, until Ctrl-C or the subscription ends.
///
/// The statuses the seed fold holds are the baseline and print nothing, so a
/// start never replays history: only a change seen while running is a line. A
/// session that first appears mid-run prints with a null `from`. `filters` and
/// `scope` pick the sessions as they do for `list`; a session that leaves the
/// selection is forgotten, so coming back reads as new.
///
/// State revisions come from re-reading the cache every [`POLL`], not from the
/// subscription: a subscription wakes only for notes this process writes, and
/// the cache is shared with every other agentium run, so a revision a `log` or
/// `send` synced in first never wakes it. The subscription still carries
/// conversation messages (for `last_activity`) and makes a revision this
/// process wrote show at once rather than on the next poll.
async fn stream_transitions(
    engine: &Engine,
    mut watch: agentium_core::SessionWatch,
    mut fold: SessionReducer,
    author: &Pubkey,
    filters: &ListFilters,
    scope: ListScope,
) -> Result<()> {
    let ndb = engine.ndb();
    let mut seen = statuses(&select(&fold, filters, scope));
    // The seed already folded everything in the window: mark it read, so the
    // first poll doesn't refold a same-second revision over the newer one.
    let mut read = RecentRevisions::default();
    read.read_new(ndb, &Transaction::new(ndb)?, author, now_secs())?;
    read.mark_all();
    let mut poll = tokio::time::interval(POLL);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            keys = watch.next_keys() => {
                let Some(keys) = keys else {
                    break; // the subscription ended: the database is gone.
                };
                let txn = Transaction::new(ndb)?;
                for key in keys {
                    // Messages only advance last-activity; revisions wait for
                    // the poll below, which orders them.
                    let Ok(note) = ndb.get_note_by_key(&txn, key) else { continue };
                    if note.kind() == AI_CONVERSATION_KIND {
                        fold.ingest(&note);
                    }
                }
            }
            _ = poll.tick() => {}
        }

        // One revision at a time, oldest first, diffing after each: a quick
        // turn's working and done revisions can land between two polls, and
        // folding them together would keep only the newest, hiding the move.
        let txn = Transaction::new(ndb)?;
        read.read_new(ndb, &txn, author, now_secs())?;
        let mut out = std::io::stdout().lock();
        for key in read.take_unread() {
            let Ok(note) = ndb.get_note_by_key(&txn, key) else {
                continue;
            };
            fold.ingest(&note);
            let rows = select(&fold, filters, scope);
            for line in transitions(&seen, &rows) {
                writeln!(out, "{}", serde_json::to_string(&line)?)?;
            }
            seen = statuses(&rows);
        }
        out.flush()?;
    }

    Ok(())
}

/// The session-state revisions of the last [`WINDOW`] seconds that `watch
/// --jsonl` has folded, and the ones it hasn't yet.
///
/// The fold lets a same-second revision replace the one it holds, so refolding
/// a revision already folded could flip a session back to an older status. A
/// revision is folded once, and new ones in nostrdb's ingest order: by
/// `created_at`, then by note key, which nostrdb hands out as it writes.
#[derive(Default)]
struct RecentRevisions {
    /// Folded revisions, by note key, with their `created_at` for pruning.
    folded: HashMap<NoteKey, u64>,
    /// Revisions read but not folded yet, in the order to fold them. Emptied
    /// by every [`take_unread`](Self::take_unread), before the next read.
    unread: Vec<Revision>,
}

/// A session-state revision's place in the order [`RecentRevisions`] folds.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Revision {
    /// The revision's `created_at`, compared first.
    created_at: u64,
    /// Its note key, ordering revisions of the same second.
    key: NoteKey,
}

impl RecentRevisions {
    /// Read `author`'s session-state revisions from the last [`WINDOW`] before
    /// `now` that aren't folded yet into the unread list, oldest first, and
    /// forget folded ones that have left the window.
    fn read_new(&mut self, ndb: &Ndb, txn: &Transaction, author: &Pubkey, now: u64) -> Result<()> {
        let since = now.saturating_sub(WINDOW);
        let filter = Filter::new()
            .kinds([AI_SESSION_STATE_KIND as u64])
            .authors([author.bytes()])
            .since(since)
            .build();
        for r in ndb.query(txn, &[filter], POLL_LIMIT)? {
            let rev = Revision {
                created_at: r.note.created_at(),
                key: r.note_key,
            };
            if !self.folded.contains_key(&rev.key) {
                self.unread.push(rev);
            }
        }
        self.unread.sort_unstable();
        self.folded.retain(|_, at| *at >= since);
        Ok(())
    }

    /// Mark every unread revision folded without folding it: the seed already did.
    fn mark_all(&mut self) {
        let _ = self.take_unread().count();
    }

    /// Hand out the unread revisions in fold order, marking each folded.
    fn take_unread(&mut self) -> impl Iterator<Item = NoteKey> + '_ {
        self.unread.drain(..).map(|rev| {
            self.folded.insert(rev.key, rev.created_at);
            rev.key
        })
    }
}

/// Put `frame` on screen under an "updated" clock line: redrawn in place on a
/// terminal, or appended after a blank separator (`separate`) into a pipe.
fn show(screen: Option<&ScreenGuard>, separate: bool, frame: &str, width: Option<usize>) {
    let stamped = format!("{}\n{frame}", clip(&updated_line(now_secs()), width));
    if let Some(screen) = screen {
        screen.draw(&stamped);
        return;
    }
    let mut out = std::io::stdout().lock();
    let sep = if separate { "\n" } else { "" };
    let _ = write!(out, "{sep}{stamped}");
    let _ = out.flush();
}

/// The line width to clip to: `$COLUMNS` when it's a positive number, else the
/// terminal's own width, else `None` — a pipe has no width to wrap at, so its
/// frames go out whole rather than losing their rightmost (last-activity) column.
fn terminal_width() -> Option<usize> {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|&c| c > 0)
        .or_else(stdout_width)
}
