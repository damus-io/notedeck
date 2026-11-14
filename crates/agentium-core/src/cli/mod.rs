//! What the `agentium` CLI and notedeck's agentium tools (`nd agentium …`)
//! share: which sessions a command means, how a transcript is filtered, and
//! the text each prints. Every read goes through a caller's nostrdb
//! transaction and returns owned values, so the CLI prints them and a tool
//! answers with them.

pub mod config;
pub mod control;
pub mod grep;
pub mod list;
pub mod log;
pub mod show;
pub mod spawn;
pub mod term;
pub mod transcript;
pub mod watch;

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;

use crate::session_loader::{
    load_deleted_session_states_for_author, load_session_states_for_author,
    resolve_session_including_deleted, SessionState,
};

/// What a session selector that is missing says: name one, or set the
/// variable an agent's own session is in.
pub const NO_SESSION: &str =
    "no session — pass a selector (see `agentium list`) or set $AGENTIUM_SESSION";

/// The session `selector` names — an `agentium:<word-id>` ref, a word-id, a
/// session id or a unique prefix — across the live *and* tombstoned sets, so
/// a durable ref still resolves after the session is deleted.
pub fn resolve_selected(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    selector: Option<&str>,
) -> Result<SessionState, String> {
    let selector = selector.ok_or(NO_SESSION)?;
    let live = load_session_states_for_author(ndb, txn, author);
    let deleted = load_deleted_session_states_for_author(ndb, txn, author);
    resolve_session_including_deleted(&live, &deleted, selector).cloned()
}
