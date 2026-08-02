use hashbrown::HashSet;
use nostrdb::Filter;
use std::time::Instant;

use super::state::{
    FullHistoryFetchRequest, FullHistoryLocalPresenceResult, FullHistoryNeedBatch,
    FullHistoryOutput, FullHistoryRuntime, PendingIngestion, FULL_HISTORY_FETCH_CHUNK,
};
use crate::{
    relay::{FullHistoryRelayFilter, FullHistorySubId, SubscribeTask},
    NoteId,
};

/// Missing ids grouped by their source full-history relay/filter pair.
///
/// The staged protocol REQ uses an `ids` filter derived from `ids`; the
/// `source_filter` remains attached so later snapshot retarget can cancel fetch
/// subscriptions by the history filter that produced them.
struct FullHistoryFetchBatch {
    history_id: FullHistorySubId,
    target: FullHistoryRelayFilter,
    ids: HashSet<NoteId>,
}

impl FullHistoryRuntime {
    /// Apply one completed local presence request and stage missing-event fetches.
    ///
    /// Returns subs whose queued needs are already local without staging a fetch.
    /// Those subs need a fresh verification round because their previous local set
    /// did not contain events now present in ndb.
    pub(super) fn apply_local_presence_result(
        &mut self,
        result: FullHistoryLocalPresenceResult,
        now: Instant,
    ) -> Option<(FullHistoryOutput, Vec<FullHistorySubId>)> {
        let need_batches = self.take_local_presence_plan(result.request_id)?;
        Some(self.stage_local_presence_batches(need_batches, &result.missing_ids, now))
    }

    fn stage_local_presence_batches(
        &mut self,
        need_batches: Vec<FullHistoryNeedBatch>,
        missing_ids: &HashSet<NoteId>,
        now: Instant,
    ) -> (FullHistoryOutput, Vec<FullHistorySubId>) {
        let mut planner = NeedFetchPlanner {
            fetching_subs: HashSet::new(),
            locally_satisfied_subs: HashSet::new(),
            unresolved_subs: HashSet::new(),
            pending_ingestion_ids: HashSet::new(),
            pending_ingestion_deadline: None,
            relay_batches: Vec::new(),
            touched_subs: HashSet::new(),
            now,
        };

        if need_batches.is_empty() {
            return (FullHistoryOutput::default(), Vec::new());
        }
        for batch in need_batches {
            self.plan_need_batch(batch, missing_ids, &mut planner);
        }

        let mut output = FullHistoryOutput::default();
        if let Some(deadline) = planner.pending_ingestion_deadline {
            if let Some(request) = self
                .enqueue_pending_ingestion_presence_request(planner.pending_ingestion_ids, deadline)
            {
                output.pending_ingestion_presence_requests.push(request);
            }
        }

        output.fetch_requests = stage_fetch_oneshots(planner.relay_batches);
        output.relay_demand_changes =
            self.refresh_relay_transport_demand_for_subs(planner.touched_subs);
        let verification_ready = verification_ready_subs(
            self,
            &planner.fetching_subs,
            &planner.locally_satisfied_subs,
            &planner.unresolved_subs,
        );
        (output, verification_ready)
    }

    pub(super) fn queue_need_presence_request(&mut self, now: Instant) -> FullHistoryOutput {
        let need_batches = self.take_need_batches(now);
        let touched_subs = need_batches
            .iter()
            .map(|batch| batch.history_id)
            .collect::<HashSet<_>>();
        let Some(request) = self.enqueue_local_presence_request(need_batches) else {
            return FullHistoryOutput::default();
        };
        FullHistoryOutput {
            local_presence_requests: vec![request],
            relay_demand_changes: self.refresh_relay_transport_demand_for_subs(touched_subs),
            ..Default::default()
        }
    }

    fn plan_need_batch(
        &mut self,
        batch: FullHistoryNeedBatch,
        candidate_ids: &HashSet<NoteId>,
        planner: &mut NeedFetchPlanner,
    ) {
        let Some(tracked) = self.tracked_subs.get_mut(&batch.history_id) else {
            return;
        };

        let history_id = batch.history_id;
        let target = batch.target;
        let retries_started = batch.retries_started;
        planner.touched_subs.insert(history_id);

        for id in batch.ids {
            if !candidate_ids.contains(&id) {
                planner.locally_satisfied_subs.insert(history_id);
                tracked.progress.clear_fetch_state(&id);
                continue;
            }
            if tracked.progress.pending_ingestion(&id).is_some() {
                tracked
                    .progress
                    .queue_fetch_candidate(id, target.clone(), retries_started);
                continue;
            }
            if retries_started == 0
                && tracked
                    .progress
                    .fetch_state_suppresses_need(&id, &target.relay)
            {
                planner.unresolved_subs.insert(history_id);
                continue;
            }

            let pending = PendingIngestion {
                target: target.clone(),
                started_at: planner.now,
                retries_started,
            };
            planner.pending_ingestion_ids.insert(id);
            planner.pending_ingestion_deadline =
                Some(planner.pending_ingestion_deadline.map_or_else(
                    || pending.timeout_deadline(),
                    |deadline| deadline.min(pending.timeout_deadline()),
                ));
            tracked.progress.start_pending_ingestion(id, pending);
            planner.fetching_subs.insert(history_id);
            push_fetch_id(&mut planner.relay_batches, history_id, &target, id);
        }
    }
}

struct NeedFetchPlanner {
    fetching_subs: HashSet<FullHistorySubId>,
    locally_satisfied_subs: HashSet<FullHistorySubId>,
    unresolved_subs: HashSet<FullHistorySubId>,
    pending_ingestion_ids: HashSet<NoteId>,
    pending_ingestion_deadline: Option<Instant>,
    relay_batches: Vec<FullHistoryFetchBatch>,
    touched_subs: HashSet<FullHistorySubId>,
    now: Instant,
}

fn push_fetch_id(
    relay_batches: &mut Vec<FullHistoryFetchBatch>,
    history_id: FullHistorySubId,
    target: &FullHistoryRelayFilter,
    id: NoteId,
) {
    if let Some(fetch_batch) = relay_batches.iter_mut().find(|fetch_batch| {
        fetch_batch.history_id == history_id && fetch_batch.target.semantically_matches(target)
    }) {
        fetch_batch.ids.insert(id);
        return;
    }

    let mut ids = HashSet::new();
    ids.insert(id);
    relay_batches.push(FullHistoryFetchBatch {
        history_id,
        target: target.clone(),
        ids,
    });
}

fn stage_fetch_oneshots(relay_batches: Vec<FullHistoryFetchBatch>) -> Vec<FullHistoryFetchRequest> {
    let mut requests = Vec::new();
    for batch in relay_batches {
        let ids: Vec<NoteId> = batch.ids.into_iter().collect();
        for chunk in ids.chunks(FULL_HISTORY_FETCH_CHUNK) {
            let filter = Filter::new().ids(chunk.iter().map(|id| id.bytes())).build();
            requests.push(FullHistoryFetchRequest {
                owner: batch.history_id,
                filter: batch.target.filter.clone(),
                subscribe: SubscribeTask {
                    filters: vec![filter],
                    relays: batch.target.relay_pkgs(),
                },
            });
        }
    }
    requests
}

fn verification_ready_subs(
    full_history: &FullHistoryRuntime,
    fetching_subs: &HashSet<FullHistorySubId>,
    locally_satisfied_subs: &HashSet<FullHistorySubId>,
    unresolved_subs: &HashSet<FullHistorySubId>,
) -> Vec<FullHistorySubId> {
    locally_satisfied_subs
        .iter()
        .copied()
        .filter_map(|history_id| {
            if fetching_subs.contains(&history_id) || unresolved_subs.contains(&history_id) {
                return None;
            }
            let tracked = full_history.tracked_subs.get(&history_id)?;
            if !tracked.progress.pending_needs.is_empty() {
                return None;
            }
            tracked
                .progress
                .pending_ingestion_is_empty()
                .then_some(history_id)
        })
        .collect()
}
