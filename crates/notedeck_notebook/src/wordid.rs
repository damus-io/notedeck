//! Human-friendly node references over the shared [`wordid`] crate.
//!
//! The 33-bit BIP-39 encoding is identical across notebook nodes, headway cards,
//! and agentium sessions, so it lives once in the `wordid` crate. This module adds
//! the notebook-specific URI scheme on top: a full node reference is
//! `notebook:<word-id>`, e.g. `notebook:maple-river-canyon`.
//!
//! Single-segment, like agentium sessions and unlike headway's two-segment
//! `headway:<board>/<word-id>`. A node's identity is its 32-byte event id; the
//! word-id is a global rendering of it, and the canvas a node currently lives on
//! is *not* part of that identity — node resolution ignores it, the CLI scopes by
//! `--canvas`, and longform notes aren't canvas-scoped at all. So the `notebook:`
//! scheme is the whole reference marker; a bare word-id is never a reference.
//!
//! A URI scheme rather than the old `@` sigil: consistent with the sibling
//! schemes (`nostr:`, `headway:`) so the reference registry dispatches by scheme,
//! and — like them — surviving nostrdb's tokenizer whole inside note content.

pub use wordid::{SEP, encode};

/// The URI scheme that precedes a node word-id in a full reference.
pub const SCHEME: &str = "notebook";

/// The full, sayable node reference: `notebook:<word-id>`, e.g.
/// `notebook:maple-river-canyon`. What copy/paste surfaces render and what
/// [`parse_ref`] accepts back.
pub fn node_ref(id: &[u8; 32]) -> String {
    format!("{SCHEME}:{}", encode(id))
}

/// The word-id out of a node reference: strips the required `notebook:` scheme and
/// returns the trailing word-id. `None` for a bare word-id or a hex id — the
/// scheme is what distinguishes a reference from a plain selector, so it is
/// mandatory (there is no board/canvas segment to serve that role, as there is in
/// headway's scheme-less `board/word-id` shorthand).
///
/// The word-id shape is *not* validated here; resolution re-encodes each candidate
/// and matches. The returned slice borrows from `sel`.
pub fn parse_ref(sel: &str) -> Option<&str> {
    let words = sel.strip_prefix(SCHEME)?.strip_prefix(':')?;
    (!words.is_empty()).then_some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_ref_round_trips_through_parse_ref() {
        let id = [0x34u8; 32];
        let words = encode(&id);
        let r = node_ref(&id);
        assert_eq!(r, format!("notebook:{words}"));
        assert_eq!(parse_ref(&r), Some(words.as_str()));
    }

    #[test]
    fn parse_ref_requires_the_scheme() {
        assert_eq!(
            parse_ref("notebook:maple-river-canyon"),
            Some("maple-river-canyon")
        );
        // A bare word-id is not a reference — no scheme.
        assert_eq!(parse_ref("maple-river-canyon"), None);
        // A hex id has no scheme either.
        assert_eq!(parse_ref("deadbeef"), None);
        // An empty word-id after the scheme is not a reference.
        assert_eq!(parse_ref("notebook:"), None);
    }
}
