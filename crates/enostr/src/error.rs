//use nostr::prelude::secp256k1;
use std::array::TryFromSliceError;
use thiserror::Error;

/// Websocket error data retained after adapting from `ewebsock`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebSocketError {
    message: String,
    raw_os_error: Option<i32>,
}

impl WebSocketError {
    /// Build websocket error data without a structured OS error code.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            raw_os_error: None,
        }
    }

    /// Return the original websocket error message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Return the OS error code reported by the native websocket backend.
    pub fn raw_os_error(&self) -> Option<i32> {
        self.raw_os_error
    }
}

impl std::fmt::Display for WebSocketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for WebSocketError {}

impl From<ewebsock::Error> for WebSocketError {
    fn from(error: ewebsock::Error) -> Self {
        Self {
            message: error.message().to_owned(),
            raw_os_error: error.raw_os_error(),
        }
    }
}

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

    #[error("websocket error: {0}")]
    WebSocket(WebSocketError),

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
