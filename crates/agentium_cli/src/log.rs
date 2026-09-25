//! `agentium log` — print (or `--follow`) one session's conversation, and the
//! pager both `log` and `grep` write through.

use std::env;
use std::io::IsTerminal;

use agentium_core::Engine;
use agentium_core::messages::Message;
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::term::{paint, status_style};
use crate::transcript::{MessageView, first_after, render_message, render_messages};

/// `agentium log <session>` — print one session's kind-1988 conversation,
/// one entry per message, in order.
///
/// Resolves the selector across the live *and* tombstoned sets (so a deleted
/// session's transcript still reads), then loads its messages with
/// [`load_session_messages_for_author`] — which already orders them by
/// [`EventOrder`] (millisecond wall-clock) and applies the single shared
/// note→message mapping ([`render_conversation_note`]). We render in the order
/// returned; we do **not** re-sort by `seq` (that axis was refactored out of the
/// display order) or reimplement the mapping.
///
/// `--role`/`--last`/`--tools` filter the rendered stream (see [`MessageView`]);
/// `--json` emits each message's structured [`Message::to_json`] view; `--jsonl`
/// short-circuits to the reconstructed claude-code JSONL (a different,
/// `seq`-ordered axis — see below). The whole thing is built into one string and
/// handed to [`emit`], which routes it through a pager (like `git log`) when
/// appropriate.
///
/// [`Message::to_json`]: agentium_core::messages::Message::to_json
///
/// [`load_session_messages_for_author`]: agentium_core::session_loader::load_session_messages_for_author
/// [`EventOrder`]: agentium_core::session_loader::EventOrder
/// [`render_conversation_note`]: agentium_core::session_loader::render_conversation_note
pub(crate) fn cmd_log(
    engine: &Engine,
    author: &Pubkey,
    selector: Option<&str>,
    view: &MessageView,
    as_json: bool,
) -> Result<()> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_messages_for_author,
        load_session_states_for_author, resolve_session_including_deleted,
    };
    use agentium_core::session_reconstructor::reconstruct_jsonl_lines;

    let selector = selector
        .ok_or("no session — pass a selector (see `agentium list`) or set $AGENTIUM_SESSION")?;

    let txn = Transaction::new(engine.ndb())?;
    let live = load_session_states_for_author(engine.ndb(), &txn, author);
    let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
    let state = resolve_session_including_deleted(&live, &deleted, selector)?;

    // Resolve pager/color up front (both are output concerns). `auto` color
    // follows the effective sink: a real tty *or* a color-aware pager — so the
    // default built-in pager (`less -R`) still shows color, and piping raw needs
    // `--color always` to keep it. Bounded by the pager decision so a
    // non-terminal run (a pipe) is plain and unpaged.
    let stdout_tty = std::io::stdout().is_terminal();
    let use_pager = view.pager.enabled(stdout_tty);
    let color = view.color.enabled(stdout_tty || use_pager);

    let output = if view.jsonl {
        // `--jsonl`: emit the reconstructed claude-code JSONL from the lossless
        // kind-1989 archive, raw. This is a *different* ordering axis than the
        // display stream — `reconstruct_jsonl_lines` sorts by `seq` to reproduce
        // the original claude-code line order of the source archive, which is
        // correct here. The function is not author-scoped, but the engine's cache
        // only holds this identity's own PNS-decrypted events (the device key
        // registered at `open_ndb` is the only one whose kind-1080 envelopes ndb
        // can decrypt), so it only ever sees our own kind-1989 events. Display
        // filters and color don't apply.
        let lines = reconstruct_jsonl_lines(engine.ndb(), &txn, &state.claude_session_id)
            .map_err(|e| e.to_string())?;
        join_lines(lines)
    } else {
        // The loader already returns messages in `EventOrder` and applies the
        // shared mapping; `MessageView` only slices/hides, never re-sorts.
        let messages =
            load_session_messages_for_author(engine.ndb(), &txn, author, &state.claude_session_id)
                .messages;
        let selected = view.select(&messages);

        if as_json {
            let rows: Vec<serde_json::Value> = selected.iter().map(|m| m.to_json()).collect();
            let mut json = serde_json::to_string_pretty(&rows)?;
            json.push('\n');
            json
        } else {
            render_messages(&selected, color)
        }
    };

    emit(&output, use_pager)
}

/// `agentium log <session> --follow` — print the current tail, then keep
/// following, appending each new message as it lands until Ctrl-C. The reading
/// *mode* of [`cmd_log`], not a separate command: same selector resolution, same
/// [`EventOrder`]-ordered loader, same shared note→message mapping.
///
/// The loop is the engine's documented *wait, then re-read the snapshot*: an ndb
/// subscription ([`Engine::watch_session`]) wakes on each new kind-1988 event and
/// [`Engine::watch_sessions`] on each kind-31988 state revision; on either wake
/// we re-read the whole ordered conversation and print only the suffix past the
/// highest order already shown (via [`first_after`]) — never a message count, so
/// a slightly-out-of-order live insert can't reprint or misorder. A status
/// change (e.g. `-> needs_input`) is surfaced as a distinct line. The engine's
/// [`Session`] stays connected (as in [`run`](crate::run)) so new relay envelopes keep firing
/// the watch.
///
/// A live stream can't be paged or reconstructed from the point-in-time archive,
/// so `--pager`/`--jsonl` were already rejected in parsing (see
/// [`MessageView::check_follow`]); `--last`/`--role`/`--tools` shape the initial
/// tail and the streamed messages, and `--json` emits newline-delimited
/// role-tagged objects instead of the rendered text.
///
/// [`EventOrder`]: agentium_core::session_loader::EventOrder
pub(crate) async fn cmd_follow(
    engine: &Engine,
    author: &Pubkey,
    selector: Option<&str>,
    view: &MessageView,
    as_json: bool,
) -> Result<()> {
    use agentium_core::session_loader::{
        EventOrder, load_deleted_session_states_for_author, load_session_messages_for_author,
        load_session_states_for_author, resolve_session_including_deleted,
    };

    let selector = selector
        .ok_or("no session — pass a selector (see `agentium list`) or set $AGENTIUM_SESSION")?;

    // Resolve the selector once to the stable session id (its kind-1988 `d` tag)
    // and its starting status; every watch and re-read below keys off that id.
    let (session_id, mut last_status) = {
        let txn = Transaction::new(engine.ndb())?;
        let live = load_session_states_for_author(engine.ndb(), &txn, author);
        let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
        let state = resolve_session_including_deleted(&live, &deleted, selector)?;
        (state.claude_session_id.clone(), state.status.clone())
    };

    // A live follow never pages, so `auto` color just follows the real stdout
    // tty (there is no color-aware pager to also satisfy, unlike `cmd_log`).
    let color = view.color.enabled(std::io::stdout().is_terminal());

    // Subscribe *before* the initial read so an event that lands in the gap
    // between reading the tail and entering the loop still wakes us: the
    // subscriptions capture from now on, the read captures history, and together
    // they leave no hole. `watch_session` is scoped to this session's kind-1988
    // events; `watch_sessions` wakes on any kind-31988 revision (we re-resolve
    // and compare, so an unrelated session's change prints nothing).
    let mut msg_watch = engine.watch_session(&session_id)?;
    let mut state_watch = engine.watch_sessions()?;

    // Initial tail: print it honoring --last/--role/--tools, but seed the cursor
    // at the whole conversation's max order (not the filtered subset's) so the
    // streamed delta resumes strictly after everything already shown.
    let mut printed_any = false;
    let mut last_order: Option<EventOrder> = {
        let txn = Transaction::new(engine.ndb())?;
        let loaded = load_session_messages_for_author(engine.ndb(), &txn, author, &session_id);
        let selected = view.select(&loaded.messages);
        emit_follow_messages(&selected, color, as_json, &mut printed_any);
        loaded.max_order
    };

    // Follow until Ctrl-C. Ctrl-C breaks cleanly; either watch ending (`false`,
    // e.g. the database was torn down) also ends the loop.
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            changed = msg_watch.changed() => {
                if !changed {
                    break;
                }
            }
            changed = state_watch.changed() => {
                if !changed {
                    break;
                }
            }
        }

        // Message delta: re-read the full ordered snapshot, emit only the suffix
        // whose order is past `last_order` (role/tool filtered, but never tailed
        // — `--last` bounds only the initial view), then advance the cursor to
        // the new global max.
        {
            let txn = Transaction::new(engine.ndb())?;
            let loaded = load_session_messages_for_author(engine.ndb(), &txn, author, &session_id);
            let start = first_after(&loaded.orders, last_order);
            let fresh: Vec<&Message> = loaded.messages[start..]
                .iter()
                .filter(|m| view.keep(m))
                .collect();
            emit_follow_messages(&fresh, color, as_json, &mut printed_any);
            if loaded.max_order.is_some() {
                last_order = loaded.max_order;
            }
        }

        // Status transition: re-resolve the session's kind-31988 status and, when
        // it flips, print a distinct line (so `-> needs_input` surfaces mid-follow).
        {
            let txn = Transaction::new(engine.ndb())?;
            let live = load_session_states_for_author(engine.ndb(), &txn, author);
            let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
            if let Ok(state) = resolve_session_including_deleted(&live, &deleted, &session_id)
                && state.status != last_status
            {
                emit_follow_status(&state.status, color, as_json);
                last_status = state.status.clone();
            }
        }
    }

    Ok(())
}

/// Stream a batch of followed messages to stdout, flushed so the follower sees
/// each immediately. Text mode reuses [`render_message`] and mirrors
/// [`render_messages`]' spacing — a blank line *between* entries, none before
/// the first — with `printed_any` carrying that "have we printed yet" state
/// across batches. JSON mode emits one compact role-tagged object per line
/// (newline-delimited, the streaming counterpart to `log --json`'s array),
/// reusing the same [`Message::to_json`] shape.
///
/// [`Message::to_json`]: agentium_core::messages::Message::to_json
fn emit_follow_messages(messages: &[&Message], color: bool, as_json: bool, printed_any: &mut bool) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    for m in messages {
        if as_json {
            if let Ok(line) = serde_json::to_string(&m.to_json()) {
                let _ = writeln!(out, "{line}");
            }
        } else {
            // A blank line separates entries (mirroring `render_messages`), but
            // not before the very first one printed across all batches.
            if *printed_any {
                let _ = writeln!(out);
            }
            let _ = write!(out, "{}", render_message(m, color));
        }
        *printed_any = true;
    }
    let _ = out.flush();
}

/// Print a session's new status as a distinct, colored line while following (or
/// a `{"event":"status",…}` object under `--json`), so a mid-follow transition
/// like `-> needs_input` stands out from the message stream.
fn emit_follow_status(status: &str, color: bool, as_json: bool) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    if as_json {
        let obj = serde_json::json!({ "event": "status", "status": status });
        if let Ok(line) = serde_json::to_string(&obj) {
            let _ = writeln!(out, "{line}");
        }
    } else {
        let (glyph, label, sgr) = status_style(status);
        let line = paint(color, sgr, &format!("── {glyph} {label} ──"));
        let _ = writeln!(out, "\n{line}");
    }
    let _ = out.flush();
}

/// Join JSONL lines into a single newline-terminated block (empty stays empty),
/// so the whole archive rides the same [`emit`] path as the rendered transcript.
fn join_lines(lines: Vec<String>) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Write `output` to stdout, or through a pager when `use_pager`.
///
/// The pager command comes from `$AGENTIUM_PAGER`, then `$PAGER`, else the
/// built-in default `less -R` (`-R` so the rendered ANSI color survives —
/// answering "keep color when it's long"). If the pager can't be spawned (not
/// installed, empty command), we fall back to printing plainly rather than
/// failing. A broken pipe (the user quit the pager early) is ignored.
pub(crate) fn emit(output: &str, use_pager: bool) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if !use_pager {
        print!("{output}");
        return Ok(());
    }

    let pager = env::var("AGENTIUM_PAGER")
        .ok()
        .or_else(|| env::var("PAGER").ok())
        .unwrap_or_else(|| "less -R".to_string());
    let mut parts = pager.split_whitespace();
    let Some(program) = parts.next() else {
        print!("{output}");
        return Ok(());
    };

    let child = Command::new(program)
        .args(parts)
        .stdin(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        // No usable pager (e.g. `less` absent) — degrade to a plain print.
        Err(_) => {
            print!("{output}");
            return Ok(());
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        // Ignore the write result: a pager the user quits early closes the pipe,
        // and that EPIPE is expected, not an error worth surfacing.
        let _ = stdin.write_all(output.as_bytes());
    }
    let _ = child.wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_lines_terminates_and_keeps_empty() {
        assert_eq!(join_lines(vec![]), "");
        assert_eq!(join_lines(vec!["a".into(), "b".into()]), "a\nb\n");
    }
}
