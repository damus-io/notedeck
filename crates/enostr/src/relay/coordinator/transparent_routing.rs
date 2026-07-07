use hashbrown::HashMap;

use crate::relay::{
    indexed_queue::IndexedQueue, transparent::TransparentData, OutboxSubId, OutboxSubscriptions,
    RelayRoutingPreference, RelayType,
};

/// Owns coordinator route assignment plus transparent demotion indexes.
#[derive(Default)]
pub(super) struct RouteIndex {
    routes: HashMap<OutboxSubId, RelayType>,
    transparent: TransparentRoutingState,
}

impl RouteIndex {
    /// Sets a route to transparent and updates demotion indexes.
    pub(super) fn set_transparent_route(
        &mut self,
        id: OutboxSubId,
        policy: RelayRoutingPreference,
    ) {
        self.routes.insert(id, RelayType::Transparent);
        self.transparent.enter(id, policy);
    }

    /// Sets a route to compaction and removes transparent index membership.
    pub(super) fn set_compaction_route(&mut self, id: OutboxSubId) {
        self.routes.insert(id, RelayType::Compaction);
        self.transparent.exit(id);
    }

    /// Clears route ownership and removes transparent index membership.
    pub(super) fn clear_route(&mut self, id: OutboxSubId) {
        self.routes.remove(&id);
        self.transparent.exit(id);
    }

    /// Rebuilds demotion indexes from current dedicated relay state.
    pub(super) fn rebuild_from_dedicated(
        &mut self,
        subs: &OutboxSubscriptions,
        transparent: &TransparentData,
    ) {
        self.transparent.clear_index();
        for id in transparent.request_ids() {
            let policy = subs.routing_preference(&id).unwrap_or_default();
            self.transparent.enter(id, policy);
        }
    }

    /// Returns transparent downgrade victims ordered from least to most
    /// disruptive: no-preference first, then preferred, then required.
    pub(super) fn limit_reduction_candidates(&self) -> Vec<OutboxSubId> {
        self.transparent.limit_reduction_candidates()
    }

    pub(super) fn route_type(&self, id: &OutboxSubId) -> Option<RelayType> {
        self.routes.get(id).copied()
    }

    pub(super) fn route_ids(&self) -> Vec<OutboxSubId> {
        self.routes.keys().copied().collect()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&OutboxSubId, &RelayType)> {
        self.routes.iter()
    }
}

/// Tracks transparent routing and demotion candidate order for coordinator decisions.
///
/// Invariants:
/// - every `indexed_class` entry points to exactly one demotion queue.
/// - queued nodes are physically removed in O(1) on transparent exit.
/// - `RequireDedicated` subscriptions are tracked for downgrade selection but
///   never considered demotable during normal transparent pressure handling.
#[derive(Default)]
struct TransparentRoutingState {
    indexed_class: HashMap<OutboxSubId, RelayRoutingPreference>,
    required: IndexedQueue<OutboxSubId>,
    preferred: IndexedQueue<OutboxSubId>,
    non_preferred: IndexedQueue<OutboxSubId>,
}

impl TransparentRoutingState {
    /// Returns transparent downgrade victims ordered from least to most
    /// disruptive: no-preference first, then preferred, then required.
    fn limit_reduction_candidates(&self) -> Vec<OutboxSubId> {
        self.non_preferred
            .iter()
            .chain(self.preferred.iter())
            .chain(self.required.iter())
            .collect()
    }

    /// Inserts or updates one transparent route in the demotion index.
    fn enter(&mut self, id: OutboxSubId, policy: RelayRoutingPreference) {
        let Some(current_policy) = self.indexed_class.get(&id).copied() else {
            self.indexed_class.insert(id, policy);
            self.queue_mut(policy).push_back_if_missing(id);
            return;
        };

        if current_policy == policy {
            return;
        }

        self.queue_mut(current_policy).remove(id);
        self.indexed_class.insert(id, policy);
        self.queue_mut(policy).push_back_if_missing(id);
    }

    /// Removes one transparent route from the demotion index in O(1).
    fn exit(&mut self, id: OutboxSubId) {
        let Some(policy) = self.indexed_class.remove(&id) else {
            return;
        };
        self.queue_mut(policy).remove(id);
    }

    fn clear_index(&mut self) {
        self.indexed_class.clear();
        self.required.clear();
        self.preferred.clear();
        self.non_preferred.clear();
    }

    fn queue_mut(&mut self, policy: RelayRoutingPreference) -> &mut IndexedQueue<OutboxSubId> {
        match policy {
            RelayRoutingPreference::RequireDedicated => &mut self.required,
            RelayRoutingPreference::PreferDedicated => &mut self.preferred,
            RelayRoutingPreference::NoPreference => &mut self.non_preferred,
        }
    }

    #[cfg(test)]
    fn demotable_queue_lengths(&self) -> (usize, usize) {
        (self.non_preferred.len(), self.preferred.len())
    }

    #[cfg(test)]
    fn has_indexed_entry(&self, id: OutboxSubId) -> bool {
        self.indexed_class.contains_key(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_reduction_candidates_include_required_after_demotable_routes() {
        let mut state = RouteIndex::default();
        let required = OutboxSubId(1);
        let preferred = OutboxSubId(2);

        state.set_transparent_route(required, RelayRoutingPreference::RequireDedicated);
        state.set_transparent_route(preferred, RelayRoutingPreference::PreferDedicated);

        assert_eq!(
            state.limit_reduction_candidates(),
            vec![preferred, required]
        );
    }

    #[test]
    fn policy_change_reindexes_existing_transparent_route_immediately() {
        let mut state = RouteIndex::default();
        let id = OutboxSubId(7);

        state.set_transparent_route(id, RelayRoutingPreference::RequireDedicated);
        assert_eq!(state.transparent.demotable_queue_lengths(), (0, 0));
        assert!(state.transparent.has_indexed_entry(id));

        state.set_transparent_route(id, RelayRoutingPreference::PreferDedicated);

        assert_eq!(state.transparent.demotable_queue_lengths(), (0, 1));
        assert!(state.transparent.has_indexed_entry(id));
        assert_eq!(state.limit_reduction_candidates(), vec![id]);
    }

    #[test]
    fn exit_removes_queue_entry_immediately() {
        let mut state = RouteIndex::default();
        let stale = OutboxSubId(11);
        let active = OutboxSubId(12);

        state.set_transparent_route(stale, RelayRoutingPreference::PreferDedicated);
        state.set_transparent_route(active, RelayRoutingPreference::PreferDedicated);
        state.clear_route(stale);

        assert_eq!(state.transparent.demotable_queue_lengths(), (0, 1));
        assert!(!state.transparent.has_indexed_entry(stale));
        assert_eq!(state.limit_reduction_candidates(), vec![active]);
    }
}
