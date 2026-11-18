//! `nd agentium list` — session selection, the `list` filters and scope, and the
//! per-host row rendering (plus the `--json` session shape other commands reuse).

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;

use super::term::{
    abbreviate_home, col, paint, relative_time, status_style, StatusStyle, SGR_BOLD,
    SGR_NEEDS_INPUT,
};
use crate::session_loader::SessionState;

/// The `--json` view of a session: every [`SessionState`] field, plus the
/// rendered `agentium:word-word-word` URI the terminal rows show but the raw
/// struct omits (it carries only the underlying `claude_session_id`). Flattened
/// so the extra field sits alongside the state, not nested under it.
#[derive(serde::Serialize)]
pub struct SessionJson<'a> {
    #[serde(flatten)]
    state: &'a SessionState,
    /// The sayable reference (`crate::SessionState::agentium_uri`) an
    /// external agent quotes without re-encoding the word-id itself.
    agentium_uri: String,
}

impl<'a> SessionJson<'a> {
    pub fn new(state: &'a SessionState) -> Self {
        SessionJson {
            state,
            agentium_uri: state.agentium_uri(),
        }
    }
}

/// Which sessions `list` shows. Tombstoned sessions are hidden by default so the
/// list stays clean; `--deleted`/`--all` surface them so a soft-deleted session
/// (and the durable `agentium:` ref that quotes it) is still discoverable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListScope {
    /// Live sessions only — the default.
    Live,
    /// Only tombstoned (deleted) sessions.
    Deleted,
    /// Live and tombstoned sessions together.
    All,
}

impl ListScope {
    /// Select the scope from the `--all`/`--deleted` flags. `--all` (live +
    /// deleted) wins over `--deleted` (deleted only); neither leaves the default
    /// live-only list.
    pub fn from_flags(all: bool, deleted: bool) -> ListScope {
        match (all, deleted) {
            (true, _) => ListScope::All,
            (false, true) => ListScope::Deleted,
            (false, false) => ListScope::Live,
        }
    }

    /// Whether a session with `status` is in this scope — the per-row form of
    /// the scope, for a reader (`watch`) that holds tombstones itself rather
    /// than choosing a loader.
    pub fn admits(self, status: &str) -> bool {
        let deleted = status == crate::session_loader::DELETED_STATUS;
        match self {
            ListScope::Live => !deleted,
            ListScope::Deleted => deleted,
            ListScope::All => true,
        }
    }
}

/// The kind-31988 session-state set `scope` selects, narrowed to the rows
/// `filters` keeps — the session-selection half shared by `list` and
/// [`grep`](super::grep::search), so "which sessions does `--deleted`/`--cwd`/… mean" is answered
/// in exactly one place. Reads through the caller's `txn` (nostrdb allows one
/// reader per thread, so the caller owns it).
pub fn load_sessions(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    filters: &ListFilters,
    scope: ListScope,
) -> Vec<SessionState> {
    use crate::session_loader::{
        load_deleted_session_states_for_author, load_session_states_for_author, sort_sessions,
    };

    let mut sessions = match scope {
        ListScope::Live => load_session_states_for_author(ndb, txn, author),
        ListScope::Deleted => load_deleted_session_states_for_author(ndb, txn, author),
        ListScope::All => {
            let mut v = load_session_states_for_author(ndb, txn, author);
            v.extend(load_deleted_session_states_for_author(ndb, txn, author));
            v
        }
    };
    sessions.retain(|s| filters.matches(s));
    // `ListScope::All` glues two individually-ordered loads together, and a
    // concatenation of ordered lists is not itself ordered. Re-apply the loader's
    // total order so every scope hands back the same contract.
    sort_sessions(&mut sessions);
    sessions
}

/// `nd agentium list`'s text: this identity's sessions, grouped by host.
///
/// `sessions` arrive in [`session_order`](crate::session_loader::session_order)
/// — most recently updated first, ties broken by session id — as
/// [`load_sessions`] returns them. The text regroups them with
/// [`group_by_host`]: hosts ordered by their newest session, newest-first
/// within a host. (`--json` is the flat newest-first list, ungrouped: see
/// [`SessionJson`].)
///
/// One row per session: a colored status glyph + label, the title, the
/// working directory, backend, permission mode, and how long ago it last
/// updated. Sessions waiting on the user are counted on a line of their own
/// up front. `color` paints the status colors.
pub fn render_list(sessions: &[SessionState], now: u64, color: bool) -> String {
    if sessions.is_empty() {
        return "no sessions\n".to_owned();
    }

    let mut out = String::new();

    // Surface sessions waiting on the user up front — the one status a human
    // scanning the list most needs to act on.
    let waiting = sessions
        .iter()
        .filter(|s| s.status == "needs_input")
        .count();
    if waiting > 0 {
        let note = format!("{waiting} session(s) need input");
        out.push_str(&paint(color, SGR_NEEDS_INPUT, &note));
        out.push_str("\n\n");
    }

    // Size the leading `agentium:` column to the longest ref in the list so
    // every URI renders in full — a truncated ref can't be copied into
    // `show`/`send`, which is the whole point of leading with it.
    let layout = RowLayout {
        sref_width: RowLayout::sref_width(sessions.iter()),
        host_width: None,
        flag_needs_input: false,
    };

    for group in group_by_host(sessions) {
        out.push_str(&paint(color, SGR_BOLD, group.host));
        out.push('\n');
        for s in group.sessions {
            out.push_str(&session_row(s, s.created_at, now, color, &layout));
            out.push('\n');
        }
    }

    out
}

/// Column layout shared by `list`'s per-host rows and `watch`'s flat dashboard,
/// so the two never drift into separate copies of the column set.
pub struct RowLayout {
    /// Width of the leading `agentium:` ref column; the caller sizes it to the
    /// longest ref shown so the full, copyable URI is never truncated.
    pub sref_width: usize,
    /// Width of a host column after the title, or `None` to omit it (`list`
    /// already prints the host as a group header).
    pub host_width: Option<usize>,
    /// Mark a `needs_input` row with an amber `»` in the leading gutter, for a
    /// flat view where the session waiting on the user must stand out.
    pub flag_needs_input: bool,
}

impl RowLayout {
    /// The leading-ref width that fits every session in `sessions`.
    pub fn sref_width<'a>(sessions: impl Iterator<Item = &'a SessionState>) -> usize {
        sessions
            .map(|s| {
                crate::wordid::session_ref(&s.claude_session_id)
                    .chars()
                    .count()
            })
            .max()
            .unwrap_or(0)
    }
}

/// Render one session as a padded, aligned row.
///
/// Leads with the session's sayable `agentium:word-word-word` reference — the
/// selector a human copies into `show`/`send`/etc. — then the status, title,
/// (optionally) host, working dir, backend (or the agent spec, for a session
/// under one), permission mode, and how long ago `last_active` was. `list` passes the state revision's `created_at` there;
/// `watch` passes the fold's last activity, which also counts streamed messages.
pub fn session_row(
    s: &SessionState,
    last_active: u64,
    now: u64,
    color: bool,
    layout: &RowLayout,
) -> String {
    let StatusStyle { glyph, label, sgr } = status_style(&s.status);
    let gutter = if layout.flag_needs_input && s.status == "needs_input" {
        paint(color, SGR_NEEDS_INPUT, "» ")
    } else {
        "  ".to_string()
    };
    let sref = crate::wordid::session_ref(&s.claude_session_id);
    let sref_col = paint(color, "90", &col(&sref, layout.sref_width));
    let status_col = paint(color, sgr, &format!("{glyph} {}", col(&label, 11)));
    let title = col(s.display_title(), 30);
    let host = match layout.host_width {
        Some(width) => format!("{}  ", col(host_label(&s.hostname), width)),
        None => String::new(),
    };
    let cwd = col(&abbreviate_home(&s.cwd, &s.home_dir), 26);
    // A session under an agent spec shows the spec: its backend is always
    // the native loop's, so `butler` says more than `openai`.
    let backend = s
        .agent_spec
        .as_deref()
        .filter(|spec| !spec.is_empty())
        .or(s.backend.as_deref())
        .unwrap_or("-");
    let backend = col(backend, 8);
    let mode = col(s.permission_mode.as_deref().unwrap_or("-"), 12);
    format!(
        "{gutter}{sref_col}  {status_col}  {title}  {host}{}  {backend}  {mode}  {}",
        paint(color, "90", &cwd),
        paint(color, "90", &relative_time(now, last_active)),
    )
}

/// How a session's host reads in a header or column: its hostname, or a
/// placeholder for a state event that never recorded one.
pub fn host_label(hostname: &str) -> &str {
    if hostname.is_empty() {
        "(unknown host)"
    } else {
        hostname
    }
}

/// One host's sessions, as `list` prints them under a host header.
struct HostGroup<'a> {
    /// The host's label (see [`host_label`]).
    host: &'a str,
    /// Its sessions, newest first.
    sessions: Vec<&'a SessionState>,
}

/// Group sessions by host, ordering hosts by their most recent activity and
/// sessions within a host newest-first — mirroring how the desktop groups the
/// scene by host then cwd.
///
/// Both sorts below key on `created_at` alone, at whole-second resolution, and
/// `sort_by_key` is stable — so a same-second tie falls through to the input
/// order. That is deterministic only because the input arrives in
/// [`crate::session_loader::session_order`], whose session-id tiebreak
/// this function inherits rather than repeats.
fn group_by_host(sessions: &[SessionState]) -> Vec<HostGroup<'_>> {
    let mut groups: Vec<HostGroup> = Vec::new();
    for s in sessions {
        let host = host_label(&s.hostname);
        match groups.iter_mut().find(|g| g.host == host) {
            Some(g) => g.sessions.push(s),
            None => groups.push(HostGroup {
                host,
                sessions: vec![s],
            }),
        }
    }
    // Newest-first within each host, then hosts by their newest session.
    for g in &mut groups {
        g.sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.sessions.first().map_or(0, |s| s.created_at)));
    groups
}

/// `list` row filters, all optional and case-insensitive. `status` matches the
/// raw status token exactly; `host`, `cwd`, and `backend` match as substrings.
pub struct ListFilters {
    pub host: Option<String>,
    pub status: Option<String>,
    pub cwd: Option<String>,
    pub backend: Option<String>,
}

impl ListFilters {
    /// Whether `s` passes every filter that was given.
    pub fn matches(&self, s: &SessionState) -> bool {
        let contains = |hay: &str, needle: &Option<String>| {
            needle
                .as_ref()
                .is_none_or(|n| hay.to_lowercase().contains(&n.to_lowercase()))
        };
        let eq = |hay: &str, needle: &Option<String>| {
            needle.as_ref().is_none_or(|n| hay.eq_ignore_ascii_case(n))
        };
        contains(&s.hostname, &self.host)
            && eq(&s.status, &self.status)
            && contains(&s.cwd, &self.cwd)
            && contains(s.backend.as_deref().unwrap_or(""), &self.backend)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::session_loader::sort_sessions;

    /// A SessionState with sensible defaults, overriding the fields the tests
    /// care about. (End-to-end coverage over a real relay lives in a separate
    /// card; these exercise the pure rendering/filtering logic.)
    pub fn session(host: &str, title: &str, status: &str, created_at: u64) -> SessionState {
        SessionState {
            model: None,
            claude_session_id: format!("{host}-{title}"),
            title: title.to_string(),
            custom_title: None,
            cwd: "/home/u/proj".to_string(),
            status: status.to_string(),
            indicator: None,
            hostname: host.to_string(),
            home_dir: "/home/u".to_string(),
            backend: Some("claude".to_string()),
            permission_mode: Some("default".to_string()),
            created_at,
            cli_session_id: None,
            spawn_id: None,
            issue_url: None,
            project: None,
            project_root: None,
            parent_session: None,
            relation: Default::default(),
            profile: None,
            agent_spec: None,
        }
    }

    #[test]
    fn display_title_prefers_nonempty_custom() {
        let mut s = session("h", "derived", "idle", 0);
        assert_eq!(s.display_title(), "derived");
        s.custom_title = Some("Custom".into());
        assert_eq!(s.display_title(), "Custom");
        s.custom_title = Some(String::new());
        assert_eq!(s.display_title(), "derived");
    }

    /// A row shows a session's agent spec where it shows the backend: every
    /// spec session runs on the native loop's backend, so the spec is what
    /// tells a butler from a plain chat.
    #[test]
    fn a_row_shows_the_agent_spec_over_the_backend() {
        let layout = RowLayout {
            sref_width: 0,
            host_width: None,
            flag_needs_input: false,
        };
        let mut s = session("mac", "t", "idle", 0);
        s.backend = Some("openai".into());
        assert!(session_row(&s, 0, 0, false, &layout).contains("openai"));

        s.agent_spec = Some("butler".into());
        let row = session_row(&s, 0, 0, false, &layout);
        assert!(row.contains("butler") && !row.contains("openai"), "{row}");
    }

    #[test]
    fn list_scope_from_flags() {
        assert_eq!(ListScope::from_flags(false, false), ListScope::Live);
        assert_eq!(ListScope::from_flags(false, true), ListScope::Deleted);
        assert_eq!(ListScope::from_flags(true, false), ListScope::All);
        // --all wins over --deleted.
        assert_eq!(ListScope::from_flags(true, true), ListScope::All);
    }

    #[test]
    fn list_scope_admits_by_tombstone() {
        assert!(ListScope::Live.admits("working"));
        assert!(!ListScope::Live.admits("deleted"));
        assert!(ListScope::Deleted.admits("deleted"));
        assert!(!ListScope::Deleted.admits("idle"));
        assert!(ListScope::All.admits("deleted") && ListScope::All.admits("idle"));
    }

    #[test]
    fn group_by_host_orders_by_recency() {
        let sessions = vec![
            session("mac", "old", "idle", 100),
            session("linux", "newest", "working", 300),
            session("mac", "new", "idle", 200),
        ];
        let groups = group_by_host(&sessions);
        // linux first: it holds the single newest session (300).
        assert_eq!(groups[0].host, "linux");
        assert_eq!(groups[1].host, "mac");
        // within mac, newest-first.
        let mac: Vec<_> = groups[1]
            .sessions
            .iter()
            .map(|s| s.title.as_str())
            .collect();
        assert_eq!(mac, vec!["new", "old"]);
    }

    /// A same-second batch must not shuffle between runs.
    ///
    /// `group_by_host` stable-sorts on whole-second `created_at`, so equal-aged
    /// rows keep their input order. Before `session_order` existed that input was
    /// `query_replaceable_filtered`'s `HashMap` drain — randomized per run, which
    /// is what made `nd agentium list` reorder itself over a frozen db. The existing
    /// recency test uses distinct timestamps and so cannot see the tie.
    #[test]
    fn group_by_host_ties_break_by_session_id() {
        // All one second apart from nothing: every row ties.
        let mut sessions = vec![
            session("mac", "c", "idle", 100),
            session("mac", "a", "idle", 100),
            session("mac", "b", "idle", 100),
        ];
        sort_sessions(&mut sessions);
        let groups = group_by_host(&sessions);

        let titles: Vec<_> = groups[0]
            .sessions
            .iter()
            .map(|s| s.title.as_str())
            .collect();
        assert_eq!(
            titles,
            vec!["a", "b", "c"],
            "the session-id tiebreak has to survive grouping",
        );
    }

    #[test]
    fn group_by_host_buckets_empty_hostname() {
        let sessions = [session("", "x", "idle", 1)];
        let groups = group_by_host(&sessions);
        assert_eq!(groups[0].host, "(unknown host)");
    }

    #[test]
    fn filters_match_case_insensitively() {
        let s = session("MacBook", "t", "working", 0);
        let f = |host, status, cwd, backend| ListFilters {
            host,
            status,
            cwd,
            backend,
        };
        // host is a case-insensitive substring.
        assert!(f(Some("mac".into()), None, None, None).matches(&s));
        assert!(!f(Some("linux".into()), None, None, None).matches(&s));
        // status is an exact (case-insensitive) token, not a substring.
        assert!(f(None, Some("WORKING".into()), None, None).matches(&s));
        assert!(!f(None, Some("work".into()), None, None).matches(&s));
        // backend is a substring; empty filters match everything.
        assert!(f(None, None, None, Some("clau".into())).matches(&s));
        assert!(f(None, None, None, None).matches(&s));
    }

    #[test]
    fn session_row_plain_is_uncolored_and_complete() {
        let s = session("mac", "Hello", "working", 0);
        let sref = crate::wordid::session_ref(&s.claude_session_id);
        let layout = RowLayout {
            sref_width: sref.chars().count(),
            host_width: None,
            flag_needs_input: false,
        };
        let row = session_row(&s, s.created_at, 60, false, &layout);
        assert!(!row.contains('\x1b'), "no ANSI when color=false: {row:?}");
        assert!(row.contains("agentium:"), "row leads with the sayable ref");
        assert!(
            row.contains(&sref) && !row.contains('…'),
            "the full, copyable ref renders untruncated: {row:?}"
        );
        assert!(row.contains("● Working"));
        assert!(row.contains("Hello"));
        assert!(row.contains("~/proj")); // cwd home-abbreviated
        assert!(row.contains("claude"));
        assert!(row.contains("default"));
        assert!(row.contains("1m ago")); // 60s since created_at 0
    }
}
