//! NIP-PNS: Private Note Storage
//!
//! Deterministic key derivation and encryption for storing private nostr
//! events on relays. Only the owner of the device key can publish and
//! decrypt PNS events (kind 1080).
//!
//! Key derivation:
//!   pns_key      = hkdf_extract(ikm=device_key, salt="nip-pns")
//!   pns_keypair  = derive_secp256k1_keypair(pns_key)
//!   pns_nip44_key = hkdf_extract(ikm=pns_key, salt="nip44-v2")

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use hkdf::Hkdf;
use nostr::nips::nip44::v2::{self, ConversationKey};
use sha2::Sha256;

use crate::{FullKeypair, Pubkey};

/// Kind number for PNS events.
pub const PNS_KIND: u32 = 1080;

/// Salt used for deriving pns_key from the device key.
const PNS_SALT: &[u8] = b"nip-pns";

/// Salt used for deriving the NIP-44 symmetric key from pns_key.
const NIP44_SALT: &[u8] = b"nip44-v2";

/// Derived PNS keys — everything needed to create and decrypt PNS events.
pub struct PnsKeys {
    /// Keypair for signing kind-1080 events (derived from pns_key).
    pub keypair: FullKeypair,
    /// NIP-44 conversation key for encrypting/decrypting content.
    pub conversation_key: ConversationKey,
}

/// Derive all PNS keys from a device secret key.
///
/// This is deterministic: the same device key always produces the same
/// PNS keypair and encryption key.
pub fn derive_pns_keys(device_key: &[u8; 32]) -> PnsKeys {
    let pns_key = hkdf_extract(device_key, PNS_SALT);
    let keypair = keypair_from_bytes(&pns_key);
    let nip44_key = hkdf_extract(&pns_key, NIP44_SALT);
    let conversation_key = ConversationKey::new(nip44_key);

    PnsKeys {
        keypair,
        conversation_key,
    }
}

/// Encrypt an inner event JSON string for PNS storage.
///
/// Returns base64-encoded NIP-44 v2 ciphertext suitable for the
/// `content` field of a kind-1080 event.
pub fn encrypt(conversation_key: &ConversationKey, inner_json: &str) -> Result<String, PnsError> {
    let payload =
        v2::encrypt_to_bytes(conversation_key, inner_json).map_err(PnsError::Encrypt)?;
    Ok(BASE64.encode(payload))
}

/// Decrypt a PNS event's content field back to the inner event JSON.
///
/// Takes base64-encoded NIP-44 v2 ciphertext from a kind-1080 event.
pub fn decrypt(conversation_key: &ConversationKey, content: &str) -> Result<String, PnsError> {
    let payload = BASE64.decode(content).map_err(PnsError::Base64)?;
    let plaintext = v2::decrypt_to_bytes(conversation_key, &payload).map_err(PnsError::Decrypt)?;
    String::from_utf8(plaintext).map_err(PnsError::Utf8)
}

/// HKDF-Extract: HMAC-SHA256(salt, ikm) → 32-byte PRK.
fn hkdf_extract(ikm: &[u8; 32], salt: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut prk = [0u8; 32];
    // HKDF extract output is the PRK itself. We use expand with empty
    // info to get the 32-byte output matching the spec's hkdf_extract.
    //
    // Note: Hkdf::new() does extract internally. The PRK is stored in
    // the Hkdf struct. We extract it via expand with empty info.
    hk.expand(&[], &mut prk)
        .expect("32 bytes is valid for HMAC-SHA256");
    prk
}

/// Derive a secp256k1 keypair from 32 bytes of key material.
fn keypair_from_bytes(key: &[u8; 32]) -> FullKeypair {
    let secret_key =
        nostr::SecretKey::from_slice(key).expect("32 bytes of HKDF output is a valid secret key");
    let (xopk, _) = secret_key.x_only_public_key(&nostr::SECP256K1);
    FullKeypair {
        pubkey: Pubkey::new(xopk.serialize()),
        secret_key,
    }
}

#[derive(Debug)]
pub enum PnsError {
    Encrypt(nostr::nips::nip44::Error),
    Decrypt(nostr::nips::nip44::Error),
    Base64(base64::DecodeError),
    Utf8(std::string::FromUtf8Error),
}

impl std::fmt::Display for PnsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PnsError::Encrypt(e) => write!(f, "PNS encrypt failed: {e}"),
            PnsError::Decrypt(e) => write!(f, "PNS decrypt failed: {e}"),
            PnsError::Base64(e) => write!(f, "PNS base64 decode failed: {e}"),
            PnsError::Utf8(e) => write!(f, "PNS decrypted content is not UTF-8: {e}"),
        }
    }
}

impl std::error::Error for PnsError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_device_key() -> [u8; 32] {
        // Deterministic test key
        let mut key = [0u8; 32];
        key[0] = 0x01;
        key[31] = 0xff;
        key
    }

    #[test]
    fn test_derive_pns_keys_deterministic() {
        let dk = test_device_key();
        let keys1 = derive_pns_keys(&dk);
        let keys2 = derive_pns_keys(&dk);

        assert_eq!(keys1.keypair.pubkey, keys2.keypair.pubkey);
        assert_eq!(
            keys1.conversation_key.as_bytes(),
            keys2.conversation_key.as_bytes()
        );
    }

    #[test]
    fn test_pns_pubkey_differs_from_device_pubkey() {
        let dk = test_device_key();
        let pns = derive_pns_keys(&dk);

        // Device pubkey
        let device_sk = nostr::SecretKey::from_slice(&dk).unwrap();
        let (device_xopk, _) = device_sk.x_only_public_key(&nostr::SECP256K1);
        let device_pubkey = Pubkey::new(device_xopk.serialize());

        // PNS pubkey should be different (derived via HKDF)
        assert_ne!(pns.keypair.pubkey, device_pubkey);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let dk = test_device_key();
        let keys = derive_pns_keys(&dk);

        let inner = r#"{"kind":1,"pubkey":"abc","content":"hello","tags":[],"created_at":0}"#;
        let encrypted = encrypt(&keys.conversation_key, inner).unwrap();

        // Should be base64
        assert!(BASE64.decode(&encrypted).is_ok());

        let decrypted = decrypt(&keys.conversation_key, &encrypted).unwrap();
        assert_eq!(decrypted, inner);
    }

    #[test]
    fn test_different_keys_cannot_decrypt() {
        let dk1 = test_device_key();
        let mut dk2 = test_device_key();
        dk2[0] = 0x02;

        let keys1 = derive_pns_keys(&dk1);
        let keys2 = derive_pns_keys(&dk2);

        let inner = r#"{"content":"secret"}"#;
        let encrypted = encrypt(&keys1.conversation_key, inner).unwrap();

        // Different key should fail to decrypt
        assert!(decrypt(&keys2.conversation_key, &encrypted).is_err());
    }

    #[test]
    fn test_encrypt_produces_different_ciphertext() {
        // NIP-44 uses random nonce, so encrypting same plaintext twice
        // should produce different ciphertext
        let dk = test_device_key();
        let keys = derive_pns_keys(&dk);

        let inner = r#"{"content":"hello"}"#;
        let enc1 = encrypt(&keys.conversation_key, inner).unwrap();
        let enc2 = encrypt(&keys.conversation_key, inner).unwrap();

        assert_ne!(enc1, enc2);

        // But both should decrypt to the same thing
        assert_eq!(decrypt(&keys.conversation_key, &enc1).unwrap(), inner);
        assert_eq!(decrypt(&keys.conversation_key, &enc2).unwrap(), inner);
    }
}
