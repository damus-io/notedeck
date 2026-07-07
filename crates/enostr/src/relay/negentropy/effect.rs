use std::time::Instant;

use crate::{
    relay::{FullHistorySubId, SubPassRevocation},
    Filter,
};

/// Relay-local negentropy work selected by one relay coordination transition.
#[derive(Debug)]
pub(in crate::relay) enum NegentropyRelayEffect {
    RevocateSessions {
        generation: Option<u64>,
        revocations: Vec<SubPassRevocation>,
    },
    RelayDisconnect,
    Timeout {
        generation: Option<u64>,
        now: Instant,
    },
    CancelOwner {
        generation: Option<u64>,
        owner_history_id: FullHistorySubId,
    },
    CancelOwnerFilters {
        generation: Option<u64>,
        owner_history_id: FullHistorySubId,
        filters: Vec<Filter>,
    },
    DropSessionsWithoutNegClose,
}
