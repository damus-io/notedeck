mod error;
mod event;
mod relay;

pub use error::Error;
pub use event::Event;
pub use relay::pool::RelayPool;
pub use relay::Relay;

pub type Result<T> = std::result::Result<T, error::Error>;
