use hashbrown::HashSet;
use nostrdb::{Filter, Note};
use std::collections::BTreeMap;

use crate::relay::outbox::OutboxPool;
use crate::relay::{
    same_canonical_filter_set, FullHistoryConfig, FullHistorySubId, ModifyTask, NormRelayUrl,
    OutboxSubId, OutboxTask, RelayId, RelayReqStatus, RelayStatus, RelayUrlPkgs,
};
use crate::{relay::outbox::OutboxSession, EventClientMessage, Wakeup};

/// OutboxSessionHandler is the RAII wrapper apps use to stage subscription
/// updates; dropping it flushes the recorded operations into the OutboxPool.
pub struct OutboxSessionHandler<'a, W>
where
    W: Wakeup,
{
    pub outbox: &'a mut OutboxPool,
    pub(crate) session: OutboxSession,
    pub(crate) wakeup: W,
}

impl<'a, W> Drop for OutboxSessionHandler<'a, W>
where
    W: Wakeup,
{
    fn drop(&mut self) {
        let session = std::mem::take(&mut self.session);
        self.outbox.ingest_session(session, &self.wakeup);
    }
}

impl<'a, W> OutboxSessionHandler<'a, W>
where
    W: Wakeup,
{
    pub fn new(outbox: &'a mut OutboxPool, wakeup: W) -> Self {
        Self {
            outbox,
            session: OutboxSession::default(),
            wakeup,
        }
    }

    pub fn subscribe(&mut self, filters: Vec<Filter>, urls: RelayUrlPkgs) -> OutboxSubId {
        let new_id = self.outbox.registry.next();
        self.session.subscribe(new_id, filters, urls);
        new_id
    }

    /// Stage one standalone full-history subscription.
    ///
    /// This returns a [`FullHistorySubId`] with its own lifecycle. Pairing it
    /// with a live [`OutboxSubId`] is host-owned policy, not an enostr
    /// invariant.
    pub fn subscribe_full_history(
        &mut self,
        full_history: FullHistoryConfig,
        relays: HashSet<NormRelayUrl>,
    ) -> FullHistorySubId {
        let new_id = self.outbox.history_registry.next();
        self.session
            .upsert_full_history(new_id, full_history, relays);
        new_id
    }

    pub fn oneshot(&mut self, filters: Vec<Filter>, urls: RelayUrlPkgs) -> OutboxSubId {
        let new_id = self.outbox.registry.next();
        self.session.oneshot(new_id, filters, urls);
        new_id
    }

    pub fn modify_filters(&mut self, id: OutboxSubId, filters: Vec<Filter>) {
        self.session.new_filters(id, filters);
    }

    pub fn modify_relays(&mut self, id: OutboxSubId, relays: HashSet<NormRelayUrl>) {
        self.session.new_relays(id, relays);
    }

    /// Stage the full desired declaration for an existing durable subscription.
    pub fn modify_full(
        &mut self,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relays: HashSet<NormRelayUrl>,
    ) {
        self.session.modify_full(id, filters, relays);
    }

    /// Stage an update for one standalone full-history subscription.
    pub fn modify_full_history(
        &mut self,
        id: FullHistorySubId,
        full_history: FullHistoryConfig,
        relays: HashSet<NormRelayUrl>,
    ) {
        self.session.upsert_full_history(id, full_history, relays);
    }

    pub fn unsubscribe(&mut self, id: OutboxSubId) {
        self.session.unsubscribe(id);
    }

    pub fn remove_full_history(&mut self, id: FullHistorySubId) {
        self.session.remove_full_history(id);
    }

    pub fn broadcast_note(&mut self, note: &Note, relays: Vec<RelayId>) {
        self.outbox.broadcast_note(note, relays, &self.wakeup);
    }

    /// Broadcast an already-built event message to the requested relay targets.
    pub fn broadcast_event(&mut self, msg: EventClientMessage, relays: Vec<RelayId>) {
        self.outbox.broadcast_event(msg, relays, &self.wakeup);
    }

    /// Snapshot per-relay request status for one live subscription.
    pub fn status(&self, id: &OutboxSubId) -> hashbrown::HashMap<&NormRelayUrl, RelayReqStatus> {
        let committed = self.outbox.status(id);
        let committed_filters = self.outbox.filters(id).map(Vec::as_slice);
        let Some(task) = self.session.tasks.get(id) else {
            return committed;
        };

        staged_status(task, committed, committed_filters)
    }

    /// Snapshot websocket relay statuses for display/debug UI.
    pub fn websocket_statuses(&self) -> BTreeMap<&NormRelayUrl, RelayStatus> {
        self.outbox.websocket_statuses()
    }
}

fn staged_status<'a>(
    task: &'a OutboxTask,
    committed: hashbrown::HashMap<&'a NormRelayUrl, RelayReqStatus>,
    committed_filters: Option<&[Filter]>,
) -> hashbrown::HashMap<&'a NormRelayUrl, RelayReqStatus> {
    match task {
        OutboxTask::Modify(ModifyTask::Filters(_)) => {
            initial_query_status(committed.keys().copied())
        }
        OutboxTask::Modify(ModifyTask::Relays(task)) => {
            relay_modify_status(task.0.iter(), &committed)
        }
        OutboxTask::Modify(ModifyTask::Full(task)) => {
            full_modify_status(task, committed, committed_filters)
        }
        OutboxTask::Subscribe(task) | OutboxTask::Oneshot(task) => {
            initial_query_status(task.relays.iter())
        }
        OutboxTask::FullHistoryFetch(task) => initial_query_status(task.subscribe.relays.iter()),
        OutboxTask::Unsubscribe => hashbrown::HashMap::new(),
    }
}

fn full_modify_status<'a>(
    task: &'a crate::relay::FullModificationTask,
    committed: hashbrown::HashMap<&'a NormRelayUrl, RelayReqStatus>,
    committed_filters: Option<&[Filter]>,
) -> hashbrown::HashMap<&'a NormRelayUrl, RelayReqStatus> {
    let Some(committed_filters) = committed_filters else {
        return initial_query_status(task.relays.iter());
    };

    if !same_canonical_filter_set(committed_filters, task.filters.as_slice()) {
        return initial_query_status(task.relays.iter());
    }

    relay_modify_status(task.relays.iter(), &committed)
}

fn relay_modify_status<'a, 'b>(
    relays: impl IntoIterator<Item = &'a NormRelayUrl>,
    committed: &hashbrown::HashMap<&'b NormRelayUrl, RelayReqStatus>,
) -> hashbrown::HashMap<&'a NormRelayUrl, RelayReqStatus> {
    relays
        .into_iter()
        .map(|relay| {
            (
                relay,
                committed
                    .get(relay)
                    .copied()
                    .unwrap_or(RelayReqStatus::InitialQuery),
            )
        })
        .collect()
}

fn initial_query_status<'a>(
    relays: impl IntoIterator<Item = &'a NormRelayUrl>,
) -> hashbrown::HashMap<&'a NormRelayUrl, RelayReqStatus> {
    relays
        .into_iter()
        .map(|relay| (relay, RelayReqStatus::InitialQuery))
        .collect()
}
