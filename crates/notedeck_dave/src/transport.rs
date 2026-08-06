//! Desktop implementation of the engine's [`Transport`] over notedeck's
//! [`RemoteApi`].
//!
//! This is the notedeck side of the boundary defined in `agentium-core`: it maps
//! the engine's host-neutral publish/subscribe requests onto notedeck's relay
//! facade. It is constructed per use from an `AppContext`'s `remote` + `accounts`
//! handles, so the `AppContext` itself never crosses into the engine — only this
//! thin adapter does.

use agentium_core::transport::{SubscriptionId, SubscriptionSpec, Transport};
use enostr::{NormRelayUrl, RelayId};
use notedeck::{
    Accounts, FullHistoryConfig, RemoteApi, ScopedSubIdentity, SubConfig, SubKey, SubOwnerKey,
};

/// Bridges the engine's [`Transport`] boundary onto notedeck's [`RemoteApi`].
///
/// Maps a [`SubscriptionSpec`] (target relay + live/history filters) onto a
/// notedeck [`SubConfig`], and a [`SubscriptionId`] onto an account-scoped
/// [`ScopedSubIdentity`]. Holds the `remote` facade and `accounts` for the
/// duration of one call; declare it right before a publish/subscribe and let it
/// drop.
pub struct RemoteApiTransport<'o, 'a> {
    remote: &'o mut RemoteApi<'a>,
    accounts: &'o Accounts,
}

impl<'o, 'a> RemoteApiTransport<'o, 'a> {
    /// Wrap the remote facade and selected-account state for one transport use.
    pub fn new(remote: &'o mut RemoteApi<'a>, accounts: &'o Accounts) -> Self {
        Self { remote, accounts }
    }
}

/// Resolve an engine [`SubscriptionId`] to notedeck's account-scoped identity.
///
/// Shared with the discovery-settle poll in `lib.rs` so the
/// `SubscriptionId -> ScopedSubIdentity` mapping has a single source of truth:
/// the settle check must query EOSE for the *same* scoped identity this
/// transport declared the subscription under.
pub(crate) fn scoped_identity(id: &SubscriptionId) -> ScopedSubIdentity {
    ScopedSubIdentity::account(SubOwnerKey::new(id.owner), SubKey::new(id.key))
}

impl Transport for RemoteApiTransport<'_, '_> {
    fn publish_event_json(&mut self, note_json: String, relays: Vec<NormRelayUrl>) {
        let relays = relays.into_iter().map(RelayId::Websocket).collect();
        self.remote
            .publisher_explicit()
            .publish_event_json(note_json, relays);
    }

    fn set_subscription(&mut self, spec: SubscriptionSpec) {
        let mut config = SubConfig::live(spec.live_filters).explicit_relay(spec.relay);
        // `FullHistoryConfig` requires a non-empty filter set; only declare a
        // backfill when the spec actually asks for one.
        if !spec.history_filters.is_empty() {
            config = config.full_history(FullHistoryConfig::new(spec.history_filters));
        }
        let _ = self
            .remote
            .scoped_subs(self.accounts)
            .set_sub(scoped_identity(&spec.id), config.build());
    }

    fn drop_subscription(&mut self, id: &SubscriptionId) {
        let _ = self
            .remote
            .scoped_subs(self.accounts)
            .drop_owner(SubOwnerKey::new(id.owner));
    }
}
