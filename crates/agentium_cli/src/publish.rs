//! Shared plumbing for the commands that publish an event: the bounded
//! post-publish flush and the one-line `--json` record.

use std::time::Duration;

use nostrdb_net::relay::sync::Result;

/// Bound on the settle half of the post-publish flush (see [`flush_publish`]).
const PUBLISH_FLUSH: Duration = Duration::from_secs(2);

/// How long [`flush_publish`] lets the session drain an already-handed-off
/// publish before the process exits out from under it.
///
/// Sized from the failure it fixes rather than from a happy-path round trip: on
/// a two-core Linux box `agentium interrupt` lost its event outright in roughly
/// one run in ten, and 500ms closed that to 0 in 60 runs.
const PUBLISH_DRAIN: Duration = Duration::from_millis(500);

/// Flush a just-published event before the process exits.
///
/// Two waits, because one command has to cross two hand-offs and only the first
/// of them is observable.
///
/// [`Engine::wait_for_sync`](agentium_core::Engine::wait_for_sync) is a FIFO barrier, so once it resolves the loop has
/// dequeued our `Publish` and handed the event to the relay pool. That is *not*
/// the same as the event having been sent: `Session::publish` is
/// fire-and-forget, and a publish for a relay whose socket is still opening sits
/// in the pool's pending map. Exiting there drops it with the process — the CLI
/// prints "sent" and the relay never sees the event. (Measured against the
/// relay's own ndb on a Linux reproducer: `relay_notes=0` on a run the CLI had
/// reported success for.)
///
/// Nothing in the transport reports the second hand-off — there is no publish
/// ack or drained-pending signal on `Session` — so the second wait is a bounded
/// drain rather than a barrier. It should become one: the right fix is a
/// publish-completion barrier upstream in nostrdb_net's `Session`
/// (headway:notedeck/physical-pink-universe), at which point this takes a
/// condition to wait on and [`PUBLISH_DRAIN`] goes away.
pub(crate) async fn flush_publish(engine: &agentium_core::Engine) {
    let _ = tokio::time::timeout(PUBLISH_FLUSH, engine.wait_for_sync()).await;
    tokio::time::sleep(PUBLISH_DRAIN).await;
}

/// Render a single-record `--json` payload as exactly one line.
///
/// `spawn` and `send` each emit one object describing one action, so the
/// line-oriented shell idioms apply to them — and pretty-printing broke every
/// one of those idioms silently. `agentium spawn --json | tail -1 | jq -r
/// .session` (or `head -1`, or `read -r`) yielded an empty string against a
/// multi-line object, which reads as a failed spawn: the caller re-runs, and now
/// there are two agents in the worktree. That is the concrete path a real
/// duplicate took, so the compact form is part of the fix and not a cosmetic
/// change.
///
/// `list`/`show`/`log --json` stay pretty-printed. Those are whole documents
/// (an array, a nested object) that were never line-parseable in either form,
/// so compacting them would trade readability for nothing.
pub(crate) fn json_line(value: &serde_json::Value) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
