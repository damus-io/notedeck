//! `agentium grep` — search message text across every selected session. The
//! smart-case pattern compilation and match highlighting behind it live in
//! [`cli_term`], shared with `headway grep`.

use std::io::IsTerminal;

use agentium_core::Engine;
use agentium_core::session_loader::SessionState;
use cli_term::highlight;
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;
use regex::Regex;

use crate::list::{ListFilters, ListScope, SessionJson, load_sessions};
use crate::log::PAGER_VAR;
use crate::term::{SGR_BOLD, abbreviate_home, col, paint};
use crate::transcript::{MessageView, message_body, message_role, role_style};

/// `agentium grep <pattern>` — search message text across every session the
/// `list` filters select, printing each match under its session's header.
///
/// This exists for the single sync. The shell equivalent — loop over
/// `list --json`, run `agentium log <session> | grep` per row — re-opens the
/// cache and re-reconciles the relay once per session, and that reconcile is
/// seconds of wall clock against a fraction of a second of actual folding. Here
/// the corpus is synced once (or not at all, under `--no-sync`) and every
/// session is read from the same transaction.
///
/// The per-session read is linear in that session's own size, not in the corpus:
/// see [`session_conversation_filter`], which keeps the author out of the ndb
/// filter so the `d`-tag index is actually used. Before that, each session's load
/// rescanned every kind-1988 note in the cache and `--all` took 38.5s over 879
/// sessions.
///
/// [`session_conversation_filter`]: agentium_core::session_loader
///
/// Session selection is [`load_sessions`] — the same `--host`/`--cwd`/`--status`/
/// `--backend` filters and `--deleted`/`--all` scope `list` uses. Message
/// selection is the same [`MessageView`] `log` uses, so `--role assistant`
/// or `--tools` changes *what is searched*, not just what is shown — and since
/// tool messages are folded by default, a plain `grep` searches the human
/// conversation. That default matters more here than in `log`: a tool_result's
/// searched text is its one-line render summary, never its full output, so tool
/// hits are mostly the command line that happened to mention the word. The
/// searched text is [`message_body`] — exactly the body `log` renders — matched
/// per line, like `grep`.
pub(crate) fn cmd_grep(
    engine: &Engine,
    author: &Pubkey,
    filters: &ListFilters,
    scope: ListScope,
    pattern: &Regex,
    view: &MessageView,
    as_json: bool,
) -> Result<()> {
    use agentium_core::session_loader::load_session_messages_for_author;

    // Output concerns resolve up front, as in `cmd_log`: a match list is as
    // page-worthy as a transcript, and `--color always` is how you keep the
    // highlight when piping into your own `less -R`.
    let stdout_tty = std::io::stdout().is_terminal();
    let use_pager = view.pager.enabled(stdout_tty);
    let color = view.color.enabled(stdout_tty || use_pager);

    // One transaction for every session read below — nostrdb allows a single
    // reader per thread, so opening one per session would fail (and re-reading
    // the state set per session would be the slow shape this command replaces).
    let txn = Transaction::new(engine.ndb())?;
    let sessions = load_sessions(engine, &txn, author, filters, scope);

    let mut rows: Vec<GrepSessionJson> = Vec::new();
    let mut output = String::new();

    for state in &sessions {
        let loaded =
            load_session_messages_for_author(engine.ndb(), &txn, author, &state.claude_session_id);
        let mut matches: Vec<GrepMatch> = Vec::new();
        for m in view.select(&loaded.messages) {
            // Match per line, like `grep`: a multi-line message contributes one
            // hit per matching line rather than dumping the whole body.
            matches.extend(
                message_body(m)
                    .lines()
                    .filter(|line| pattern.is_match(line))
                    .map(|line| GrepMatch {
                        role: message_role(m),
                        sgr: role_style(m).1,
                        text: line.trim_end().to_string(),
                    }),
            );
        }
        if matches.is_empty() {
            continue;
        }
        if as_json {
            rows.push(GrepSessionJson {
                session: SessionJson::new(state),
                matches,
            });
            continue;
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&grep_header(state, color));
        // Size the role column to the roles this session actually matched, like
        // `list` sizes its `agentium:` column — the canonical role tokens run
        // from `user` to `permission_request`, so a fixed width would either
        // truncate the long ones or pad every common one into the distance.
        let role_width = matches
            .iter()
            .map(|m| m.role.chars().count())
            .max()
            .unwrap_or(0);
        for m in &matches {
            output.push_str(&grep_match_line(m, pattern, role_width, color));
        }
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if output.is_empty() {
        println!("no matches");
        return Ok(());
    }
    cli_term::emit(&output, use_pager, PAGER_VAR);
    Ok(())
}

/// One matching line, with the role it came from. `sgr` is that role's color
/// (from [`role_style`]), carried alongside the token so the renderer doesn't
/// have to map the role name back to a [`Message`](agentium_core::messages::Message) variant.
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
/// (untruncated, so it can be pasted straight into `log`/`send`), its title, and
/// its home-abbreviated working directory.
fn grep_header(state: &SessionState, color: bool) -> String {
    let sref = state.agentium_uri();
    let cwd = abbreviate_home(&state.cwd, &state.home_dir);
    format!(
        "{}  {}  {}\n",
        paint(color, SGR_BOLD, &sref),
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
struct GrepSessionJson<'a> {
    #[serde(flatten)]
    session: SessionJson<'a>,
    matches: Vec<GrepMatch>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list::tests::session;
    use cli_term::{CaseMode, compile_pattern};

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
