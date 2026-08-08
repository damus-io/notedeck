//! Human-friendly ids shared across notedeck ref systems.
//!
//! A nostr entity's real identity is its 32-byte event id: secure and
//! decentralized, but not something a human can say in a commit message or
//! chat. We can't mint a dense sequential number (`HEADWAY-42`) instead, because
//! that needs a single coordinator to hand numbers out — and these systems are
//! offline-first, so two of your own devices editing while partitioned would
//! both mint the same number. That's Zooko's triangle: a name can be at most two
//! of {human-meaningful, secure, decentralized}, and without global consensus we
//! can't have all three.
//!
//! So rather than make the id sequential, we make the *hash* sayable: encode the
//! leading 33 bits of the id as three BIP-39 words. Callers wrap their own URI
//! scheme around it (`headway:<board>/…`, `notebook:…`, `agentium:…`), giving
//! references like `headway:dave/maple-river-canyon`. This keeps the secure +
//! decentralized corners (it's just a rendering of the id) and claws back most of
//! the human-meaningful one.
//!
//! 3 words × 11 bits = 33 bits (~8.5 billion), collision-free well past any
//! realistic number of cards/nodes/sessions. Resolution is by re-encoding each
//! candidate and matching, exactly like a git short hash; a full hex id always
//! resolves too, so a reference written down today never becomes invalid.
//!
//! Two entry points:
//! - [`encode`] for entities keyed by a 32-byte event id (headway cards,
//!   notebook nodes).
//! - [`encode_str`] for entities keyed by an opaque *string* id (an agentium
//!   session's replaceable-event d-tag), which is SHA-256'd to 32 bytes first.

use bip39::Language;
use sha2::{Digest, Sha256};

/// Separator between words in a rendered id.
pub const SEP: char = '-';

/// Render the leading 33 bits of a 32-byte id as three BIP-39 words joined by
/// `-`, e.g. `maple-river-canyon`. No slug is included; callers prefix their
/// own.
pub fn encode(id: &[u8; 32]) -> String {
    let words = Language::English.word_list();
    let [a, b, c] = indices(id);
    format!("{}{SEP}{}{SEP}{}", words[a], words[b], words[c])
}

/// Render a string id as three BIP-39 words. For ids that aren't 32-byte event
/// ids (e.g. a session's d-tag), SHA-256 the string to 32 bytes, then [`encode`]
/// the leading 33 bits of the digest. Stable for a given string.
pub fn encode_str(id: &str) -> String {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&Sha256::digest(id.as_bytes()));
    encode(&bytes)
}

/// The end index of a `word-word-word` run of *exactly three* lowercase-letter
/// (BIP-39-shaped) words beginning at `start` in `bytes`, or `None` if there isn't
/// one. Each word is a non-empty run of ASCII lowercase letters (`[a-z]+`) and the
/// three are joined by a single [`SEP`]; a fourth separated word is rejected, so a
/// longer hyphenated run isn't mistaken for a word id.
///
/// This is the `find`-half scanner every scheme parser (`headway:`, `notebook:`,
/// `agentium:`) shares to recognise a rendered id inside a run of prose: it works
/// on raw bytes with a `start` offset so a parser can scan right past its own
/// scheme/slug, and it allocates nothing (it runs on the per-frame render path).
///
/// It recognises the *shape* only — three word-shaped tokens — not that they are
/// valid BIP-39 or resolve to a real entity; a caller re-encodes candidates to
/// confirm (see [`encode`]).
pub fn three_words_end(bytes: &[u8], start: usize) -> Option<usize> {
    let sep = SEP as u8;
    let mut pos = start;
    for word in 0..3 {
        let word_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_lowercase() {
            pos += 1;
        }
        if pos == word_start {
            return None; // empty word (leading/double/trailing separator)
        }
        if word < 2 {
            // Require a single separator before each of the next two words.
            if pos < bytes.len() && bytes[pos] == sep {
                pos += 1;
            } else {
                return None; // fewer than three words
            }
        }
    }
    // A separator right after the third word means a fourth is coming: not a bare
    // three-word id.
    if pos < bytes.len() && bytes[pos] == sep {
        return None;
    }
    Some(pos)
}

/// A parsed word-id reference: the word-id, plus an optional scope segment that
/// precedes it. The two shapes every notedeck ref system uses collapse to this
/// one type — headway's board-scoped `<board>/<word-id>` is [`Scoped`], while a
/// notebook node or agentium session's bare `<word-id>` is [`Bare`] — so generic
/// code can accept both by matching one enum instead of each system hand-rolling
/// (and drifting) its own parser.
///
/// This models only the *body* of a reference — the part after any `scheme:`.
/// The scheme (`headway`, `notebook`, `agentium`) is a per-system marker, so it's
/// stripped by [`parse_uri`](Self::parse_uri) / [`parse_uri_or_body`](Self::parse_uri_or_body)
/// before the shared grammar runs. Slices borrow from the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordIdRef<'a> {
    /// `<scope>/<word-id>` — a scope slug, then the word-id (headway cards, whose
    /// board segment scopes the fold and self-routes).
    Scoped {
        /// The scope slug before the `/` (a headway board id).
        scope: &'a str,
        /// The word-id after the `/`.
        words: &'a str,
    },
    /// `<word-id>` — just the word-id, no scope (notebook nodes, agentium
    /// sessions, whose identity needs no scope).
    Bare {
        /// The word-id.
        words: &'a str,
    },
}

impl<'a> WordIdRef<'a> {
    /// The word-id (the `word-word-word` tail), regardless of scope.
    pub fn words(&self) -> &'a str {
        match self {
            Self::Scoped { words, .. } | Self::Bare { words } => words,
        }
    }

    /// The scope slug, if this reference carried one (headway), else `None`
    /// (notebook/agentium).
    pub fn scope(&self) -> Option<&'a str> {
        match self {
            Self::Scoped { scope, .. } => Some(scope),
            Self::Bare { .. } => None,
        }
    }

    /// Parse the *body* of a reference — the part after any `scheme:`. A body
    /// carrying a `<scope>/` prefix is [`Scoped`] (the scope must be non-empty and
    /// slug-shaped: ASCII alphanumeric or [`SEP`]); otherwise the whole body is a
    /// [`Bare`] word-id. Returns `None` for an empty word-id or a non-slug scope.
    ///
    /// The word-id itself is only checked non-empty here — its three-word shape is
    /// confirmed at *resolution* by re-encoding each candidate (like a git short
    /// hash), so a *partial* word-id (as the headway interactive prefix/substring
    /// search feeds in) still parses.
    pub fn parse_body(body: &'a str) -> Option<Self> {
        match body.split_once('/') {
            Some((scope, words)) => {
                let slug_ok = !scope.is_empty()
                    && scope.chars().all(|c| c.is_ascii_alphanumeric() || c == SEP);
                (slug_ok && !words.is_empty()).then_some(Self::Scoped { scope, words })
            }
            None => (!body.is_empty()).then_some(Self::Bare { words: body }),
        }
    }

    /// Parse a full URI `scheme:<body>`, requiring the exact `<scheme>:` prefix
    /// (the free-text path, where the scheme is what distinguishes a reference
    /// from ordinary prose). The scoped/bare distinction then comes from the body.
    pub fn parse_uri(sel: &'a str, scheme: &str) -> Option<Self> {
        let body = sel.strip_prefix(scheme)?.strip_prefix(':')?;
        Self::parse_body(body)
    }

    /// Parse with the `<scheme>:` prefix **optional** — the interactive resolver
    /// path (headway's `ref_board`, agentium's `resolve_session`), where the app
    /// is implied by context so a scheme-less body (`board/<word-id>`) is accepted
    /// alongside the full `scheme:board/<word-id>`.
    pub fn parse_uri_or_body(sel: &'a str, scheme: &str) -> Option<Self> {
        let body = sel
            .strip_prefix(scheme)
            .and_then(|r| r.strip_prefix(':'))
            .unwrap_or(sel);
        Self::parse_body(body)
    }
}

/// The three 11-bit word indices for an id: the 33 most-significant bits.
fn indices(id: &[u8; 32]) -> [usize; 3] {
    // Pull the first 5 bytes (40 bits) into the low end of a u64, then keep the
    // top 33 and slice them into three 11-bit groups.
    let bits = u64::from_be_bytes([0, 0, 0, id[0], id[1], id[2], id[3], id[4]]) >> 7;
    [
        ((bits >> 22) & 0x7ff) as usize,
        ((bits >> 11) & 0x7ff) as usize,
        (bits & 0x7ff) as usize,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All-zero id → word index 0 thrice; 0xff first bytes → index 2047.
    #[test]
    fn known_vectors() {
        assert_eq!(encode(&[0u8; 32]), "abandon-abandon-abandon");
        assert_eq!(encode(&[0xffu8; 32]), "zoo-zoo-zoo");
    }

    #[test]
    fn shape_and_determinism() {
        let id: [u8; 32] = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0]
            .iter()
            .copied()
            .cycle()
            .take(32)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let a = encode(&id);
        assert_eq!(a, encode(&id), "encoding is deterministic");
        assert_eq!(a.split(SEP).count(), 3, "three words");
    }

    /// The encoding only looks at the first 33 bits, so two ids that differ only
    /// after byte 5 collide — verify that's the *only* thing that matters.
    #[test]
    fn uses_leading_33_bits_only() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        a[5] = 0xff; // byte 5 onwards is ignored
        b[31] = 0x07;
        assert_eq!(encode(&a), encode(&b));

        b[0] = 0x80; // a difference inside the first 33 bits must change it
        assert_ne!(encode(&a), encode(&b));
    }

    /// `encode_str` hashes first, so it's deterministic and distinguishes
    /// different strings.
    #[test]
    fn encode_str_is_stable_and_distinct() {
        assert_eq!(encode_str("session-abc"), encode_str("session-abc"));
        assert_ne!(encode_str("session-abc"), encode_str("session-xyz"));
        assert_eq!(encode_str("x").split(SEP).count(), 3);
    }

    /// The shared `find`-half scanner matches exactly three dash-joined lowercase
    /// words, honours a start offset, and rejects the near-misses.
    #[test]
    fn three_words_end_matches_exactly_three() {
        let end = |s: &str| three_words_end(s.as_bytes(), 0);
        assert_eq!(end("maple-river-canyon"), Some(18));
        // Stops at the first non-word byte, returning the run's end.
        let s = "maple-river-canyon rest";
        assert_eq!(three_words_end(s.as_bytes(), 0), Some(18));
        // Honours the start offset, so a parser can scan right past its scheme.
        let s = "notebook:maple-river-canyon";
        assert_eq!(three_words_end(s.as_bytes(), 9), Some(s.len()));

        assert_eq!(end("maple-river"), None); // only two words
        assert_eq!(end("maple-river-canyon-extra"), None); // a fourth word
        assert_eq!(end("maple--river-canyon"), None); // empty middle word
        assert_eq!(end("-maple-river-canyon"), None); // leading separator
        assert_eq!(end("maple-river-canyon-"), None); // trailing separator
        assert_eq!(end("Maple-river-canyon"), None); // uppercase isn't a word byte
        assert_eq!(end(""), None);
    }

    /// `parse_body` splits a `<scope>/<word-id>` into [`WordIdRef::Scoped`] and a
    /// scope-less body into [`WordIdRef::Bare`], validating the scope is a
    /// non-empty slug while leaving the word-id shape to resolution.
    #[test]
    fn parse_body_splits_scope_from_bare() {
        // Scoped: a slug, a `/`, then the word-id.
        assert_eq!(
            WordIdRef::parse_body("commerce/maple-river-canyon"),
            Some(WordIdRef::Scoped {
                scope: "commerce",
                words: "maple-river-canyon"
            })
        );
        // Slugs may carry digits and dashes (a year-prefixed board).
        assert_eq!(
            WordIdRef::parse_body("2024-goals/maple-river-canyon"),
            Some(WordIdRef::Scoped {
                scope: "2024-goals",
                words: "maple-river-canyon"
            })
        );
        // Bare: no `/`, so the whole body is the word-id.
        assert_eq!(
            WordIdRef::parse_body("maple-river-canyon"),
            Some(WordIdRef::Bare {
                words: "maple-river-canyon"
            })
        );
        // A partial word-id (interactive prefix search) still parses — the
        // three-word shape is a resolution-time concern, not a parse-time one.
        assert_eq!(
            WordIdRef::parse_body("commerce/map"),
            Some(WordIdRef::Scoped {
                scope: "commerce",
                words: "map"
            })
        );
        assert_eq!(
            WordIdRef::parse_body("deadbeef"),
            Some(WordIdRef::Bare { words: "deadbeef" })
        );

        // Empty word-id after a scope, or an empty/whole-empty body: not a ref.
        assert_eq!(WordIdRef::parse_body("commerce/"), None);
        assert_eq!(WordIdRef::parse_body(""), None);
        // Empty scope before the `/`.
        assert_eq!(WordIdRef::parse_body("/maple-river-canyon"), None);
        // A non-slug scope (a space) is not a scoped ref.
        assert_eq!(WordIdRef::parse_body("not a slug/a-b-c"), None);
    }

    /// The accessors read the word-id and optional scope back out uniformly.
    #[test]
    fn wordidref_accessors() {
        let scoped = WordIdRef::parse_body("dave/red-green-blue").unwrap();
        assert_eq!(scoped.words(), "red-green-blue");
        assert_eq!(scoped.scope(), Some("dave"));

        let bare = WordIdRef::parse_body("red-green-blue").unwrap();
        assert_eq!(bare.words(), "red-green-blue");
        assert_eq!(bare.scope(), None);
    }

    /// `parse_uri` requires the exact `<scheme>:` prefix; without it there is no
    /// reference (the scheme is what separates a ref from prose in free text).
    #[test]
    fn parse_uri_requires_the_scheme() {
        assert_eq!(
            WordIdRef::parse_uri("headway:commerce/maple-river-canyon", "headway"),
            Some(WordIdRef::Scoped {
                scope: "commerce",
                words: "maple-river-canyon"
            })
        );
        assert_eq!(
            WordIdRef::parse_uri("notebook:maple-river-canyon", "notebook"),
            Some(WordIdRef::Bare {
                words: "maple-river-canyon"
            })
        );
        // A scheme-less body is rejected when the scheme is required.
        assert_eq!(
            WordIdRef::parse_uri("commerce/maple-river-canyon", "headway"),
            None
        );
        assert_eq!(WordIdRef::parse_uri("maple-river-canyon", "notebook"), None);
        // The scheme must sit right before the `:` — a bare word-id or a bare
        // scheme with no separator is not a reference.
        assert_eq!(WordIdRef::parse_uri("headway", "headway"), None);
        assert_eq!(WordIdRef::parse_uri("headwaycommerce", "headway"), None);
    }

    /// `parse_uri_or_body` accepts the same input with the scheme *optional*, the
    /// interactive-resolver path where the app is implied by context.
    #[test]
    fn parse_uri_or_body_scheme_is_optional() {
        // Both the full URI and the scheme-less shorthand parse identically.
        let full = WordIdRef::parse_uri_or_body("headway:commerce/maple-river-canyon", "headway");
        let short = WordIdRef::parse_uri_or_body("commerce/maple-river-canyon", "headway");
        assert_eq!(
            full,
            Some(WordIdRef::Scoped {
                scope: "commerce",
                words: "maple-river-canyon"
            })
        );
        assert_eq!(full, short);

        // A board literally named after the scheme is not eaten by the strip:
        // `headway/a-b-c` (scheme-less) keeps the board `headway`, and
        // `headway-team/a-b-c` keeps `headway-team`.
        assert_eq!(
            WordIdRef::parse_uri_or_body("headway/a-b-c", "headway"),
            Some(WordIdRef::Scoped {
                scope: "headway",
                words: "a-b-c"
            })
        );
        assert_eq!(
            WordIdRef::parse_uri_or_body("headway-team/a-b-c", "headway"),
            Some(WordIdRef::Scoped {
                scope: "headway-team",
                words: "a-b-c"
            })
        );
    }
}
