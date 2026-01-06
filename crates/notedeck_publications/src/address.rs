//! Event addressing for NKBIP-01 publications
//!
//! Events are addressed using NIP-33 format: `kind:pubkey:d-tag`

use std::fmt;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AddressError {
    #[error("Invalid address format: expected kind:pubkey:dtag")]
    InvalidFormat,

    #[error("Invalid kind: {0}")]
    InvalidKind(String),

    #[error("Invalid pubkey: {0}")]
    InvalidPubkey(String),

    #[error("Missing d-tag")]
    MissingDTag,
}

/// NIP-33 event address in format `kind:pubkey:dtag`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EventAddress {
    pub kind: u32,
    pub pubkey: [u8; 32],
    pub dtag: String,
}

impl EventAddress {
    /// Create a new event address
    pub fn new(kind: u32, pubkey: [u8; 32], dtag: String) -> Self {
        Self { kind, pubkey, dtag }
    }

    /// Parse an address from an `a` tag value
    ///
    /// Format: `kind:pubkey_hex:dtag`
    pub fn from_a_tag(tag_value: &str) -> Result<Self, AddressError> {
        let parts: Vec<&str> = tag_value.splitn(3, ':').collect();

        if parts.len() < 3 {
            return Err(AddressError::InvalidFormat);
        }

        let kind = parts[0]
            .parse::<u32>()
            .map_err(|_| AddressError::InvalidKind(parts[0].to_string()))?;

        let pubkey_hex = parts[1];
        if pubkey_hex.len() != 64 {
            return Err(AddressError::InvalidPubkey(pubkey_hex.to_string()));
        }

        let mut pubkey = [0u8; 32];
        hex::decode_to_slice(pubkey_hex, &mut pubkey)
            .map_err(|_| AddressError::InvalidPubkey(pubkey_hex.to_string()))?;

        let dtag = parts[2].to_string();
        if dtag.is_empty() {
            return Err(AddressError::MissingDTag);
        }

        Ok(Self { kind, pubkey, dtag })
    }

    /// Convert to string format
    pub fn to_string_format(&self) -> String {
        format!("{}:{}:{}", self.kind, hex::encode(self.pubkey), self.dtag)
    }

    /// Get the pubkey as hex string
    pub fn pubkey_hex(&self) -> String {
        hex::encode(self.pubkey)
    }
}

impl fmt::Display for EventAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string_format())
    }
}

impl TryFrom<&str> for EventAddress {
    type Error = AddressError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::from_a_tag(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_address() {
        let addr_str = "30040:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef:my-publication";
        let addr = EventAddress::from_a_tag(addr_str).unwrap();

        assert_eq!(addr.kind, 30040);
        assert_eq!(addr.dtag, "my-publication");
        assert_eq!(addr.pubkey_hex(), "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef");
    }

    #[test]
    fn test_roundtrip() {
        let original = "30041:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:chapter-1";
        let addr = EventAddress::from_a_tag(original).unwrap();
        assert_eq!(addr.to_string_format(), original);
    }

    #[test]
    fn test_invalid_format() {
        assert!(EventAddress::from_a_tag("invalid").is_err());
        assert!(EventAddress::from_a_tag("30040:short").is_err());
    }

    #[test]
    fn test_dtag_with_colons() {
        // d-tag can contain colons
        let addr_str = "30040:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef:my:complex:dtag";
        let addr = EventAddress::from_a_tag(addr_str).unwrap();
        assert_eq!(addr.dtag, "my:complex:dtag");
    }
}
