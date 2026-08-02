use std::time::{Duration, Instant};

use negentropy::{Negentropy, NegentropyStorageVector};
use nostrdb::Filter;

use crate::relay::{FullHistorySubId, RelayConnectionPriority, RelayDemandPriority, SubPass};

/// Time to wait for the first relay response to `NEG-OPEN`.
pub(super) const NEGENTROPY_OPEN_TIMEOUT: Duration = Duration::from_secs(120);
/// Time to wait for the next relay response after a session has advanced.
pub(super) const NEGENTROPY_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// One active relay-local negentropy session.
///
/// Only `protocol` is NIP-77 state. `target` and `relay_demand` are local
/// outbox bookkeeping retained here because they are created, refreshed, and
/// removed with the active relay session lifecycle.
pub(super) struct ActiveSession {
    pub(super) protocol: ActiveNegentropySession,
    pub(super) target: ActiveFullHistoryTarget,
    pub(super) relay_demand: ActiveSessionRelayDemand,
}

/// NIP-77 protocol state for one active relay-local session.
pub(super) struct ActiveNegentropySession {
    pub(super) neg: Negentropy<'static, NegentropyStorageVector>,
    pub(super) sub_pass: SubPass,
    pub(super) opened_at: Instant,
    pub(super) last_response_at: Option<Instant>,
}

/// Full-history target owning one active relay-local negentropy session.
pub(super) struct ActiveFullHistoryTarget {
    pub(super) filter: Filter,
    pub(super) owner_history_id: FullHistorySubId,
}

/// Websocket demand contributed by one active negentropy session.
///
/// This is not sent over NIP-77. Outbox admission uses it while the session is
/// active because an open negentropy exchange is active relay demand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ActiveSessionRelayDemand {
    pub(crate) priority: RelayConnectionPriority,
    pub(crate) connection_weight: u32,
}

impl ActiveSessionRelayDemand {
    /// Build demand for one active relay-local negentropy session.
    pub(crate) fn single(demand_priority: RelayDemandPriority, connection_weight: u32) -> Self {
        Self {
            priority: RelayConnectionPriority {
                strongest_demand: demand_priority,
                request_count: 1,
            },
            connection_weight,
        }
    }

    /// Merge optional active-session demands using the same ordering inputs as
    /// relay websocket admission.
    pub(crate) fn merge_optional(left: Option<Self>, right: Option<Self>) -> Option<Self> {
        match (left, right) {
            (Some(left), Some(right)) => Some(Self {
                priority: left.priority.merge(right.priority),
                connection_weight: left.connection_weight.max(right.connection_weight),
            }),
            (Some(demand), None) | (None, Some(demand)) => Some(demand),
            (None, None) => None,
        }
    }
}

impl ActiveSession {
    /// Creates one active relay-local negentropy session record.
    pub(super) fn new(
        neg: Negentropy<'static, NegentropyStorageVector>,
        sub_pass: SubPass,
        opened_at: Instant,
        filter: Filter,
        owner_history_id: FullHistorySubId,
        relay_demand: ActiveSessionRelayDemand,
    ) -> Self {
        Self {
            protocol: ActiveNegentropySession {
                neg,
                sub_pass,
                opened_at,
                last_response_at: None,
            },
            target: ActiveFullHistoryTarget {
                filter,
                owner_history_id,
            },
            relay_demand,
        }
    }

    /// Record a relay response for timeout accounting.
    pub(super) fn record_response(&mut self, now: Instant) {
        self.protocol.last_response_at = Some(now);
    }

    /// Returns when this session should be timed out if no relay response arrives.
    pub(super) fn timeout_deadline(&self) -> Instant {
        match self.protocol.last_response_at {
            Some(last_response_at) => last_response_at + NEGENTROPY_RESPONSE_TIMEOUT,
            None => self.protocol.opened_at + NEGENTROPY_OPEN_TIMEOUT,
        }
    }
}

/// Builds the local negentropy state machine and initial relay payload.
pub(super) fn prepare_negentropy(
    storage: NegentropyStorageVector,
) -> Option<(Negentropy<'static, NegentropyStorageVector>, String)> {
    let mut neg = Negentropy::owned(storage, 0).ok()?;
    let init_msg = neg.initiate().ok()?;
    Some((neg, hex::encode(&init_msg)))
}
