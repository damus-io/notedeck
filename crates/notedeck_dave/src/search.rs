//! Dave's answer to the chrome's universal search (the Cmd-K bar, see
//! [`notedeck::search`]): the account's agent sessions, on every host, by
//! title, by reference and by the issue they were spawned for.
//!
//! The sessions are the shared session fold's live views (the
//! [`AgentiumSessionCache`](crate::session_cache::AgentiumSessionCache) the
//! inline `agentium:` chips read), so deleted sessions are already gone and a
//! remote host's session is found the same as a local one. A session matches:
//!
//! - **by reference**: the query is its `agentium:<word-id>` (or the bare
//!   word-id, looked up in the fold's word-id index by the caller), its
//!   session id, or its CLI session id, exactly; or a prefix of either id;
//! - **by issue**: the query is the `headway:` reference it was spawned for
//!   (`--issue-url`), whole, without its scheme, or just the card's word-id,
//!   so a card finds the sessions that worked it. Not a part of it: a board's
//!   name would list every session spawned for that board's cards;
//! - **by title**: the shared [`fuzzy_match`] on its display title.
//!
//! Hits rank by that score, then by last activity, newest first. Each opens
//! through its `agentium:<word-id>` reference, which the app's
//! [`AgentiumRefParser`](crate::reference::AgentiumRefParser) resolves to its
//! state note and the chrome routes to Dave, the way a click on the session's
//! inline chip is.
//!
//! Message text is not searched (as `agentium grep` does): that needs a
//! full-text query per keystroke, on a worker.

use agentium_core::session_loader::SessionState;
use agentium_core::wordid;
use nostrdb_net::NoteId;
use notedeck::fuzzy::fuzzy_match;
use notedeck::{OpenUri, SearchHit, SearchHits, SearchQuery};

use crate::path_utils::abbreviate_with_home;
use crate::session_cache::SessionView;

/// What a session hit is marked as in the bar.
const KIND_SESSION: &str = "Session";

/// The score of a query that names the session exactly, by reference or id:
/// above any title match, so the session it names lists first.
const EXACT: i32 = 1 << 20;

/// The score of a query that begins one of the session's ids.
const ID_PREFIX: i32 = 1 << 19;

/// The score of a query naming the issue the session was spawned for.
const ISSUE: i32 = 1 << 18;

/// The shortest id prefix that matches: a UUID's first group, so a word
/// spelled in hex (`decade`) isn't taken for one.
const MIN_ID_PREFIX: usize = 8;

/// One session that matched, before the cut to the provider's cap.
struct Ranked {
    /// Its index in the sessions searched.
    at: usize,
    /// How well it matched (see the module docs).
    score: i32,
    /// When it was last active, breaking ties in `score`, newest first.
    last_activity: u64,
}

/// Answer `query` from `sessions`, the account's live session views, into
/// `out`, best first, up to its cap. `by_word_id` is the state note of the
/// session whose word-id the query is, if any, from the fold's index.
#[profiling::function]
pub(crate) fn search(
    sessions: &[SessionView],
    query: &SearchQuery,
    by_word_id: Option<NoteId>,
    out: &mut SearchHits,
) {
    let text = query.text;
    let mut positions = Vec::new();
    let mut ranked: Vec<Ranked> = Vec::new();
    for (at, view) in sessions.iter().enumerate() {
        let state = &view.state;
        if state.is_deleted() {
            continue;
        }
        let named = by_word_id == Some(view.note_id);
        let Some(score) = score(state, named, text, &mut positions) else {
            continue;
        };
        ranked.push(Ranked {
            at,
            score,
            last_activity: view.last_activity,
        });
    }

    ranked.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.last_activity.cmp(&a.last_activity))
    });
    for r in ranked {
        if out.is_full() {
            break;
        }
        out.push(hit(&sessions[r.at].state, r.score));
    }
}

/// How well `text` matches `state`, the best of its ways to match (see the
/// module docs), or `None` when it doesn't. `named` is whether the query is
/// the session's word-id.
fn score(state: &SessionState, named: bool, text: &str, positions: &mut Vec<usize>) -> Option<i32> {
    let cli = state.cli_session_id.as_deref();
    if named || state.claude_session_id == text || cli == Some(text) {
        return Some(EXACT);
    }
    let id_prefix = text.len() >= MIN_ID_PREFIX
        && (state.claude_session_id.starts_with(text) || cli.is_some_and(|c| c.starts_with(text)));
    if id_prefix {
        return Some(ID_PREFIX);
    }
    if state
        .issue_url
        .as_deref()
        .is_some_and(|issue| names_issue(issue, text))
    {
        return Some(ISSUE);
    }
    fuzzy_match(text, state.display_title(), positions)
}

/// Whether `text` names `issue`, a `headway:<board>/<word-id>` reference:
/// the whole of it, the part after its scheme, or the part after its board.
fn names_issue(issue: &str, text: &str) -> bool {
    let after = |sep| issue.split_once(sep).is_some_and(|(_, rest)| rest == text);
    issue == text || after(':') || after('/')
}

/// The bar's row for `state`: its title, marked as a session, with where and
/// how it runs, opening its `agentium:` reference.
fn hit(state: &SessionState, score: i32) -> SearchHit {
    let uri = OpenUri::new(wordid::session_ref(&state.claude_session_id));
    let cwd = abbreviate_with_home(std::path::Path::new(&state.cwd), &state.home_dir);
    SearchHit::new(state.display_title(), uri, score)
        .kind(KIND_SESSION)
        .subtitled(format!("{} · {} · {}", state.hostname, cwd, state.status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use notedeck::search::HITS_PER_PROVIDER;

    /// A live session `id` titled `title`, last active at `last_activity`.
    fn view(id: &str, title: &str, last_activity: u64) -> SessionView {
        // Distinct per session: the tests' ids differ within 32 bytes.
        let mut note_id = [0u8; 32];
        for (b, c) in note_id.iter_mut().zip(id.bytes()) {
            *b = c;
        }
        SessionView {
            note_id: NoteId::new(note_id),
            state: SessionState {
                claude_session_id: id.to_owned(),
                title: title.to_owned(),
                cwd: "/home/jb55/dev/notedeck".to_owned(),
                home_dir: "/home/jb55".to_owned(),
                hostname: "monad".to_owned(),
                status: "working".to_owned(),
                ..SessionState::default()
            },
            last_activity,
        }
    }

    /// The titles `query` finds in `sessions`, best first.
    fn found(sessions: &[SessionView], query: &str, by_word_id: Option<NoteId>) -> Vec<String> {
        let mut out = SearchHits::new(HITS_PER_PROVIDER);
        search(sessions, &SearchQuery { text: query }, by_word_id, &mut out);
        out.hits().iter().map(|h| h.title.clone()).collect()
    }

    /// A title query finds its session as a hit that opens its `agentium:`
    /// reference, subtitled with host, abbreviated cwd and status, and
    /// misses the sessions it doesn't match.
    #[test]
    fn a_title_query_finds_its_session_and_opens_its_reference() {
        let sessions = [
            view("session-relay", "Fix relay reconnect", 10),
            view("session-cmdk", "Cmd-K session search", 20),
        ];
        let mut out = SearchHits::new(HITS_PER_PROVIDER);
        search(
            &sessions,
            &SearchQuery {
                text: "relay reconnect",
            },
            None,
            &mut out,
        );
        let [hit] = out.hits() else {
            panic!("one hit: {:?}", out.hits());
        };
        assert_eq!(hit.title, "Fix relay reconnect");
        assert_eq!(hit.kind.as_deref(), Some("Session"));
        assert_eq!(
            hit.subtitle.as_deref(),
            Some("monad · ~/dev/notedeck · working")
        );
        assert_eq!(
            hit.uri.reference,
            wordid::session_ref("session-relay"),
            "opens the session's agentium: reference"
        );
    }

    /// A custom title is the one searched and shown.
    #[test]
    fn a_custom_title_is_searched_and_shown() {
        let mut renamed = view("session-a", "claude's own title", 1);
        renamed.state.custom_title = Some("Renamed by hand".to_owned());
        assert_eq!(found(&[renamed], "renamed", None), ["Renamed by hand"]);
    }

    /// A session named by reference lists first, above a title match:
    /// by word-id (as the fold's index finds it), by session id, by CLI
    /// session id, or by a prefix of an id.
    #[test]
    fn a_reference_or_id_lists_its_session_first() {
        let mut target = view("8f2c1d9e-aaaa", "Unrelated title", 1);
        target.state.cli_session_id = Some("cli-77ab31".to_owned());
        let decoy = view("session-decoy", "8f2c 8f2c cli-77ab31 decoy", 100);
        let sessions = [decoy, target];
        let note = sessions[1].note_id;

        assert_eq!(
            found(&sessions, "maple-river-canyon", Some(note))[0],
            "Unrelated title",
            "by word-id"
        );
        assert_eq!(
            found(&sessions, "8f2c1d9e-aaaa", None)[0],
            "Unrelated title"
        );
        assert_eq!(found(&sessions, "cli-77ab31", None)[0], "Unrelated title");
        assert_eq!(found(&sessions, "8f2c1d9e", None)[0], "Unrelated title");
    }

    /// The issue a session was spawned for finds it, by the card's whole
    /// reference, the reference without its scheme, or its word-id, but not
    /// by any other part of it.
    #[test]
    fn the_issue_it_was_spawned_for_finds_a_session() {
        let mut worked = view("session-card", "Dave sessions in Cmd-K", 1);
        worked.state.issue_url = Some("headway:notedeck/label-weekend-expire".to_owned());
        let other = view("session-other", "Something else", 2);
        let sessions = [worked, other];
        assert_eq!(
            found(&sessions, "headway:notedeck/label-weekend-expire", None),
            ["Dave sessions in Cmd-K"]
        );
        assert_eq!(
            found(&sessions, "notedeck/label-weekend-expire", None),
            ["Dave sessions in Cmd-K"]
        );
        assert_eq!(
            found(&sessions, "label-weekend-expire", None),
            ["Dave sessions in Cmd-K"]
        );
        // The board's name is part of the ref, but names no one issue.
        assert!(found(&sessions, "notedeck", None).is_empty());
    }

    /// A deleted session is never a hit, even by its exact id.
    #[test]
    fn a_deleted_session_is_not_found() {
        let mut gone = view("session-gone", "Deleted session", 1);
        gone.state.status = agentium_core::session_loader::DELETED_STATUS.to_owned();
        let note = gone.note_id;
        let sessions = [gone];
        assert!(found(&sessions, "deleted", None).is_empty());
        assert!(found(&sessions, "session-gone", Some(note)).is_empty());
    }

    /// Equal matches rank the most recently active first, and the answer
    /// stops at the provider's cap.
    #[test]
    fn ties_rank_by_last_activity_and_stop_at_the_cap() {
        let sessions: Vec<SessionView> = (0..HITS_PER_PROVIDER as u64 + 3)
            .map(|i| view(&format!("session-{i}"), "Review pass", i))
            .collect();
        let mut out = SearchHits::new(HITS_PER_PROVIDER);
        search(&sessions, &SearchQuery { text: "review" }, None, &mut out);
        assert_eq!(out.hits().len(), HITS_PER_PROVIDER);
        let newest = wordid::session_ref(&format!("session-{}", HITS_PER_PROVIDER + 2));
        assert_eq!(out.hits()[0].uri.reference, newest);
    }
}
