//! `agentium interrupt` — abort a live session's in-flight turn.

use agentium_core::Engine;
use agentium_core::cli::control::{DeletedMiss, resolve_live};
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::publish::flush_publish;

/// `agentium interrupt <session>` — abort a session's in-flight turn.
///
/// The CLI companion to pressing Esc in Dave: publishes a kind-1988 interrupt
/// command that the session's host applies by aborting the current turn/tool
/// loop. Resolves the selector against the **live** set — a tombstoned session
/// has no running backend to interrupt, so a match against only the deleted set
/// is reported as such (reopen it with `resume` first); any other miss surfaces
/// the resolver's own "no session matching" error so a typo fails loudly.
///
/// Mirrors [`cmd_send`](crate::send::cmd_send)'s resolve → engine-verb → bounded post-publish flush,
/// minus the message body and reported event id — an interrupt is fire-and-forget.
pub(crate) async fn cmd_interrupt(engine: &Engine, author: &Pubkey, selector: &str) -> Result<()> {
    let (session_id, uri) = {
        let txn = Transaction::new(engine.ndb())?;
        let state = resolve_live(engine.ndb(), &txn, author, selector, DeletedMiss::Interrupt)?;
        (state.claude_session_id.clone(), state.agentium_uri())
    };

    engine.interrupt_session(&session_id)?;

    // Flush: the publish rides the loop's FIFO, so a settle barrier enqueued
    // after it resolves once the loop has drained (sent) the publish. Bounded so
    // an unreachable relay can't stall exit — the event is already ingested
    // locally regardless.
    flush_publish(engine).await;

    println!("interrupt sent to {uri}");
    Ok(())
}
