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

use std::cmp::Reverse;
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::time::Duration;

use agentium_core::Engine;
use agentium_core::session_events::{AI_CONVERSATION_KIND, AI_SESSION_STATE_KIND};
use agentium_core::session_fold::{SessionReducer, SessionView, fold_sessions, reduce_delta};
use cli_term::ColorWhen;
use nostrdb::{Filter, Ndb, NoteKey, Transaction};
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::list::{ListFilters, ListScope, RowLayout, SessionJson, host_label, session_row};
use crate::term::{SGR_NEEDS_INPUT, ScreenGuard, fit, now_secs, paint, status_style, stdout_width};

/// How often the dashboard redraws with nothing new, so its relative times
/// ("3m ago") keep aging while every session is quiet.
const TICK: Duration = Duration::from_secs(30);

/// How often `watch --jsonl` re-reads the cache for session-state revisions
/// other agentium runs wrote, which its subscription never hears.
const POLL: Duration = Duration::from_secs(1);

/// How far back, in seconds, each poll looks. A revision that reaches the
/// cache later than this after it was made (a host offline that long) is missed.
const WINDOW: u64 = 600;

/// The most revisions one poll reads: far above what [`WINDOW`] holds.
const POLL_LIMIT: i32 = 5000;

/// The widest the host column grows; longer hostnames are clipped.
const MAX_HOST_WIDTH: usize = 16;

/// The statuses in dashboard order — the one needing the user first, finished
/// sessions last. Anything else (a tombstone under `--all`, an unknown token)
/// sorts after all of them.
const STATUS_ORDER: [&str; 7] = [
    "needs_input",
    "working",
    "error",
    "pending",
    "waiting",
    "idle",
    "done",
];

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

/// `watch --jsonl`: print one [`Transition`] line for each session whose status
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

/// One `watch --jsonl` line: a session's status moved `from` → `to`.
#[derive(serde::Serialize)]
struct Transition<'a> {
    /// The status before, or `None` for a session first seen mid-run.
    from: Option<&'a str>,
    /// The status now.
    to: &'a str,
    /// Unix seconds of the session's newest state revision or message.
    last_activity: u64,
    /// The session as `list --json` shows it, `agentium_uri` included.
    session: SessionJson<'a>,
}

/// The status of each of `rows`, by session id: the baseline the next wake's
/// [`transitions`] compare against.
fn statuses(rows: &[&SessionView]) -> HashMap<String, String> {
    rows.iter()
        .map(|v| (v.state.claude_session_id.clone(), v.state.status.clone()))
        .collect()
}

/// The [`Transition`] of each of `rows` whose status isn't the one `seen`
/// holds for it, including a session `seen` has never held.
fn transitions<'a>(
    seen: &'a HashMap<String, String>,
    rows: &'a [&'a SessionView],
) -> impl Iterator<Item = Transition<'a>> {
    rows.iter().filter_map(move |v| {
        let from = seen.get(&v.state.claude_session_id).map(String::as_str);
        if from == Some(v.state.status.as_str()) {
            return None;
        }
        Some(Transition {
            from,
            to: &v.state.status,
            last_activity: v.last_activity,
            session: SessionJson::new(&v.state),
        })
    })
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

/// The dashboard's clock line, in UTC (the CLI carries no timezone database).
fn updated_line(now: u64) -> String {
    let day = now % 86_400;
    format!(
        "agentium watch — updated {:02}:{:02}:{:02}Z",
        day / 3600,
        day / 60 % 60,
        day % 60
    )
}

/// The fold's sessions that `scope` and `filters` keep, in dashboard order.
fn select<'a>(
    fold: &'a SessionReducer,
    filters: &ListFilters,
    scope: ListScope,
) -> Vec<&'a SessionView> {
    let mut rows: Vec<&SessionView> = fold
        .views_including_deleted()
        .filter(|v| scope.admits(&v.state.status) && filters.matches(&v.state))
        .collect();
    sort_rows(&mut rows);
    rows
}

/// Dashboard order: by [`status_rank`], then most recently active first, then
/// session id so equal rows never swap places between redraws.
fn sort_rows(rows: &mut [&SessionView]) {
    rows.sort_by(|a, b| {
        status_rank(&a.state.status)
            .cmp(&status_rank(&b.state.status))
            .then(Reverse(a.last_activity).cmp(&Reverse(b.last_activity)))
            .then(a.state.claude_session_id.cmp(&b.state.claude_session_id))
    });
}

/// A status's place in [`STATUS_ORDER`]; unlisted statuses rank last.
fn status_rank(status: &str) -> usize {
    STATUS_ORDER
        .iter()
        .position(|s| *s == status)
        .unwrap_or(STATUS_ORDER.len())
}

/// One dashboard frame (without the clock line): a count per status, then one
/// row per session in the order given, each clipped to `width` when there is
/// one. Pure, so tests render it with a fixed `now`.
fn render_frame(rows: &[&SessionView], now: u64, color: bool, width: Option<usize>) -> String {
    if rows.is_empty() {
        return "no sessions\n".to_string();
    }
    let layout = RowLayout {
        sref_width: RowLayout::sref_width(rows.iter().map(|v| &v.state)),
        host_width: Some(
            rows.iter()
                .map(|v| host_label(&v.state.hostname).chars().count())
                .max()
                .unwrap_or(0)
                .min(MAX_HOST_WIDTH),
        ),
        flag_needs_input: true,
    };

    let mut out = clip(&counts_line(rows, color), width);
    out.push_str("\n\n");
    for v in rows {
        let row = session_row(&v.state, v.last_activity, now, color, &layout);
        out.push_str(&clip(&row, width));
        out.push('\n');
    }
    out
}

/// "5 sessions · 1 needs input · 2 working · 2 idle": the total, then how many
/// sessions hold each status present, in dashboard order. The needs-input count
/// is painted amber, as `list`'s summary line is.
fn counts_line(rows: &[&SessionView], color: bool) -> String {
    let mut line = format!(
        "{} session{}",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    );
    // `rows` is already in dashboard order, so each status's run is contiguous.
    let mut i = 0;
    while i < rows.len() {
        let status = &rows[i].state.status;
        let n = rows[i..]
            .iter()
            .take_while(|v| &v.state.status == status)
            .count();
        let label = status_style(status).label;
        let part = format!("{n} {}", label.to_lowercase());
        let part = if status == "needs_input" {
            paint(color, SGR_NEEDS_INPUT, &part)
        } else {
            part
        };
        line.push_str(" · ");
        line.push_str(&part);
        i += n;
    }
    line
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

/// [`fit`] `line` to `width`, or leave it whole when there is no width.
fn clip(line: &str, width: Option<usize>) -> String {
    match width {
        Some(width) => fit(line, width),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list::tests::session;
    use nostrdb_net::NoteId;

    /// A folded view of a test session, last active at `last_activity`.
    fn view(host: &str, title: &str, status: &str, last_activity: u64) -> SessionView {
        SessionView {
            note_id: NoteId::new([0; 32]),
            state: session(host, title, status, 0),
            last_activity,
        }
    }

    fn titles(rows: &[&SessionView]) -> Vec<String> {
        rows.iter().map(|v| v.state.title.clone()).collect()
    }

    #[test]
    fn transitions_name_only_changed_and_new_sessions() {
        let before = [view("h", "a", "working", 1), view("h", "b", "idle", 1)];
        let seen = statuses(&before.iter().collect::<Vec<_>>());
        let after = [
            view("h", "a", "done", 2),
            view("h", "b", "idle", 2),
            view("h", "c", "working", 2),
        ];
        let rows: Vec<&SessionView> = after.iter().collect();
        let lines: Vec<serde_json::Value> = transitions(&seen, &rows)
            .map(|t| serde_json::to_value(&t).unwrap())
            .collect();

        assert_eq!(lines.len(), 2, "b kept its status: {lines:?}");
        assert_eq!(lines[0]["from"], "working");
        assert_eq!(lines[0]["to"], "done");
        assert_eq!(lines[0]["last_activity"], 2);
        assert_eq!(lines[0]["session"]["title"], "a");
        assert!(
            lines[0]["session"]["agentium_uri"]
                .as_str()
                .unwrap()
                .starts_with("agentium:")
        );
        assert_eq!(lines[1]["from"], serde_json::Value::Null);
        assert_eq!(lines[1]["to"], "working");
    }

    #[test]
    fn rows_sort_by_status_then_recency() {
        let views = [
            view("h", "done", "done", 900),
            view("h", "idle-old", "idle", 100),
            view("h", "idle-new", "idle", 500),
            view("h", "err", "error", 50),
            view("h", "ask", "needs_input", 10),
            view("h", "busy", "working", 20),
            view("h", "pend", "pending", 30),
            view("h", "parked", "waiting", 40),
            view("h", "tomb", "deleted", 999),
        ];
        let mut rows: Vec<&SessionView> = views.iter().collect();
        sort_rows(&mut rows);
        assert_eq!(
            titles(&rows),
            [
                "ask", "busy", "err", "pend", "parked", "idle-new", "idle-old", "done", "tomb"
            ],
        );
    }

    #[test]
    fn same_status_and_age_ties_break_by_session_id() {
        let views = [
            view("h", "c", "idle", 5),
            view("h", "a", "idle", 5),
            view("h", "b", "idle", 5),
        ];
        let mut rows: Vec<&SessionView> = views.iter().collect();
        sort_rows(&mut rows);
        assert_eq!(titles(&rows), ["a", "b", "c"]);
    }

    #[test]
    fn frame_counts_statuses_and_leads_with_the_waiting_session() {
        let views = [
            view("linux", "Quiet", "idle", 940),
            view("macbook", "Streaming", "working", 990),
            view("macbook", "Waiting", "needs_input", 700),
        ];
        let mut rows: Vec<&SessionView> = views.iter().collect();
        sort_rows(&mut rows);
        let frame = render_frame(&rows, 1_000, false, None);
        let lines: Vec<&str> = frame.lines().collect();

        assert!(!frame.contains('\x1b'), "no ANSI when color is off");
        assert_eq!(lines[0], "3 sessions · 1 needs input · 1 working · 1 idle");
        assert_eq!(lines[1], "");
        // The session waiting on the user comes first and carries the marker.
        assert!(lines[2].starts_with("» ") && lines[2].contains("Waiting"));
        assert!(lines[3].starts_with("  ") && lines[3].contains("Streaming"));
        assert!(lines[4].contains("Quiet"));
        // The host is its own column, and age is the fold's last activity.
        assert!(lines[2].contains("macbook") && lines[4].contains("linux"));
        assert!(lines[3].contains("10s ago"), "{}", lines[3]);
        assert!(lines[2].contains("5m ago"), "{}", lines[2]);
    }

    #[test]
    fn frame_clips_lines_to_width() {
        let views = [view("h", "A long enough title", "working", 0)];
        let rows: Vec<&SessionView> = views.iter().collect();
        let frame = render_frame(&rows, 0, true, Some(40));
        for line in frame.lines() {
            let visible: String = line
                .split('\x1b')
                .enumerate()
                .map(|(i, s)| {
                    if i == 0 {
                        s
                    } else {
                        s.split_once('m').map_or("", |p| p.1)
                    }
                })
                .collect();
            assert!(visible.chars().count() <= 40, "{visible:?}");
        }
    }

    #[test]
    fn empty_frame_says_so() {
        assert_eq!(render_frame(&[], 0, false, Some(80)), "no sessions\n");
    }

    #[test]
    fn updated_line_is_utc_clock() {
        assert_eq!(
            updated_line(86_400 * 3 + 3_600 * 13 + 60 * 5 + 9),
            "agentium watch — updated 13:05:09Z"
        );
    }
}
