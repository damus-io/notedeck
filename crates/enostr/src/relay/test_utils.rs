//! Test utilities for relay testing
//!
//! This module provides mock implementations and helpers for unit and integration tests.

use hashbrown::HashSet;
use nostrdb::Filter;

use crate::relay::{
    NormRelayUrl, OutboxSession, OutboxSubId, OutboxSubscriptions, OutboxTask,
    RelayRoutingPreference, RelayUrlPkgs, SubscribeTask,
};
use crate::Wakeup;

/// A mock Wakeup implementation that tracks how many times wake() was called.
///
/// This is useful for unit tests to verify that wakeups are triggered correctly
/// without needing a real UI/event loop.
#[derive(Clone)]
pub struct MockWakeup {}

impl MockWakeup {
    /// Create a new MockWakeup with zero wakeup count.
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for MockWakeup {
    fn default() -> Self {
        Self::new()
    }
}

impl Wakeup for MockWakeup {
    fn wake(&self) {}
}

/// Returns a task for `id`, panicking when the task is missing.
#[track_caller]
pub fn expect_task(session: &OutboxSession, id: OutboxSubId) -> &OutboxTask {
    session
        .tasks
        .get(&id)
        .unwrap_or_else(|| panic!("Expected task for {:?}", id))
}

// ==================== SubRegistry tests ====================

pub fn trivial_filter() -> Vec<Filter> {
    vec![Filter::new().kinds(vec![1]).build()]
}

pub fn filters_json(filters: &[Filter]) -> Vec<String> {
    filters
        .iter()
        .map(|f| f.json().expect("serialize filter to json"))
        .collect()
}

/// Inserts a standard test subscription using the provided routing preference.
pub fn insert_sub_with_policy(
    subs: &mut OutboxSubscriptions,
    id: OutboxSubId,
    policy: RelayRoutingPreference,
) {
    let mut relays = RelayUrlPkgs::new(HashSet::new());
    relays.routing_preference = policy;
    subs.new_subscription(
        id,
        SubscribeTask {
            filters: vec![Filter::new().kinds([1]).limit(1).build()],
            relays,
        },
        false,
    );
}

/// Inserts a standard test subscription pinned to one relay and routing preference.
pub fn insert_sub_with_policy_for_relay(
    subs: &mut OutboxSubscriptions,
    id: OutboxSubId,
    policy: RelayRoutingPreference,
    relay_url: &str,
) {
    let mut urls = HashSet::new();
    urls.insert(NormRelayUrl::new(relay_url).expect("valid test relay url"));
    let mut relays = RelayUrlPkgs::new(urls);
    relays.routing_preference = policy;
    subs.new_subscription(
        id,
        SubscribeTask {
            filters: vec![Filter::new().kinds([1]).limit(1).build()],
            relays,
        },
        false,
    );
}
