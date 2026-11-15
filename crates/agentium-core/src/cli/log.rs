//! `nd agentium log` — one session's conversation, filtered, or the claude-code
//! JSONL reconstructed from its archive.

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;

use super::transcript::{render_messages, MessageFilter};
use crate::messages::Message;
use crate::session_loader::load_session_messages_for_author;
use crate::session_reconstructor::reconstruct_jsonl_lines;

/// What `nd agentium log` read for one session.
pub enum Transcript {
    /// The kind-1988 conversation, in [`EventOrder`], narrowed by a
    /// [`MessageFilter`].
    ///
    /// [`EventOrder`]: crate::session_loader::EventOrder
    Messages(Vec<Message>),
    /// `--jsonl`: the claude-code JSONL reconstructed from the lossless
    /// kind-1989 archive, one line per entry.
    Jsonl(Vec<String>),
}

impl Transcript {
    /// Resolve `selector` across the live *and* tombstoned sets (so a deleted
    /// session's transcript still reads) and read its transcript.
    ///
    /// The messages come from [`load_session_messages_for_author`], which
    /// already orders them by [`EventOrder`] (millisecond wall-clock) and
    /// applies the single shared note→message mapping; `filter` only slices
    /// and hides, never re-sorts (`seq` was refactored out of the display
    /// order).
    ///
    /// `jsonl` reads the archive instead. That is a *different* ordering axis
    /// than the display stream: [`reconstruct_jsonl_lines`] sorts by `seq` to
    /// reproduce the original claude-code line order, which is correct there.
    /// It is not author-scoped, but the cache only unwraps this identity's own
    /// PNS envelopes, so it only ever sees our own kind-1989 events. `filter`
    /// doesn't apply to it.
    ///
    /// [`EventOrder`]: crate::session_loader::EventOrder
    pub fn load(
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        selector: Option<&str>,
        filter: &MessageFilter,
        jsonl: bool,
    ) -> Result<Self, String> {
        let state = super::resolve_selected(ndb, txn, author, selector)?;
        if jsonl {
            return reconstruct_jsonl_lines(ndb, txn, &state.claude_session_id)
                .map(Transcript::Jsonl)
                .map_err(|e| e.to_string());
        }
        let messages =
            load_session_messages_for_author(ndb, txn, author, &state.claude_session_id).messages;
        Ok(Transcript::Messages(
            filter.select(&messages).into_iter().cloned().collect(),
        ))
    }

    /// The `log --json` value: each message's role-tagged
    /// [`Message::to_json`], or each JSONL line parsed (a line that isn't
    /// JSON stays a string).
    pub fn json(&self) -> serde_json::Value {
        match self {
            Transcript::Messages(messages) => messages.iter().map(Message::to_json).collect(),
            Transcript::Jsonl(lines) => lines
                .iter()
                .map(|line| {
                    serde_json::from_str(line)
                        .unwrap_or_else(|_| serde_json::Value::String(line.clone()))
                })
                .collect(),
        }
    }

    /// The `log` text: the rendered transcript (`color` paints its role
    /// headers), or the JSONL raw, newline-terminated.
    pub fn render(&self, color: bool) -> String {
        match self {
            Transcript::Messages(messages) => {
                let refs: Vec<&Message> = messages.iter().collect();
                render_messages(&refs, color)
            }
            Transcript::Jsonl(lines) => join_lines(lines),
        }
    }
}

/// Join JSONL lines into a single newline-terminated block (empty stays empty).
fn join_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_lines_terminates_and_keeps_empty() {
        assert_eq!(join_lines(&[]), "");
        assert_eq!(join_lines(&["a".into(), "b".into()]), "a\nb\n");
    }
}
