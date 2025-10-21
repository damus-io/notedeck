// Copyright (c) 2023 Doug Hoyte
// Copyright (c) 2023 Yuki Kishimoto
// Distributed under the MIT software license

use core::array::TryFromSliceError;
use core::fmt;

/// Error
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// ID too big
    IdTooBig,
    /// Invalid ID size
    InvalidIdSize,
    /// Frame size limit too small
    FrameSizeLimitTooSmall,
    /// Not sealed
    NotSealed,
    /// Already sealed
    AlreadySealed,
    /// Already built initial message
    AlreadyBuiltInitialMessage,
    /// Initiator error
    Initiator,
    /// Non-initiator error
    NonInitiator,
    /// Unexpected mode
    UnexpectedMode(u64),
    /// Parse ends prematurely
    ParseEndsPrematurely,
    /// Protocol version not found
    ProtocolVersionNotFound,
    /// Invalid protocol version
    InvalidProtocolVersion,
    /// Unsupported protocol version
    UnsupportedProtocolVersion,
    /// Try from slice error
    TryFromSlice,
    /// Bad range
    BadRange,
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdTooBig => write!(f, "ID too big"),
            Self::InvalidIdSize => write!(f, "Invalid ID size"),
            Self::FrameSizeLimitTooSmall => write!(f, "Frame size limit too small"),
            Self::NotSealed => write!(f, "Not sealed"),
            Self::AlreadySealed => write!(f, "Already sealed"),
            Self::AlreadyBuiltInitialMessage => write!(f, "Already built initial message"),
            Self::Initiator => write!(f, "initiator not asking for have/need IDs"),
            Self::NonInitiator => write!(f, "non-initiator asking for have/need IDs"),
            Self::UnexpectedMode(m) => write!(f, "Unexpected mode: {}", m),
            Self::ParseEndsPrematurely => write!(f, "parse ends prematurely"),
            Self::ProtocolVersionNotFound => write!(f, "protocol version not found"),
            Self::InvalidProtocolVersion => write!(f, "invalid negentropy protocol version byte"),
            Self::UnsupportedProtocolVersion => {
                write!(f, "server does not support our negentropy protocol version")
            }
            Self::TryFromSlice => write!(f, "could not convert slice to array"),
            Self::BadRange => write!(f, "bad range"),
        }
    }
}

impl From<TryFromSliceError> for Error {
    fn from(_: TryFromSliceError) -> Self {
        Self::TryFromSlice
    }
}
