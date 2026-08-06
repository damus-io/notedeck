//! Human-friendly session references (`agentium:word-word-word`).
//!
//! The 33-bit BIP-39 encoding is shared with headway cards and notebook nodes,
//! so it lives once in the [`wordid`] crate; this module adds only the
//! agentium-specific URI scheme and the string-keyed input.
//!
//! Unlike the older sibling refs (`board#…`, `canvas@…`), agentium is greenfield
//! with no legacy references to stay compatible with, so it ships URI-native
//! from the start: `agentium:<word-id>`. The `:` scheme separator survives
//! nostrdb's tokenizer intact (where a mid-word `#` would be split off as a
//! hashtag) and needs no shell quoting — the direction the sibling refs are
//! migrating toward.
//!
//! One deliberate difference from the siblings: they encode a card/node's
//! 32-byte nostr *event id*. We can't do that here, because a session's
//! kind-31988 state event is **replaceable** — its event id changes on every
//! status/title update — so a word-id built from it would drift as the session
//! runs. The d-tag (`claude_session_id`), by contrast, is fixed for the
//! session's whole life. It isn't 32 bytes, so [`wordid::encode_str`] SHA-256's
//! it first. The result is stable, and (like a git short hash) resolved by
//! re-encoding each candidate and matching — see
//! [`resolve_session`](crate::session_loader::resolve_session).

/// The URI scheme that precedes a session word-id in a full reference, e.g.
/// `agentium:maple-river-canyon`. A scheme (not headway's `#` sigil) so the ref
/// survives nostrdb tokenization and needs no shell quoting.
pub const SCHEME: &str = "agentium";

/// The full, sayable reference for a session id, e.g.
/// `agentium:maple-river-canyon`. This is what `list`/`show` print and what
/// [`resolve_session`](crate::session_loader::resolve_session) accepts back.
pub fn session_ref(session_id: &str) -> String {
    format!("{SCHEME}:{}", encode_session_id(session_id))
}

/// Render a session id (its stable kind-31988 d-tag) as three BIP-39 words.
/// The id is a string, not a 32-byte event id, so it's hashed first — see
/// [`wordid::encode_str`].
pub fn encode_session_id(session_id: &str) -> String {
    wordid::encode_str(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ref_is_slug_prefixed_and_stable() {
        let id = "3f0e-uuid-like-string";
        assert_eq!(
            session_ref(id),
            format!("agentium:{}", encode_session_id(id))
        );
        assert_eq!(encode_session_id(id), encode_session_id(id));
        assert_ne!(
            encode_session_id(id),
            encode_session_id("a-different-session")
        );
    }
}
