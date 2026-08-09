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

/// Re-export the shared `find`-half scanner so the inline `agentium:` reference
/// parser recognises a rendered word-id inside prose without depending on the
/// `wordid` crate directly (it reaches it through this module, next to [`SCHEME`]).
pub use wordid::three_words_end;

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

/// The word-id out of a **free-text** session reference: strips the required
/// `agentium:` scheme and returns the trailing word-id, or `None` for a bare
/// word-id or a plain id (no scheme). The scheme is what distinguishes a
/// reference from ordinary `word-word-word` prose, so it is mandatory here —
/// unlike [`resolve_session`](crate::session_loader::resolve_session), the
/// interactive resolver, which accepts the scheme optionally because the app is
/// implied by context.
///
/// The word-id shape is *not* validated; resolution re-encodes each candidate
/// session's id and matches (git-short-hash style). Single-segment: a session's
/// identity carries no scope, so only a [`Bare`](wordid::WordIdRef::Bare) body is
/// a session reference. The returned slice borrows from `sel`.
pub fn parse_ref(sel: &str) -> Option<&str> {
    match wordid::WordIdRef::parse_uri(sel, SCHEME)? {
        wordid::WordIdRef::Bare { words } => Some(words),
        wordid::WordIdRef::Scoped { .. } => None,
    }
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

    #[test]
    fn session_ref_round_trips_through_parse_ref() {
        let id = "3f0e-uuid-like-string";
        let r = session_ref(id);
        assert_eq!(parse_ref(&r), Some(encode_session_id(id).as_str()));
    }

    #[test]
    fn parse_ref_requires_the_scheme() {
        assert_eq!(
            parse_ref("agentium:maple-river-canyon"),
            Some("maple-river-canyon")
        );
        // A bare word-id is not a reference — no scheme.
        assert_eq!(parse_ref("maple-river-canyon"), None);
        // A scoped body has no meaning for a session (single-segment identity).
        assert_eq!(parse_ref("agentium:work/maple-river-canyon"), None);
        // An empty word-id after the scheme is not a reference.
        assert_eq!(parse_ref("agentium:"), None);
    }
}
