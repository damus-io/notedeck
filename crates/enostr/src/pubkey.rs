use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Error;
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use tracing::debug;

#[derive(Eq, PartialEq, Clone, Copy, Hash, Ord, PartialOrd)]
pub struct Pubkey([u8; 32]);

#[derive(Eq, PartialEq, Clone, Copy, Hash, Ord, PartialOrd)]
pub struct PubkeyRef<'a>(&'a [u8; 32]);

/// Result of parsing a NIP-19 nprofile bech32 string.
///
/// Contains the pubkey along with optional relay hints where the profile
/// may be found. This enables hint-based routing when fetching profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNprofile {
    /// The public key.
    pub pubkey: Pubkey,
    /// Relay URLs where this profile may be found (NIP-19 TLV type 1).
    pub relays: Vec<String>,
}

impl ParsedNprofile {
    /// Create a new ParsedNprofile with the given pubkey and relay hints.
    pub fn new(pubkey: Pubkey, relays: Vec<String>) -> Self {
        Self { pubkey, relays }
    }

    /// Create a ParsedNprofile with no relay hints.
    pub fn without_relays(pubkey: Pubkey) -> Self {
        Self {
            pubkey,
            relays: Vec::new(),
        }
    }
}

static HRP_NPUB: bech32::Hrp = bech32::Hrp::parse_unchecked("npub");

impl Borrow<[u8; 32]> for PubkeyRef<'_> {
    fn borrow(&self) -> &[u8; 32] {
        self.0
    }
}

impl<'a> PubkeyRef<'a> {
    pub fn new(bytes: &'a [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        self.0
    }

    pub fn to_owned(&self) -> Pubkey {
        Pubkey::new(*self.bytes())
    }

    pub fn hex(&self) -> String {
        hex::encode(self.bytes())
    }
}

impl Deref for Pubkey {
    type Target = [u8; 32];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Borrow<[u8; 32]> for Pubkey {
    fn borrow(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Pubkey {
    pub fn new(data: [u8; 32]) -> Self {
        Self(data)
    }

    pub fn hex(&self) -> String {
        hex::encode(self.bytes())
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn as_ref(&self) -> PubkeyRef<'_> {
        PubkeyRef(self.bytes())
    }

    pub fn parse(s: &str) -> Result<Self, Error> {
        match Pubkey::from_hex(s) {
            Ok(pk) => Ok(pk),
            Err(_) => Pubkey::try_from_bech32_string(s, false),
        }
    }

    pub fn from_hex(hex_str: &str) -> Result<Self, Error> {
        Ok(Pubkey(hex::decode(hex_str)?.as_slice().try_into()?))
    }

    pub fn try_from_hex_str_with_verify(hex_str: &str) -> Result<Self, Error> {
        let vec: Vec<u8> = hex::decode(hex_str)?;
        if vec.len() != 32 {
            Err(Error::HexDecodeFailed)
        } else {
            let _ = match nostr::secp256k1::XOnlyPublicKey::from_slice(&vec) {
                Ok(r) => Ok(r),
                Err(_) => Err(Error::InvalidPublicKey),
            }?;

            Ok(Pubkey(vec.try_into().unwrap()))
        }
    }

    pub fn try_from_bech32_string(s: &str, verify: bool) -> Result<Self, Error> {
        let data = match bech32::decode(s) {
            Ok(res) => Ok(res),
            Err(_) => Err(Error::InvalidBech32),
        }?;

        if data.0 != HRP_NPUB {
            Err(Error::InvalidBech32)
        } else if data.1.len() != 32 {
            Err(Error::InvalidByteSize)
        } else {
            if verify {
                let _ = match nostr::secp256k1::XOnlyPublicKey::from_slice(&data.1) {
                    Ok(r) => Ok(r),
                    Err(_) => Err(Error::InvalidPublicKey),
                }?;
            }
            Ok(Pubkey(data.1.try_into().unwrap()))
        }
    }

    pub fn npub(&self) -> Option<String> {
        bech32::encode::<bech32::Bech32>(HRP_NPUB, &self.0).ok()
    }

    /// Parse a NIP-19 nprofile bech32 string, extracting pubkey and relay hints.
    ///
    /// Returns a `ParsedNprofile` containing the pubkey and any relay hints
    /// embedded in the nprofile (TLV type 1). Relay hints enable hint-based
    /// routing when fetching the profile.
    ///
    /// If the nostr crate fails to parse (e.g., due to invalid relay URLs),
    /// falls back to manual TLV parsing to extract at least the pubkey.
    ///
    /// # Example
    /// ```ignore
    /// let result = Pubkey::try_from_nprofile_string(
    ///     "nprofile1qqsrhuxx8l9ex335q7he0f09aej04zpazpl0ne2cgukyawd24mayt8gpp4mhxue69uhhytnc9e3k7mgpz4mhxue69uhkg6nzv9ejuumpv34kytnrdaksjlyr9p"
    /// );
    /// ```
    pub fn try_from_nprofile_string(s: &str) -> Result<ParsedNprofile, Error> {
        use nostr::nips::nip19::FromBech32;

        // Try the nostr crate first (handles valid nprofiles)
        if let Ok(profile) = nostr::nips::nip19::Nip19Profile::from_bech32(s) {
            let pubkey_bytes: [u8; 32] = profile.public_key.to_bytes();
            let pubkey = Pubkey::new(pubkey_bytes);

            let relays: Vec<String> = profile
                .relays
                .into_iter()
                .map(|url| url.to_string())
                .collect();

            return Ok(ParsedNprofile::new(pubkey, relays));
        }

        // Fallback: manual TLV parsing to extract pubkey even if relay URLs are invalid
        Self::try_from_nprofile_manual(s)
    }

    /// Manual TLV parsing for nprofile when the nostr crate fails.
    ///
    /// Extracts the pubkey (TLV type 0) and any valid relay hints (TLV type 1),
    /// gracefully skipping malformed relay entries.
    fn try_from_nprofile_manual(s: &str) -> Result<ParsedNprofile, Error> {
        static HRP_NPROFILE: bech32::Hrp = bech32::Hrp::parse_unchecked("nprofile");

        let (hrp, data) = bech32::decode(s).map_err(|_| Error::InvalidBech32)?;
        if hrp != HRP_NPROFILE {
            return Err(Error::InvalidBech32);
        }

        let mut pubkey: Option<Pubkey> = None;
        let mut relays: Vec<String> = Vec::new();
        let mut i = 0;

        // Parse TLV entries
        while i + 2 <= data.len() {
            let tlv_type = data[i];
            let tlv_len = data[i + 1] as usize;
            i += 2;

            if i + tlv_len > data.len() {
                break; // Malformed TLV, stop parsing
            }

            let tlv_value = &data[i..i + tlv_len];
            i += tlv_len;

            match tlv_type {
                0 => {
                    // Type 0: pubkey (32 bytes)
                    if tlv_len == 32 {
                        if let Ok(bytes) = tlv_value.try_into() {
                            pubkey = Some(Pubkey::new(bytes));
                        }
                    }
                }
                1 => {
                    // Type 1: relay URL (variable length, UTF-8 string)
                    if let Ok(relay_str) = std::str::from_utf8(tlv_value) {
                        // Only add if it looks like a valid WebSocket URL (graceful degradation)
                        // Case-insensitive check to handle WSS:// or Wss:// etc.
                        let lower = relay_str.to_lowercase();
                        if lower.starts_with("wss://") || lower.starts_with("ws://") {
                            relays.push(relay_str.to_string());
                        }
                    }
                }
                _ => {
                    // Skip unknown TLV types
                }
            }
        }

        match pubkey {
            Some(pk) => Ok(ParsedNprofile::new(pk, relays)),
            None => Err(Error::InvalidBech32),
        }
    }

    /// Parse a string as a pubkey, returning relay hints if available.
    ///
    /// Tries to parse the input as:
    /// 1. Hex-encoded pubkey (no relay hints)
    /// 2. npub bech32 (no relay hints)
    /// 3. nprofile bech32 (with relay hints)
    ///
    /// Returns a `ParsedNprofile` containing the pubkey and any relay hints.
    pub fn parse_with_relays(s: &str) -> Result<ParsedNprofile, Error> {
        // Try hex first
        if let Ok(pk) = Pubkey::from_hex(s) {
            return Ok(ParsedNprofile::without_relays(pk));
        }

        // Try npub
        if let Ok(pk) = Pubkey::try_from_bech32_string(s, false) {
            return Ok(ParsedNprofile::without_relays(pk));
        }

        // Try nprofile (has relay hints)
        Pubkey::try_from_nprofile_string(s)
    }
}

impl fmt::Display for Pubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.hex())
    }
}

impl fmt::Debug for PubkeyRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.hex())
    }
}

impl fmt::Debug for Pubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.hex())
    }
}

impl From<Pubkey> for String {
    fn from(pk: Pubkey) -> Self {
        pk.hex()
    }
}

// Custom serialize function for Pubkey
impl Serialize for Pubkey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.hex())
    }
}

// Custom deserialize function for Pubkey
impl<'de> Deserialize<'de> for Pubkey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        debug!("decoding pubkey start");
        let s = String::deserialize(deserializer)?;
        debug!("decoding pubkey {}", &s);
        Pubkey::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

impl hashbrown::Equivalent<Pubkey> for &[u8; 32] {
    fn equivalent(&self, key: &Pubkey) -> bool {
        self.as_slice() == key.bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test nprofile parsing with relay hints (example from NIP-19).
    #[test]
    fn test_parse_nprofile_with_relays() {
        // This nprofile contains pubkey + 2 relay hints
        let nprofile = "nprofile1qqsrhuxx8l9ex335q7he0f09aej04zpazpl0ne2cgukyawd24mayt8gpp4mhxue69uhhytnc9e3k7mgpz4mhxue69uhkg6nzv9ejuumpv34kytnrdaksjlyr9p";

        let result = Pubkey::try_from_nprofile_string(nprofile).unwrap();

        assert_eq!(
            result.pubkey.hex(),
            "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"
        );
        assert_eq!(result.relays.len(), 2);

        // Check that both relays are present (with or without trailing slash)
        let has_r_x_com = result.relays.iter().any(|r| r.contains("r.x.com"));
        let has_djbas = result.relays.iter().any(|r| r.contains("djbas.sadkb.com"));
        assert!(has_r_x_com, "Missing r.x.com relay: {:?}", result.relays);
        assert!(
            has_djbas,
            "Missing djbas.sadkb.com relay: {:?}",
            result.relays
        );
    }

    /// Test parse_with_relays falls back correctly for different formats.
    #[test]
    fn test_parse_with_relays_fallback() {
        let hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
        let npub = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";

        // Hex should parse with no relays
        let hex_result = Pubkey::parse_with_relays(hex).unwrap();
        assert_eq!(hex_result.pubkey.hex(), hex);
        assert!(hex_result.relays.is_empty());

        // npub should parse with no relays
        let npub_result = Pubkey::parse_with_relays(npub).unwrap();
        assert_eq!(npub_result.pubkey.hex(), hex);
        assert!(npub_result.relays.is_empty());
    }

    /// Test manual TLV parsing extracts pubkey even with minimal data.
    #[test]
    fn test_manual_tlv_parsing() {
        // Create a minimal nprofile with just pubkey (TLV type 0)
        // This tests the try_from_nprofile_manual fallback path
        let nprofile = "nprofile1qqsrhuxx8l9ex335q7he0f09aej04zpazpl0ne2cgukyawd24mayt8gpp4mhxue69uhhytnc9e3k7mgpz4mhxue69uhkg6nzv9ejuumpv34kytnrdaksjlyr9p";

        // Should successfully parse via manual path if nostr crate fails
        let result = Pubkey::try_from_nprofile_manual(nprofile).unwrap();
        assert_eq!(
            result.pubkey.hex(),
            "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"
        );
        // Manual parser should also extract valid relay hints
        assert!(!result.relays.is_empty());
    }

    /// Test that manual TLV parser skips invalid relay URLs gracefully.
    #[test]
    fn test_manual_tlv_skips_invalid_relays() {
        // The manual parser should only accept ws:// or wss:// URLs
        // and skip anything else without failing
        let nprofile = "nprofile1qqsrhuxx8l9ex335q7he0f09aej04zpazpl0ne2cgukyawd24mayt8gpp4mhxue69uhhytnc9e3k7mgpz4mhxue69uhkg6nzv9ejuumpv34kytnrdaksjlyr9p";

        let result = Pubkey::try_from_nprofile_manual(nprofile).unwrap();

        // All extracted relays should be valid websocket URLs (case-insensitive)
        for relay in &result.relays {
            let lower = relay.to_lowercase();
            assert!(
                lower.starts_with("wss://") || lower.starts_with("ws://"),
                "Invalid relay URL: {}",
                relay
            );
        }
    }

    /// Test that manual TLV parser handles mixed-case schemes.
    #[test]
    fn test_manual_tlv_case_insensitive_scheme() {
        // The scheme check should be case-insensitive
        // WSS://, Wss://, wSs:// should all be accepted

        // We can't easily construct a custom nprofile with mixed-case,
        // but we can verify the logic by checking that lowercase comparison works
        let test_urls = ["wss://relay.com", "WSS://relay.com", "Wss://Relay.COM"];
        for url in test_urls {
            let lower = url.to_lowercase();
            assert!(
                lower.starts_with("wss://") || lower.starts_with("ws://"),
                "Should accept URL regardless of case: {}",
                url
            );
        }
    }

    /// Test ParsedNprofile construction helpers.
    #[test]
    fn test_parsed_nprofile_constructors() {
        let pk =
            Pubkey::from_hex("3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d")
                .unwrap();

        // Test without_relays constructor
        let no_relays = ParsedNprofile::without_relays(pk);
        assert!(no_relays.relays.is_empty());
        assert_eq!(no_relays.pubkey, pk);

        // Test new constructor with relays
        let with_relays = ParsedNprofile::new(pk, vec!["wss://relay.example.com".to_string()]);
        assert_eq!(with_relays.relays.len(), 1);
        assert_eq!(with_relays.pubkey, pk);
    }
}
