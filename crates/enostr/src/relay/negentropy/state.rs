use hashbrown::HashMap;
use negentropy::NegentropyStorageVector;
use std::time::Instant;

use nostrdb::Filter;

use crate::relay::{FullHistorySubId, SubPass};
use crate::{ClientMessage, NoteId};

use super::{
    protocol::{neg_open_msg, NegSessionId},
    relay::NegentropyStartResult,
    session::{prepare_negentropy, ActiveSession, ActiveSessionRelayDemand},
};

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

/// Parsed relay `NOTICE` text for negentropy capability handling.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum NegentropyNoticeKind {
    /// The relay explicitly rejected a negentropy client frame.
    Unsupported,
    /// The notice does not describe negentropy capability.
    Other,
}

impl NegentropyNoticeKind {
    /// Parses one raw relay NOTICE into a negentropy capability outcome.
    pub(super) fn parse(notice: &str) -> Self {
        let notice = notice.to_ascii_lowercase();
        if notice.contains("negentropy disabled")
            || notice.contains("unknown message type: neg-open")
            || notice.contains("unknown message type: neg-close")
        {
            return Self::Unsupported;
        }

        Self::Other
    }
}

/// Relay-local NIP-77 state hosted alongside the regular routing engines.
#[derive(Default)]
pub(crate) struct NegentropyData {
    pub(super) active_sessions: HashMap<NegSessionId, ActiveSession>,
    pub(super) capability: Option<bool>,
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
    #[cfg(test)]
    pub(crate) fn has_pending_work(&self) -> bool {
        !self.active_sessions.is_empty()
    }

    /// Whether this relay still has work for one full-history owner.
    #[cfg(test)]
    pub(crate) fn has_pending_work_for_owner(&self, owner_history_id: FullHistorySubId) -> bool {
        self.active_sessions
            .values()
            .any(|session| session.target.owner_history_id == owner_history_id)
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
            session.target.owner_history_id == owner_history_id
                && session.target.filter.same_canonical_attributes(filter)
        })
    }

    /// Start one relay-facing NIP-77 session using an already granted sub-pass.
    pub(crate) fn try_start_full_history(
        &mut self,
        pass: SubPass,
        storage: impl FnOnce() -> NegentropyStorageVector,
        filter: Filter,
        owner_history_id: FullHistorySubId,
        relay_demand: ActiveSessionRelayDemand,
    ) -> NegentropyStartResult {
        if self.is_unsupported()
            || self.is_filter_blocked(&filter)
            || self.has_active_session_for_owner_filter(owner_history_id, &filter)
        {
            return NegentropyStartResult::Rejected(pass);
        }

        let filter_json = match filter.json() {
            Ok(filter_json) => filter_json,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    filter_elements = filter.num_elements(),
                    "negentropy could not serialize NEG-OPEN filter"
                );
                return NegentropyStartResult::Rejected(pass);
            }
        };

        let (neg, init_hex) = match prepare_negentropy(storage()) {
            Some(value) => value,
            None => return NegentropyStartResult::Rejected(pass),
        };

        let session_id = NegSessionId::new(uuid::Uuid::new_v4().to_string());
        let msg: ClientMessage = neg_open_msg(&session_id, filter_json, &init_hex);
        self.active_sessions.insert(
            session_id.clone(),
            ActiveSession::new(
                neg,
                pass,
                Instant::now(),
                filter,
                owner_history_id,
                relay_demand,
            ),
        );
        NegentropyStartResult::Started(msg)
    }

    /// Remember that this relay rejected one filter as too broad.
    pub(crate) fn block_filter(&mut self, filter: Filter) {
        if !self.is_filter_blocked(&filter) {
            self.blocked_filters.push(filter);
        }
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

    /// Return one active session id for coordinator tests.
    #[cfg(test)]
    pub(crate) fn first_active_session_id_for_test(&self) -> Option<String> {
        self.active_sessions
            .keys()
            .next()
            .map(|session_id| session_id.as_str().to_owned())
    }

    /// Return aggregate relay demand from every active relay-local session.
    pub(crate) fn active_session_relay_demand(&self) -> Option<ActiveSessionRelayDemand> {
        self.active_sessions.values().fold(None, |demand, session| {
            ActiveSessionRelayDemand::merge_optional(demand, Some(session.relay_demand))
        })
    }

    /// Refresh active relay-local session demand for a retained owner/filter.
    pub(crate) fn refresh_active_session_relay_demand_for_owner_filter(
        &mut self,
        owner_history_id: FullHistorySubId,
        filter: &Filter,
        relay_demand: ActiveSessionRelayDemand,
    ) {
        for session in self.active_sessions.values_mut() {
            if session.target.owner_history_id == owner_history_id
                && session.target.filter.same_canonical_attributes(filter)
            {
                session.relay_demand = relay_demand;
            }
        }
    }
}
