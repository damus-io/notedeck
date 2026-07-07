use crate::{
    relay::{
        coordinator::FullHistoryNegentropyCapacityGrant, negentropy::NegentropyRelayEffect,
        NormRelayUrl, OutboxSubId, RelayReqStatus,
    },
    ClientMessage,
};

use super::{OutboxSubRelayEose, RelayTransportDemand};

/// Committed read-model fact emitted by [`super::OutboxPool`] protocol transitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::relay) enum OutboxPoolFact {
    RelayReqStatus {
        id: OutboxSubId,
        relay: NormRelayUrl,
        status: Option<RelayReqStatus>,
    },
    OutboxSubRelayEose {
        id: OutboxSubId,
        relay_eose: Option<OutboxSubRelayEose>,
    },
}

/// Transport intent emitted by pool protocol transitions.
#[derive(Clone, Debug)]
pub(in crate::relay) enum OutboxTransportEffect {
    SendRelayFrame {
        relay: NormRelayUrl,
        generation: u64,
        message: ClientMessage,
    },
}

/// Exact aggregate relay-demand change caused by one pool transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::relay) struct RelayDemandChanged {
    pub(in crate::relay) relay: NormRelayUrl,
    pub(in crate::relay) demand: Option<RelayTransportDemand>,
}

/// Full-history/negentropy consequence produced by one pool transition.
#[derive(Debug)]
pub(in crate::relay) enum OutboxFullHistoryEffect {
    NegentropyCapacityGranted {
        relay: NormRelayUrl,
        grant: FullHistoryNegentropyCapacityGrant,
    },
    NegentropyEffect {
        relay: NormRelayUrl,
        effect: NegentropyRelayEffect,
    },
}

/// Output returned by one synchronous [`super::OutboxPool`] transition.
#[derive(Debug, Default)]
pub(in crate::relay) struct OutboxPoolOutput {
    pub(in crate::relay) facts: Vec<OutboxPoolFact>,
    pub(in crate::relay) relay_demand_changes: Vec<RelayDemandChanged>,
    pub(in crate::relay) transport_effects: Vec<OutboxTransportEffect>,
    pub(in crate::relay) full_history_effects: Vec<OutboxFullHistoryEffect>,
}

impl OutboxPoolOutput {
    pub(in crate::relay) fn extend(&mut self, output: OutboxPoolOutput) {
        self.facts.extend(output.facts);
        self.relay_demand_changes
            .extend(output.relay_demand_changes);
        self.transport_effects.extend(output.transport_effects);
        self.full_history_effects
            .extend(output.full_history_effects);
    }
}
