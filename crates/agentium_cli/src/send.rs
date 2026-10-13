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
///
/// `immediate` (`--immediate`) also interrupts the session's running turn, on
/// the session this resolve picked, so a selector can't match a different one
/// for the second verb. The message goes first: a host that sees a user message
/// mid-turn queues it in the chat, an interrupt ends the turn's stream, and the
/// stream's end dispatches the queued message as the next turn
/// (`handle_stream_end` in notedeck_dave). Nothing on that path drops it.
///
/// Only a session whose state says a turn is running ([`turn_running`]) is
/// interrupted. The host dispatches a user message before it applies an
/// interrupt in the same frame, so an idle session would start the message's
/// turn and then abort it. The status is as fresh as the sync-settle made it:
/// a turn that ends between that read and the host's apply is the one case the
/// interrupt can still land on the message's own turn.
pub(crate) async fn cmd_send(
    engine: &Engine,
    author: &Pubkey,
    selector: &str,
    text: &str,
    immediate: bool,
    as_json: bool,
) -> Result<()> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_states_for_author, resolve_session,
    };

    // Resolve to the session id + URI against the live set, dropping the borrow
    // of the loaded state vectors before we publish. A miss that turns out to be
    // a tombstoned session is redirected to `resume`; any other miss propagates
    // the resolver's error.
    let Target {
        session_id,
        uri,
        status,
    } = {
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
        Target {
            session_id: state.claude_session_id.clone(),
            uri: state.agentium_uri(),
            status: state.status.clone(),
        }
    };

    // Build + ingest + publish the kind-1988 `user` message; the returned event
    // carries the durable note id we report. Sent from inside a session, it
    // names that session as its sender.
    let sender = sender_session();
    let opts = agentium_core::SendOptions {
        from_session: sender.as_deref(),
    };
    let built = engine.send_message(&session_id, text, opts)?;
    let event_id = hex::encode(built.note_id);

    // Queued behind the message on the loop's FIFO, so it is published second.
    let interrupted = immediate && turn_running(&status);
    if interrupted {
        engine.interrupt_session(&session_id)?;
    }

    // Flush: the publish rides the loop's FIFO, so a settle barrier enqueued
    // after it resolves once the loop has drained (sent) the publish. Bounded so
    // an unreachable relay can't stall exit — the event is already ingested
    // locally regardless.
    flush_publish(engine).await;

    if as_json {
        let mut obj = serde_json::json!({ "session": uri, "event_id": event_id });
        if immediate {
            obj["interrupted"] = interrupted.into();
        }
        println!("{}", json_line(&obj)?);
        return Ok(());
    }

    // A short id prefix reads cleanly at a glance; the full hex is in `--json`.
    let short = &event_id[..event_id.len().min(8)];
    println!("sent to {uri} (event {short}…)");
    if interrupted {
        println!("interrupted {uri}");
    } else if immediate {
        println!("not interrupted: {uri} is {status}, so the message starts its turn");
    }
    Ok(())
}

/// The session a `send` resolved to, read out of the loaded state before
/// anything is published.
struct Target {
    session_id: String,
    uri: String,
    /// The session state's `status` (`working`, `idle`, …).
    status: String,
}

/// Whether a session in `status` has a turn in flight that an interrupt would
/// stop: one streaming (`working`), or one blocked on a permission answer
/// (`needs_input`). Idle, done, waiting, errored and pending sessions have none.
fn turn_running(status: &str) -> bool {
    matches!(status, "working" | "needs_input")
}

/// The `agentium:` ref of the session this command runs inside, from
/// `$AGENTIUM_SESSION`, which a Dave host sets for every agent it runs. `None`
/// when a person runs `agentium send` from their own shell, so their message
/// carries no sender.
fn sender_session() -> Option<String> {
    std::env::var("AGENTIUM_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::turn_running;

    #[test]
    fn only_working_and_needs_input_have_a_turn_to_interrupt() {
        assert!(turn_running("working"));
        assert!(turn_running("needs_input"));
        for status in ["idle", "done", "waiting", "error", "pending", ""] {
            assert!(!turn_running(status), "{status}");
        }
    }
}
