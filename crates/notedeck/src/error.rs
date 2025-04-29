use std::io;

/// App related errors
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("image error: {0}")]
    Image(#[from] image::error::ImageError),

    #[error("io error: {0}")]
    Io(#[from] io::Error),

    #[error("subscription error: {0}")]
    SubscriptionError(SubscriptionError),

    #[error("filter error: {0}")]
    Filter(FilterError),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Nostrdb(#[from] nostrdb::Error),

    #[error("generic error: {0}")]
    Generic(String),

    #[error("zaps error: {0}")]
    Zap(#[from] ZapError),
}

#[derive(Debug, thiserror::Error, Clone)]
pub enum ZapError {
    #[error("invalid lud16")]
    InvalidLud16(String),
    #[error("invalid endpoint response")]
    EndpointError(String),
    #[error("bech encoding/decoding error")]
    Bech(String),
    #[error("serialization/deserialization problem")]
    Serialization(String),
    #[error("nwc error")]
    NWC(String),
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Generic(s)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum FilterError {
    #[error("empty contact list")]
    EmptyContactList,

    #[error("filter not ready")]
    FilterNotReady,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone, thiserror::Error)]
pub enum SubscriptionError {
    #[error("no active subscriptions")]
    NoActive,

    /// When a timeline has an unexpected number
    /// of active subscriptions. Should only happen if there
    /// is a bug in notedeck
    #[error("unexpected subscription count")]
    UnexpectedSubscriptionCount(i32),
}

impl Error {
    pub fn unexpected_sub_count(c: i32) -> Self {
        Error::SubscriptionError(SubscriptionError::UnexpectedSubscriptionCount(c))
    }

    pub fn no_active_sub() -> Self {
        Error::SubscriptionError(SubscriptionError::NoActive)
    }

    pub fn empty_contact_list() -> Self {
        Error::Filter(FilterError::EmptyContactList)
    }
}

pub fn show_one_error_message(ui: &mut egui::Ui, message: &str) {
    let id = ui.id().with(("error", message));
    let res: Option<()> = ui.ctx().data(|d| d.get_temp(id));

    if res.is_none() {
        ui.ctx().data_mut(|d| d.insert_temp(id, ()));
        tracing::error!(message);
    }
}
