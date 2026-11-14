//! `agentium grep` — print what [`agentium_core::cli::grep`] finds across
//! every selected session; that module is shared with the nd tools.

use std::io::IsTerminal;

use agentium_core::Engine;
use agentium_core::cli::grep;
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;
use regex::Regex;

use crate::list::{ListFilters, ListScope};
use crate::log::PAGER_VAR;
use crate::transcript::MessageView;

/// `agentium grep <pattern>` — search message text across every session the
/// `list` filters select, printing each match under its session's header (see
/// [`grep::search`]).
///
/// This exists for the single sync. The shell equivalent — loop over
/// `list --json`, run `agentium log <session> | grep` per row — re-opens the
/// cache and re-reconciles the relay once per session, and that reconcile is
/// seconds of wall clock against a fraction of a second of actual folding. Here
/// the corpus is synced once (or not at all, under `--no-sync`) and every
/// session is read from the same transaction.
pub(crate) fn cmd_grep(
    engine: &Engine,
    author: &Pubkey,
    filters: &ListFilters,
    scope: ListScope,
    pattern: &Regex,
    view: &MessageView,
    as_json: bool,
) -> Result<()> {
    // Output concerns resolve up front, as in `cmd_log`: a match list is as
    // page-worthy as a transcript, and `--color always` is how you keep the
    // highlight when piping into your own `less -R`.
    let stdout_tty = std::io::stdout().is_terminal();
    let use_pager = view.pager.enabled(stdout_tty);
    let color = view.color.enabled(stdout_tty || use_pager);

    let txn = Transaction::new(engine.ndb())?;
    let hits = grep::search(
        engine.ndb(),
        &txn,
        author,
        filters,
        scope,
        pattern,
        &view.filter,
    );

    if as_json {
        println!("{}", serde_json::to_string_pretty(&grep::json(&hits))?);
        return Ok(());
    }
    if hits.is_empty() {
        println!("no matches");
        return Ok(());
    }
    cli_term::emit(&grep::render(&hits, pattern, color), use_pager, PAGER_VAR);
    Ok(())
}
