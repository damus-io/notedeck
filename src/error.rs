use std::{fmt, io};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FilterError {
    EmptyContactList,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum SubscriptionError {
    //#[error("No active subscriptions")]
    NoActive,

    /// When a timeline has an unexpected number
    /// of active subscriptions. Should only happen if there
    /// is a bug in notedeck
    //#[error("Unexpected subscription count")]
    UnexpectedSubscriptionCount(i32),
}

impl Error {
    pub fn unexpected_sub_count(c: i32) -> Self {
        Error::SubscriptionError(SubscriptionError::UnexpectedSubscriptionCount(c))
    }

    pub fn no_active_sub() -> Self {
        Error::SubscriptionError(SubscriptionError::NoActive)
    }
}

impl fmt::Display for SubscriptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoActive => write!(f, "No active subscriptions"),
            Self::UnexpectedSubscriptionCount(c) => {
                write!(f, "Unexpected subscription count: {}", c)
            }
        }
    }
}

#[derive(Debug)]
pub enum Error {
    TimelineNotFound,
    LoadFailed,
    SubscriptionError(SubscriptionError),
    Filter(FilterError),
    Io(io::Error),
    Nostr(enostr::Error),
    Ndb(nostrdb::Error),
    Image(image::error::ImageError),
    Generic(String),
}

impl Error {
    pub fn empty_contact_list() -> Self {
        Error::Filter(FilterError::EmptyContactList)
    }
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyContactList => {
                write!(f, "empty contact list")
            }
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SubscriptionError(e) => {
                write!(f, "{e}")
            }
            Self::TimelineNotFound => write!(f, "Timeline not found"),
            Self::LoadFailed => {
                write!(f, "load failed")
            }
            Self::Filter(e) => {
                write!(f, "{e}")
            }
            Self::Nostr(e) => write!(f, "{e}"),
            Self::Ndb(e) => write!(f, "{e}"),
            Self::Image(e) => write!(f, "{e}"),
            Self::Generic(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Generic(s)
    }
}

impl From<nostrdb::Error> for Error {
    fn from(e: nostrdb::Error) -> Self {
        Error::Ndb(e)
    }
}

impl From<image::error::ImageError> for Error {
    fn from(err: image::error::ImageError) -> Self {
        Error::Image(err)
    }
}

impl From<enostr::Error> for Error {
    fn from(err: enostr::Error) -> Self {
        Error::Nostr(err)
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<FilterError> for Error {
    fn from(err: FilterError) -> Self {
        Error::Filter(err)
    }
}
