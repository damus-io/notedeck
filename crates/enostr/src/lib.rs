mod client;
mod error;
mod filter;
mod keypair;
mod note;
// The NIP-PNS module now lives in `nostrdb_net`; re-export it so `enostr::pns`
// still resolves for consumers (collapse-via-reexport).
pub use nostrdb_net::pns;
mod profile;
mod pubkey;
mod relay;
mod replaceable;
// The NIP-SNS module now lives in `nostrdb_net`; re-export it so `enostr::sns`
// still resolves for consumers (collapse-via-reexport).
pub use nostrdb_net::sns;

pub use client::{ClientMessage, EventClientMessage};
pub use error::Error;
pub use filter::Filter;
pub use keypair::{FilledKeypair, FullKeypair, Keypair, KeypairUnowned, SerializableKeypair};
pub use nostr::SecretKey;
pub use note::{Note, NoteId};
pub use profile::ProfileState;
pub use pubkey::{Pubkey, PubkeyRef};
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

/// Install the process-wide rustls [`CryptoProvider`] used for `wss://` relay
/// connections.
///
/// rustls 0.23 requires an explicit process-level provider whenever the
/// aws-lc-rs vs ring backend is ambiguous; the first TLS handshake otherwise
/// panics in `get_default_or_install_from_crate_features`. The notedeck app
/// installs one during its own startup (`notedeck::install_crypto`), but the
/// standalone CLIs (agentium/headway/notebook) drive enostr's relay/sync stack
/// directly and never run that init, so they must install it themselves before
/// opening any relay. Call this once at the top of `main`.
///
/// Idempotent: the [`Err`] returned when a provider is already installed is
/// ignored, so calling it more than once (or after notedeck already installed
/// one) is harmless.
///
/// Matches notedeck's platform choice: ring on Windows (fewer build
/// requirements than aws-lc-rs, which needs cmake/NASM), aws-lc-rs elsewhere.
///
/// [`CryptoProvider`]: rustls::crypto::CryptoProvider
pub fn install_crypto() {
    #[cfg(windows)]
    let provider = rustls::crypto::ring::default_provider();
    #[cfg(not(windows))]
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    let _ = provider.install_default();
}
