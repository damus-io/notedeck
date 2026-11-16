//! `nd agentium grep` — search message text across every selected session. The
//! smart-case pattern compilation and match highlighting behind it live in
//! [`cli_term`], shared with `nd headway grep`.

use std::collections::{HashMap, HashSet};

use cli_term::highlight;
use nostrdb::{Filter, Ndb, Note, Transaction};
use nostrdb_net::Pubkey;
use regex::Regex;

use super::list::{load_sessions, ListFilters, ListScope, SessionJson};
use super::term::{abbreviate_home, col, paint, status_style, StatusStyle, SGR_BOLD};
use super::transcript::{message_body, message_role, role_style, MessageFilter};
use crate::messages::{Message, PermissionResponseType};
use crate::session_events::{get_tag_value, AI_CONVERSATION_KIND, LIVE_EVENT_SOURCE};
use crate::session_loader::{
    load_session_messages_for_author, render_conversation_note, SessionState,
};
use crate::split_message::is_split_part;

/// One session that had a match, and its matching lines.
pub struct SessionMatches {
    /// The session's state.
    pub state: SessionState,
    /// Its matching lines, in transcript order.
    matches: Vec<GrepMatch>,
}

/// `nd agentium grep <pattern>` — search message text across every session the
/// `list` filters select: the sessions with a match, in `list`'s order.
///
/// This exists for the single read. The shell equivalent — loop over
/// `list --json`, run `nd agentium log <session> | grep` per row — re-opens the
/// cache once per session. Here every session is read from the same
/// transaction.
///
/// The per-session read is linear in that session's own size, not in the corpus:
/// see `session_conversation_filter` in [`crate::session_loader`], which keeps
/// the author out of the ndb filter so the `d`-tag index is actually used.
/// Before that, each session's load rescanned every kind-1988 note in the cache
/// and `--all` took 38.5s over 879 sessions.
///
/// Session selection is [`load_sessions`] — the same `--host`/`--cwd`/`--status`/
/// `--backend` filters `list` uses, though `grep`'s scope defaults to every
/// session, deleted ones included, rather than `list`'s live-only; `--deleted`
/// narrows it to the deleted ones. Message selection is the same
/// [`MessageFilter`] `log` uses, so `--role assistant` or `--tools` changes
/// *what is searched*, not just what is shown — and since tool messages are
/// folded by default, a plain `grep` searches the human conversation. That
/// default matters more here than in `log`: a tool_result's searched text is
/// its one-line render summary, never its full output, so tool hits are mostly
/// the command line that happened to mention the word. The searched text is
/// [`message_body`] — exactly the body `log` renders — matched per line, like
/// `grep`.
///
/// Loading a transcript is nearly all of the cost (98% of 5.5 s over 4,191
/// sessions, matching is <1%), so only the sessions [`candidates`] can't rule
/// out are loaded, after one raw pass over every kind-1988 note (0.5–1 s on
/// that db). The exact check above still decides what matches, so the output
/// is the same. A rare pattern drops from ~6 s to ~1 s; one that most
/// sessions match still loads most of them, and costs that pass more.
pub fn search(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    filters: &ListFilters,
    scope: ListScope,
    pattern: &Regex,
    filter: &MessageFilter,
) -> Vec<SessionMatches> {
    let candidates = candidates(ndb, txn, author, pattern, filter);
    load_sessions(ndb, txn, author, filters, scope)
        .into_iter()
        .filter(|state| {
            candidates
                .as_ref()
                .is_none_or(|ids| ids.contains(state.claude_session_id.as_str()))
        })
        .filter_map(|state| session_matches(ndb, txn, author, state, pattern, filter))
        .collect()
}

/// `state`'s matching lines, from its loaded transcript: the exact check
/// [`search`] makes of each session [`candidates`] leaves it. `None` when
/// none match.
fn session_matches(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    state: SessionState,
    pattern: &Regex,
    filter: &MessageFilter,
) -> Option<SessionMatches> {
    let loaded = load_session_messages_for_author(ndb, txn, author, &state.claude_session_id);
    let mut matches: Vec<GrepMatch> = Vec::new();
    for m in filter.select(&loaded.messages) {
        // Match per line, like `grep`: a multi-line message contributes
        // one hit per matching line rather than dumping the whole body.
        matches.extend(
            message_body(m)
                .lines()
                .filter(|line| pattern.is_match(line))
                .map(|line| GrepMatch {
                    role: message_role(m),
                    sgr: role_style(m).sgr,
                    text: line.trim_end().to_string(),
                }),
        );
    }
    (!matches.is_empty()).then_some(SessionMatches { state, matches })
}

/// The ids of the sessions [`search`] has to load for `pattern`: every one
/// with a note that could put a matching line in a message `filter` keeps.
/// `None` when the pass fails, and every session has to be loaded.
///
/// One [`fold`](Ndb::fold) over every kind-1988 note. A note whose role
/// `filter` drops is passed over on its tags alone. Where a note's content
/// *is* its message's body ([`NoteBody::is_content`]: what a person or the
/// agent wrote), the content is matched per line, as the body would be,
/// without building a [`Message`]. Any other note is rendered on its own and
/// its body matched: see [`rendered_note_matches`]. So the candidates are a
/// superset of the sessions with a match, and nothing [`search`] would have
/// found is lost.
fn candidates<'a>(
    ndb: &Ndb,
    txn: &'a Transaction,
    author: &Pubkey,
    pattern: &Regex,
    filter: &MessageFilter,
) -> Option<HashSet<&'a str>> {
    let notes = Filter::new().kinds([AI_CONVERSATION_KIND as u64]).build();
    ndb.fold(txn, &[notes], HashSet::new(), |mut ids, note| {
        if note.pubkey() != author.bytes() {
            return ids;
        }
        let Some(ConversationNote { session, role }) = conversation_note(&note) else {
            return ids;
        };
        if ids.contains(session) {
            return ids;
        }
        let Some(body) = note_body(&note, role) else {
            return ids;
        };
        if !body.roles.iter().any(|role| filter.keeps_role(role)) {
            return ids;
        }
        let could_match = if is_split_part(&note) {
            // A split message's body is every part's content joined, so one
            // of its lines can run across two parts: load it to know.
            true
        } else if body.is_content {
            note.content().lines().any(|line| pattern.is_match(line))
        } else {
            rendered_note_matches(&note, pattern, filter)
        };
        if could_match {
            ids.insert(session);
        }
        ids
    })
    .ok()
}

/// Whether `note`'s message, rendered on its own, has a line matching
/// `pattern` and is one `filter` keeps.
///
/// Rendered alone, a note's body is the body its row shows once the session
/// is loaded, with one exception: a permission request's decision is read
/// from its response, another note, so each decision is tried. The loader's
/// folds only replace a row by a later note's message (a tool's result, a
/// subagent's next state), which is rendered and matched when its own note
/// comes by, so matching every note alone misses nothing.
fn rendered_note_matches(note: &Note, pattern: &Regex, filter: &MessageFilter) -> bool {
    let Some(mut message) = render_conversation_note(note, &HashMap::new()) else {
        return false;
    };
    if !filter.keep(&message) {
        return false;
    }
    let matches = |m: &Message| message_body(m).lines().any(|line| pattern.is_match(line));
    let Message::PermissionRequest(request) = &mut message else {
        return matches(&message);
    };
    [
        None,
        Some(PermissionResponseType::Allowed),
        Some(PermissionResponseType::Denied),
    ]
    .into_iter()
    .any(|response| {
        request.response = response;
        matches(&Message::PermissionRequest(request.clone()))
    })
}

/// A kind-1988 note's session (its `d` tag) and its `role` tag.
struct ConversationNote<'a> {
    session: &'a str,
    role: &'a str,
}

/// The session and role of a conversation note, read in one pass over its
/// tags, the first of each as [`get_tag_value`] reads them; `None` when it
/// lacks either.
fn conversation_note<'a>(note: &Note<'a>) -> Option<ConversationNote<'a>> {
    let mut session = None;
    let mut role = None;
    for tag in note.tags() {
        match tag.get_str(0) {
            Some("d") => session = session.or(tag.get_str(1)),
            Some("role") => role = role.or(tag.get_str(1)),
            _ => {}
        }
    }
    Some(ConversationNote {
        session: session?,
        role: role?,
    })
}

/// What a note renders as, read off its tags.
#[derive(Debug, PartialEq, Eq)]
struct NoteBody {
    /// The [`message_role`]s the note's message can have.
    roles: &'static [&'static str],
    /// Whether the message's body is the note's content, unchanged. When it
    /// isn't, the body is built from the note: a tool's name and summary, a
    /// todo list's items, a permission request's tool and decision.
    is_content: bool,
}

/// What `note`, whose `role` tag is `role`, renders as. This mirrors
/// [`render_conversation_note`], which the tests hold it to. `None` for a
/// note that renders no message (progress, queue operations, turn markers).
fn note_body(note: &Note, role: &str) -> Option<NoteBody> {
    let content = |roles| NoteBody {
        roles,
        is_content: true,
    };
    let built = |roles| NoteBody {
        roles,
        is_content: false,
    };
    Some(match role {
        "user" => content(&["user"]),
        "assistant" => content(&["assistant"]),
        "error" => content(&["error"]),
        "system" => content(&["system"]),
        // A Dave host's tool call with a tool id is a running row; any other
        // is assistant text.
        "tool_call"
            if get_tag_value(note, "source") == Some(LIVE_EVENT_SOURCE)
                && get_tag_value(note, "tool-id").is_some() =>
        {
            built(&["tool_running"])
        }
        "tool_call" => content(&["assistant"]),
        "tool_result" => built(&["tool_result"]),
        "permission_request" => built(&["permission_request"]),
        "permission_response" => built(&["user"]),
        "compaction_complete" => built(&["compaction"]),
        "subagent" => built(&["subagent"]),
        "todo" => built(&["todo"]),
        _ => return None,
    })
}

/// The `grep --json` value: one object per session that had a match.
pub fn json(hits: &[SessionMatches]) -> Vec<GrepSessionJson<'_>> {
    hits.iter()
        .map(|hit| GrepSessionJson {
            session: SessionJson::new(&hit.state),
            matches: &hit.matches,
        })
        .collect()
}

/// The `grep` text: each session's header, then its matching lines with every
/// occurrence of `pattern` highlighted; `no matches` when there are none.
pub fn render(hits: &[SessionMatches], pattern: &Regex, color: bool) -> String {
    if hits.is_empty() {
        return "no matches\n".to_owned();
    }
    let mut output = String::new();
    for hit in hits {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&grep_header(&hit.state, color));
        // Size the role column to the roles this session actually matched, like
        // `list` sizes its `agentium:` column — the canonical role tokens run
        // from `user` to `permission_request`, so a fixed width would either
        // truncate the long ones or pad every common one into the distance.
        let role_width = hit
            .matches
            .iter()
            .map(|m| m.role.chars().count())
            .max()
            .unwrap_or(0);
        for m in &hit.matches {
            output.push_str(&grep_match_line(m, pattern, role_width, color));
        }
    }
    output
}

/// One matching line, with the role it came from. `sgr` is that role's color
/// (from [`role_style`]), carried alongside the token so the renderer doesn't
/// have to map the role name back to a [`Message`](crate::messages::Message) variant.
#[derive(serde::Serialize)]
struct GrepMatch {
    role: &'static str,
    /// Skipped in `--json`: an ANSI color code is a terminal-rendering detail,
    /// not something a machine consumer of the match should see.
    #[serde(skip)]
    sgr: &'static str,
    text: String,
}

/// The header line introducing a session's matches: its full `agentium:` ref
/// (untruncated, so it can be pasted straight into `log`/`send`), its status as
/// `list` shows it (glyph, label and color, so a deleted or waiting session
/// reads at a glance now that `grep` searches deleted ones too), its title, and
/// its home-abbreviated working directory.
fn grep_header(state: &SessionState, color: bool) -> String {
    let sref = state.agentium_uri();
    let StatusStyle { glyph, label, sgr } = status_style(&state.status);
    let cwd = abbreviate_home(&state.cwd, &state.home_dir);
    format!(
        "{}  {}  {}  {}\n",
        paint(color, SGR_BOLD, &sref),
        paint(color, sgr, &format!("{glyph} {label}")),
        state.display_title(),
        paint(color, "90", &cwd),
    )
}

/// One match row: an indented, role-colored label padded to `role_width`,
/// followed by the matching line with every occurrence of the pattern
/// highlighted.
fn grep_match_line(m: &GrepMatch, pattern: &Regex, role_width: usize, color: bool) -> String {
    format!(
        "  {}  {}\n",
        paint(color, m.sgr, &col(m.role, role_width)),
        highlight(pattern, &m.text, color),
    )
}

/// The `grep --json` shape: one object per session that had a match, carrying
/// the same fields `list --json` emits (flattened, so `agentium_uri` sits at the
/// top level and feeds straight into `log`/`send`) plus its matching lines.
/// Grouped rather than one flat row per match, so a session's identity isn't
/// repeated once per hit.
#[derive(serde::Serialize)]
pub struct GrepSessionJson<'a> {
    #[serde(flatten)]
    session: SessionJson<'a>,
    matches: &'a [GrepMatch],
}

#[cfg(test)]
mod tests {
    use super::super::list::tests::session;
    use super::*;
    use crate::messages::{SubagentInfo, SubagentStatus};
    use crate::session_events::{
        build_live_events, build_permission_request_event, build_permission_response_event,
        build_subagent_event, BuiltEvent, LiveEventTags, ThreadingState, MAX_WIRE_EVENT_BYTES,
        TURN_END_ROLE,
    };
    use crate::session_loader::ToolResultContent;
    use crate::test_support::{pubkey_of, TempNdb, TEST_SECKEY};
    use cli_term::{compile_pattern, CaseMode};
    use nostrdb::IngestMetadata;

    /// Ingest each event, waiting for it to be stored.
    async fn ingest(ndb: &Ndb, events: &[BuiltEvent]) {
        let filter = Filter::new().build();
        for event in events {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest");
            let _ = ndb.wait_for_notes(sub, 1).await.unwrap();
        }
    }

    /// One session per kind of note `grep` can match in, each with its state
    /// note: text a person or the agent wrote, tool traffic, a permission
    /// request answered by another note, built bodies (compaction, todo,
    /// subagent), a message split across notes, and one with nothing in it.
    async fn seed(ndb: &Ndb) {
        let sk = &TEST_SECKEY;
        let mut events = Vec::new();
        let live = |session: &str, role: &str, content: &str, tags: LiveEventTags| {
            let mut threading = ThreadingState::new();
            build_live_events(content, role, session, None, tags, &mut threading, sk).unwrap()
        };
        for title in ["plain", "tools", "perm", "built", "split", "quiet"] {
            events.push(
                session("mac", title, "idle", 1_000)
                    .build_event(sk)
                    .unwrap(),
            );
        }

        let none = LiveEventTags::default;
        events.extend(live(
            "mac-plain",
            "user",
            "the terminal needs a resize hook",
            none(),
        ));
        events.extend(live(
            "mac-plain",
            "assistant",
            "first line\nline with REFINE_INTERVAL in it",
            none(),
        ));
        events.extend(live("mac-plain", "error", "boom: disk full", none()));
        events.extend(live("mac-plain", "system", "system booted", none()));
        events.extend(live("mac-plain", TURN_END_ROLE, "", none()));

        let bash = || LiveEventTags {
            tool_id: Some("t1"),
            tool_name: Some("Bash"),
            ..LiveEventTags::default()
        };
        events.extend(live("mac-tools", "tool_call", "cargo test -p foo", bash()));
        let result = ToolResultContent::encode("ran the tests", Some("output: 42 passed"), None);
        events.extend(live("mac-tools", "tool_result", &result, bash()));
        events.extend(live(
            "mac-tools",
            "tool_call",
            "legacy tool call text",
            none(),
        ));

        let mut threading = ThreadingState::new();
        let perm_id = uuid::Uuid::new_v4();
        let request = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({"command": "rm -rf /"}),
            "mac-perm",
            &mut threading,
            sk,
        )
        .unwrap();
        let response = build_permission_response_event(
            &perm_id,
            &request.note_id,
            false,
            Some("not that one"),
            false,
            false,
            "mac-perm",
            &mut threading,
            sk,
        )
        .unwrap();
        events.extend([request, response]);

        events.extend(live("mac-built", "compaction_complete", "123456", none()));
        let todo = r#"{"todos":[{"content":"write the prefilter","status":"in_progress"}]}"#;
        events.extend(live("mac-built", "todo", todo, none()));
        let subagent = SubagentInfo {
            task_id: "task-1".into(),
            description: "map the loader".into(),
            subagent_type: "Explore".into(),
            status: SubagentStatus::Completed,
            output: String::new(),
            max_output_size: 0,
            tool_results: vec![],
            background: false,
            agent_id: None,
            transcript: Default::default(),
        };
        events.push(build_subagent_event(&subagent, "mac-built", &mut threading, sk).unwrap());

        // One line, too long for a note, so it is cut across parts and only
        // the joined message holds both ends of it.
        let long = format!("start {} end", "a".repeat(3 * MAX_WIRE_EVENT_BYTES));
        let split = live("mac-split", "assistant", &long, none());
        assert!(split.len() > 1, "the message is split");
        events.extend(split);

        events.extend(live("mac-quiet", "user", "nothing to see", none()));
        ingest(ndb, &events).await;
    }

    /// The prefilter loads fewer sessions and finds the same matches: for
    /// every pattern and message filter, [`search`] equals checking every
    /// session's loaded transcript.
    #[tokio::test]
    async fn search_matches_loading_every_session() {
        let db = TempNdb::new();
        seed(&db.ndb).await;
        let txn = Transaction::new(&db.ndb).unwrap();
        let author = pubkey_of(&TEST_SECKEY);
        let all = ListFilters {
            host: None,
            status: None,
            cwd: None,
            backend: None,
        };
        let view = |roles: &[&str], show_tools, last| MessageFilter {
            roles: roles.iter().map(|r| r.to_string()).collect(),
            last,
            show_tools,
        };
        let views = [
            view(&[], false, None),
            view(&[], true, None),
            view(&["user"], false, None),
            view(&["assistant"], true, None),
            view(&["permission_request"], false, None),
            view(&[], false, Some(1)),
        ];
        let patterns = [
            "terminal",
            "^line with REFINE",
            "boom",
            "booted",
            "cargo test",
            "Bash",
            "passed",
            "legacy tool",
            "Bash  \\[denied\\]",
            "pending",
            "not that one",
            "tokens before",
            "doing: write",
            "Explore: map",
            "completed",
            "start a+ end",
            "nothing",
            "zzz-no-such-text",
        ];
        let as_json = |hits: &[SessionMatches]| serde_json::to_string(&json(hits)).unwrap();
        let mut found = HashSet::new();
        for pattern in patterns {
            let re = compile_pattern(pattern, CaseMode::Smart).unwrap();
            for filter in &views {
                let hits = search(&db.ndb, &txn, &author, &all, ListScope::All, &re, filter);
                let loaded: Vec<SessionMatches> =
                    load_sessions(&db.ndb, &txn, &author, &all, ListScope::All)
                        .into_iter()
                        .filter_map(|s| session_matches(&db.ndb, &txn, &author, s, &re, filter))
                        .collect();
                assert_eq!(
                    as_json(&hits),
                    as_json(&loaded),
                    "{pattern:?} under {:?}/tools={}",
                    filter.roles,
                    filter.show_tools
                );
                found.extend(hits.iter().map(|hit| hit.state.claude_session_id.clone()));
            }
        }
        // Not a vacuous comparison: every session with something in it matched.
        for session in [
            "mac-plain",
            "mac-tools",
            "mac-perm",
            "mac-built",
            "mac-split",
        ] {
            assert!(found.contains(session), "{session} never matched");
        }
    }

    /// [`note_body`] says what each note renders as, without rendering it:
    /// every rendered note's role is one it names, and where it says the body
    /// is the content, it is.
    #[tokio::test]
    async fn note_body_mirrors_the_loader() {
        let db = TempNdb::new();
        seed(&db.ndb).await;
        let txn = Transaction::new(&db.ndb).unwrap();
        let notes = Filter::new().kinds([AI_CONVERSATION_KIND as u64]).build();
        let checked = db
            .ndb
            .fold(&txn, &[notes], 0, |checked, note| {
                let role = get_tag_value(&note, "role").unwrap();
                let Some(message) = render_conversation_note(&note, &HashMap::new()) else {
                    return checked;
                };
                let body = note_body(&note, role)
                    .unwrap_or_else(|| panic!("a {role} note renders, so it has a body"));
                assert!(
                    body.roles.contains(&message_role(&message)),
                    "a {role} note renders as {}",
                    message_role(&message)
                );
                if body.is_content && !is_split_part(&note) {
                    assert_eq!(message_body(&message), note.content(), "{role}");
                }
                checked + 1
            })
            .unwrap();
        assert!(checked >= 12, "only {checked} notes checked");
    }

    /// A permission request's body carries the decision from its response,
    /// a note the prefilter doesn't read, so every decision is tried.
    #[tokio::test]
    async fn every_permission_decision_is_tried() {
        let db = TempNdb::new();
        seed(&db.ndb).await;
        let txn = Transaction::new(&db.ndb).unwrap();
        let notes = Filter::new().kinds([AI_CONVERSATION_KIND as u64]).build();
        let request = db
            .ndb
            .query(&txn, &[notes], 100)
            .unwrap()
            .into_iter()
            .find(|r| get_tag_value(&r.note, "role") == Some("permission_request"))
            .expect("the request")
            .note;
        let view = MessageFilter {
            roles: vec![],
            last: None,
            show_tools: false,
        };
        for decision in ["pending", "allowed", "denied"] {
            let re = Regex::new(&format!(r"^Bash  \[{decision}\]$")).unwrap();
            assert!(rendered_note_matches(&request, &re, &view), "{decision}");
        }
        let users_only = MessageFilter {
            roles: vec!["user".into()],
            ..view
        };
        let any = Regex::new("Bash").unwrap();
        assert!(!rendered_note_matches(&request, &any, &users_only));
    }

    #[test]
    fn grep_header_leads_with_the_full_ref() {
        let s = session("mac", "Hello", "working", 0);
        let header = grep_header(&s, false);
        assert!(
            !header.contains('\x1b'),
            "no ANSI when color=false: {header:?}"
        );
        assert!(
            header.contains(&s.agentium_uri()) && !header.contains('…'),
            "the full, pasteable ref leads the header: {header:?}"
        );
        assert!(header.contains("Hello"));
        assert!(header.contains("~/proj"), "cwd is home-abbreviated");
        assert!(
            header.contains(&format!("{}  ● Working  Hello", s.agentium_uri())),
            "the status sits between the ref and the title: {header:?}"
        );
    }

    /// The status is painted in `list`'s color for it, so a deleted session
    /// reads dim beside a live one.
    #[test]
    fn grep_header_colors_the_status() {
        let s = session("mac", "Gone", "deleted", 0);
        let header = grep_header(&s, true);
        assert!(header.contains("\x1b[90m⊘ Deleted\x1b[0m"), "{header:?}");
    }

    #[test]
    fn grep_match_line_pads_the_role_and_keeps_the_text() {
        let m = GrepMatch {
            role: "assistant",
            sgr: "32",
            text: "the terminal needs a resize hook".to_string(),
        };
        let line = grep_match_line(
            &m,
            &compile_pattern("terminal", CaseMode::Sensitive).unwrap(),
            18,
            false,
        );
        assert!(
            line.starts_with("  assistant "),
            "indented, padded role: {line:?}"
        );
        assert!(line.ends_with("the terminal needs a resize hook\n"));
        assert!(!line.contains('\x1b'), "no ANSI when color=false: {line:?}");
    }
}
