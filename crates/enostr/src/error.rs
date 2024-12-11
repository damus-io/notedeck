//use nostr::prelude::secp256k1;
use std::array::TryFromSliceError;
use std::fmt;

#[derive(Debug)]
pub enum Error {
    Empty,
    DecodeFailed,
    HexDecodeFailed,
    InvalidBech32,
    InvalidByteSize,
    InvalidSignature,
    InvalidPublicKey,
    // Secp(secp256k1::Error),
    Json(serde_json::Error),
    Nostrdb(nostrdb::Error),
    Generic(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "message is empty"),
            Self::DecodeFailed => write!(f, "decoding failed"),
            Self::InvalidSignature => write!(f, "invalid signature"),
            Self::HexDecodeFailed => write!(f, "hex decoding failed"),
            Self::InvalidByteSize => write!(f, "invalid byte size"),
            Self::InvalidBech32 => write!(f, "invalid bech32 string"),
            Self::InvalidPublicKey => write!(f, "invalid public key"),
            //Self::Secp(e) => write!(f, "{e}"),
            Self::Json(e) => write!(f, "{e}"),
            Self::Nostrdb(e) => write!(f, "{e}"),
            Self::Generic(e) => write!(f, "{e}"),
        }
    }
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

/*
impl From<secp256k1::Error> for Error {
    fn from(e: secp256k1::Error) -> Self {
        Error::Secp(e)
    }
}
*/

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

impl From<nostrdb::Error> for Error {
    fn from(e: nostrdb::Error) -> Self {
        Error::Nostrdb(e)
    }
}
