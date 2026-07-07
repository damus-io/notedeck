use std::time::Instant;

use negentropy::Id;
use nostrdb::Filter;

use crate::{
    relay::{frame::RelayFrameSink, FullHistorySubId, SubPass},
    ClientMessage, NoteId,
};

use super::{
    protocol::{neg_close_msg, neg_msg, NegSessionId},
    session::ActiveSession,
    state::{NegErrKind, NegentropyData, NegentropyNeed, NegentropyNoticeKind, NegentropyRetry},
};

/// Explicit effects returned by relay-local negentropy transitions.
#[derive(Default)]
pub(crate) struct NegentropyRelayEffects {
    pub(crate) returned_passes: Vec<SubPass>,
    pub(crate) needs: Vec<NegentropyNeed>,
    pub(crate) retries: Vec<NegentropyRetry>,
}

impl NegentropyRelayEffects {
    fn return_pass(&mut self, pass: SubPass) {
        self.returned_passes.push(pass);
    }

    fn retry(&mut self, owner_history_id: FullHistorySubId, filter: Filter) {
        self.retries.push(NegentropyRetry {
            owner_history_id,
            filter,
        });
    }

    fn extend(&mut self, other: Self) {
        self.returned_passes.extend(other.returned_passes);
        self.needs.extend(other.needs);
        self.retries.extend(other.retries);
    }

    pub(crate) fn take_needs(&mut self) -> Vec<NegentropyNeed> {
        std::mem::take(&mut self.needs)
    }

    pub(crate) fn take_retries(&mut self) -> Vec<NegentropyRetry> {
        std::mem::take(&mut self.retries)
    }
}

/// Explicit effects from revoking relay-local negentropy sessions.
#[derive(Default)]
pub(crate) struct NegentropyRevocationEffects {
    pub(crate) revoked_passes: Vec<SubPass>,
    pub(crate) retries: Vec<NegentropyRetry>,
}

impl NegentropyRevocationEffects {
    fn revoke_pass(&mut self, pass: SubPass) {
        self.revoked_passes.push(pass);
    }

    fn retry(&mut self, owner_history_id: FullHistorySubId, filter: Filter) {
        self.retries.push(NegentropyRetry {
            owner_history_id,
            filter,
        });
    }

    pub(crate) fn take_retries(&mut self) -> Vec<NegentropyRetry> {
        std::mem::take(&mut self.retries)
    }
}

pub(crate) enum NegentropyStartResult {
    Started(ClientMessage),
    Rejected(SubPass),
}

/// Borrow wrapper over relay-local negentropy state.
pub(crate) struct NegentropyRelay<'a> {
    frame_sink: RelayFrameSink,
    data: &'a mut NegentropyData,
}

impl<'a> NegentropyRelay<'a> {
    /// Creates one scoped relay-local negentropy operator.
    pub(crate) fn new(frame_sink: impl Into<RelayFrameSink>, data: &'a mut NegentropyData) -> Self {
        Self {
            frame_sink: frame_sink.into(),
            data,
        }
    }

    pub(crate) fn take_frames(&mut self) -> Vec<crate::relay::frame::QueuedRelayFrame> {
        std::mem::replace(&mut self.frame_sink, RelayFrameSink::disconnected()).into_frames()
    }

    /// Handle one `NEG-MSG` round-trip from the relay.
    pub(crate) fn handle_neg_msg(
        &mut self,
        session_id: &str,
        payload_hex: &str,
    ) -> (Option<ClientMessage>, NegentropyRelayEffects) {
        let mut effects = NegentropyRelayEffects::default();
        let now = Instant::now();
        let Some(mut session) = self.data.active_sessions.remove(session_id) else {
            tracing::warn!(
                session_id,
                "negentropy received NEG-MSG for unknown session"
            );
            return (None, effects);
        };

        let mut have_ids = Vec::new();
        let mut need_ids = Vec::new();
        let payload = match hex::decode(payload_hex) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!(session_id, "negentropy hex decode: {err}");
                let session_id = NegSessionId::new(session_id.to_owned());
                return (
                    None,
                    self.fail_session_or_mark_unsupported(&session_id, session),
                );
            }
        };

        let owner_history_id = session.target.owner_history_id;
        let result =
            session
                .protocol
                .neg
                .reconcile_with_ids(&payload, &mut have_ids, &mut need_ids);
        let surfaced_filter = (!need_ids.is_empty()).then(|| session.target.filter.clone());

        match result {
            Ok(Some(next_msg)) => {
                session.record_response(now);
                self.data.capability = Some(true);
                if let Some(filter) = surfaced_filter.as_ref() {
                    effects.extend(surface_need_ids(owner_history_id, filter, need_ids));
                }
                let session_id = NegSessionId::new(session_id.to_owned());
                self.data
                    .active_sessions
                    .insert(session_id.clone(), session);
                (Some(neg_msg(&session_id, &hex::encode(next_msg))), effects)
            }
            Ok(None) => {
                self.data.capability = Some(true);
                if let Some(filter) = surfaced_filter.as_ref() {
                    effects.extend(surface_need_ids(owner_history_id, filter, need_ids));
                }
                effects.return_pass(session.protocol.sub_pass);
                let session_id = NegSessionId::new(session_id.to_owned());
                (Some(neg_close_msg(&session_id)), effects)
            }
            Err(err) => {
                tracing::warn!(session_id, "negentropy reconcile: {err}");
                let session_id = NegSessionId::new(session_id.to_owned());
                (
                    None,
                    self.fail_session_or_mark_unsupported(&session_id, session),
                )
            }
        }
    }

    /// Handle one relay `NEG-ERR` message.
    ///
    /// Per NIP-77, NEG-ERR means the relay *supports* negentropy but rejected
    /// this specific request. `blocked:` means the filter is too broad,
    /// `closed:` means the session timed out. Neither marks the relay as
    /// unsupported.
    pub(crate) fn handle_neg_err(
        &mut self,
        session_id: &str,
        reason: &str,
    ) -> NegentropyRelayEffects {
        let mut effects = NegentropyRelayEffects::default();
        let kind = NegErrKind::parse(reason);
        let Some(session) = self.data.active_sessions.remove(session_id) else {
            tracing::warn!(
                session_id,
                reason,
                "negentropy received NEG-ERR for unknown session"
            );
            return effects;
        };

        tracing::warn!(
            session_id,
            owner_history_id = ?session.target.owner_history_id,
            kind = ?kind,
            reason,
            "negentropy NEG-ERR"
        );
        self.data.capability = Some(true);
        effects.return_pass(session.protocol.sub_pass);

        if matches!(kind, NegErrKind::Blocked) {
            self.data.block_filter(session.target.filter);
        } else {
            effects.retry(session.target.owner_history_id, session.target.filter);
        }
        effects
    }

    /// Handle one relay `NOTICE` that may describe negentropy capability.
    pub(crate) fn handle_notice(&mut self, notice: &str) -> NegentropyRelayEffects {
        if NegentropyNoticeKind::parse(notice) != NegentropyNoticeKind::Unsupported {
            return NegentropyRelayEffects::default();
        }

        tracing::warn!(notice, "negentropy NOTICE marks relay unsupported");
        self.mark_unsupported()
    }

    /// Expire sessions without recent relay responses.
    pub(crate) fn handle_timeout(&mut self, now: Instant) -> NegentropyRelayEffects {
        if self.data.is_unsupported() {
            return NegentropyRelayEffects::default();
        }

        let expired = self
            .data
            .next_timeout_deadline()
            .is_some_and(|deadline| deadline <= now);
        if !expired {
            return NegentropyRelayEffects::default();
        }

        tracing::warn!("negentropy timed out waiting for relay response");
        if self.data.capability == Some(true) {
            self.retry_expired_sessions(now)
        } else {
            self.mark_unsupported()
        }
    }

    /// Drop all relay-local sessions on disconnect.
    pub(crate) fn handle_relay_disconnect(&mut self) -> NegentropyRelayEffects {
        self.drop_sessions_without_neg_close()
    }

    /// Drop all relay-local sessions without sending `NEG-CLOSE` frames.
    pub(crate) fn drop_sessions_without_neg_close(&mut self) -> NegentropyRelayEffects {
        let mut effects = NegentropyRelayEffects::default();
        let sessions = self.remove_all_sessions_collect();
        for (_, session) in sessions {
            effects.return_pass(session.protocol.sub_pass);
            effects.retry(session.target.owner_history_id, session.target.filter);
        }
        effects
    }

    /// Revocate passes held by relay-local sessions selected for limit reduction.
    pub(crate) fn revocate_sessions(&mut self, count: usize) -> NegentropyRevocationEffects {
        if count == 0 {
            return NegentropyRevocationEffects::default();
        }

        let session_ids: Vec<NegSessionId> = self
            .data
            .active_sessions
            .keys()
            .take(count)
            .cloned()
            .collect();

        for session_id in &session_ids {
            self.send_neg_close(session_id);
        }

        let mut effects = NegentropyRevocationEffects::default();
        for session_id in session_ids {
            let Some(session) = self.data.active_sessions.remove(&session_id) else {
                continue;
            };

            effects.revoke_pass(session.protocol.sub_pass);
            effects.retry(session.target.owner_history_id, session.target.filter);
        }
        effects
    }

    /// Cancel all relay-local negentropy sessions and surfaced needs owned by
    /// one full-history subscription.
    pub(crate) fn cancel_owner(
        &mut self,
        owner_history_id: FullHistorySubId,
    ) -> NegentropyRelayEffects {
        self.cancel_matching_work(|candidate_owner_history_id, _| {
            candidate_owner_history_id == owner_history_id
        })
    }

    /// Cancel relay-local negentropy work owned by one sub for the given filters.
    pub(crate) fn cancel_owner_filters(
        &mut self,
        owner_history_id: FullHistorySubId,
        filters: &[Filter],
    ) -> NegentropyRelayEffects {
        if filters.is_empty() {
            return NegentropyRelayEffects::default();
        }

        self.cancel_matching_work(|candidate_owner_history_id, candidate_filter| {
            candidate_owner_history_id == owner_history_id
                && filters
                    .iter()
                    .any(|filter| filter.same_canonical_attributes(candidate_filter))
        })
    }

    fn cancel_matching_work(
        &mut self,
        mut should_cancel: impl FnMut(FullHistorySubId, &Filter) -> bool,
    ) -> NegentropyRelayEffects {
        let mut effects = NegentropyRelayEffects::default();
        let session_ids: Vec<NegSessionId> = self
            .data
            .active_sessions
            .iter()
            .filter(|(_, session)| {
                should_cancel(session.target.owner_history_id, &session.target.filter)
            })
            .map(|(session_id, _)| session_id.clone())
            .collect();

        for session_id in &session_ids {
            self.send_neg_close(session_id);
        }

        for session_id in session_ids {
            effects.extend(self.remove_session(session_id.as_str()));
        }

        effects
    }

    fn remove_session(&mut self, session_id: &str) -> NegentropyRelayEffects {
        let mut effects = NegentropyRelayEffects::default();
        if let Some(session) = self.data.active_sessions.remove(session_id) {
            effects.return_pass(session.protocol.sub_pass);
        }
        effects
    }

    fn remove_all_sessions(&mut self) -> NegentropyRelayEffects {
        let mut effects = NegentropyRelayEffects::default();
        for (_, session) in self.remove_all_sessions_collect() {
            effects.return_pass(session.protocol.sub_pass);
        }
        effects
    }

    fn retry_expired_sessions(&mut self, now: Instant) -> NegentropyRelayEffects {
        let mut effects = NegentropyRelayEffects::default();
        for (session_id, session) in self.data.take_expired_sessions(now) {
            self.send_neg_close(&session_id);
            tracing::warn!(
                session_id = session_id.as_str(),
                owner_history_id = ?session.target.owner_history_id,
                received_response = session.protocol.last_response_at.is_some(),
                elapsed_ms = now
                    .saturating_duration_since(session.protocol.last_response_at.unwrap_or(session.protocol.opened_at))
                    .as_millis(),
                "negentropy retrying timed-out session"
            );
            effects.extend(self.retry_session(session));
        }
        effects
    }

    fn send_neg_close(&mut self, session_id: &NegSessionId) {
        self.frame_sink.send(neg_close_msg(session_id));
    }

    fn fail_session_or_mark_unsupported(
        &mut self,
        session_id: &NegSessionId,
        session: ActiveSession,
    ) -> NegentropyRelayEffects {
        self.send_neg_close(session_id);
        if self.data.capability == Some(true) {
            tracing::warn!(
                session_id = session_id.as_str(),
                owner_history_id = ?session.target.owner_history_id,
                "negentropy retrying failed session"
            );
            self.retry_session(session)
        } else {
            let mut effects = NegentropyRelayEffects::default();
            effects.return_pass(session.protocol.sub_pass);
            effects.extend(self.mark_unsupported());
            effects
        }
    }

    fn retry_session(&mut self, session: ActiveSession) -> NegentropyRelayEffects {
        let mut effects = NegentropyRelayEffects::default();
        effects.return_pass(session.protocol.sub_pass);
        effects.retry(session.target.owner_history_id, session.target.filter);
        effects
    }

    /// Mark the relay unsupported for negentropy and clear all active sessions.
    fn mark_unsupported(&mut self) -> NegentropyRelayEffects {
        tracing::warn!(
            active_sessions = self.data.active_session_count(),
            "negentropy marking relay unsupported"
        );
        self.data.capability = Some(false);
        self.remove_all_sessions()
    }

    fn remove_all_sessions_collect(&mut self) -> Vec<(NegSessionId, ActiveSession)> {
        let mut sessions = Vec::new();
        for (session_id, session) in self.data.active_sessions.drain() {
            sessions.push((session_id, session));
        }
        sessions
    }
}

fn surface_need_ids(
    owner_history_id: FullHistorySubId,
    filter: &Filter,
    need_ids: Vec<Id>,
) -> NegentropyRelayEffects {
    let mut effects = NegentropyRelayEffects::default();
    effects
        .needs
        .extend(need_ids.into_iter().map(|id| NegentropyNeed {
            owner_history_id,
            filter: filter.clone(),
            id: NoteId::new(id.to_bytes()),
        }));
    effects
}
