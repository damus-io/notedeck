//! NIP-SNS: Shared Note Storage
//!
//! The sealed, multi-writer sibling of [`crate::pns`]. A shared 32-byte
//! `team_root` secret derives a channel keypair that every member holds. Any
//! member publishes a kind-1081 **envelope** (signed by the shared team
//! keypair, symmetric-encrypted with the shared nip44 key) wrapping a kind-13
//! **seal** (signed by the member, ECDH-encrypted to the team keypair) wrapping
//! the **rumor** — the actual board action, carrying the member's real pubkey.
//! The seal is what proves authorship even though every member can decrypt.
//!
//! This module is the *publish + derivation* half only. nostrdb owns the ingest
//! auto-unwrap (register a `team_root`, match kind-1081 by the team pubkey,
//! symmetric-decrypt the envelope, then reuse the existing seal peel), so the
//! running app never unwraps an envelope itself — it queries the rumors nostrdb
//! has already stored. The derivation and envelope byte format defined here MUST
//! match the nostrdb C side — see `docs/nip-sns-sealed-shared-storage.md`. The
//! tests below re-implement that ingest peel purely to round-trip the format.
//!
//! Key derivation:
//!   team_keypair   = derive_secp256k1_keypair(team_root)
//!   team_nip44_key = hkdf_extract(ikm=team_root, salt="nip44-v2")

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use hkdf::Hkdf;
use nostr::key::PublicKey;
use nostr::nips::nip44;
use nostr::nips::nip44::v2::{self, ConversationKey};
use nostrdb::{Note, NoteBuilder};
use sha2::Sha256;

use crate::{FullKeypair, Pubkey};

/// Kind number for the SNS envelope (outer wrapper). Provisional, per the spec.
pub const SNS_ENVELOPE_KIND: u32 = 1081;
/// Kind number for the NIP-59 seal (reused verbatim).
pub const SEAL_KIND: u16 = 13;
/// Kind number for the SNS key-share rumor (delivered gift-wrapped).
pub const KEYSHARE_KIND: u32 = 1082;

/// Salt for deriving the envelope's NIP-44 symmetric key from `team_root`.
const NIP44_SALT: &[u8] = b"nip44-v2";

/// Everything derived from a `team_root` needed to publish to and read a
/// channel.
pub struct SnsKeys {
    /// Shared channel keypair. Its pubkey **is** the channel (members subscribe
    /// to envelopes authored by it, and it signs them); its secret decrypts
    /// seals (as the ECDH recipient).
    pub team_keypair: FullKeypair,
    /// Symmetric NIP-44 conversation key for the outer envelope layer.
    pub envelope_key: ConversationKey,
}

/// Derive all SNS channel keys from a 32-byte `team_root`.
///
/// Deterministic: the same root always yields the same channel keypair and
/// envelope key. Returns `None` only if `team_root` is not a valid secp256k1
/// secret key (negligible for a random root, but the root arrives over the wire
/// so it is not assumed valid). This derivation must byte-match the nostrdb C
/// `ndb_ingester_add_sns_key`.
pub fn derive_sns_keys(team_root: &[u8; 32]) -> Option<SnsKeys> {
    let team_keypair = FullKeypair::from_secret_bytes(team_root)?;
    let nip44_key = hkdf_extract(team_root, NIP44_SALT);
    let envelope_key = ConversationKey::new(nip44_key);
    Some(SnsKeys {
        team_keypair,
        envelope_key,
    })
}

/// Wrap a rumor (the inner board action as event JSON, carrying the member's
/// real pubkey) into a signed kind-1081 SNS envelope ready to publish.
///
/// `member` is the authoring member's real keypair — the seal is signed by it,
/// so the rumor nostrdb ingests is attributable to that pubkey. `created_at`
/// stamps both wrapper layers (the rumor keeps its own timestamp inside
/// `rumor_json`). Returns `None` if encryption or note construction fails.
pub fn wrap_rumor(
    keys: &SnsKeys,
    member: &FullKeypair,
    rumor_json: &str,
    created_at: u64,
) -> Option<Note<'static>> {
    let team_pk = nostrcrate_pk(&keys.team_keypair.pubkey)?;

    // Seal (kind 13): signed by the member, ECDH-encrypted member ⇄ team pubkey.
    let sealed_rumor =
        nip44::encrypt(&member.secret_key, &team_pk, rumor_json, nip44::Version::V2).ok()?;
    let seal_json = NoteBuilder::new()
        .kind(SEAL_KIND as u32)
        .content(&sealed_rumor)
        .created_at(created_at)
        .sign(&member.secret_key.secret_bytes())
        .build()?
        .json()
        .ok()?;

    // Envelope (kind 1081): signed by the team keypair, symmetric-encrypted with
    // the shared envelope key. No routing tags — the channel pubkey is the only
    // addressing, and it is unguessable (derived from the secret root).
    let payload = v2::encrypt_to_bytes(&keys.envelope_key, &seal_json).ok()?;
    let content = BASE64.encode(payload);
    NoteBuilder::new()
        .kind(SNS_ENVELOPE_KIND)
        .content(&content)
        .created_at(created_at)
        .sign(&keys.team_keypair.secret_key.secret_bytes())
        .build()
}

/// HMAC-SHA256(key=salt, msg=ikm) → 32-byte key (HKDF-Extract only, matching the
/// nostrdb C derivation). Shared with [`crate::pns`]'s scheme.
fn hkdf_extract(ikm: &[u8; 32], salt: &[u8]) -> [u8; 32] {
    let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), ikm);
    let mut out = [0u8; 32];
    out.copy_from_slice(&prk);
    out
}

/// Convert an enostr [`Pubkey`] (32-byte x-only) to a `nostr` [`PublicKey`].
fn nostrcrate_pk(pk: &Pubkey) -> Option<PublicKey> {
    PublicKey::from_slice(pk.bytes()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::util::JsonUtil;
    use nostr::{Event, Kind};

    fn test_root() -> [u8; 32] {
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = 0x22;
        root
    }

    /// Re-implements nostrdb's ingest peel (symmetric envelope decrypt → verify
    /// and ECDH-decrypt the kind-13 seal) so a [`wrap_rumor`] can be round-tripped
    /// in-process. Production never does this — nostrdb unwraps on ingest and the
    /// app only ever sees the stored rumor. Returns the verified author and the
    /// inner rumor JSON.
    fn unwrap_envelope(keys: &SnsKeys, envelope: &Note) -> Option<(Pubkey, String)> {
        if envelope.kind() != SNS_ENVELOPE_KIND {
            return None;
        }
        let payload = BASE64.decode(envelope.content()).ok()?;
        let seal_bytes = v2::decrypt_to_bytes(&keys.envelope_key, &payload).ok()?;
        let seal = Event::from_json(String::from_utf8(seal_bytes).ok()?).ok()?;
        if seal.kind != Kind::from(SEAL_KIND) {
            return None;
        }
        seal.verify().ok()?;
        let rumor_json =
            nip44::decrypt(&keys.team_keypair.secret_key, &seal.pubkey, &seal.content).ok()?;
        Some((Pubkey::new(seal.pubkey.to_bytes()), rumor_json))
    }

    #[test]
    fn derive_is_deterministic() {
        let root = test_root();
        let a = derive_sns_keys(&root).expect("keys");
        let b = derive_sns_keys(&root).expect("keys");
        assert_eq!(a.team_keypair.pubkey, b.team_keypair.pubkey);
        assert_eq!(a.envelope_key.as_bytes(), b.envelope_key.as_bytes());
    }

    /// Fixed vector so the nostrdb C `ndb_ingester_add_sns_key` has a target to
    /// match. `team_root` = 0x11,0x00…0x00,0x22.
    #[test]
    fn derive_matches_fixed_vector() {
        let keys = derive_sns_keys(&test_root()).expect("keys");
        let expected_team_pubkey =
            "d6623502bcf67f6758e25080111ad9221181c33cfcba14d74dc9e3784ecfe1f7";
        assert_eq!(
            hex::encode(keys.team_keypair.pubkey.bytes()),
            expected_team_pubkey,
            "SNS team pubkey must match the nostrdb C derivation"
        );
    }

    #[test]
    fn wrap_unwrap_roundtrip_attributes_the_member() {
        let keys = derive_sns_keys(&test_root()).expect("keys");
        let member = FullKeypair::generate();
        let rumor = format!(
            r#"{{"kind":1621,"pubkey":"{}","content":"a card","tags":[],"created_at":42}}"#,
            member.pubkey.hex()
        );

        let envelope = wrap_rumor(&keys, &member, &rumor, 100).expect("envelope");
        assert_eq!(envelope.kind(), SNS_ENVELOPE_KIND);
        // Envelope is authored by the shared channel keypair, not the member.
        assert_eq!(envelope.pubkey(), keys.team_keypair.pubkey.bytes());

        let (author, rumor_json) = unwrap_envelope(&keys, &envelope).expect("unwrap");
        assert_eq!(rumor_json, rumor);
        // Authorship survives the wrap: attributed to the real member.
        assert_eq!(author, member.pubkey);
    }

    #[test]
    fn wrong_root_cannot_unwrap() {
        let keys = derive_sns_keys(&test_root()).expect("keys");
        let member = FullKeypair::generate();
        let envelope = wrap_rumor(&keys, &member, r#"{"kind":1}"#, 1).expect("envelope");

        let mut other = test_root();
        other[0] = 0x33;
        let other_keys = derive_sns_keys(&other).expect("keys");
        assert!(unwrap_envelope(&other_keys, &envelope).is_none());
    }
}
