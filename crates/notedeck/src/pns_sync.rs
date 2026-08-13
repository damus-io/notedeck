//! Host-level PNS (private note session) sync helpers.
//!
//! A PNS note is a kind-1080 envelope whose content is an account-private inner
//! note (notebook longform, dave session state, a headway board-pref) NIP-44
//! encrypted to, and signed by, an *unlinkable* keypair HKDF-derived from the
//! account secret (`enostr::pns::derive_pns_keys`). Relays only ever see the opaque
//! envelope; nostrdb — seeded with the account key at sign-in — auto-unwraps it on
//! ingest so the inner note becomes queryable by its own kind/author.
//!
//! Because the derived PNS pubkey is a pure function of the account secret, one
//! subscription for `[kinds:1080, authors:pns_pubkey]` pulls back *every* app's
//! private notes for the account. These helpers give the host
//! `PrivateSyncDriver` that filter centrally, replacing each app's bespoke PNS
//! sub. Unlike dave's previous windowed sub this is full-history (no time bound) —
//! the negentropy reconcile only transfers what the device is actually missing.

use enostr::Pubkey;
use nostrdb::Filter;

/// The unlinkable pubkey that signs (and thus authors) the account's kind-1080 PNS
/// envelopes, derived from the account secret. Every device for the same account
/// derives the same pubkey, so this names the account's private-note stream.
pub fn pns_author(account_secret: &[u8; 32]) -> Pubkey {
    enostr::pns::derive_pns_keys(account_secret).keypair.pubkey
}

/// Filter for the account's kind-1080 PNS envelope stream, authored by
/// [`pns_author`]. Full-history: no `since`/time window — negentropy backfills only
/// the envelopes this device lacks, so bounding the window would just risk dropping
/// older private notes for no bandwidth saving.
pub fn pns_envelope_filter(pns_pubkey: &Pubkey) -> Filter {
    Filter::new()
        .kinds([enostr::pns::PNS_KIND as u64])
        .authors([pns_pubkey.bytes()])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derived PNS pubkey is deterministic in the account secret (so every
    /// device names the same private-note stream), and the filter targets that
    /// pubkey's kind-1080 envelopes.
    #[test]
    fn pns_author_is_deterministic_and_filter_targets_it() {
        let secret = [7u8; 32];
        let a = pns_author(&secret);
        let b = pns_author(&secret);
        assert_eq!(a, b, "same secret must derive the same PNS author");

        let other = pns_author(&[9u8; 32]);
        assert_ne!(a, other, "a different secret derives a different author");

        let json = pns_envelope_filter(&a).json().expect("filter json");
        assert!(json.contains("1080"), "filter targets the PNS kind");
        assert!(
            json.contains(&a.hex()),
            "filter targets the derived PNS author"
        );
    }
}
