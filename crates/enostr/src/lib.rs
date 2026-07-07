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
#[cfg(test)]
mod test_support;

pub use client::{ClientMessage, EventClientMessage};
pub use error::{Error, WebSocketError};
pub use ewebsock;
pub use filter::Filter;
pub use keypair::{FilledKeypair, FullKeypair, Keypair, KeypairUnowned, SerializableKeypair};
pub use nostr::SecretKey;
pub use note::{Note, NoteId};
pub use profile::ProfileState;
pub use pubkey::{Pubkey, PubkeyRef};
pub use relay::message::{RelayEvent, RelayMessage};
pub use relay::same_canonical_filter_set;
pub use relay::{
    full_history_targets_have_work, EventIngestCapability, EventIngestRequest,
    FullHistoryCapability, FullHistoryConfig, FullHistoryLocalPresenceRequest,
    FullHistoryLocalPresenceResult, FullHistoryLocalSetRequest, FullHistoryLocalSetResult,
    FullHistoryPendingIngestionPresenceRequest, FullHistoryPendingIngestionPresenceResult,
    FullHistorySubId, FullHistoryTarget, Nip11ApplyOutcome, Nip11Capability, Nip11FetchRequest,
    Nip11LimitationsRaw, NormRelayUrl, OutboxEvent, OutboxIdRegistry, OutboxService,
    OutboxServiceConfig, OutboxServiceOutput, OutboxSubId, OutboxSubRelayEose, RawEventData,
    RelayCoordinatorLimits, RelayDemandPriority, RelayId, RelayImplType, RelayLegReadiness,
    RelayLimitations, RelayReqId, RelayReqStatus, RelayRoutingPreference, RelayStatus, RelayType,
    RelayUrlPkgs, RelayUrlPolicy, RelayUrlSource,
};
pub use replaceable::{query_replaceable, query_replaceable_filtered};

pub type Result<T> = std::result::Result<T, error::Error>;
