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
pub(crate) use agentium_core::cli::control::Decision;
use agentium_core::cli::control::{
    DeletedMiss, pick_request, question_approve_error, resolve_live, short_perm_id,
};
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::publish::{flush_publish, json_line};

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
        let state = resolve_live(engine.ndb(), &txn, author, selector, DeletedMiss::Running)?;
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
        return Err(question_approve_error(request, &uri).into());
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
        short_perm_id(&request.perm_id),
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
        let state = resolve_live(engine.ndb(), &txn, author, selector, DeletedMiss::Running)?;
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
