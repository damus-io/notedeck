//! `agentium approve`/`deny`/`mode` — answer a live session's permission
//! request, or change its permission mode, from the command line.
//!
//! The CLI side of what Dave's permission card and Ctrl+M do. The engine
//! already builds these events and the host already consumes them (it is the
//! remote-observer path), so this module is resolution and reporting:
//! which session, which request, and what landed.
//!
//! A response is matched on the host by `perm-id` against the requests it
//! holds *in memory*. If the host restarted since the request was made, the
//! response is dropped and the request keeps looking pending; no ack comes
//! back, so the CLI can't tell.

use agentium_core::Engine;
use agentium_core::session_loader::{PendingPermission, SessionState};
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::publish::{flush_publish, json_line};

/// Whether a permission response allows or denies the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Approve,
    Deny,
}

impl Decision {
    /// The past-tense verb the report prints (`approved`/`denied`).
    fn verb(self) -> &'static str {
        match self {
            Decision::Approve => "approved",
            Decision::Deny => "denied",
        }
    }
}

/// What `approve`/`deny` were asked to do, as parsed.
pub(crate) struct ResponseOpts {
    pub(crate) decision: Decision,
    /// A perm-id prefix picking one of several pending requests; `None` means
    /// the newest.
    pub(crate) request: Option<String>,
    /// Reply text sent with the decision (a deny reason, or a note with an
    /// approve), which the agent sees.
    pub(crate) message: Option<String>,
    /// Deny *and* stop the turn, rather than letting the agent carry on without
    /// the tool. Only meaningful with [`Decision::Deny`].
    pub(crate) interrupt: bool,
}

/// `agentium approve|deny <session>` — answer a pending permission request.
///
/// Picks the request (the newest pending one, or the one `--request` names by
/// perm-id prefix), then publishes a kind-1988 `permission_response` linked to
/// it, which the session's host applies to the waiting tool call. Refuses to
/// approve an `AskUserQuestion` request: approving one sends the answers, and
/// a bare approve has none. Denying one is fine.
pub(crate) async fn cmd_respond(
    engine: &Engine,
    author: &Pubkey,
    selector: &str,
    opts: &ResponseOpts,
    as_json: bool,
) -> Result<()> {
    use agentium_core::session_loader::{
        load_session_messages_for_author, pending_permission_requests,
    };

    let (session_id, uri, pending) = {
        let txn = Transaction::new(engine.ndb())?;
        let state = resolve_live(engine, &txn, author, selector)?;
        let loaded =
            load_session_messages_for_author(engine.ndb(), &txn, author, &state.claude_session_id);
        (
            state.claude_session_id.clone(),
            state.agentium_uri(),
            pending_permission_requests(&loaded),
        )
    };

    let request = pick_request(&pending, opts.request.as_deref(), &uri)?;
    if request.is_question && opts.decision == Decision::Approve {
        return Err(format!(
            "request {} on {uri} is an AskUserQuestion — it needs answers, which a bare \
             approve can't send; answer it in Dave, or `agentium deny` it",
            short_id(&request.perm_id)
        )
        .into());
    }

    let perm_id = request.perm_id.to_string();
    let built = engine.respond_permission(
        &session_id,
        &perm_id,
        opts.decision == Decision::Approve,
        opts.message.clone(),
        opts.interrupt,
    )?;
    let event_id = hex::encode(built.note_id);

    flush_publish(engine).await;

    if as_json {
        let obj = serde_json::json!({
            "session": uri,
            "event_id": event_id,
            "perm_id": perm_id,
            "tool_name": request.tool_name,
            "decision": opts.decision.verb(),
            "interrupt": opts.interrupt,
        });
        println!("{}", json_line(&obj)?);
        return Ok(());
    }

    let stop = if opts.interrupt {
        " and stopped the turn"
    } else {
        ""
    };
    println!(
        "{} {} request {} on {uri}{stop} (event {}…)",
        opts.decision.verb(),
        request.tool_name,
        short_id(&request.perm_id),
        &event_id[..8],
    );
    Ok(())
}

/// `agentium mode <session> <mode>` — change a live session's permission mode.
///
/// `mode` arrives already normalized to a canonical spelling by the parser: the
/// host maps any string it doesn't know to `default`, so an unnormalized alias
/// would silently put the session in the wrong mode.
pub(crate) async fn cmd_mode(
    engine: &Engine,
    author: &Pubkey,
    selector: &str,
    mode: &str,
    as_json: bool,
) -> Result<()> {
    let (session_id, uri) = {
        let txn = Transaction::new(engine.ndb())?;
        let state = resolve_live(engine, &txn, author, selector)?;
        (state.claude_session_id.clone(), state.agentium_uri())
    };

    let built = engine.set_permission_mode(&session_id, mode)?;
    let event_id = hex::encode(built.note_id);

    flush_publish(engine).await;

    if as_json {
        let obj = serde_json::json!({ "session": uri, "event_id": event_id, "mode": mode });
        println!("{}", json_line(&obj)?);
        return Ok(());
    }
    println!("set {uri} to {mode} mode (event {}…)", &event_id[..8]);
    Ok(())
}

/// Resolve `selector` against the live sessions. A match among only the
/// tombstoned ones is reported as such (there's no running backend to answer),
/// and any other miss surfaces the resolver's own error.
fn resolve_live(
    engine: &Engine,
    txn: &Transaction,
    author: &Pubkey,
    selector: &str,
) -> Result<SessionState> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_states_for_author, resolve_session,
    };

    let live = load_session_states_for_author(engine.ndb(), txn, author);
    match resolve_session(&live, selector) {
        Ok(state) => Ok(state.clone()),
        Err(live_err) => {
            let deleted = load_deleted_session_states_for_author(engine.ndb(), txn, author);
            if let Ok(gone) = resolve_session(&deleted, selector) {
                return Err(
                    format!("{} is deleted — nothing is running", gone.agentium_uri()).into(),
                );
            }
            Err(live_err.into())
        }
    }
}

/// Choose the request to answer from a session's pending ones (oldest first).
///
/// With no `prefix`, the newest: that is the one blocking the agent right now.
/// With one, the single pending request whose perm-id starts with it; a prefix
/// that matches none or several errors, listing what is pending.
fn pick_request<'p>(
    pending: &'p [PendingPermission],
    prefix: Option<&str>,
    uri: &str,
) -> Result<&'p PendingPermission> {
    let Some(prefix) = prefix else {
        return pending
            .last()
            .ok_or_else(|| format!("no pending permission request for {uri}").into());
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
            )
            .into())
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
        .map(|p| format!("{} {}", short_id(&p.perm_id), p.tool_name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A perm-id's first 8 hex digits — enough to pick it out with `--request`.
pub(crate) fn short_id(perm_id: &uuid::Uuid) -> String {
    perm_id.to_string()[..8].to_string()
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
    fn pick_defaults_to_the_newest() {
        let p = two();
        assert_eq!(
            pick_request(&p, None, "agentium:x").unwrap().tool_name,
            "Edit"
        );
    }

    #[test]
    fn pick_errors_when_nothing_is_pending() {
        let err = pick_request(&[], None, "agentium:x")
            .unwrap_err()
            .to_string();
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
        let ambiguous = pick_request(&p, Some("aaaa"), "agentium:x")
            .unwrap_err()
            .to_string();
        assert!(ambiguous.contains("more than one"), "{ambiguous}");
        assert!(
            ambiguous.contains("aaaa1111 Bash, aaaa2222 Edit"),
            "{ambiguous}"
        );

        let unknown = pick_request(&p, Some("ffff"), "agentium:x")
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("matches no"), "{unknown}");
    }
}
