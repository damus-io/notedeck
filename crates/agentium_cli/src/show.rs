//! `agentium show` — print the git-show-style detail
//! [`agentium_core::cli::show`] reads and renders for one resolved session;
//! that module is shared with the nd tools.

use std::io::IsTerminal;

use agentium_core::Engine;
use agentium_core::cli::show::SessionDetail;
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::term::now_secs;

/// `agentium show <session>` — git-show-style detail for one resolved session
/// (see [`SessionDetail`]). With `as_json`, the same detail is a single
/// structured object.
pub(crate) fn cmd_show(
    engine: &Engine,
    author: &Pubkey,
    selector: Option<&str>,
    as_json: bool,
) -> Result<()> {
    let txn = Transaction::new(engine.ndb())?;
    let detail = SessionDetail::load(engine.ndb(), &txn, author, selector)?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&detail.json())?);
        return Ok(());
    }

    let color = std::io::stdout().is_terminal();
    print!("{}", detail.render(now_secs(), color));
    Ok(())
}
