//! `agentium watch`'s dashboard, shared by the CLI and notedeck's
//! `agentium_watch` tools: which of the folded sessions a frame shows and in
//! what order, the frame's text, and the status transitions `watch --jsonl`
//! streams. Everything here is pure over a [`SessionReducer`]'s views, so a
//! caller feeds it whatever fold and clock it has.

use std::cmp::Reverse;
use std::collections::HashMap;

use super::list::{host_label, session_row, ListFilters, ListScope, RowLayout, SessionJson};
use super::term::{fit, paint, status_style, SGR_NEEDS_INPUT};
use crate::session_fold::{SessionReducer, SessionView};

/// How often a dashboard redraws with nothing new, so its relative times
/// ("3m ago") keep aging while every session is quiet.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(30);

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

/// One `watch --jsonl` line: a session's status moved `from` → `to`.
#[derive(serde::Serialize)]
pub struct Transition<'a> {
    /// The status before, or `None` for a session first seen mid-run.
    pub from: Option<&'a str>,
    /// The status now.
    pub to: &'a str,
    /// Unix seconds of the session's newest state revision or message.
    pub last_activity: u64,
    /// The session as `list --json` shows it, `agentium_uri` included.
    pub session: SessionJson<'a>,
}

/// The status of each of `rows`, by session id: the baseline the next wake's
/// [`transitions`] compare against.
pub fn statuses(rows: &[&SessionView]) -> HashMap<String, String> {
    rows.iter()
        .map(|v| (v.state.claude_session_id.clone(), v.state.status.clone()))
        .collect()
}

/// The [`Transition`] of each of `rows` whose status isn't the one `seen`
/// holds for it, including a session `seen` has never held.
pub fn transitions<'a>(
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

/// The dashboard's clock line, in UTC (the CLI carries no timezone database).
pub fn updated_line(now: u64) -> String {
    let day = now % 86_400;
    format!(
        "agentium watch — updated {:02}:{:02}:{:02}Z",
        day / 3600,
        day / 60 % 60,
        day % 60
    )
}

/// The fold's sessions that `scope` and `filters` keep, in dashboard order.
pub fn select<'a>(
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
pub fn sort_rows(rows: &mut [&SessionView]) {
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
pub fn render_frame(rows: &[&SessionView], now: u64, color: bool, width: Option<usize>) -> String {
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

/// [`fit`] `line` to `width`, or leave it whole when there is no width.
pub fn clip(line: &str, width: Option<usize>) -> String {
    match width {
        Some(width) => fit(line, width),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::list::tests::session;
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
        assert!(lines[0]["session"]["agentium_uri"]
            .as_str()
            .unwrap()
            .starts_with("agentium:"));
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
            ["ask", "busy", "err", "pend", "parked", "idle-new", "idle-old", "done", "tomb"],
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
