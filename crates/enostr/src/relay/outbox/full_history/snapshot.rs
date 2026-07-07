use nostrdb::Filter;

use crate::relay::{
    full_history_filters_from_relay_targets,
    subscription::{FullHistoryRelayFilter, FullHistoryUpsertTask},
    FullHistorySubId, NormRelayUrl,
};

/// Stable snapshot of one background full-history declaration.
#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct FullHistorySnapshot {
    pub(in crate::relay::outbox) id: FullHistorySubId,
    pub(in crate::relay::outbox) relay_filters: Vec<FullHistoryRelayFilter>,
}

impl FullHistorySnapshot {
    /// Returns true when both snapshots describe the same full-history query,
    /// regardless of relay/filter ordering.
    pub(in crate::relay::outbox) fn semantically_matches(&self, other: &Self) -> bool {
        self.id == other.id
            && full_history_relay_filter_diff(&self.relay_filters, &other.relay_filters).is_empty()
            && full_history_relay_filter_diff(&other.relay_filters, &self.relay_filters).is_empty()
    }

    /// Returns true when query routes and relay transport policy are unchanged.
    #[cfg(test)]
    pub(in crate::relay::outbox) fn fully_matches_targets(
        &self,
        id: FullHistorySubId,
        targets: &[FullHistoryRelayFilter],
    ) -> bool {
        fn full_matches(left: &FullHistoryRelayFilter, right: &FullHistoryRelayFilter) -> bool {
            left.has_same_relay_filter(right) && left.relay_policy == right.relay_policy
        }

        fn all_full_match(
            left: &[FullHistoryRelayFilter],
            right: &[FullHistoryRelayFilter],
        ) -> bool {
            left.iter()
                .all(|candidate| right.iter().any(|other| full_matches(candidate, other)))
        }

        self.id == id
            && all_full_match(&self.relay_filters, targets)
            && all_full_match(targets, &self.relay_filters)
    }

    pub(in crate::relay::outbox) fn contains_relay_filter_target(
        &self,
        target: &FullHistoryRelayFilter,
    ) -> bool {
        self.relay_filters
            .iter()
            .any(|relay_filter| relay_filter.semantically_matches(target))
    }

    pub(in crate::relay::outbox) fn target_for_relay_filter(
        &self,
        relay: &NormRelayUrl,
        filter: &Filter,
    ) -> Option<FullHistoryRelayFilter> {
        self.relay_filters
            .iter()
            .find(|relay_filter| {
                &relay_filter.relay == relay
                    && relay_filter.filter.same_canonical_attributes(filter)
            })
            .cloned()
    }

    /// Materialize all relay/filter pairs represented by this snapshot.
    pub(in crate::relay::outbox) fn relay_filters(&self) -> Vec<FullHistoryRelayFilter> {
        self.relay_filters.clone()
    }

    /// Return the distinct filter set represented by this snapshot.
    pub(in crate::relay::outbox) fn filters(&self) -> Vec<Filter> {
        full_history_filters_from_relay_targets(&self.relay_filters)
    }

    /// Return the relay packages represented by this snapshot.
    #[cfg(test)]
    pub(in crate::relay::outbox) fn relay_pkgs(&self) -> Vec<crate::relay::RelayUrlPkgs> {
        use crate::relay::subscription::full_history_relay_pkgs_from_relay_targets;

        full_history_relay_pkgs_from_relay_targets(&self.relay_filters)
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
        relay_filters: task.targets.clone(),
    }
}
