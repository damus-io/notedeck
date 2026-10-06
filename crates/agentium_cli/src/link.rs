//! `agentium link` / `agentium unlink` — set or clear the issue a live session
//! works, after it was spawned.

use std::time::Duration;

use agentium_core::Engine;
use agentium_core::session_loader::latest_valid_session_for_author;
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::permission::resolve_live;
use crate::publish::{flush_publish, json_line};

/// How long `link`/`unlink` waits for the host to republish the session with
/// the new link before reporting that it hasn't.
///
/// The host applies the command on its next frame and publishes the state on
/// the same pass, so a live host answers well inside a second; the margin is
/// for the relay round trip.
const LINK_CONFIRM: Duration = Duration::from_secs(10);

/// `agentium link <session> <issue-url>` (`issue_url = Some`) or `agentium
/// unlink <session>` (`None`): link a live session to the issue it works, or
/// clear the link, after the fact — the counterpart of `spawn --issue-url` for a
/// session that was spawned without one.
///
/// The issue is part of the session's kind-31988 state, which only its host
/// publishes, so this publishes a kind-1988 set-issue command
/// ([`Engine::set_issue_url`]) and then waits, bounded by [`LINK_CONFIRM`], for
/// the host's republished state to carry the new link. Only that answer is
/// reported as success: a host running a Dave build from before `link` ignores
/// the command, and saying "linked" there would be a lie. An offline host still
/// applies the command when it next syncs, so the error says so.
///
/// Resolves against the **live** set, like `mode`/`interrupt`: a deleted
/// session has no host to republish it. A session already in the asked-for
/// state publishes nothing.
pub(crate) async fn cmd_link(
    engine: &Engine,
    author: &Pubkey,
    selector: &str,
    issue_url: Option<&str>,
    as_json: bool,
) -> Result<()> {
    let (session_id, uri, current) = {
        let txn = Transaction::new(engine.ndb())?;
        let state = resolve_live(engine, &txn, author, selector)?;
        (
            state.claude_session_id.clone(),
            state.agentium_uri(),
            state.issue_url.clone(),
        )
    };

    if current.as_deref() == issue_url {
        return emit(&uri, issue_url, None, true, as_json);
    }

    // Watch the session list *before* publishing so the host's republished
    // state can't land between the publish and our first read.
    let mut watch = engine.watch_sessions()?;
    let built = engine.set_issue_url(&session_id, issue_url)?;
    let event_id = hex::encode(built.note_id);
    flush_publish(engine).await;

    // `check, then wait`: re-read the session's latest state, else block on the
    // watch for the next state event.
    let confirmed = tokio::time::timeout(LINK_CONFIRM, async {
        loop {
            {
                let txn = Transaction::new(engine.ndb())?;
                let latest =
                    latest_valid_session_for_author(engine.ndb(), &txn, author, &session_id);
                if latest.is_some_and(|s| s.issue_url.as_deref() == issue_url) {
                    return Ok::<bool, Box<dyn std::error::Error>>(true);
                }
            }
            if !watch.changed().await {
                return Ok(false);
            }
        }
    })
    .await;

    match confirmed {
        Ok(Ok(true)) => emit(&uri, issue_url, Some(&event_id), false, as_json),
        Ok(Err(e)) => Err(e),
        Ok(Ok(false)) | Err(_) => Err(format!(
            "sent the command to {uri} (event {}…), but its host hasn't republished the \
             session within {}s. An offline host applies it when it next syncs; a host running \
             a Dave build from before `agentium link` ignores it and needs rebuilding.",
            &event_id[..8],
            LINK_CONFIRM.as_secs(),
        )
        .into()),
    }
}

/// Report a confirmed (or already-true) link: `linked <uri> to <issue>` /
/// `unlinked <uri>`, or with `--json` one line of `{ session, issue_url,
/// event_id, unchanged }` (`event_id` is `null` when nothing was published).
fn emit(
    uri: &str,
    issue_url: Option<&str>,
    event_id: Option<&str>,
    unchanged: bool,
    as_json: bool,
) -> Result<()> {
    if as_json {
        let obj = serde_json::json!({
            "session": uri,
            "issue_url": issue_url,
            "event_id": event_id,
            "unchanged": unchanged,
        });
        println!("{}", json_line(&obj)?);
        return Ok(());
    }
    let already = if unchanged { " (already)" } else { "" };
    match issue_url {
        Some(issue) => println!("linked {uri} to {issue}{already}"),
        None => println!("unlinked {uri}{already}"),
    }
    Ok(())
}
