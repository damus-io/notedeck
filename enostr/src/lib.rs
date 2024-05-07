mod client;
mod error;
mod event;
mod filter;
mod keypair;
mod profile;
mod pubkey;
mod relay;

pub use client::ClientMessage;
pub use error::Error;
pub use event::{Event, EventId};
pub use ewebsock;
pub use filter::Filter;
pub use keypair::{FullKeypair, Keypair};
pub use nostr::SecretKey;
pub use profile::Profile;
pub use pubkey::Pubkey;
pub use relay::message::{RelayEvent, RelayMessage};
pub use relay::pool::{PoolEvent, RelayPool};
pub use relay::{Relay, RelayStatus};

pub type Result<T> = std::result::Result<T, error::Error>;
