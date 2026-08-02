use hashbrown::{HashMap, HashSet};

use super::{aggregate_outbox_sub_relay_eose, OutboxSubRelayEose};
use crate::relay::{
    NormRelayUrl, OutboxSubId, OutboxSubscriptions, RelayLegReadiness, RelayReqStatus,
};

/// One relay leg whose EOSE tracking should be invalidated.
pub(super) struct ChangedRelayLeg {
    pub(super) relay: NormRelayUrl,
    pub(super) sub_id: OutboxSubId,
}

/// Planned post-EOSE effects derived from fully completed subscription IDs.
///
/// Route-aware callers decide which fully completed subscriptions are safe to
/// optimize with `since`; the tracker layer stays ignorant of routing mode.
pub(super) struct FullyEosedEffectsPlan {
    pub(super) remove_oneshots: HashSet<OutboxSubId>,
    pub(super) optimize_since: HashSet<OutboxSubId>,
    pub(super) optimize_since_at: Option<u64>,
}

impl FullyEosedEffectsPlan {
    pub(super) fn is_empty(&self) -> bool {
        self.remove_oneshots.is_empty() && self.optimize_since.is_empty()
    }
}

/// Maintained relay-leg readiness for one subscription.
#[derive(Default)]
struct EoseSubState {
    legs: HashMap<NormRelayUrl, RelayLegReadiness>,
    aggregate: OutboxSubRelayEose,
}

impl EoseSubState {
    fn recompute_aggregate(&mut self) {
        self.aggregate = aggregate_outbox_sub_relay_eose(self.legs.values().copied());
    }

    fn set_leg(&mut self, relay: NormRelayUrl, readiness: RelayLegReadiness) {
        self.legs.insert(relay, readiness);
        self.recompute_aggregate();
    }

    fn remove_leg(&mut self, relay: &NormRelayUrl) {
        self.legs.remove(relay);
        self.recompute_aggregate();
    }

    #[cfg(test)]
    fn has_any_eose(&self) -> bool {
        self.legs
            .values()
            .any(|readiness| matches!(readiness, RelayLegReadiness::Placed(RelayReqStatus::Eose)))
    }
}

/// Tracks relay-leg readiness for each subscription.
#[derive(Default)]
pub(super) struct EoseTracker {
    by_sub: HashMap<OutboxSubId, EoseSubState>,
}

impl EoseTracker {
    /// Set the current readiness for one retained relay leg.
    ///
    /// Returns true only when this mutation changes the aggregate from not
    /// fully EOSE to fully EOSE.
    pub(super) fn set_relay_leg_readiness(
        &mut self,
        relay: NormRelayUrl,
        id: OutboxSubId,
        readiness: RelayLegReadiness,
    ) -> bool {
        let state = self.by_sub.entry(id).or_default();
        let was_all_eosed = state.aggregate.all_eosed;
        state.set_leg(relay, readiness);
        !was_all_eosed && state.aggregate.all_eosed
    }

    /// Remove one relay leg from the maintained aggregate.
    ///
    /// Returns true only when this mutation changes the aggregate from not
    /// fully EOSE to fully EOSE.
    pub(super) fn remove_relay_leg(&mut self, relay: &NormRelayUrl, id: OutboxSubId) -> bool {
        let Some(state) = self.by_sub.get_mut(&id) else {
            return false;
        };
        let was_all_eosed = state.aggregate.all_eosed;
        state.remove_leg(relay);
        let now_all_eosed = state.aggregate.all_eosed;
        if state.legs.is_empty() {
            self.by_sub.remove(&id);
        }
        !was_all_eosed && now_all_eosed
    }

    /// Marks one relay leg as EOSE-complete.
    ///
    /// Returns true only when this mutation changes the aggregate from not
    /// fully EOSE to fully EOSE.
    pub(super) fn mark_relay_eose(
        &mut self,
        relay: &NormRelayUrl,
        id: OutboxSubId,
        subs: &OutboxSubscriptions,
    ) -> bool {
        let Some(sub) = subs.get(&id) else {
            return false;
        };
        if !sub.relays.contains(relay) {
            return false;
        }

        self.set_relay_leg_readiness(
            relay.clone(),
            id,
            RelayLegReadiness::Placed(RelayReqStatus::Eose),
        )
    }

    /// Removes all EOSE state for a subscription when it is dropped.
    pub(super) fn remove_sub(&mut self, id: &OutboxSubId) {
        self.by_sub.remove(id);
    }

    /// True when every currently routed relay leg has reached EOSE.
    #[cfg(test)]
    pub(super) fn is_fully_eosed(&self, subs: &OutboxSubscriptions, id: &OutboxSubId) -> bool {
        let _ = subs;
        self.by_sub
            .get(id)
            .is_some_and(|state| state.aggregate.all_eosed)
    }

    /// Return maintained aggregate readiness for one subscription.
    pub(super) fn sub_relay_eose(&self, id: &OutboxSubId) -> Option<OutboxSubRelayEose> {
        self.by_sub.get(id).map(|state| state.aggregate)
    }

    /// True once at least one relay leg has reached EOSE for this subscription.
    #[cfg(test)]
    pub(super) fn has_any_eose(&self, subs: &OutboxSubscriptions, id: &OutboxSubId) -> bool {
        let _ = subs;
        self.by_sub.get(id).is_some_and(EoseSubState::has_any_eose)
    }
}
