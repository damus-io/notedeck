mod client;
mod error;
mod filter;
mod keypair;
mod note;
mod profile;
mod pubkey;
mod relay;

pub use client::{ClientMessage, EventClientMessage};
pub use error::Error;
pub use ewebsock;
pub use filter::Filter;
pub use keypair::{FilledKeypair, FullKeypair, Keypair, KeypairUnowned, SerializableKeypair};
pub use nostr::SecretKey;
pub use note::{Note, NoteId};
pub use profile::ProfileState;
pub use pubkey::{Pubkey, PubkeyRef};
pub use relay::message::{RelayEvent, RelayMessage};
pub use relay::pool::{PoolEvent, PoolEventBuf, PoolRelay, RelayPool};
pub use relay::subs_debug::{OwnedRelayEvent, RelayLogEvent, SubsDebug, TransferStats};
pub use relay::{RelayStatus, WebsocketConn};

pub type Result<T> = std::result::Result<T, error::Error>;

pub trait Wakeup: Send + Sync + Clone + 'static {
    fn wake(&self);
}
