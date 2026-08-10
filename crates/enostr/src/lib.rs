mod client;
mod error;
mod filter;
mod keypair;
mod note;
pub mod pns;
mod profile;
mod pubkey;
mod relay;
mod replaceable;
pub mod sns;

pub use client::{ClientMessage, EventClientMessage};
pub use error::Error;
pub use filter::Filter;
pub use keypair::{FilledKeypair, FullKeypair, Keypair, KeypairUnowned, SerializableKeypair};
pub use nostr::SecretKey;
pub use note::{Note, NoteId};
pub use profile::ProfileState;
pub use pubkey::{ParsedNprofile, Pubkey, PubkeyRef};
pub use relay::message::{RelayEvent, RelayMessage};
pub use relay::same_canonical_filter_set;
pub use relay::{
    EventChecker, FullHistoryConfig, FullHistorySubId, NegSetProvider, Nip11ApplyOutcome,
    Nip11FetchRequest, Nip11LimitationsRaw, NormRelayUrl, OutboxPool, OutboxRecvBudget,
    OutboxRecvResult, OutboxSession, OutboxSessionHandler, OutboxSubId, RelayCoordinatorLimits,
    RelayId, RelayImplType, RelayLimitations, RelayReqId, RelayReqStatus, RelayRoutingPreference,
    RelayStatus, RelayType, RelayUrlPkgs, SubPass, SubPassGuardian, SubPassRevocation,
    WebsocketConn, WsEvent, WsMessage,
};
pub use replaceable::{query_replaceable, query_replaceable_filtered};

pub type Result<T> = std::result::Result<T, error::Error>;

pub trait Wakeup: Send + Sync + Clone + 'static {
    fn wake(&self);
}
