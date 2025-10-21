// Copyright (c) 2019-2022 Naja Melan
// Copyright (c) 2023-2024 Yuki Kishimoto
// Distributed under the MIT software license

use std::error::Error;
use std::fmt;
use std::ops::Deref;

use futures::channel::mpsc;

/// The error type for errors happening in `pharos`.
///
/// Use [`PharErr::kind()`] to know which kind of error happened.
#[derive(Debug)]
pub struct PharErr {
    pub(crate) inner: Option<Box<dyn Error + Send + Sync>>,
    pub(crate) kind: ErrorKind,
}

impl From<ErrorKind> for PharErr {
    fn from(kind: ErrorKind) -> Self {
        Self { inner: None, kind }
    }
}

impl From<mpsc::SendError> for PharErr {
    fn from(inner: mpsc::SendError) -> Self {
        Self {
            inner: Some(Box::new(inner)),
            kind: ErrorKind::SendError,
        }
    }
}

/// The different kind of errors that can happen when you use the `pharos` API.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    #[doc(hidden)]
    // This variant is only used internally.
    SendError,

    /// The pharos object is already closed. You can no longer send messages or observe it.
    /// This should only happen if you call [SinkExt::close](https://docs.rs/futures-preview/0.3.0-alpha.19/futures/sink/trait.SinkExt.html#method.close) on it.
    Closed,

    /// The minimum valid buffer size for [`Channel::Bounded`](crate::observable::Channel) is `1`, you sent in `0`.
    MinChannelSizeOne,
}

impl PartialEq<&ErrorKind> for ErrorKind {
    fn eq(&self, other: &&ErrorKind) -> bool {
        self == *other
    }
}

impl PartialEq<ErrorKind> for &ErrorKind {
    fn eq(&self, other: &ErrorKind) -> bool {
        *self == other
    }
}

impl Error for PharErr {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        // Somehow using as_deref gives us thread bound problems...
        #[allow(clippy::option_as_ref_deref)]
        self.inner
            .as_ref()
            .map(|e| -> &(dyn Error + 'static) { e.deref() })
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SendError | Self::Closed => write!(f, "Channel closed."),
            Self::MinChannelSizeOne => write!(
                f,
                "The minimum valid buffer size for Channel::Bounded is 1, you send in 0.",
            ),
        }
    }
}

impl fmt::Display for PharErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = match self.source() {
            Some(e) => format!(" Caused by: {}", e),
            None => String::new(),
        };

        write!(f, "pharos::PharErr: {}{}", self.kind, inner)
    }
}
