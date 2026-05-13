use hashbrown::HashMap;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use nostrdb::Filter;

use crate::relay::FullHistorySubId;
use crate::NoteId;

use super::{protocol::NegSessionId, session::ActiveSession};

/// Parsed `NEG-ERR` reason per NIP-77.
#[derive(Debug)]
pub(crate) enum NegErrKind {
    /// `"blocked:"` — filter matched too many records on the relay.
    Blocked,
    /// `"closed:"` — relay reclaimed the session due to inactivity.
    Closed,
    /// Unknown prefix — treat as transient.
    Unknown,
}

impl NegErrKind {
    /// Parses one raw relay NEG-ERR reason into a structured kind.
    pub(super) fn parse(reason: &str) -> Self {
        if reason.starts_with("blocked:") {
            Self::Blocked
        } else if reason.starts_with("closed:") {
            Self::Closed
        } else {
            Self::Unknown
        }
    }
}

/// Relay-local NIP-77 state hosted alongside the regular routing engines.
#[derive(Default)]
pub(crate) struct NegentropyData {
    pub(super) active_sessions: HashMap<NegSessionId, ActiveSession>,
    pub(super) capability: Option<bool>,
    pub(super) surfaced_need_ids: Vec<NegentropyNeed>,
    pub(super) retry_neg_sets: Vec<NegentropyRetry>,
    /// Filters that received `blocked:` from this relay.
    pub(super) blocked_filters: Vec<Filter>,
}

/// Relay-scoped missing event surfaced by one owner-tagged negentropy session.
#[derive(Clone, Debug)]
pub(crate) struct NegentropyNeed {
    pub(crate) owner_history_id: FullHistorySubId,
    pub(crate) filter: Filter,
    pub(crate) id: NoteId,
}

impl PartialEq for NegentropyNeed {
    fn eq(&self, other: &Self) -> bool {
        self.owner_history_id == other.owner_history_id
            && self.id == other.id
            && self.filter.same_canonical_attributes(&other.filter)
    }
}

impl Eq for NegentropyNeed {}

/// Relay-scoped negentropy session that should be retried after a transient
/// relay-side failure.
#[derive(Clone, Debug)]
pub(crate) struct NegentropyRetry {
    pub(crate) owner_history_id: FullHistorySubId,
    pub(crate) filter: Filter,
}

impl NegentropyData {
    /// Whether this relay still has negentropy work that needs polling.
    pub(crate) fn has_pending_work(&self) -> bool {
        !self.active_sessions.is_empty()
            || !self.surfaced_need_ids.is_empty()
            || !self.retry_neg_sets.is_empty()
    }

    /// Whether the relay is known to reject or ignore negentropy.
    pub(crate) fn is_unsupported(&self) -> bool {
        self.capability == Some(false)
    }

    /// Whether this relay has returned `blocked:` for the given filter.
    pub(crate) fn is_filter_blocked(&self, filter: &Filter) -> bool {
        self.blocked_filters
            .iter()
            .any(|blocked| blocked.same_canonical_attributes(filter))
    }

    /// Whether a relay-local session already covers one full-history
    /// owner/filter pair.
    pub(crate) fn has_active_session_for_owner_filter(
        &self,
        owner_history_id: FullHistorySubId,
        filter: &Filter,
    ) -> bool {
        self.active_sessions.values().any(|session| {
            session.owner_history_id == owner_history_id
                && session.filter.same_canonical_attributes(filter)
        })
    }

    /// Remember that this relay rejected one filter as too broad.
    pub(crate) fn block_filter(&mut self, filter: Filter) {
        if !self.is_filter_blocked(&filter) {
            self.blocked_filters.push(filter);
        }
    }

    /// Drain missing ids surfaced by completed sessions on this relay.
    pub(crate) fn drain_need_ids(&mut self) -> Vec<NegentropyNeed> {
        std::mem::take(&mut self.surfaced_need_ids)
    }

    /// Drain transient per-session retry requests for this relay.
    pub(crate) fn drain_retry_neg_sets(&mut self) -> Vec<NegentropyRetry> {
        std::mem::take(&mut self.retry_neg_sets)
    }

    /// Earliest active session timeout deadline for this relay, if any.
    pub(crate) fn next_timeout_deadline(&self) -> Option<Instant> {
        self.active_sessions
            .values()
            .map(ActiveSession::timeout_deadline)
            .min()
    }

    /// Remove active sessions whose current timeout has elapsed.
    pub(super) fn take_expired_sessions(
        &mut self,
        now: Instant,
    ) -> Vec<(NegSessionId, ActiveSession)> {
        let session_ids: Vec<NegSessionId> = self
            .active_sessions
            .iter()
            .filter(|(_, session)| session.timeout_deadline() <= now)
            .map(|(session_id, _)| session_id.clone())
            .collect();

        session_ids
            .into_iter()
            .filter_map(|session_id| {
                self.active_sessions
                    .remove(&session_id)
                    .map(|session| (session_id, session))
            })
            .collect()
    }

    /// Number of relay-local negentropy sessions currently holding passes.
    pub(crate) fn active_session_count(&self) -> usize {
        self.active_sessions.len()
    }

    #[cfg(test)]
    /// Seed one relay-local need for outbox tests below the NIP-77 parser layer.
    pub(crate) fn seed_need_for_test(
        &mut self,
        owner_history_id: FullHistorySubId,
        filter: Filter,
        id: NoteId,
    ) {
        self.surfaced_need_ids.push(NegentropyNeed {
            owner_history_id,
            filter,
            id,
        });
    }

    #[cfg(test)]
    /// Seed one relay-local retry for outbox tests below the NIP-77 parser layer.
    pub(crate) fn seed_retry_for_test(
        &mut self,
        owner_history_id: FullHistorySubId,
        filter: Filter,
    ) {
        self.retry_neg_sets.push(NegentropyRetry {
            owner_history_id,
            filter,
        });
    }

    #[cfg(test)]
    pub(crate) fn set_capability_for_test(&mut self, capability: Option<bool>) {
        self.capability = capability;
    }

    #[cfg(test)]
    pub(crate) fn age_sessions_for_test(&mut self, duration: Duration) {
        for session in self.active_sessions.values_mut() {
            session.opened_at -= duration;
            if let Some(last_response_at) = session.last_response_at.as_mut() {
                *last_response_at -= duration;
            }
        }
    }
}
