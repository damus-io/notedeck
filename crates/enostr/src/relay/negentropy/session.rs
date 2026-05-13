use std::time::{Duration, Instant};

use negentropy::{Negentropy, NegentropyStorageVector};
use nostrdb::Filter;

use crate::relay::{FullHistorySubId, SubPass};

/// Time to wait for the first relay response to `NEG-OPEN`.
pub(super) const NEGENTROPY_OPEN_TIMEOUT: Duration = Duration::from_secs(120);
/// Time to wait for the next relay response after a session has advanced.
pub(super) const NEGENTROPY_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// One active relay-local negentropy session.
pub(super) struct ActiveSession {
    pub(super) neg: Negentropy<'static, NegentropyStorageVector>,
    pub(super) sub_pass: SubPass,
    pub(super) opened_at: Instant,
    pub(super) last_response_at: Option<Instant>,
    pub(super) filter: Filter,
    pub(super) owner_history_id: FullHistorySubId,
}

impl ActiveSession {
    /// Creates one active relay-local negentropy session record.
    pub(super) fn new(
        neg: Negentropy<'static, NegentropyStorageVector>,
        sub_pass: SubPass,
        opened_at: Instant,
        filter: Filter,
        owner_history_id: FullHistorySubId,
    ) -> Self {
        Self {
            neg,
            sub_pass,
            opened_at,
            last_response_at: None,
            filter,
            owner_history_id,
        }
    }

    /// Record a relay response for timeout accounting.
    pub(super) fn record_response(&mut self, now: Instant) {
        self.last_response_at = Some(now);
    }

    /// Returns when this session should be timed out if no relay response arrives.
    pub(super) fn timeout_deadline(&self) -> Instant {
        match self.last_response_at {
            Some(last_response_at) => last_response_at + NEGENTROPY_RESPONSE_TIMEOUT,
            None => self.opened_at + NEGENTROPY_OPEN_TIMEOUT,
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
