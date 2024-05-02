use std::{fmt, io};

#[derive(Debug)]
pub enum Error {
    NoActiveSubscription,
    LoadFailed,
    Io(io::Error),
    Nostr(enostr::Error),
    Ndb(nostrdb::Error),
    Image(image::error::ImageError),
    Anyhow(anyhow::Error),
    //Eframe(eframe::Error),
    Generic(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoActiveSubscription => {
                write!(f, "subscription not active in timeline")
            }
            Self::LoadFailed => {
                write!(f, "load failed")
            }
            Self::Nostr(e) => write!(f, "{e}"),
            Self::Ndb(e) => write!(f, "{e}"),
            Self::Image(e) => write!(f, "{e}"),
            Self::Generic(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Anyhow(e) => write!(f, "{e}"),
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

impl From<anyhow::Error> for Error {
    fn from(err: anyhow::Error) -> Self {
        Error::Anyhow(err)
    }
}

impl From<enostr::Error> for Error {
    fn from(err: enostr::Error) -> Self {
        Error::Nostr(err)
    }
}

/*
impl From<eframe::Error> for Error {
    fn from(err: eframe::Error) -> Self {
        Error::Eframe(err)
    }
}
*/

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}
