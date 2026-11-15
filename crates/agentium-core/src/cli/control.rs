//! What the commands that drive a live session share — `send`, `interrupt`,
//! `approve`/`deny`, `mode` and `link` in the `agentium` CLI, and the
//! `nd agentium` tools that replace them: which session a selector means
//! when it has to be running, which permission request an answer goes to,
//! and the checks on the values those commands carry.

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;

use crate::session_loader::{
    load_deleted_session_states_for_author, load_session_states_for_author, resolve_session,
    PendingPermission, SessionState,
};

/// What resolving a session for a command that needs it running says when
/// the selector names only a deleted one: the clause after `<ref> is deleted
/// — `.
#[derive(Clone, Copy, Debug)]
pub enum DeletedMiss {
    /// `send`: there is no backend reading the conversation, so point at
    /// `resume`, the command that revives one.
    Send,
    /// `interrupt`: nothing to stop.
    Interrupt,
    /// Everything else: nothing to answer the command.
    Running,
}

impl DeletedMiss {
    /// The error for `selector` matching the deleted session `uri`.
    fn error(self, uri: &str, selector: &str) -> String {
        match self {
            DeletedMiss::Send => format!(
                "{uri} is deleted — reopen it with `nd agentium resume {selector}` before sending"
            ),
            DeletedMiss::Interrupt => format!("{uri} is deleted — nothing is running to interrupt"),
            DeletedMiss::Running => format!("{uri} is deleted — nothing is running"),
        }
    }
}

/// Resolve `selector` against the **live** sessions, for a command whose
/// session has to have a host to act on it. A match among only the
/// tombstoned ones is reported as `miss` words it, and any other miss
/// surfaces the resolver's own "no session matching" error, so a typo fails
/// loudly instead of rooting a stray thread nobody reads.
pub fn resolve_live(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    selector: &str,
    miss: DeletedMiss,
) -> Result<SessionState, String> {
    let live = load_session_states_for_author(ndb, txn, author);
    match resolve_session(&live, selector) {
        Ok(state) => Ok(state.clone()),
        Err(live_err) => {
            let deleted = load_deleted_session_states_for_author(ndb, txn, author);
            if let Ok(gone) = resolve_session(&deleted, selector) {
                return Err(miss.error(&gone.agentium_uri(), selector));
            }
            Err(live_err)
        }
    }
}

/// Whether a session in `status` has a turn in flight that an interrupt would
/// stop: one streaming (`working`), or one blocked on a permission answer
/// (`needs_input`). Idle, done, waiting, errored and pending sessions have none.
pub fn turn_running(status: &str) -> bool {
    matches!(status, "working" | "needs_input")
}

/// Whether a permission response allows or denies the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Deny,
}

impl Decision {
    /// The past-tense verb a report prints (`approved`/`denied`).
    pub fn verb(self) -> &'static str {
        match self {
            Decision::Approve => "approved",
            Decision::Deny => "denied",
        }
    }
}

/// Choose the request to answer from a session's pending ones (oldest first).
///
/// With no `prefix`, the newest: that is the one blocking the agent right now.
/// With one, the single pending request whose perm-id starts with it; a prefix
/// that matches none or several errors, listing what is pending.
pub fn pick_request<'p>(
    pending: &'p [PendingPermission],
    prefix: Option<&str>,
    uri: &str,
) -> Result<&'p PendingPermission, String> {
    let Some(prefix) = prefix else {
        return pending
            .last()
            .ok_or_else(|| format!("no pending permission request for {uri}"));
    };

    let prefix = prefix.to_ascii_lowercase();
    let mut matches = pending
        .iter()
        .filter(|p| p.perm_id.to_string().starts_with(&prefix));
    let first = matches.next();
    let ambiguous = matches.next().is_some();
    match first {
        Some(request) if !ambiguous => Ok(request),
        _ => {
            let why = if ambiguous {
                "matches more than one"
            } else {
                "matches no"
            };
            Err(format!(
                "--request {prefix} {why} pending request on {uri}; pending: {}",
                describe_pending(pending)
            ))
        }
    }
}

/// The pending requests as `<short-id> <tool>` pairs for an error message, or
/// `none`.
fn describe_pending(pending: &[PendingPermission]) -> String {
    if pending.is_empty() {
        return "none".to_string();
    }
    pending
        .iter()
        .map(|p| format!("{} {}", short_perm_id(&p.perm_id), p.tool_name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A perm-id's first 8 hex digits — enough to pick it out with `--request`.
pub fn short_perm_id(perm_id: &uuid::Uuid) -> String {
    perm_id.to_string()[..8].to_string()
}

/// The refusal for approving an `AskUserQuestion` request: approving one
/// sends its answers, and a bare approve has none. Denying one is fine.
pub fn question_approve_error(request: &PendingPermission, uri: &str) -> String {
    format!(
        "request {} on {uri} is an AskUserQuestion — it needs answers, which a bare \
         approve can't send; answer it in Dave, or `nd agentium deny` it",
        short_perm_id(&request.perm_id)
    )
}

/// A permission mode as a command typed it, normalized to its canonical wire
/// spelling: the host reads an unknown string as `default`, so an alias that
/// reached it unnormalized would silently put the session in the wrong mode.
pub fn parse_mode(value: &str) -> Result<String, String> {
    crate::permission_mode::parse_permission_mode(value)
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "unknown permission mode '{value}' — expected one of: {}",
                crate::permission_mode::PERMISSION_MODES.join(", "),
            )
        })
}

/// A `--model` value: trimmed, non-empty, and a single word.
///
/// Which models exist is the backend's business (Claude takes `opus`/`sonnet`
/// aliases or full ids, Codex its own ids), so the name itself is passed
/// through. What is caught here is a value no backend could take — empty, or
/// with whitespace inside, which is usually a stray word swallowed as the
/// flag's value — so it fails before anything is published.
pub fn parse_model(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.contains(char::is_whitespace) {
        return Err(format!(
            "--model '{value}' is not a model name — pass an alias the backend \
             knows (e.g. opus, sonnet) or a full model id"
        ));
    }
    Ok(value.to_string())
}

/// An issue URL — `spawn --issue-url`'s value or `link`'s argument, named by
/// `what` in the error: a URI naming the issue the session works.
///
/// Stored verbatim, so this only checks it *is* a URI — a non-empty
/// `scheme:rest` with an RFC 3986 scheme. That rejects a bare headway word-id
/// (or `board/word-id`), which looks plausible but names no board and so can't
/// be opened from the session; the error points at the routable form instead.
pub fn parse_issue_url(value: &str, what: &str) -> Result<String, String> {
    let value = value.trim();
    let has_scheme = value.split_once(':').is_some_and(|(scheme, rest)| {
        let mut chars = scheme.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && !rest.is_empty()
    });
    if !has_scheme {
        return Err(format!(
            "{what} '{value}' is not a URI — for a headway card use \
             headway:<board>/<word-id>"
        ));
    }
    Ok(value.to_string())
}

/// The first 8 characters of an event id, for a one-line report; the full
/// hex is in `--json`.
pub fn short_event(event_id: &str) -> &str {
    &event_id[..event_id.len().min(8)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn pending(id: &str, tool: &str) -> PendingPermission {
        PendingPermission {
            perm_id: Uuid::parse_str(id).unwrap(),
            tool_name: tool.into(),
            created_ms: 0,
            is_question: false,
        }
    }

    fn two() -> Vec<PendingPermission> {
        vec![
            pending("aaaa1111-0000-0000-0000-000000000000", "Bash"),
            pending("aaaa2222-0000-0000-0000-000000000000", "Edit"),
        ]
    }

    #[test]
    fn only_working_and_needs_input_have_a_turn_to_interrupt() {
        assert!(turn_running("working"));
        assert!(turn_running("needs_input"));
        for status in ["idle", "done", "waiting", "error", "pending", ""] {
            assert!(!turn_running(status), "{status}");
        }
    }

    #[test]
    fn pick_defaults_to_the_newest() {
        let p = two();
        assert_eq!(
            pick_request(&p, None, "agentium:x").unwrap().tool_name,
            "Edit"
        );
    }

    #[test]
    fn pick_errors_when_nothing_is_pending() {
        let err = pick_request(&[], None, "agentium:x").unwrap_err();
        assert!(
            err.contains("no pending permission request for agentium:x"),
            "{err}"
        );
    }

    #[test]
    fn pick_by_unique_prefix_case_insensitively() {
        let p = two();
        let got = pick_request(&p, Some("AAAA1"), "agentium:x").unwrap();
        assert_eq!(got.tool_name, "Bash");
    }

    #[test]
    fn pick_rejects_ambiguous_and_unknown_prefixes_listing_candidates() {
        let p = two();
        let ambiguous = pick_request(&p, Some("aaaa"), "agentium:x").unwrap_err();
        assert!(ambiguous.contains("more than one"), "{ambiguous}");
        assert!(
            ambiguous.contains("aaaa1111 Bash, aaaa2222 Edit"),
            "{ambiguous}"
        );

        let unknown = pick_request(&p, Some("ffff"), "agentium:x").unwrap_err();
        assert!(unknown.contains("matches no"), "{unknown}");
    }

    #[test]
    fn issue_urls_need_a_scheme() {
        assert_eq!(
            parse_issue_url(" headway:notedeck/a-b-c ", "--issue-url").unwrap(),
            "headway:notedeck/a-b-c"
        );
        assert!(parse_issue_url("https://example.com/1", "x").is_ok());
        for bad in ["a-b-c", "notedeck/a-b-c", ":x", "headway:", "1x:y"] {
            assert!(parse_issue_url(bad, "x").is_err(), "{bad}");
        }
    }

    #[test]
    fn models_are_one_word() {
        assert_eq!(parse_model(" opus ").unwrap(), "opus");
        assert!(parse_model("").is_err());
        assert!(parse_model("opus please").is_err());
    }

    #[test]
    fn modes_normalize_or_fail() {
        assert!(parse_mode("nonsense").is_err());
        assert_eq!(parse_mode("plan").unwrap(), "plan");
    }
}
