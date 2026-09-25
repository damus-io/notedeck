//! `agentium send` — publish a `user` message to a live session.

use agentium_core::Engine;
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::publish::{flush_publish, json_line};

/// `agentium send <session> <text>` — publish a `user` message to a session.
///
/// Resolves the selector against the **live** session set only, builds a
/// kind-1988 `user` event threaded onto the session's existing conversation, and
/// publishes it through the engine's [`Session`] so the session's running agent
/// (local *or* remote) picks it up over relay sync. Reports the resulting event id.
///
/// Live-only on purpose: a tombstoned (soft-deleted) session has no backend
/// reading its conversation, so a message would just root a stray thread nobody
/// consumes. When the selector matches only a deleted session we redirect to
/// `resume` (the tool for reviving one) rather than send into the void; any
/// other miss surfaces the resolver's own "no session matching" error, so a typo
/// fails loudly instead of silently starting a fresh thread.
///
/// The send runs *after* [`run`](crate::run)'s bounded sync-settle, so
/// [`Engine::send_message`] threads onto the conversation's real last event (the
/// reconcile has already pulled it) instead of starting a new thread. The
/// post-publish flush mirrors [`cmd_resume`](crate::resume::cmd_resume).
pub(crate) async fn cmd_send(
    engine: &Engine,
    author: &Pubkey,
    selector: &str,
    text: &str,
    as_json: bool,
) -> Result<()> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_states_for_author, resolve_session,
    };

    // Resolve to the session id + URI against the live set, dropping the borrow
    // of the loaded state vectors before we publish. A miss that turns out to be
    // a tombstoned session is redirected to `resume`; any other miss propagates
    // the resolver's error.
    let (session_id, uri) = {
        let txn = Transaction::new(engine.ndb())?;
        let live = load_session_states_for_author(engine.ndb(), &txn, author);
        let state = match resolve_session(&live, selector) {
            Ok(state) => state,
            Err(live_err) => {
                let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
                if let Ok(gone) = resolve_session(&deleted, selector) {
                    return Err(format!(
                        "{} is deleted — reopen it with `agentium resume {selector}` before sending",
                        gone.agentium_uri()
                    )
                    .into());
                }
                return Err(live_err.into());
            }
        };
        (state.claude_session_id.clone(), state.agentium_uri())
    };

    // Build + ingest + publish the kind-1988 `user` message; the returned event
    // carries the durable note id we report.
    let built = engine.send_message(&session_id, text)?;
    let event_id = hex::encode(built.note_id);

    // Flush: the publish rides the loop's FIFO, so a settle barrier enqueued
    // after it resolves once the loop has drained (sent) the publish. Bounded so
    // an unreachable relay can't stall exit — the event is already ingested
    // locally regardless.
    flush_publish(engine).await;

    if as_json {
        let obj = serde_json::json!({ "session": uri, "event_id": event_id });
        println!("{}", json_line(&obj)?);
        return Ok(());
    }

    // A short id prefix reads cleanly at a glance; the full hex is in `--json`.
    let short = &event_id[..event_id.len().min(8)];
    println!("sent to {uri} (event {short}…)");
    Ok(())
}
