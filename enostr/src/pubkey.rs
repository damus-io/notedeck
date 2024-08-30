use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Error;
use nostr::bech32::Hrp;
use std::fmt;
use tracing::debug;

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub struct Pubkey([u8; 32]);

static HRP_NPUB: Hrp = Hrp::parse_unchecked("npub");

impl Pubkey {
    pub fn new(data: &[u8; 32]) -> Self {
        Self(*data)
    }

    pub fn hex(&self) -> String {
        hex::encode(self.bytes())
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
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
        let data = match nostr::bech32::decode(s) {
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
}

impl fmt::Display for Pubkey {
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
