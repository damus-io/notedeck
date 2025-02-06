use std::io;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("timeline not found")]
    TimelineNotFound,

    #[error("timeline is missing a subscription")]
    MissingSubscription,

    #[error("load failed")]
    LoadFailed,

    #[error("network error: {0}")]
    Nostr(#[from] enostr::Error),

    #[error("database error: {0}")]
    Ndb(#[from] nostrdb::Error),

    #[error("io error: {0}")]
    Io(#[from] io::Error),

    #[error("notedeck app error: {0}")]
    App(#[from] notedeck::Error),

    #[error("generic error: {0}")]
    Generic(String),
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Generic(s)
    }
}
