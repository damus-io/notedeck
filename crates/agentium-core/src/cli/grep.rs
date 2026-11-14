//! `agentium grep` — search message text across every selected session. The
//! smart-case pattern compilation and match highlighting behind it live in
//! [`cli_term`], shared with `headway grep`.

use cli_term::highlight;
use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;
use regex::Regex;

use super::list::{load_sessions, ListFilters, ListScope, SessionJson};
use super::term::{abbreviate_home, col, paint, status_style, StatusStyle, SGR_BOLD};
use super::transcript::{message_body, message_role, role_style, MessageFilter};
use crate::session_loader::{load_session_messages_for_author, SessionState};

/// One session that had a match, and its matching lines.
pub struct SessionMatches {
    /// The session's state.
    pub state: SessionState,
    /// Its matching lines, in transcript order.
    matches: Vec<GrepMatch>,
}

/// `agentium grep <pattern>` — search message text across every session the
/// `list` filters select: the sessions with a match, in `list`'s order.
///
/// This exists for the single read. The shell equivalent — loop over
/// `list --json`, run `agentium log <session> | grep` per row — re-opens the
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
pub fn search(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    filters: &ListFilters,
    scope: ListScope,
    pattern: &Regex,
    filter: &MessageFilter,
) -> Vec<SessionMatches> {
    load_sessions(ndb, txn, author, filters, scope)
        .into_iter()
        .filter_map(|state| {
            let loaded =
                load_session_messages_for_author(ndb, txn, author, &state.claude_session_id);
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
        })
        .collect()
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
    use cli_term::{compile_pattern, CaseMode};

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
