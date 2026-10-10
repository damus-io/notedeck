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

/// The next `headway:<board>/<word-id>` in a run of prose, as a byte range into
/// `text`, or `None` if it holds none.
///
/// The `find` half of the inline reference grammar, shared by the app's inline
/// parser (`notedeck_headway::HeadwayRefParser`) and anything else that scans
/// text for cards, such as Dave's session inspector. The scheme is **required**:
/// the scheme-less `<board>/<word-id>` shorthand [`parse_ref`] accepts is too
/// ambiguous against ordinary `word/word` prose to scan for. It must begin at a
/// slug boundary, so `myheadway:` is not a match on the tail of a longer word.
///
/// Syntax only, and allocation-free, since it runs while drawing a frame: a
/// match may name no real card.
pub fn find_ref(text: &str) -> Option<std::ops::Range<usize>> {
    // The scheme with its separator.
    const PREFIX: &str = "headway:";
    let bytes = text.as_bytes();
    let mut search = 0;
    while let Some(rel) = text[search..].find(PREFIX) {
        let start = search + rel;
        // Continue past this scheme on the next iteration regardless of outcome.
        search = start + PREFIX.len();

        if start > 0 && is_slug_byte(bytes[start - 1]) {
            continue;
        }

        // Board slug: a run of slug bytes after the scheme, then a single `/`.
        let slug_start = search;
        let mut i = slug_start;
        while i < bytes.len() && is_slug_byte(bytes[i]) {
            i += 1;
        }
        if i == slug_start || i >= bytes.len() || bytes[i] != b'/' {
            continue;
        }

        // Three dash-joined words right after the `/`.
        if let Some(end) = three_words_end(bytes, i + 1) {
            return Some(start..end);
        }
    }
    None
}

/// A byte is part of a board slug: lowercase ascii, a digit, or `-` (see
/// `store::board_slug`). Uppercase is excluded, since slugs are lowercase.
fn is_slug_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
}

/// A card reference split by [`parse_ref`], both halves borrowing the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardRef<'a> {
    /// The board slug, e.g. `"dave"`.
    pub board: &'a str,
    /// The card's word-id, e.g. `"maple-river-canyon"`; not validated.
    pub words: &'a str,
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
///
/// A thin adapter over the shared [`wordid::WordIdRef`] grammar: a headway card is
/// always board-scoped, so only a [`Scoped`](wordid::WordIdRef::Scoped) body is a
/// card reference — a bare word-id resolves to `None`.
pub fn parse_ref(sel: &str) -> Option<CardRef<'_>> {
    match wordid::WordIdRef::parse_uri_or_body(sel, SCHEME)? {
        wordid::WordIdRef::Scoped { scope, words } => Some(CardRef {
            board: scope,
            words,
        }),
        // A bare word-id carries no board segment, so it's never a card reference.
        wordid::WordIdRef::Bare { .. } => None,
    }
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
        assert_eq!(
            parse_ref(&r),
            Some(CardRef {
                board: "dave",
                words: &words
            })
        );
    }

    #[test]
    fn parse_ref_accepts_scheme_and_scheme_less() {
        assert_eq!(
            parse_ref("headway:commerce/maple-river-canyon"),
            Some(CardRef {
                board: "commerce",
                words: "maple-river-canyon"
            })
        );
        assert_eq!(
            parse_ref("commerce/maple-river-canyon"),
            Some(CardRef {
                board: "commerce",
                words: "maple-river-canyon"
            })
        );
        // A hyphenated board slug (e.g. a year-prefixed board) is a valid segment.
        assert_eq!(
            parse_ref("2024-goals/maple-river-canyon"),
            Some(CardRef {
                board: "2024-goals",
                words: "maple-river-canyon"
            })
        );
    }

    #[test]
    fn find_ref_matches_a_whole_ref_in_prose() {
        let s = "see headway:dave/maple-river-canyon, then";
        assert_eq!(&s[find_ref(s).unwrap()], "headway:dave/maple-river-canyon");
        // The first of two, and one past a near miss.
        let s = "headway:dave/a-b and headway:2024-goals/one-two-three";
        assert_eq!(&s[find_ref(s).unwrap()], "headway:2024-goals/one-two-three");
        // No scheme, a glued scheme, no slug, a fourth word.
        assert_eq!(find_ref("dave/maple-river-canyon"), None);
        assert_eq!(find_ref("myheadway:dave/maple-river-canyon"), None);
        assert_eq!(find_ref("headway:/maple-river-canyon"), None);
        assert_eq!(find_ref("headway:dave/one-two-three-four"), None);
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
