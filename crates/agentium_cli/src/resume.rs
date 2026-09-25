//! `agentium resume` — reopen a closed session's backend.

use agentium_core::Engine;
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::publish::flush_publish;

/// `agentium resume <session>` — reopen a closed session's backend.
///
/// Resolves the selector across the live *and* tombstoned sets (so a durable
/// `agentium:` ref still resolves after the session was soft-deleted), then
/// publishes a kind-31989 `resume_session` command targeting the session's host.
/// The host reopens the session — reviving its `agentium:` ref, rehydrating its
/// history, and resuming the CLI backend with `claude --resume`.
///
/// Errors early (before publishing) only when nothing matches the selector. A
/// session whose backend never started (empty `cli_session`) or a legacy event
/// (no `cli_session` tag) still resumes: the host reopens a fresh backend or
/// resumes from the d-tag respectively, mirroring the GUI's click-to-reopen.
pub(crate) async fn cmd_resume(engine: &Engine, author: &Pubkey, selector: &str) -> Result<()> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_states_for_author,
        resolve_session_including_deleted,
    };

    // Resolve to the fields the resume command needs, then drop the borrow of the
    // loaded state vectors before we publish.
    let (target_host, cwd, backend, target_sid, cli_sid, uri) = {
        let txn = Transaction::new(engine.ndb())?;
        let live = load_session_states_for_author(engine.ndb(), &txn, author);
        let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
        let state = resolve_session_including_deleted(&live, &deleted, selector)?;

        // Resolve the `claude --resume` id exactly as the GUI's host-side
        // hydrator does (`hydrate_session_from_state` in notedeck_dave): a
        // non-empty `cli_session` is the real CLI id; an empty one means the
        // backend never started, so the host reopens a *fresh* backend (no
        // `--resume`); an absent tag is a legacy event whose d-tag *is* the CLI
        // id. We never bail — the GUI reopens all three, so the CLI must too.
        //
        // This value is advisory: the host's `reopen_session` re-derives the
        // resume id itself from the session's own state and ignores what the
        // resume command carries. We still resolve it faithfully for
        // forward-compat and so `--json`/logs report a sensible id.
        let cli = match state.cli_session_id.as_deref() {
            Some(cli) if !cli.is_empty() => cli.to_string(),
            Some(_) => String::new(),
            None => state.claude_session_id.clone(),
        };
        (
            state.hostname.clone(),
            state.cwd.clone(),
            state
                .backend
                .clone()
                .unwrap_or_else(|| "claude".to_string()),
            state.claude_session_id.clone(),
            cli,
            state.agentium_uri(),
        )
    };

    if target_host.is_empty() {
        return Err(format!("{uri} has no recorded host; cannot target a resume").into());
    }

    engine.resume_session(&target_host, &cwd, &backend, &target_sid, &cli_sid)?;

    flush_publish(engine).await;

    println!("resume command sent to {target_host} for {uri}");
    Ok(())
}
