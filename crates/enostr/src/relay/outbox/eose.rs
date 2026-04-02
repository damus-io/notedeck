use hashbrown::{HashMap, HashSet};

use crate::relay::{NormRelayUrl, OutboxSubId, OutboxSubscriptions};

/// One relay leg whose EOSE tracking should be invalidated.
pub(super) struct ChangedRelayLeg {
    pub(super) relay: NormRelayUrl,
    pub(super) sub_id: OutboxSubId,
}

/// Immutable tracker invalidation plan derived from one session delta.
pub(super) struct TrackerInvalidationPlan<'a> {
    pub(super) changed_legs: &'a [ChangedRelayLeg],
    pub(super) removed_subs: &'a HashSet<OutboxSubId>,
}

/// Pure projection from a session delta to tracker invalidation operations.
pub(super) fn plan_tracker_invalidation<'a>(
    changed_legs: &'a [ChangedRelayLeg],
    removed_subs: &'a HashSet<OutboxSubId>,
) -> TrackerInvalidationPlan<'a> {
    TrackerInvalidationPlan {
        changed_legs,
        removed_subs,
    }
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

/// Tracks which relay legs have reached EOSE for each subscription.
#[derive(Default)]
pub(super) struct EoseTracker {
    by_sub: HashMap<OutboxSubId, HashSet<NormRelayUrl>>,
    fully_eosed: HashSet<OutboxSubId>,
    pending_relays: HashSet<NormRelayUrl>,
    ready_fully_eosed: HashSet<OutboxSubId>,
}

impl EoseTracker {
    /// Reconciles one subscription's cached completion state against its
    /// current relay set and queues a ready transition on `false -> true`.
    fn reconcile_sub(&mut self, subs: &OutboxSubscriptions, id: OutboxSubId) {
        let Some(sub) = subs.get(&id) else {
            self.remove_sub(&id);
            return;
        };

        let was_fully_eosed = self.fully_eosed.contains(&id);
        let now_fully_eosed = {
            let Some(relays) = self.by_sub.get_mut(&id) else {
                self.fully_eosed.remove(&id);
                self.ready_fully_eosed.remove(&id);
                return;
            };

            relays.retain(|relay| sub.relays.contains(relay));
            !sub.relays.is_empty() && relays.len() == sub.relays.len()
        };

        if self.by_sub.get(&id).is_some_and(HashSet::is_empty) {
            self.by_sub.remove(&id);
        }

        if now_fully_eosed {
            self.fully_eosed.insert(id);
            if !was_fully_eosed {
                self.ready_fully_eosed.insert(id);
            }
            return;
        }

        self.fully_eosed.remove(&id);
        self.ready_fully_eosed.remove(&id);
    }

    /// Computes full-EOSE status from the tracker source of truth.
    fn computed_fully_eosed(&self, subs: &OutboxSubscriptions, id: &OutboxSubId) -> bool {
        let Some(sub) = subs.get(id) else {
            return false;
        };
        if sub.relays.is_empty() {
            return false;
        }

        self.by_sub
            .get(id)
            .is_some_and(|relays| sub.relays.iter().all(|relay| relays.contains(relay)))
    }

    /// Marks one relay as having pending EOSE entries to ingest.
    pub(super) fn note_relay_pending(&mut self, relay: &NormRelayUrl) {
        if !self.pending_relays.contains(relay) {
            self.pending_relays.insert(relay.clone());
        }
    }

    /// Returns and clears relays that should run EOSE ingest work.
    pub(super) fn drain_pending_relays(&mut self) -> HashSet<NormRelayUrl> {
        std::mem::take(&mut self.pending_relays)
    }

    /// Records the relay pending flag after one ingest pass.
    pub(super) fn set_relay_pending_state(&mut self, relay: &NormRelayUrl, has_pending: bool) {
        if has_pending {
            self.note_relay_pending(relay);
        } else {
            self.pending_relays.remove(relay);
        }
    }

    /// Marks one relay leg as EOSE-complete and queues fully-EOSE subscriptions.
    pub(super) fn mark_relay_eose(
        &mut self,
        relay: &NormRelayUrl,
        id: OutboxSubId,
        subs: &OutboxSubscriptions,
    ) {
        let Some(sub) = subs.get(&id) else {
            return;
        };
        if !sub.relays.contains(relay) {
            return;
        }

        let eosed_relays = self.by_sub.entry(id).or_default();
        if !eosed_relays.insert(relay.clone()) {
            return;
        }

        self.reconcile_sub(subs, id);
    }

    /// Returns and clears fully-EOSE subscriptions pending post-processing.
    pub(super) fn drain_fully_eosed(&mut self) -> HashSet<OutboxSubId> {
        std::mem::take(&mut self.ready_fully_eosed)
    }

    /// Invalidates one relay leg after subscribe/unsubscribe restaging and
    /// reconciles the subscription against its current relay set.
    pub(super) fn invalidate_relay_leg(
        &mut self,
        relay: &NormRelayUrl,
        id: OutboxSubId,
        subs: &OutboxSubscriptions,
    ) {
        let Some(relays) = self.by_sub.get_mut(&id) else {
            return;
        };
        relays.remove(relay);
        self.reconcile_sub(subs, id);
    }

    /// Removes all EOSE state for a subscription when it is dropped.
    pub(super) fn remove_sub(&mut self, id: &OutboxSubId) {
        self.by_sub.remove(id);
        self.fully_eosed.remove(id);
        self.ready_fully_eosed.remove(id);
    }

    /// True when every currently routed relay leg has reached EOSE.
    pub(super) fn is_fully_eosed(&self, subs: &OutboxSubscriptions, id: &OutboxSubId) -> bool {
        let computed = self.computed_fully_eosed(subs, id);
        debug_assert_eq!(
            self.fully_eosed.contains(id),
            computed,
            "fully_eosed cache must match current relay-set reconciliation"
        );
        computed
    }

    /// True once at least one relay leg has reached EOSE for this subscription.
    pub(super) fn has_any_eose(&self, subs: &OutboxSubscriptions, id: &OutboxSubId) -> bool {
        if subs.get(id).is_none() {
            return false;
        }
        self.by_sub.get(id).is_some_and(|relays| !relays.is_empty())
    }
}
