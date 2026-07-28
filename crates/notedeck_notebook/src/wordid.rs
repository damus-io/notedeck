//! Human-friendly node references.
//!
//! A node's real identity is its 32-byte nostr event id: secure and
//! decentralized, but not something a human can say while arranging a canvas. We
//! can't mint a dense sequential number (`node-42`) instead, because that needs a
//! single coordinator to hand numbers out — and the notebook is offline-first, so
//! two of your own devices editing while partitioned would both mint the same
//! number. That's Zooko's triangle: a name can be at most two of
//! {human-meaningful, secure, decentralized}, and without global consensus we
//! can't have all three.
//!
//! So rather than make the id sequential, we make the *hash* sayable: encode the
//! leading 33 bits of the event id as three BIP-39 words. The CLI prefixes the
//! canvas id and an `@` sigil, giving ids like `notebook@maple-river-canyon`.
//!
//! This is deliberately the *same* encoding as [`headway::wordid`] — same
//! wordlist, same 33-bit slice — so the implementations stay easy to reason about
//! side by side. The two are kept apart not by the words but by the sigil at the
//! render layer: headway renders `<board>#<words>`, the notebook renders
//! `<canvas>@<words>`, so a reference is never mistaken for the other system's.
//!
//! 3 words × 11 bits = 33 bits (~8.5 billion), collision-free well past any
//! realistic canvas (the birthday bound puts even-odds past ~109,000 nodes).
//! Resolution is by re-encoding each node and matching, exactly like a git short
//! hash; a full hex id always resolves too, so a reference written down today
//! never becomes invalid.

use bip39::Language;

/// Separator between words in a rendered id.
const SEP: char = '-';

/// Render the leading 33 bits of an event id as three BIP-39 words joined by
/// `-`, e.g. `maple-river-canyon`. The canvas id and `@` sigil are *not*
/// included; the CLI prefixes them when rendering for a human.
pub fn encode(id: &[u8; 32]) -> String {
    let words = Language::English.word_list();
    let [a, b, c] = indices(id);
    format!("{}{SEP}{}{SEP}{}", words[a], words[b], words[c])
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
}
