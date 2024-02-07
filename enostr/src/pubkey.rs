use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Error;
use hex;
use log::debug;
use nostr::key::XOnlyPublicKey;
use std::fmt;

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub struct Pubkey(XOnlyPublicKey);

impl Pubkey {
    pub fn hex(&self) -> String {
        hex::encode(self.bytes())
    }

    pub fn bytes(&self) -> [u8; 32] {
        self.0.serialize()
    }

    pub fn from_hex(hex_str: &str) -> Result<Self, Error> {
        Ok(Pubkey(XOnlyPublicKey::from_slice(
            hex::decode(hex_str)?.as_slice(),
        )?))
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
