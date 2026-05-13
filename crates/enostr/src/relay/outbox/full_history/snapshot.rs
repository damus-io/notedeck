use nostrdb::Filter;

use crate::relay::{
    same_canonical_filter_set, subscription::FullHistoryUpsertTask, FullHistorySubId, NormRelayUrl,
};

/// Stable snapshot of one background full-history declaration.
#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct FullHistorySnapshot {
    pub(in crate::relay::outbox) id: FullHistorySubId,
    pub(in crate::relay::outbox) relays: Vec<NormRelayUrl>,
    pub(in crate::relay::outbox) filters: Vec<Filter>,
}

impl FullHistorySnapshot {
    /// Returns true when both snapshots describe the same full-history query,
    /// regardless of relay/filter ordering.
    pub(in crate::relay::outbox) fn semantically_matches(&self, other: &Self) -> bool {
        self.id == other.id
            && self.sorted_relays() == other.sorted_relays()
            && same_canonical_filter_set(&self.filters, &other.filters)
    }

    /// Whether this snapshot still contains one relay/filter pair.
    pub(in crate::relay::outbox) fn contains_relay_filter(
        &self,
        relay: &NormRelayUrl,
        filter: &Filter,
    ) -> bool {
        self.relays
            .iter()
            .any(|snapshot_relay| snapshot_relay == relay)
            && self
                .filters
                .iter()
                .any(|snapshot_filter| snapshot_filter.same_canonical_attributes(filter))
    }

    /// Materialize all relay/filter pairs represented by this snapshot.
    pub(in crate::relay::outbox) fn relay_filters(&self) -> Vec<FullHistoryRelayFilter> {
        self.filters
            .iter()
            .flat_map(|filter| {
                self.relays
                    .iter()
                    .cloned()
                    .map(|relay| FullHistoryRelayFilter {
                        relay,
                        filter: filter.clone(),
                    })
            })
            .collect()
    }

    /// Canonicalize relay ordering for snapshot comparisons.
    fn sorted_relays(&self) -> Vec<String> {
        let mut relays: Vec<String> = self.relays.iter().map(ToString::to_string).collect();
        relays.sort_unstable();
        relays
    }
}

/// One relay/filter pair represented by a full-history snapshot.
#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct FullHistoryRelayFilter {
    pub(in crate::relay::outbox) relay: NormRelayUrl,
    pub(in crate::relay::outbox) filter: Filter,
}

impl FullHistoryRelayFilter {
    /// Whether two relay/filter pairs target the same relay and canonical filter.
    fn semantically_matches(&self, other: &Self) -> bool {
        self.relay == other.relay && self.filter.same_canonical_attributes(&other.filter)
    }
}

/// Result of reconciling a tracked full-history snapshot with a new one.
pub(in crate::relay::outbox) enum FullHistoryUpsert {
    Unchanged,
    Inserted,
    Changed {
        added: Vec<FullHistoryRelayFilter>,
        removed: Vec<FullHistoryRelayFilter>,
        filters_changed: bool,
    },
}

pub(in crate::relay::outbox) fn full_history_relay_filter_diff(
    left: &[FullHistoryRelayFilter],
    right: &[FullHistoryRelayFilter],
) -> Vec<FullHistoryRelayFilter> {
    left.iter()
        .filter(|candidate| {
            !right
                .iter()
                .any(|other| candidate.semantically_matches(other))
        })
        .cloned()
        .collect()
}

pub(in crate::relay::outbox) fn full_history_snapshot_from_task(
    id: FullHistorySubId,
    task: &FullHistoryUpsertTask,
) -> FullHistorySnapshot {
    FullHistorySnapshot {
        id,
        relays: task.relays.iter().cloned().collect(),
        filters: task.filters.to_vec(),
    }
}
