use hashbrown::HashSet;
use nostrdb::Filter;
use std::time::Instant;

use super::super::{OutboxSession, SubRegistry};
use super::state::{
    FullHistoryNeedBatch, FullHistoryTracker, PendingIngestion, FULL_HISTORY_FETCH_CHUNK,
};
use crate::{
    relay::{FullHistorySubId, NormRelayUrl, RelayUrlPkgs},
    NoteId,
};

/// Missing ids grouped by their source full-history relay/filter pair.
///
/// The staged protocol REQ uses an `ids` filter derived from `ids`; the
/// `source_filter` remains attached so later snapshot retarget can cancel fetch
/// subscriptions by the history filter that produced them.
struct FullHistoryFetchBatch {
    history_id: FullHistorySubId,
    relay: NormRelayUrl,
    source_filter: Filter,
    ids: HashSet<NoteId>,
}

/// Poll queued negentropy needs and stage missing-event fetches.
///
/// Returns subs whose queued needs are already local without staging a fetch.
/// Those subs need a fresh verification round because their previous local set
/// did not contain events now present in ndb.
pub(super) fn stage_queued_need_fetches(
    full_history: &mut FullHistoryTracker,
    registry: &mut SubRegistry,
    session: &mut OutboxSession,
    limit: usize,
    now: Instant,
) -> Vec<FullHistorySubId> {
    let need_batches = full_history.take_need_batches(limit, now);
    if need_batches.is_empty() {
        return Vec::new();
    }

    let mut candidate_ids: HashSet<NoteId> = need_batches
        .iter()
        .flat_map(|batch| batch.ids.iter().copied())
        .collect();
    if let Some(checker) = full_history.event_checker.as_deref() {
        checker.retain_missing(&mut candidate_ids);
    }

    let mut planner = NeedFetchPlanner {
        candidate_ids: &candidate_ids,
        fetching_subs: HashSet::new(),
        locally_satisfied_subs: HashSet::new(),
        unresolved_subs: HashSet::new(),
        relay_batches: Vec::new(),
        now,
    };

    for batch in need_batches {
        poll_need_batch(full_history, batch, &mut planner);
    }

    stage_fetch_oneshots(registry, session, planner.relay_batches);
    verification_ready_subs(
        full_history,
        &planner.fetching_subs,
        &planner.locally_satisfied_subs,
        &planner.unresolved_subs,
    )
}

struct NeedFetchPlanner<'a> {
    candidate_ids: &'a HashSet<NoteId>,
    fetching_subs: HashSet<FullHistorySubId>,
    locally_satisfied_subs: HashSet<FullHistorySubId>,
    unresolved_subs: HashSet<FullHistorySubId>,
    relay_batches: Vec<FullHistoryFetchBatch>,
    now: Instant,
}

fn poll_need_batch(
    full_history: &mut FullHistoryTracker,
    batch: FullHistoryNeedBatch,
    planner: &mut NeedFetchPlanner<'_>,
) {
    let Some(tracked) = full_history.tracked_subs.get_mut(&batch.history_id) else {
        return;
    };

    let history_id = batch.history_id;
    let relay = batch.relay;
    let filter = batch.filter;
    let retries_started = batch.retries_started;

    for id in batch.ids {
        if !planner.candidate_ids.contains(&id) {
            planner.locally_satisfied_subs.insert(history_id);
            tracked.progress.clear_fetch_state(&id);
            continue;
        }
        if tracked.progress.pending_ingestion.contains_key(&id) {
            tracked.progress.queue_fetch_candidate(
                id,
                relay.clone(),
                filter.clone(),
                retries_started,
            );
            continue;
        }
        if retries_started == 0 && tracked.progress.fetch_state_suppresses_need(&id, &relay) {
            planner.unresolved_subs.insert(history_id);
            continue;
        }

        tracked.progress.pending_ingestion.insert(
            id,
            PendingIngestion {
                relay: relay.clone(),
                filter: filter.clone(),
                started_at: planner.now,
                retries_started,
            },
        );
        planner.fetching_subs.insert(history_id);
        push_fetch_id(&mut planner.relay_batches, history_id, &relay, &filter, id);
    }
}

fn push_fetch_id(
    relay_batches: &mut Vec<FullHistoryFetchBatch>,
    history_id: FullHistorySubId,
    relay: &NormRelayUrl,
    filter: &Filter,
    id: NoteId,
) {
    if let Some(fetch_batch) = relay_batches.iter_mut().find(|fetch_batch| {
        fetch_batch.history_id == history_id
            && &fetch_batch.relay == relay
            && fetch_batch.source_filter.same_canonical_attributes(filter)
    }) {
        fetch_batch.ids.insert(id);
        return;
    }

    let mut ids = HashSet::new();
    ids.insert(id);
    relay_batches.push(FullHistoryFetchBatch {
        history_id,
        relay: relay.clone(),
        source_filter: filter.clone(),
        ids,
    });
}

fn stage_fetch_oneshots(
    registry: &mut SubRegistry,
    session: &mut OutboxSession,
    relay_batches: Vec<FullHistoryFetchBatch>,
) {
    for batch in relay_batches {
        let ids: Vec<NoteId> = batch.ids.into_iter().collect();
        for chunk in ids.chunks(FULL_HISTORY_FETCH_CHUNK) {
            let filter = Filter::new().ids(chunk.iter().map(|id| id.bytes())).build();
            let mut relays = HashSet::new();
            relays.insert(batch.relay.clone());
            let new_id = registry.next();
            session.full_history_fetch(
                batch.history_id,
                new_id,
                batch.source_filter.clone(),
                vec![filter],
                RelayUrlPkgs::new(relays),
            );
        }
    }
}

fn verification_ready_subs(
    full_history: &FullHistoryTracker,
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
                .pending_ingestion
                .is_empty()
                .then_some(history_id)
        })
        .collect()
}
