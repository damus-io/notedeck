use std::fmt;

#[derive(Debug)]
pub enum Error {
    NoActiveSubscription,
    Nostr(enostr::Error),
    Ndb(nostrdb::Error),
    Image(image::error::ImageError),
    Generic(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoActiveSubscription => {
                write!(f, "subscription not active in timeline")
            }
            Self::Nostr(e) => write!(f, "{e}"),
            Self::Ndb(e) => write!(f, "{e}"),
            Self::Image(e) => write!(f, "{e}"),
            Self::Generic(e) => write!(f, "{e}"),
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
