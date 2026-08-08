//! Human-friendly card references over the shared [`wordid`] crate.
//!
//! The 33-bit BIP-39 encoding is identical across headway cards, notebook nodes,
//! and agentium sessions, so it lives once in the `wordid` crate. This module adds
//! the headway-specific URI scheme on top: a full card reference is
//! `headway:<board>/<word-id>`, e.g. `headway:dave/maple-river-canyon`.
//!
//! A URI scheme (not the old `#` sigil) so the ref survives nostrdb's tokenizer
//! intact — where a mid-word `#` would be split off as a hashtag, shattering the
//! ref across content blocks — and needs no shell quoting. The board segment
//! scopes the fold and self-routes (see [`headway_cli`'s `ref_board`] and
//! [`crate::event::resolve_card`]); a card is always board-scoped, so the segment
//! is mandatory and a bare word-id is never a reference.

pub use wordid::{SEP, encode, three_words_end};

/// The URI scheme that precedes a full card reference. Aligned with the inline
/// parser's id (`notedeck_headway::HeadwayRefParser::id()` returns `"headway"`).
pub const SCHEME: &str = "headway";

/// The full, self-routing card reference: `headway:<board>/<word-id>`, e.g.
/// `headway:dave/maple-river-canyon`. This is what copy/paste surfaces render and
/// what [`parse_ref`] / [`crate::event::resolve_card`] accept back.
pub fn card_ref(board: &str, id: &[u8; 32]) -> String {
    format!("{SCHEME}:{board}/{}", encode(id))
}

/// Split the board slug and word-id out of a card reference, accepting the full
/// `headway:<board>/<word-id>` form or the scheme-less `<board>/<word-id>`
/// shorthand (for interactive resolver input where the app is implied). Returns
/// `None` for a bare word-id, a hex id, or a non-slug board — the board segment
/// is what distinguishes a reference from a plain search term.
///
/// The word-id shape is *not* validated here; resolution re-encodes each candidate
/// and matches (see [`crate::event::resolve_card_by_wordid`]). Returned slices
/// borrow from `sel`.
pub fn parse_ref(sel: &str) -> Option<(&str, &str)> {
    let rest = sel
        .strip_prefix(SCHEME)
        .and_then(|r| r.strip_prefix(':'))
        .unwrap_or(sel);
    let (board, words) = rest.split_once('/')?;
    let slug_ok = !board.is_empty() && board.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    (slug_ok && !words.is_empty()).then_some((board, words))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_ref_round_trips_through_parse_ref() {
        let id = [0x12u8; 32];
        let words = encode(&id);
        let r = card_ref("dave", &id);
        assert_eq!(r, format!("headway:dave/{words}"));
        assert_eq!(parse_ref(&r), Some(("dave", words.as_str())));
    }

    #[test]
    fn parse_ref_accepts_scheme_and_scheme_less() {
        assert_eq!(
            parse_ref("headway:commerce/maple-river-canyon"),
            Some(("commerce", "maple-river-canyon"))
        );
        assert_eq!(
            parse_ref("commerce/maple-river-canyon"),
            Some(("commerce", "maple-river-canyon"))
        );
        // A hyphenated board slug (e.g. a year-prefixed board) is a valid segment.
        assert_eq!(
            parse_ref("2024-goals/maple-river-canyon"),
            Some(("2024-goals", "maple-river-canyon"))
        );
    }

    #[test]
    fn parse_ref_rejects_non_references() {
        // A bare word-id is never a reference — no board segment.
        assert_eq!(parse_ref("maple-river-canyon"), None);
        // A hex id has no `/`.
        assert_eq!(parse_ref("deadbeef"), None);
        // Empty segments.
        assert_eq!(parse_ref("/maple-river-canyon"), None);
        assert_eq!(parse_ref("dave/"), None);
        // A board slug with a space isn't a slug.
        assert_eq!(parse_ref("not a slug/a-b-c"), None);
    }
}
