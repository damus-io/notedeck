//! `agentium list` — print the sessions [`agentium_core::cli::list`] selects
//! and renders; that module is shared with the nd tools.

use std::io::IsTerminal;

use agentium_core::Engine;
use agentium_core::cli::list::{load_sessions, render_list};
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

pub(crate) use agentium_core::cli::list::{ListFilters, ListScope, SessionJson};

use crate::term::now_secs;

/// `agentium list` — enumerate this identity's sessions, newest first, grouped
/// by host (see [`render_list`]). With `as_json`, each session is emitted as a
/// [`SessionJson`] (the state plus its `agentium:` URI), the flat newest-first
/// list. Status colors are written only when stdout is a terminal.
pub(crate) fn cmd_list(
    engine: &Engine,
    author: &Pubkey,
    filters: &ListFilters,
    scope: ListScope,
    as_json: bool,
) -> Result<()> {
    let txn = Transaction::new(engine.ndb())?;
    let sessions = load_sessions(engine.ndb(), &txn, author, filters, scope);

    if as_json {
        // The full SessionState set plus its rendered `agentium:` URI,
        // machine-readable. External agents (e.g. the agentium Claude skill)
        // quote their own session ref into a headway done-comment straight from
        // this field rather than reimplementing the word-id encoding.
        let view: Vec<SessionJson> = sessions.iter().map(SessionJson::new).collect();
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }

    let color = std::io::stdout().is_terminal();
    print!("{}", render_list(&sessions, now_secs(), color));
    Ok(())
}
