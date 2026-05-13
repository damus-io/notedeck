use std::time::Instant;

use negentropy::{Id, NegentropyStorageVector};
use nostrdb::Filter;

use crate::{
    relay::{FullHistorySubId, SubPassGuardian, SubPassRevocation, WebsocketRelay},
    ClientMessage, NoteId,
};

use super::{
    protocol::{neg_close_msg, neg_msg, neg_open_msg, NegSessionId},
    session::{prepare_negentropy, ActiveSession},
    state::{NegErrKind, NegentropyData, NegentropyNeed, NegentropyRetry},
};

/// Borrow wrapper over relay-local negentropy state plus pass accounting.
pub(crate) struct NegentropyRelay<'a> {
    relay: Option<&'a mut WebsocketRelay>,
    data: &'a mut NegentropyData,
    sub_guardian: &'a mut SubPassGuardian,
}

impl<'a> NegentropyRelay<'a> {
    /// Creates one scoped relay-local negentropy operator.
    pub(crate) fn new(
        relay: Option<&'a mut WebsocketRelay>,
        data: &'a mut NegentropyData,
        sub_guardian: &'a mut SubPassGuardian,
    ) -> Self {
        Self {
            relay,
            data,
            sub_guardian,
        }
    }

    /// Attempt to start a new relay-local negentropy session.
    pub(crate) fn try_initiate(
        &mut self,
        storage: NegentropyStorageVector,
        filter: Filter,
        filter_json: String,
        owner_history_id: FullHistorySubId,
    ) -> Option<ClientMessage> {
        let pass = self.sub_guardian.take_pass()?;
        let (neg, init_hex) = match prepare_negentropy(storage) {
            Some(value) => value,
            None => {
                self.sub_guardian.return_pass(pass);
                return None;
            }
        };

        let session_id = NegSessionId::new(uuid::Uuid::new_v4().to_string());
        let msg = neg_open_msg(&session_id, filter_json, &init_hex);
        self.data.active_sessions.insert(
            session_id.clone(),
            ActiveSession::new(neg, pass, Instant::now(), filter, owner_history_id),
        );
        Some(msg)
    }

    /// Handle one `NEG-MSG` round-trip from the relay.
    pub(crate) fn handle_neg_msg(
        &mut self,
        session_id: &str,
        payload_hex: &str,
    ) -> Option<ClientMessage> {
        let now = Instant::now();
        let Some(mut session) = self.data.active_sessions.remove(session_id) else {
            tracing::warn!(
                session_id,
                "negentropy received NEG-MSG for unknown session"
            );
            return None;
        };

        let mut have_ids = Vec::new();
        let mut need_ids = Vec::new();
        let payload = match hex::decode(payload_hex) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!(session_id, "negentropy hex decode: {err}");
                let session_id = NegSessionId::new(session_id.to_owned());
                self.fail_session_or_mark_unsupported(&session_id, session);
                return None;
            }
        };

        let owner_history_id = session.owner_history_id;
        let result = session
            .neg
            .reconcile_with_ids(&payload, &mut have_ids, &mut need_ids);
        let surfaced_filter = (!need_ids.is_empty()).then(|| session.filter.clone());

        match result {
            Ok(Some(next_msg)) => {
                session.record_response(now);
                self.data.capability = Some(true);
                if let Some(filter) = surfaced_filter.as_ref() {
                    self.surface_need_ids(owner_history_id, filter, need_ids);
                }
                let session_id = NegSessionId::new(session_id.to_owned());
                self.data
                    .active_sessions
                    .insert(session_id.clone(), session);
                Some(neg_msg(&session_id, &hex::encode(next_msg)))
            }
            Ok(None) => {
                self.data.capability = Some(true);
                if let Some(filter) = surfaced_filter.as_ref() {
                    self.surface_need_ids(owner_history_id, filter, need_ids);
                }
                self.sub_guardian.return_pass(session.sub_pass);
                let session_id = NegSessionId::new(session_id.to_owned());
                Some(neg_close_msg(&session_id))
            }
            Err(err) => {
                tracing::warn!(session_id, "negentropy reconcile: {err}");
                let session_id = NegSessionId::new(session_id.to_owned());
                self.fail_session_or_mark_unsupported(&session_id, session);
                None
            }
        }
    }

    /// Handle one relay `NEG-ERR` message.
    ///
    /// Per NIP-77, NEG-ERR means the relay *supports* negentropy but rejected
    /// this specific request. `blocked:` means the filter is too broad,
    /// `closed:` means the session timed out. Neither marks the relay as
    /// unsupported.
    pub(crate) fn handle_neg_err(&mut self, session_id: &str, reason: &str) {
        let kind = NegErrKind::parse(reason);
        let Some(session) = self.data.active_sessions.remove(session_id) else {
            tracing::warn!(
                session_id,
                reason,
                "negentropy received NEG-ERR for unknown session"
            );
            return;
        };

        tracing::warn!(
            session_id,
            owner_history_id = ?session.owner_history_id,
            kind = ?kind,
            reason,
            "negentropy NEG-ERR"
        );
        self.data.capability = Some(true);
        self.sub_guardian.return_pass(session.sub_pass);

        if matches!(kind, NegErrKind::Blocked) {
            self.data.block_filter(session.filter);
        } else {
            self.data.retry_neg_sets.push(NegentropyRetry {
                owner_history_id: session.owner_history_id,
                filter: session.filter,
            });
        }
    }

    /// Expire sessions without recent relay responses.
    pub(crate) fn handle_timeout(&mut self, now: Instant) {
        if self.data.is_unsupported() {
            return;
        }

        let expired = self
            .data
            .next_timeout_deadline()
            .is_some_and(|deadline| deadline <= now);
        if !expired {
            return;
        }

        tracing::warn!("negentropy timed out waiting for relay response");
        if self.data.capability == Some(true) {
            self.retry_expired_sessions(now);
        } else {
            self.mark_unsupported();
        }
    }

    /// Drop all relay-local sessions on disconnect.
    pub(crate) fn handle_relay_disconnect(&mut self) {
        let sessions = self.remove_all_sessions_collect();
        for (_, session) in sessions {
            self.sub_guardian.return_pass(session.sub_pass);
            self.data.retry_neg_sets.push(NegentropyRetry {
                owner_history_id: session.owner_history_id,
                filter: session.filter,
            });
        }
    }

    /// Revocate passes held by relay-local sessions selected for limit reduction.
    pub(crate) fn revocate_sessions(&mut self, revocations: Vec<SubPassRevocation>) {
        if revocations.is_empty() {
            return;
        }

        let session_ids: Vec<NegSessionId> = self
            .data
            .active_sessions
            .keys()
            .take(revocations.len())
            .cloned()
            .collect();

        for session_id in &session_ids {
            self.send_neg_close(session_id);
        }

        for (session_id, mut revocation) in session_ids.into_iter().zip(revocations) {
            let Some(session) = self.data.active_sessions.remove(&session_id) else {
                continue;
            };

            revocation.revocate(session.sub_pass);
            self.data.retry_neg_sets.push(NegentropyRetry {
                owner_history_id: session.owner_history_id,
                filter: session.filter,
            });
        }
    }

    /// Cancel all relay-local negentropy sessions and surfaced needs owned by
    /// one full-history subscription.
    pub(crate) fn cancel_owner(&mut self, owner_history_id: FullHistorySubId) {
        self.cancel_matching_work(|candidate_owner_history_id, _| {
            candidate_owner_history_id == owner_history_id
        });
    }

    /// Cancel relay-local negentropy work owned by one sub for the given filters.
    pub(crate) fn cancel_owner_filters(
        &mut self,
        owner_history_id: FullHistorySubId,
        filters: &[Filter],
    ) {
        if filters.is_empty() {
            return;
        }

        self.cancel_matching_work(|candidate_owner_history_id, candidate_filter| {
            candidate_owner_history_id == owner_history_id
                && filters
                    .iter()
                    .any(|filter| filter.same_canonical_attributes(candidate_filter))
        });
    }

    fn surface_need_ids(
        &mut self,
        owner_history_id: FullHistorySubId,
        filter: &Filter,
        need_ids: Vec<Id>,
    ) {
        let ids: Vec<NoteId> = need_ids
            .into_iter()
            .map(|id| NoteId::new(id.to_bytes()))
            .collect();
        self.data
            .surfaced_need_ids
            .extend(ids.into_iter().map(|id| NegentropyNeed {
                owner_history_id,
                filter: filter.clone(),
                id,
            }));
    }

    fn cancel_matching_work(
        &mut self,
        mut should_cancel: impl FnMut(FullHistorySubId, &Filter) -> bool,
    ) {
        let session_ids: Vec<NegSessionId> = self
            .data
            .active_sessions
            .iter()
            .filter(|(_, session)| should_cancel(session.owner_history_id, &session.filter))
            .map(|(session_id, _)| session_id.clone())
            .collect();

        for session_id in &session_ids {
            self.send_neg_close(session_id);
        }

        for session_id in session_ids {
            self.remove_session(session_id.as_str());
        }

        self.data
            .surfaced_need_ids
            .retain(|need| !should_cancel(need.owner_history_id, &need.filter));
        self.data
            .retry_neg_sets
            .retain(|retry| !should_cancel(retry.owner_history_id, &retry.filter));
    }

    fn remove_session(&mut self, session_id: &str) {
        if let Some(session) = self.data.active_sessions.remove(session_id) {
            self.sub_guardian.return_pass(session.sub_pass);
        }
    }

    fn remove_all_sessions(&mut self) {
        for (_, session) in self.remove_all_sessions_collect() {
            self.sub_guardian.return_pass(session.sub_pass);
        }
    }

    fn retry_expired_sessions(&mut self, now: Instant) {
        for (session_id, session) in self.data.take_expired_sessions(now) {
            self.send_neg_close(&session_id);
            tracing::warn!(
                session_id = session_id.as_str(),
                owner_history_id = ?session.owner_history_id,
                received_response = session.last_response_at.is_some(),
                elapsed_ms = now
                    .saturating_duration_since(session.last_response_at.unwrap_or(session.opened_at))
                    .as_millis(),
                "negentropy retrying timed-out session"
            );
            self.retry_session(session);
        }
    }

    fn send_neg_close(&mut self, session_id: &NegSessionId) {
        let Some(relay) = self.relay.as_mut() else {
            return;
        };
        if !relay.is_connected() {
            return;
        }
        relay.conn.send(&neg_close_msg(session_id));
    }

    fn fail_session_or_mark_unsupported(
        &mut self,
        session_id: &NegSessionId,
        session: ActiveSession,
    ) {
        self.send_neg_close(session_id);
        if self.data.capability == Some(true) {
            tracing::warn!(
                session_id = session_id.as_str(),
                owner_history_id = ?session.owner_history_id,
                "negentropy retrying failed session"
            );
            self.retry_session(session);
        } else {
            self.sub_guardian.return_pass(session.sub_pass);
            self.mark_unsupported();
        }
    }

    fn retry_session(&mut self, session: ActiveSession) {
        self.sub_guardian.return_pass(session.sub_pass);
        self.data.retry_neg_sets.push(NegentropyRetry {
            owner_history_id: session.owner_history_id,
            filter: session.filter,
        });
    }

    /// Mark the relay unsupported for negentropy and clear all active sessions.
    fn mark_unsupported(&mut self) {
        tracing::warn!(
            active_sessions = self.data.active_session_count(),
            "negentropy marking relay unsupported"
        );
        self.data.capability = Some(false);
        self.remove_all_sessions();
    }

    fn remove_all_sessions_collect(&mut self) -> Vec<(NegSessionId, ActiveSession)> {
        let mut sessions = Vec::new();
        for (session_id, session) in self.data.active_sessions.drain() {
            sessions.push((session_id, session));
        }
        sessions
    }
}
