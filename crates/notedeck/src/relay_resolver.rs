//! The relay-set resolver seam.
//!
//! The remote outbox / scoped-subscription layer needs three pieces of
//! per-account state to route subscriptions and publishes: the selected
//! account's public key and its effective read / write relay sets. Historically
//! that layer reached directly into [`crate::Accounts`] for them.
//!
//! [`RelaySetResolver`] abstracts exactly that dependency so the
//! scoped-subscription engine can be parameterized on a resolver instead of a
//! concrete `Accounts`. This is the seam that lets the engine eventually move
//! into `nostrdb_net` (which must not depend on notedeck's `Accounts`): the
//! engine depends only on this trait, and notedeck supplies the implementation.

use enostr::{NormRelayUrl, RelayId};
use hashbrown::HashSet;
use nostrdb_net::Pubkey;

/// Resolves the selected account's identity and relay sets for the remote
/// outbox / scoped-subscription layer, without coupling that layer to the
/// concrete `Accounts` type.
pub trait RelaySetResolver {
    /// The currently selected account's public key.
    fn selected_account_pubkey(&self) -> Pubkey;

    /// The selected account's effective read relay set (NIP-65 advertised read
    /// relays merged with configured defaults).
    fn selected_account_read_relays(&self) -> HashSet<NormRelayUrl>;

    /// The selected account's effective write relay targets (NIP-65 advertised
    /// write relays merged with configured defaults).
    fn selected_account_write_relays(&self) -> Vec<RelayId>;
}
