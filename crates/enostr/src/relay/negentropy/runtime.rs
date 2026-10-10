use hashbrown::HashMap;
use negentropy::NegentropyStorageVector;
use nostrdb::Filter;
use std::time::Instant;

use crate::relay::{FullHistorySubId, NormRelayUrl, SubPass};

use super::{
    relay::NegentropyStartResult, session::ActiveSessionRelayDemand, state::NegentropyData,
};

/// When one relay's earliest active NIP-77 session times out.
pub(crate) struct RelayTimeoutDeadline {
    pub(crate) relay: NormRelayUrl,
    pub(crate) deadline: Instant,
}

/// Service-owned relay-scoped NIP-77 runtime state.
#[derive(Default)]
pub(crate) struct NegentropyRuntime {
    relays: HashMap<NormRelayUrl, NegentropyData>,
}

impl NegentropyRuntime {
    pub(crate) fn relay_mut(&mut self, relay: &NormRelayUrl) -> &mut NegentropyData {
        self.relays.entry(relay.clone()).or_default()
    }

    pub(crate) fn relay(&self, relay: &NormRelayUrl) -> Option<&NegentropyData> {
        self.relays.get(relay)
    }

    /// Earliest active NIP-77 timeout for a relay.
    pub(crate) fn next_timeout_deadline(&self, relay: &NormRelayUrl) -> Option<Instant> {
        self.relay(relay)?.next_timeout_deadline()
    }

    /// Earliest active NIP-77 timeout across relays accepted by `include_relay`.
    pub(crate) fn next_timeout_deadline_matching(
        &self,
        mut include_relay: impl FnMut(&NormRelayUrl) -> bool,
    ) -> Option<RelayTimeoutDeadline> {
        self.relays
            .iter()
            .filter_map(|(relay, data)| {
                let deadline = include_relay(relay)
                    .then(|| data.next_timeout_deadline())
                    .flatten()?;
                Some(RelayTimeoutDeadline {
                    relay: relay.clone(),
                    deadline,
                })
            })
            .min_by(|left, right| {
                left.deadline
                    .cmp(&right.deadline)
                    .then_with(|| left.relay.cmp(&right.relay))
            })
    }

    /// Whether a relay has NIP-77 work retained in the runtime.
    #[cfg(test)]
    pub(crate) fn has_work(&self, relay: &NormRelayUrl) -> bool {
        self.relay(relay)
            .is_some_and(|data| data.has_pending_work())
    }

    /// Start one relay-facing NIP-77 session using an already granted sub-pass.
    pub(crate) fn try_start_full_history(
        &mut self,
        relay: &NormRelayUrl,
        pass: SubPass,
        storage: impl FnOnce() -> NegentropyStorageVector,
        filter: Filter,
        owner_history_id: FullHistorySubId,
        relay_demand: ActiveSessionRelayDemand,
    ) -> NegentropyStartResult {
        let data = self.relay_mut(relay);
        data.try_start_full_history(pass, storage, filter, owner_history_id, relay_demand)
    }
}
