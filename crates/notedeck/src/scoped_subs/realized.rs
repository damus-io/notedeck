use enostr::{FullHistorySubId, FullHistoryTarget, NormRelayUrl, OutboxIdRegistry, OutboxSubId};
use hashbrown::{HashMap, HashSet};

use super::config::{ScopedSubKey, SubConfig};
use super::live::LiveSubState;
use super::live::{AuthorOutboxLiveRefresh, ScopedSubLiveRuntime};
use super::planner::{LivePlan, ScopedSubPlan};
use super::route_work::RouteWorkResult;
use super::ScopedSubOutboxOps;

/// Live and full-history outbox ids currently realized for retained scoped subs.
#[derive(Default)]
pub(super) struct ScopedSubRealizedState {
    live: ScopedSubLiveRuntime,
    full_history: HashMap<ScopedSubKey, RealizedFullHistory>,
}

pub(super) struct ActivePlanApplication<'a> {
    pub(super) account_read_relays: &'a HashSet<NormRelayUrl>,
    pub(super) scoped: ScopedSubKey,
    pub(super) previous: Option<&'a SubConfig>,
    pub(super) spec: &'a SubConfig,
    pub(super) plan: ScopedSubPlan<'a>,
}

struct LivePlanApplication<'a> {
    account_read_relays: &'a HashSet<NormRelayUrl>,
    scoped: ScopedSubKey,
    previous: Option<&'a SubConfig>,
    spec: &'a SubConfig,
    live: LivePlan<'a>,
}

#[derive(Clone, Debug)]
struct RealizedFullHistory {
    id: FullHistorySubId,
    targets: Vec<FullHistoryTarget>,
}

impl ScopedSubRealizedState {
    pub(super) fn live_state(&self, scoped: &ScopedSubKey) -> Option<&LiveSubState> {
        self.live.get(scoped)
    }

    pub(super) fn scoped_keys_for_live_id(&self, live_id: OutboxSubId) -> Vec<ScopedSubKey> {
        self.live.scoped_keys_for_live_id(live_id)
    }

    pub(super) fn contains_live(&self, scoped: &ScopedSubKey) -> bool {
        self.live.contains_key(scoped)
    }

    pub(super) fn remove_scoped(
        &mut self,
        ids: &OutboxIdRegistry,
        scoped: &ScopedSubKey,
    ) -> ScopedSubOutboxOps {
        let mut outbox_ops = self.live.remove_live_sub(ids, scoped);
        let ops = self.remove_full_history(scoped);
        outbox_ops.extend(ops);
        outbox_ops
    }

    pub(super) fn apply_active_plan(
        &mut self,
        ids: &OutboxIdRegistry,
        application: ActivePlanApplication<'_>,
    ) -> (RouteWorkResult, ScopedSubOutboxOps) {
        let mut outbox_ops = self.apply_full_history(
            ids,
            &application.scoped,
            application.plan.full_history_targets,
        );
        let (result, ops) = self.apply_live_plan(
            ids,
            LivePlanApplication {
                account_read_relays: application.account_read_relays,
                scoped: application.scoped,
                previous: application.previous,
                spec: application.spec,
                live: application.plan.live,
            },
        );
        outbox_ops.extend(ops);
        (result, outbox_ops)
    }

    fn apply_live_plan(
        &mut self,
        ids: &OutboxIdRegistry,
        application: LivePlanApplication<'_>,
    ) -> (RouteWorkResult, ScopedSubOutboxOps) {
        match application.live {
            LivePlan::Single => {
                let outbox_ops = match application.previous {
                    Some(previous) => self.live.update_single_live_state_for_set_sub(
                        ids,
                        application.account_read_relays,
                        application.scoped,
                        previous,
                        application.spec,
                    ),
                    None => self.live.ensure_live_sub(
                        ids,
                        application.account_read_relays,
                        application.scoped,
                        application.spec,
                    ),
                };
                (RouteWorkResult::Complete, outbox_ops)
            }
            LivePlan::AccountsReadPlusExplicit => {
                let outbox_ops = self.live.ensure_accounts_read_plus_explicit_live_sub(
                    ids,
                    application.account_read_relays,
                    application.scoped,
                    application.spec,
                    application.previous,
                );
                (RouteWorkResult::Complete, outbox_ops)
            }
            LivePlan::AccountsReadWithAuthorOutbox {
                plan_generation,
                routed_relays,
            } => self.live.refresh_author_outbox_live_sub(
                ids,
                AuthorOutboxLiveRefresh {
                    account_read_relays: application.account_read_relays,
                    scoped: application.scoped,
                    spec: application.spec,
                    previous: application.previous,
                    plan_generation,
                    routed_relays,
                },
            ),
        }
    }

    fn apply_full_history(
        &mut self,
        ids: &OutboxIdRegistry,
        scoped: &ScopedSubKey,
        targets: Vec<FullHistoryTarget>,
    ) -> ScopedSubOutboxOps {
        let retained_targets = targets.clone();

        if targets.is_empty() {
            return self.remove_full_history(scoped);
        }

        let mut outbox_ops = ScopedSubOutboxOps::default();
        if let Some(history_id) = self.full_history.get(scoped).map(|history| history.id) {
            if outbox_ops.modify_full_history_targets(history_id, targets) {
                if let Some(history) = self.full_history.get_mut(scoped) {
                    history.targets = retained_targets;
                }
                return outbox_ops;
            }

            self.full_history.remove(scoped);
            return outbox_ops;
        }

        let Some(history_id) = outbox_ops.try_subscribe_full_history_targets(ids, targets) else {
            return outbox_ops;
        };
        self.full_history.insert(
            scoped.clone(),
            RealizedFullHistory {
                id: history_id,
                targets: retained_targets,
            },
        );
        outbox_ops
    }

    fn remove_full_history(&mut self, scoped: &ScopedSubKey) -> ScopedSubOutboxOps {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        if let Some(history) = self.full_history.remove(scoped) {
            outbox_ops.remove_full_history(history.id);
        }
        outbox_ops
    }

    #[cfg(test)]
    pub(super) fn full_history_id_for_test(
        &self,
        scoped: &ScopedSubKey,
    ) -> Option<FullHistorySubId> {
        self.full_history.get(scoped).map(|history| history.id)
    }

    #[cfg(test)]
    pub(super) fn full_history_targets_for_test(
        &self,
        scoped: &ScopedSubKey,
    ) -> Option<&[FullHistoryTarget]> {
        self.full_history
            .get(scoped)
            .map(|history| history.targets.as_slice())
    }

    #[cfg(test)]
    pub(super) fn live_len(&self) -> usize {
        self.live.len()
    }

    #[cfg(test)]
    pub(super) fn live_sub_ids_for_test(&self, scoped: &ScopedSubKey) -> Vec<OutboxSubId> {
        let Some(live) = self.live.get(scoped) else {
            return Vec::new();
        };

        match live {
            LiveSubState::Single(id) => vec![*id],
            LiveSubState::AccountsReadPlusExplicit(state) => {
                let mut ids = Vec::new();
                if let Some(baseline) = state.baseline_id() {
                    ids.push(baseline);
                }
                if let Some(shared_live_id) = state.shared_id() {
                    ids.push(shared_live_id);
                }
                ids
            }
            LiveSubState::AccountsReadWithAuthorOutbox(state) => {
                let mut ids = Vec::new();
                if let Some(baseline) = state.baseline_id() {
                    ids.push(baseline);
                }
                if let Some(routed) = state.routed() {
                    ids.extend(routed.legs.values().filter_map(|leg| leg.live_id));
                }
                ids
            }
        }
    }

    #[cfg(test)]
    pub(super) fn routed_live_legs_for_test(
        &self,
        scoped: &ScopedSubKey,
    ) -> Vec<(NormRelayUrl, OutboxSubId)> {
        let Some(live) = self.live.get(scoped) else {
            return Vec::new();
        };

        match live {
            LiveSubState::Single(_) => Vec::new(),
            LiveSubState::AccountsReadPlusExplicit(_) => Vec::new(),
            LiveSubState::AccountsReadWithAuthorOutbox(state) => state
                .routed()
                .map(|routed| {
                    routed
                        .legs
                        .values()
                        .filter_map(|leg| leg.live_id.map(|live_id| (leg.relay.clone(), live_id)))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    #[cfg(test)]
    pub(super) fn routed_live_author_sets_for_test(
        &self,
        scoped: &ScopedSubKey,
    ) -> Vec<(NormRelayUrl, OutboxSubId, Vec<Vec<enostr::Pubkey>>)> {
        let Some(LiveSubState::AccountsReadWithAuthorOutbox(state)) = self.live.get(scoped) else {
            return Vec::new();
        };

        let Some(routed) = state.routed() else {
            return Vec::new();
        };

        routed
            .legs
            .values()
            .filter_map(|leg| {
                let live_id = leg.live_id?;
                Some((leg.relay.clone(), live_id, leg.author_sets_for_test()))
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn live_id_relay_url_source_for_test(
        &self,
        scoped: &ScopedSubKey,
        live_id: OutboxSubId,
    ) -> Option<enostr::RelayUrlSource> {
        let live = self.live.get(scoped)?;
        match live {
            LiveSubState::Single(_) => None,
            LiveSubState::AccountsReadPlusExplicit(state) => state.shared_source_for_id(live_id),
            LiveSubState::AccountsReadWithAuthorOutbox(state) => {
                state.routed().and_then(|routed| {
                    routed
                        .legs
                        .values()
                        .any(|leg| leg.live_id == Some(live_id))
                        .then_some(routed.relay_url_source)
                })
            }
        }
    }

    #[cfg(test)]
    pub(super) fn single_live_id_for_scoped_for_test(&self, scoped: &ScopedSubKey) -> OutboxSubId {
        match self.live.get(scoped) {
            Some(LiveSubState::Single(id)) => *id,
            Some(LiveSubState::AccountsReadPlusExplicit(_))
            | Some(LiveSubState::AccountsReadWithAuthorOutbox(_)) => {
                panic!("expected single live sub state for {scoped:?}");
            }
            None => {
                panic!("missing live sub state for {scoped:?}");
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn baseline_live_id_for_test(&self, scoped: &ScopedSubKey) -> Option<OutboxSubId> {
        match self.live.get(scoped) {
            Some(LiveSubState::Single(id)) => Some(*id),
            Some(LiveSubState::AccountsReadPlusExplicit(state)) => state.baseline_id(),
            Some(LiveSubState::AccountsReadWithAuthorOutbox(state)) => state.baseline_id(),
            _ => None,
        }
    }
}
