// Copyright (c) 2022-2024 Yuki Kishimoto
// Distributed under the MIT software license

use core::fmt;

use tokio_tungstenite::tungstenite::Error as WsError;
use url::ParseError;

#[cfg(feature = "tor")]
use super::tor;

#[derive(Debug)]
pub enum Error {
    /// Ws error
    Ws(WsError),
    /// Socks error
    #[cfg(feature = "socks")]
    Socks(tokio_socks::Error),
    /// Tor error
    #[cfg(feature = "tor")]
    Tor(tor::Error),
    /// Url parse error
    Url(ParseError),
    /// Timeout
    Timeout,
}

impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ws(e) => write!(f, "{e}"),
            #[cfg(feature = "socks")]
            Self::Socks(e) => write!(f, "{e}"),
            #[cfg(feature = "tor")]
            Self::Tor(e) => write!(f, "{e}"),
            Self::Url(e) => write!(f, "{e}"),
            Self::Timeout => write!(f, "timeout"),
        }
    }
}

impl From<WsError> for Error {
    fn from(e: WsError) -> Self {
        Self::Ws(e)
    }
}

#[cfg(feature = "socks")]
impl From<tokio_socks::Error> for Error {
    fn from(e: tokio_socks::Error) -> Self {
        Self::Socks(e)
    }
}

#[cfg(feature = "tor")]
impl From<tor::Error> for Error {
    fn from(e: tor::Error) -> Self {
        Self::Tor(e)
    }
}

impl Error {
    #[inline]
    #[cfg(any(feature = "socks", feature = "tor"))]
    pub(super) fn empty_host() -> Self {
        Self::Url(ParseError::EmptyHost)
    }

    #[inline]
    #[cfg(any(feature = "socks", feature = "tor"))]
    pub(super) fn invalid_port() -> Self {
        Self::Url(ParseError::InvalidPort)
    }
}
