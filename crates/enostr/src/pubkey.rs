use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Error;
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use tracing::{debug, warn};

#[derive(Eq, PartialEq, Clone, Copy, Hash, Ord, PartialOrd)]
pub struct Pubkey([u8; 32]);

#[derive(Eq, PartialEq, Clone, Copy, Hash, Ord, PartialOrd)]
pub struct PubkeyRef<'a>(&'a [u8; 32]);

static HRP_NPUB: bech32::Hrp = bech32::Hrp::parse_unchecked("npub");

/// A parsed NIP-19 nprofile, containing the public key and any relay hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNprofile {
    pub pubkey: Pubkey,
    pub relays: Vec<String>,
}

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

    /// Parse a NIP-19 nprofile1 bech32 string and extract the public key.
    pub fn from_nprofile_bech(bech: &str) -> Option<Self> {
        use nostr::nips::nip19::{FromBech32, Nip19Profile};
        let nip19_profile = Nip19Profile::from_bech32(bech).ok()?;
        Some(Pubkey::new(nip19_profile.public_key.to_bytes()))
    }

    /// Parse an nprofile bech32 string, extracting both the pubkey and relay hints.
    ///
    /// Tries the `nostr` crate first, then falls back to manual TLV parsing
    /// for robustness against non-standard encodings.
    pub fn try_from_nprofile_string(s: &str) -> Result<ParsedNprofile, Error> {
        use nostr::nips::nip19::{FromBech32, Nip19Profile};

        if let Ok(profile) = Nip19Profile::from_bech32(s) {
            let pubkey = Pubkey::new(profile.public_key.to_bytes());
            let relays = profile.relays.into_iter().map(|u| u.to_string()).collect();
            return Ok(ParsedNprofile { pubkey, relays });
        }

        Self::try_from_nprofile_manual(s)
    }

    /// Manual TLV parsing fallback for nprofile bech32 strings.
    ///
    /// NIP-19 TLV format:
    /// - Type 0x00 (32 bytes): pubkey (required)
    /// - Type 0x01 (variable): relay URL (optional, repeatable)
    fn try_from_nprofile_manual(s: &str) -> Result<ParsedNprofile, Error> {
        let (hrp, data) = bech32::decode(s).map_err(|_| Error::InvalidBech32)?;
        if hrp.to_string() != "nprofile" {
            return Err(Error::InvalidBech32);
        }

        let mut pubkey: Option<Pubkey> = None;
        let mut relays: Vec<String> = Vec::new();
        let mut pos = 0;

        while pos < data.len() {
            if pos + 2 > data.len() {
                break;
            }
            let typ = data[pos];
            let len = data[pos + 1] as usize;
            pos += 2;

            if pos + len > data.len() {
                warn!("nprofile TLV: truncated value at pos {pos}");
                break;
            }

            let value = &data[pos..pos + len];
            pos += len;

            match typ {
                0x00 => {
                    if len != 32 {
                        return Err(Error::InvalidByteSize);
                    }
                    let bytes: [u8; 32] = value.try_into().map_err(|_| Error::InvalidByteSize)?;
                    pubkey = Some(Pubkey::new(bytes));
                }
                0x01 => {
                    if let Ok(url) = std::str::from_utf8(value) {
                        let lower = url.to_lowercase();
                        if lower.starts_with("ws://") || lower.starts_with("wss://") {
                            relays.push(url.to_string());
                        }
                    }
                }
                _ => {
                    // skip unknown TLV types
                }
            }
        }

        let pubkey = pubkey.ok_or(Error::InvalidPublicKey)?;
        Ok(ParsedNprofile { pubkey, relays })
    }

    /// Parse a string as hex, npub, or nprofile, returning the pubkey and any relay hints.
    ///
    /// Tries hex first, then npub bech32, then nprofile bech32.
    /// For hex and npub inputs, the relay list will be empty.
    pub fn parse_with_relays(s: &str) -> Result<ParsedNprofile, Error> {
        if let Ok(pk) = Pubkey::from_hex(s) {
            return Ok(ParsedNprofile {
                pubkey: pk,
                relays: vec![],
            });
        }

        if let Ok(pk) = Pubkey::try_from_bech32_string(s, false) {
            return Ok(ParsedNprofile {
                pubkey: pk,
                relays: vec![],
            });
        }

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

    // npub/nprofile for jb55:
    // pubkey hex: 32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245
    const JB55_HEX: &str = "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245";

    #[test]
    fn parse_with_relays_hex() {
        let result = Pubkey::parse_with_relays(JB55_HEX).unwrap();
        assert_eq!(result.pubkey.hex(), JB55_HEX);
        assert!(result.relays.is_empty());
    }

    #[test]
    fn parse_with_relays_npub() {
        let pk = Pubkey::from_hex(JB55_HEX).unwrap();
        let npub = pk.npub().unwrap();
        let result = Pubkey::parse_with_relays(&npub).unwrap();
        assert_eq!(result.pubkey.hex(), JB55_HEX);
        assert!(result.relays.is_empty());
    }

    #[test]
    fn parse_nprofile_with_relay() {
        use nostr::nips::nip19::{Nip19Profile, ToBech32};
        use nostr::PublicKey;

        let pk = PublicKey::from_slice(&hex::decode(JB55_HEX).unwrap()).unwrap();
        let profile = Nip19Profile::new(pk, ["wss://relay.damus.io".to_string()]).unwrap();
        let bech = profile.to_bech32().unwrap();

        let result = Pubkey::try_from_nprofile_string(&bech).unwrap();
        assert_eq!(result.pubkey.hex(), JB55_HEX);
        assert_eq!(result.relays.len(), 1);
        assert!(result.relays[0].contains("relay.damus.io"));
    }

    #[test]
    fn parse_nprofile_multiple_relays() {
        use nostr::nips::nip19::{Nip19Profile, ToBech32};
        use nostr::PublicKey;

        let pk = PublicKey::from_slice(&hex::decode(JB55_HEX).unwrap()).unwrap();
        let profile = Nip19Profile::new(
            pk,
            [
                "wss://relay.damus.io".to_string(),
                "wss://nos.lol".to_string(),
            ],
        )
        .unwrap();
        let bech = profile.to_bech32().unwrap();

        let result = Pubkey::try_from_nprofile_string(&bech).unwrap();
        assert_eq!(result.pubkey.hex(), JB55_HEX);
        assert_eq!(result.relays.len(), 2);
    }

    #[test]
    fn parse_nprofile_no_relays() {
        use nostr::nips::nip19::{Nip19Profile, ToBech32};
        use nostr::PublicKey;

        let pk = PublicKey::from_slice(&hex::decode(JB55_HEX).unwrap()).unwrap();
        let profile = Nip19Profile::new(pk, Vec::<String>::new()).unwrap();
        let bech = profile.to_bech32().unwrap();

        let result = Pubkey::try_from_nprofile_string(&bech).unwrap();
        assert_eq!(result.pubkey.hex(), JB55_HEX);
        assert!(result.relays.is_empty());
    }

    #[test]
    fn parse_with_relays_nprofile() {
        use nostr::nips::nip19::{Nip19Profile, ToBech32};
        use nostr::PublicKey;

        let pk = PublicKey::from_slice(&hex::decode(JB55_HEX).unwrap()).unwrap();
        let profile = Nip19Profile::new(pk, ["wss://relay.damus.io".to_string()]).unwrap();
        let bech = profile.to_bech32().unwrap();

        let result = Pubkey::parse_with_relays(&bech).unwrap();
        assert_eq!(result.pubkey.hex(), JB55_HEX);
        assert_eq!(result.relays.len(), 1);
    }

    #[test]
    fn parse_with_relays_invalid() {
        assert!(Pubkey::parse_with_relays("garbage").is_err());
    }
}
