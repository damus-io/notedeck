//! `agentium list` — print the sessions [`agentium_core::cli::list`] selects
//! and renders; that module is shared with the nd tools.

use std::io::IsTerminal;

use agentium_core::Engine;
use agentium_core::cli::list::{load_sessions, render_list};
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

pub(crate) use agentium_core::cli::list::{
    ListFilters, ListScope, RowLayout, SessionJson, host_label, session_row,
};

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

#[cfg(test)]
pub(crate) mod tests {
    use agentium_core::session_loader::SessionState;

    /// A SessionState with sensible defaults, overriding the fields the tests
    /// care about.
    pub(crate) fn session(host: &str, title: &str, status: &str, created_at: u64) -> SessionState {
        SessionState {
            model: None,
            claude_session_id: format!("{host}-{title}"),
            title: title.to_string(),
            custom_title: None,
            cwd: "/home/u/proj".to_string(),
            status: status.to_string(),
            indicator: None,
            hostname: host.to_string(),
            home_dir: "/home/u".to_string(),
            backend: Some("claude".to_string()),
            permission_mode: Some("default".to_string()),
            created_at,
            cli_session_id: None,
            spawn_id: None,
            issue_url: None,
            project: None,
            project_root: None,
            parent_session: None,
            relation: Default::default(),
            profile: None,
        }
    }
}
