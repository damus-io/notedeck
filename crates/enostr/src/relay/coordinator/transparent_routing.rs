use hashbrown::{HashMap, HashSet};

use crate::relay::{
    indexed_queue::IndexedQueue, transparent::TransparentData, OutboxSubId, OutboxSubscriptions,
    RelayRoutingPreference, RelayType,
};

/// Tracks transparent routing and demotion candidate order for coordinator decisions.
///
/// Invariants:
/// - every `indexed_class` entry points to exactly one demotion queue.
/// - queued nodes are physically removed in O(1) on transparent exit.
/// - `RequireDedicated` subscriptions are tracked for downgrade selection but
///   never considered demotable during normal transparent pressure handling.
#[derive(Default)]
pub(super) struct TransparentRoutingState {
    indexed_class: HashMap<OutboxSubId, RelayRoutingPreference>,
    required: IndexedQueue<OutboxSubId>,
    preferred: IndexedQueue<OutboxSubId>,
    non_preferred: IndexedQueue<OutboxSubId>,
}

impl TransparentRoutingState {
    /// Sets a route to transparent and updates demotion indexes.
    pub(super) fn set_transparent_route(
        &mut self,
        routes: &mut HashMap<OutboxSubId, RelayType>,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) {
        routes.insert(id, RelayType::Transparent);
        let policy = subs.routing_preference(&id).unwrap_or_default();
        self.enter(id, policy);
    }

    /// Sets a route to compaction and removes transparent index membership.
    pub(super) fn set_compaction_route(
        &mut self,
        routes: &mut HashMap<OutboxSubId, RelayType>,
        id: OutboxSubId,
    ) {
        routes.insert(id, RelayType::Compaction);
        self.exit(id);
    }

    /// Clears route ownership and removes transparent index membership.
    pub(super) fn clear_route(
        &mut self,
        routes: &mut HashMap<OutboxSubId, RelayType>,
        id: OutboxSubId,
    ) {
        routes.remove(&id);
        self.exit(id);
    }

    /// Records a transparent unsubscribe without changing route ownership.
    pub(super) fn note_transparent_unsubscribe(&mut self, id: OutboxSubId) {
        self.exit(id);
    }

    /// Rebuilds demotion indexes from current transparent relay state.
    pub(super) fn rebuild_from_transparent(
        &mut self,
        subs: &OutboxSubscriptions,
        transparent: &TransparentData,
    ) {
        self.clear_index();
        for id in transparent.request_ids() {
            let policy = subs.routing_preference(&id).unwrap_or_default();
            self.enter(id, policy);
        }
    }

    /// Picks a demotion candidate, preferring non-preferred transparent routes first.
    pub(super) fn pick_demotable(
        &mut self,
        subs: &OutboxSubscriptions,
        incoming: OutboxSubId,
        demoted_in_current_pass: &HashSet<OutboxSubId>,
    ) -> Option<OutboxSubId> {
        self.pick_from_preference(
            RelayRoutingPreference::NoPreference,
            subs,
            incoming,
            demoted_in_current_pass,
        )
        .or_else(|| {
            self.pick_from_preference(
                RelayRoutingPreference::PreferDedicated,
                subs,
                incoming,
                demoted_in_current_pass,
            )
        })
    }

    /// Picks the oldest non-preferred demotion candidate.
    pub(super) fn pick_non_preferred(
        &mut self,
        subs: &OutboxSubscriptions,
        incoming: OutboxSubId,
        demoted_in_current_pass: &HashSet<OutboxSubId>,
    ) -> Option<OutboxSubId> {
        self.pick_from_preference(
            RelayRoutingPreference::NoPreference,
            subs,
            incoming,
            demoted_in_current_pass,
        )
    }

    /// Returns transparent downgrade victims ordered from least to most
    /// disruptive: no-preference first, then preferred, then required.
    pub(super) fn limit_reduction_candidates(&self) -> Vec<OutboxSubId> {
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

    fn pick_from_preference(
        &mut self,
        expected_policy: RelayRoutingPreference,
        subs: &OutboxSubscriptions,
        incoming: OutboxSubId,
        demoted_in_current_pass: &HashSet<OutboxSubId>,
    ) -> Option<OutboxSubId> {
        let queue_len = self.queue_len(expected_policy);
        for _ in 0..queue_len {
            let Some(sub_id) = self.queue_mut(expected_policy).pop_front() else {
                break;
            };

            let current_policy = subs.routing_preference(&sub_id).unwrap_or_default();
            if current_policy != expected_policy {
                // Routing preference can change after an entry was indexed. Repair the
                // queue lazily here so writes stay O(1) and demotion selection amortizes
                // any stale classification cleanup across future picks.
                self.indexed_class.insert(sub_id, current_policy);
                self.queue_mut(current_policy).push_back_if_missing(sub_id);
                continue;
            }

            if sub_id == incoming || demoted_in_current_pass.contains(&sub_id) {
                self.queue_mut(expected_policy).push_back_if_missing(sub_id);
                continue;
            }

            self.indexed_class.remove(&sub_id);
            return Some(sub_id);
        }

        None
    }

    fn queue(&self, policy: RelayRoutingPreference) -> &IndexedQueue<OutboxSubId> {
        match policy {
            RelayRoutingPreference::RequireDedicated => &self.required,
            RelayRoutingPreference::PreferDedicated => &self.preferred,
            RelayRoutingPreference::NoPreference => &self.non_preferred,
        }
    }

    fn queue_mut(&mut self, policy: RelayRoutingPreference) -> &mut IndexedQueue<OutboxSubId> {
        match policy {
            RelayRoutingPreference::RequireDedicated => &mut self.required,
            RelayRoutingPreference::PreferDedicated => &mut self.preferred,
            RelayRoutingPreference::NoPreference => &mut self.non_preferred,
        }
    }

    fn queue_len(&self, policy: RelayRoutingPreference) -> usize {
        self.queue(policy).len()
    }

    #[cfg(test)]
    fn demotable_queue_lengths(&self) -> (usize, usize) {
        (
            self.queue_len(RelayRoutingPreference::NoPreference),
            self.queue_len(RelayRoutingPreference::PreferDedicated),
        )
    }

    #[cfg(test)]
    fn has_indexed_entry(&self, id: OutboxSubId) -> bool {
        self.indexed_class.contains_key(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::test_utils::insert_sub_with_policy_for_relay;
    use hashbrown::{HashMap, HashSet};

    #[test]
    fn required_routes_are_not_enqueued_for_demotion() {
        let mut state = TransparentRoutingState::default();
        let mut routes = HashMap::new();
        let mut subs = OutboxSubscriptions::default();
        let required = OutboxSubId(1);
        let preferred = OutboxSubId(2);
        insert_sub_with_policy_for_relay(
            &mut subs,
            required,
            RelayRoutingPreference::RequireDedicated,
            "wss://routing-state.example.com",
        );
        insert_sub_with_policy_for_relay(
            &mut subs,
            preferred,
            RelayRoutingPreference::PreferDedicated,
            "wss://routing-state.example.com",
        );

        state.set_transparent_route(&mut routes, &subs, required);
        state.set_transparent_route(&mut routes, &subs, preferred);

        let demoted = state.pick_demotable(&subs, OutboxSubId(99), &HashSet::new());
        assert_eq!(demoted, Some(preferred));
        assert_ne!(demoted, Some(required));
    }

    #[test]
    fn policy_change_reindexes_existing_transparent_route_immediately() {
        let mut state = TransparentRoutingState::default();
        let mut routes = HashMap::new();
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(7);
        insert_sub_with_policy_for_relay(
            &mut subs,
            id,
            RelayRoutingPreference::RequireDedicated,
            "wss://routing-state.example.com",
        );

        state.set_transparent_route(&mut routes, &subs, id);
        assert_eq!(state.demotable_queue_lengths(), (0, 0));
        assert!(state.has_indexed_entry(id));

        subs.get_mut(&id).unwrap().routing_preference = RelayRoutingPreference::PreferDedicated;
        state.set_transparent_route(&mut routes, &subs, id);

        assert_eq!(state.demotable_queue_lengths(), (0, 1));
        assert!(state.has_indexed_entry(id));
        let demoted = state.pick_demotable(&subs, OutboxSubId(100), &HashSet::new());
        assert_eq!(demoted, Some(id));
    }

    #[test]
    fn exit_removes_queue_entry_immediately() {
        let mut state = TransparentRoutingState::default();
        let mut routes = HashMap::new();
        let mut subs = OutboxSubscriptions::default();
        let stale = OutboxSubId(11);
        let active = OutboxSubId(12);
        insert_sub_with_policy_for_relay(
            &mut subs,
            stale,
            RelayRoutingPreference::PreferDedicated,
            "wss://routing-state.example.com",
        );
        insert_sub_with_policy_for_relay(
            &mut subs,
            active,
            RelayRoutingPreference::PreferDedicated,
            "wss://routing-state.example.com",
        );

        state.set_transparent_route(&mut routes, &subs, stale);
        state.set_transparent_route(&mut routes, &subs, active);
        state.clear_route(&mut routes, stale);

        assert_eq!(state.demotable_queue_lengths(), (0, 1));
        assert!(!state.has_indexed_entry(stale));
        let demoted = state.pick_demotable(&subs, OutboxSubId(101), &HashSet::new());
        assert_eq!(demoted, Some(active));
    }
}
