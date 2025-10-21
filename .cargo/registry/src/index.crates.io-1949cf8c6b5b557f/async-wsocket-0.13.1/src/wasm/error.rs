// Copyright (c) 2019-2022 Naja Melan
// Copyright (c) 2023-2024 Yuki Kishimoto
// Distributed under the MIT software license

use core::fmt;
use core::str::Utf8Error;

use crate::wasm::CloseEvent;

/// WebSocket Error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// UTF-8 error
    Utf8(Utf8Error),
    /// Invalid input to [WsState::try_from( u16 )](crate::WsState).
    InvalidWsState {
        /// The user supplied value that is invalid.
        supplied: u16,
    },
    /// When trying to send and [WsState](crate::WsState) is anything but [WsState::Open](crate::WsState::Open) this error is returned.
    ConnectionNotOpen,
    /// An invalid URL was given to [WsMeta::connect](crate::WsMeta::connect), please see:
    /// [HTML Living Standard](https://html.spec.whatwg.org/multipage/web-sockets.html#dom-websocket).
    InvalidUrl {
        /// The user supplied value that is invalid.
        supplied: String,
    },
    /// An invalid close code was given to a close method. For valid close codes, please see:
    /// [MDN Documentation](https://developer.mozilla.org/en-US/docs/Web/API/CloseEvent#Status_codes).
    InvalidCloseCode {
        /// The user supplied value that is invalid.
        supplied: u16,
    },
    /// The reason string given to a close method is longer than 123 bytes, please see:
    /// [MDN Documentation](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket/close).
    ReasonStringToLong,
    /// Failed to connect to the server.
    ConnectionFailed {
        /// The close event that might hold extra code and reason information.
        event: CloseEvent,
    },
    /// When converting the JavaScript Message into a WsMessage, it's possible that
    /// a String message doesn't convert correctly as Js does not guarantee that
    /// strings are valid Unicode. Happens in `impl TryFrom< MessageEvent > for WsMessage`.
    InvalidEncoding,
    /// When converting the JavaScript Message into a WsMessage, it's not possible to
    /// convert Blob type messages, as Blob is a streaming type, that needs to be read
    /// asynchronously. If you are using the type without setting up the connection with
    /// [`WsMeta::connect`](crate::WsMeta::connect), you have to make sure to set the binary
    /// type of the connection to `ArrayBuffer`.
    ///
    /// Happens in `impl TryFrom< MessageEvent > for WsMessage`.
    CantDecodeBlob,
    /// When converting the JavaScript Message into a WsMessage, the data type was neither
    /// `Arraybuffer`, `String` nor `Blob`. This should never happen. If it does, please
    /// try to make a reproducible example and file an issue.
    ///
    /// Happens in `impl TryFrom< MessageEvent > for WsMessage`.
    UnknownDataType,
    Dom(u16),
    Other(String),
    Timeout,
}

impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Utf8(e) => write!(f, "{e}"),
            Self::InvalidWsState { supplied } => {
                write!(f, "Invalid input to conversion to WsReadyState: {supplied}")
            }
            Self::ConnectionNotOpen => write!(f, "The connection state is not \"Open\"."),
            Self::InvalidUrl { supplied } => write!(
                f,
                "An invalid URL was given to the connect method: {supplied}"
            ),
            Self::InvalidCloseCode { supplied } => write!(
                f,
                "An invalid close code was given to a close method: {supplied}"
            ),
            Self::ReasonStringToLong => {
                write!(f, "The reason string given to a close method is to long.")
            }
            Self::ConnectionFailed { event } => {
                write!(f, "Failed to connect to the server. CloseEvent: {event:?}")
            }
            Self::InvalidEncoding => write!(
                f,
                "Received a String message that couldn't be decoded to valid UTF-8"
            ),
            Self::CantDecodeBlob => write!(f, "Received a Blob message that couldn't converted."),
            Self::UnknownDataType => write!(
                f,
                "Received a message that is neither ArrayBuffer, String or Blob."
            ),
            Self::Dom(code) => write!(f, "DOM Exception: {code}"),
            Self::Other(e) => write!(f, "{e}"),
            Self::Timeout => write!(f, "timeout"),
        }
    }
}

impl From<Utf8Error> for Error {
    fn from(e: Utf8Error) -> Self {
        Self::Utf8(e)
    }
}
