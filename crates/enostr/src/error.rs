//use nostr::prelude::secp256k1;
use std::array::TryFromSliceError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("message is empty")]
    Empty,

    #[error("decoding failed: {0}")]
    DecodeFailed(String),

    #[error("hex decoding failed")]
    HexDecodeFailed,

    #[error("invalid bech32")]
    InvalidBech32,

    #[error("invalid byte size")]
    InvalidByteSize,

    #[error("invalid signature")]
    InvalidSignature,

    #[error("invalid public key")]
    InvalidPublicKey,

    #[error("invalid relay url")]
    InvalidRelayUrl,

    // Secp(secp256k1::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("nostrdb error: {0}")]
    Nostrdb(#[from] nostrdb::Error),

    #[error("{0}")]
    Generic(String),
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Generic(s)
    }
}

impl From<TryFromSliceError> for Error {
    fn from(_e: TryFromSliceError) -> Self {
        Error::InvalidByteSize
    }
}

impl From<hex::FromHexError> for Error {
    fn from(_e: hex::FromHexError) -> Self {
        Error::HexDecodeFailed
    }
}

// nostrdb-net convergence: enostr re-exports nostrdb_net's primitives, whose
// fallible methods (e.g. `Pubkey::from_hex`) yield `nostrdb_net::Error`. Bridge
// it into enostr's error so `?` still works at enostr call sites during the
// migration. Variants map 1:1 where they exist; the rest stringify.
impl From<nostrdb_net::Error> for Error {
    fn from(e: nostrdb_net::Error) -> Self {
        use nostrdb_net::Error as NnErr;
        match e {
            NnErr::Empty => Error::Empty,
            NnErr::HexDecodeFailed => Error::HexDecodeFailed,
            NnErr::InvalidBech32 => Error::InvalidBech32,
            NnErr::InvalidByteSize => Error::InvalidByteSize,
            NnErr::InvalidSignature => Error::InvalidSignature,
            NnErr::InvalidPublicKey => Error::InvalidPublicKey,
            NnErr::Nostrdb(err) => Error::Nostrdb(err),
            other => Error::Generic(other.to_string()),
        }
    }
}
