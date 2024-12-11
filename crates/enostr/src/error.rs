//use nostr::prelude::secp256k1;
use std::array::TryFromSliceError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("message is empty")]
    Empty,

    #[error("decoding failed")]
    DecodeFailed,

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

    // Secp(secp256k1::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

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
