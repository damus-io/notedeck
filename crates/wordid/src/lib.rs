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
}
