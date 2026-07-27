//! Platform-neutral publish/subscribe boundary for the agentium engine.
//!
//! The engine never touches a concrete relay stack directly. Instead it speaks
//! to a [`Transport`]: publish a pre-serialized event to relays, and declare or
//! drop a durable subscription. Desktop `notedeck_dave` implements this over
//! notedeck's `RemoteApi`; the standalone/iOS engine implements it over its own
//! relay-sync loop (see the `grape-cause-early` card). Keeping the boundary in
//! terms of plain nostr types (`NormRelayUrl`, `Filter`) means notedeck's
//! `AppContext` never crosses it.

use enostr::NormRelayUrl;
use nostrdb::Filter;

/// Stable identity for a durable engine-owned subscription.
///
/// `owner` is a lifecycle token — dropping it (see [`Transport::drop_subscription`])
/// releases every subscription it declared. `key` distinguishes multiple
/// subscriptions declared under the same owner. Desktop dave maps this onto
/// notedeck's account-scoped `ScopedSubIdentity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SubscriptionId {
    /// Owner lifecycle token (for example `"dave/pns"`).
    pub owner: &'static str,
    /// Sub key within the owner (for example `"pns"`).
    pub key: &'static str,
}

impl SubscriptionId {
    /// Construct a subscription identity from its owner and key tokens.
    pub const fn new(owner: &'static str, key: &'static str) -> Self {
        Self { owner, key }
    }
}

/// A durable subscription declaration targeted at one explicit relay.
///
/// `live_filters` drive the ongoing subscription; `history_filters` (possibly
/// empty) request a bounded backfill of past events. Both are plain nostr
/// filters so any transport can realize them however it likes.
#[derive(Clone, Debug)]
pub struct SubscriptionSpec {
    /// Stable identity used to later drop this subscription.
    pub id: SubscriptionId,
    /// The explicit relay this subscription targets.
    pub relay: NormRelayUrl,
    /// Filters for the ongoing (live) portion of the subscription.
    pub live_filters: Vec<Filter>,
    /// Optional filters for a bounded historical backfill.
    pub history_filters: Vec<Filter>,
}

/// Minimal publish/subscribe boundary between the engine and a relay stack.
///
/// This is the only relay-facing surface the engine depends on. It is
/// deliberately small: one publish path and a declare/drop pair for durable
/// subscriptions. It carries no notedeck `AppContext` and no egui types, so the
/// same engine logic runs against desktop dave's `RemoteApi` and against the
/// standalone owned relay loop.
pub trait Transport {
    /// Publish a pre-serialized event JSON to each of `relays`.
    fn publish_event_json(&mut self, note_json: String, relays: Vec<NormRelayUrl>);

    /// Declare (upsert) a durable subscription.
    fn set_subscription(&mut self, spec: SubscriptionSpec);

    /// Drop the subscription previously declared under `id`'s owner, releasing
    /// every subscription that owner declared.
    fn drop_subscription(&mut self, id: &SubscriptionId);
}
