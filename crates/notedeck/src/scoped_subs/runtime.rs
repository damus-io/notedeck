use enostr::{
    NormRelayUrl, OutboxIdRegistry, OutboxSubId, OutboxSubRelayEose, Pubkey, RelayReqStatus,
};
use hashbrown::{HashMap, HashSet};
use std::time::Instant;

use super::author_plan::{
    AuthorOutboxPlanAdvance, AuthorOutboxPlanAdvanceRequest, AuthorOutboxPlanJobCompletion,
    AuthorOutboxPlanRuntime,
};
use super::author_runtime::ScopedAuthorOutboxRuntime;
use super::config::{
    ClearSubResult, ResolvedSubScope, ScopedSubKey, SetSubResult, SubConfig, SubKey, SubOwnerKey,
    SubScope,
};
use super::config::{ScopedSubLiveReadiness, ScopedSubReadiness, ScopedSubRelayEoseStatus};
use super::live::LiveSubState;
use super::planner::{plan_scoped_sub, ScopedSubPlan};
use super::realized::{ActivePlanApplication, ScopedSubRealizedState};
use super::route_work::RouteWorkResult;
use super::store::{ScopedSubStore, ScopedSubStoreRelease};
use super::transition::{effective_sub_transition, ActiveSubTransition, EffectiveSubTransition};
use super::{
    ScopedSubCommand, ScopedSubDelta, ScopedSubEffects, ScopedSubFact, ScopedSubOutboxOps,
    ScopedSubOutput,
};

#[derive(Default)]
pub(crate) struct ScopedSubRuntime {
    ids: OutboxIdRegistry,
    pub(super) store: ScopedSubStore,
    pub(super) realized: ScopedSubRealizedState,
    pub(super) author_outbox: ScopedAuthorOutboxRuntime,
    pub(super) author_outbox_plans: AuthorOutboxPlanRuntime,
    committed_relay_eose: HashMap<OutboxSubId, OutboxSubRelayEose>,
    published_readiness: HashMap<ScopedSubKey, ScopedSubReadiness>,
}

#[derive(Clone, Copy)]
struct ScopedSubRuntimeContext<'a> {
    ids: &'a OutboxIdRegistry,
    selected_account_pubkey: Pubkey,
    account_read_relays: &'a HashSet<NormRelayUrl>,
}

impl<'a> ScopedSubRuntimeContext<'a> {
    fn new(
        ids: &'a OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        account_read_relays: &'a HashSet<NormRelayUrl>,
    ) -> Self {
        Self {
            ids,
            selected_account_pubkey,
            account_read_relays,
        }
    }
}

fn empty_relay_eose() -> ScopedSubRelayEoseStatus {
    ScopedSubRelayEoseStatus {
        tracked_relays: 0,
        unsupported_relays: 0,
        any_eose: false,
        all_eosed: false,
    }
}

fn pending_relay_eose() -> ScopedSubRelayEoseStatus {
    ScopedSubRelayEoseStatus {
        tracked_relays: 1,
        unsupported_relays: 0,
        any_eose: false,
        all_eosed: false,
    }
}

fn scoped_relay_eose(relay_eose: OutboxSubRelayEose) -> ScopedSubRelayEoseStatus {
    ScopedSubRelayEoseStatus {
        tracked_relays: relay_eose.tracked_relays,
        unsupported_relays: relay_eose.unsupported_relays,
        any_eose: relay_eose.any_eose,
        all_eosed: relay_eose.all_eosed,
    }
}

fn merge_relay_eose(
    aggregate: &mut Option<ScopedSubRelayEoseStatus>,
    next: ScopedSubRelayEoseStatus,
) {
    let Some(aggregate) = aggregate.as_mut() else {
        *aggregate = Some(next);
        return;
    };

    aggregate.tracked_relays += next.tracked_relays;
    aggregate.unsupported_relays += next.unsupported_relays;
    aggregate.any_eose |= next.any_eose;
    aggregate.all_eosed = aggregate.all_eosed && next.all_eosed && aggregate.tracked_relays > 0;
}

/// Appends committed aggregate readiness for every desired serviceable relay
/// leg on one live id.
fn append_live_id_relay_eose(
    relay_eose: &mut Option<ScopedSubRelayEoseStatus>,
    outbox_sub_relay_eose: &impl Fn(&OutboxSubId) -> Option<OutboxSubRelayEose>,
    live_id: &OutboxSubId,
) {
    if let Some(eose) = outbox_sub_relay_eose(live_id) {
        merge_relay_eose(relay_eose, scoped_relay_eose(eose));
    }
}

/// Appends a routed leg, preserving the pending leg when scopedsubs has planned
/// the author-outbox route but it has not materialized an `OutboxSubId` yet.
fn append_routed_leg_relay_eose(
    relay_eose: &mut Option<ScopedSubRelayEoseStatus>,
    outbox_sub_relay_eose: &impl Fn(&OutboxSubId) -> Option<OutboxSubRelayEose>,
    live_id: &OutboxSubId,
) {
    let next = outbox_sub_relay_eose(live_id)
        .map(scoped_relay_eose)
        .unwrap_or_else(pending_relay_eose);
    merge_relay_eose(relay_eose, next);
}

fn build_scoped_sub_plan<'a>(
    spec: &SubConfig,
    account_read_relays: &HashSet<NormRelayUrl>,
    author_outbox_advance: AuthorOutboxPlanAdvance<'a>,
) -> (ScopedSubPlan<'a>, bool) {
    let plan_pending = matches!(&author_outbox_advance, AuthorOutboxPlanAdvance::Pending);
    let author_outbox_routes = match author_outbox_advance {
        AuthorOutboxPlanAdvance::Ready { routes, generation } => Some((routes, generation)),
        AuthorOutboxPlanAdvance::Pending | AuthorOutboxPlanAdvance::NotAuthorOutbox => None,
    };
    (
        plan_scoped_sub(spec, account_read_relays, author_outbox_routes),
        plan_pending,
    )
}

fn apply_scoped_sub_plan(
    realized: &mut ScopedSubRealizedState,
    ids: &OutboxIdRegistry,
    application: ActivePlanApplication<'_>,
) -> (RouteWorkResult, ScopedSubOutboxOps) {
    realized.apply_active_plan(ids, application)
}

struct AuthorOutboxPlanApplication<'a> {
    context: ScopedSubRuntimeContext<'a>,
    scoped: ScopedSubKey,
    previous: Option<&'a SubConfig>,
    next: &'a SubConfig,
}

fn advance_author_outbox_plan(
    author_outbox_plans: &mut AuthorOutboxPlanRuntime,
    realized: &mut ScopedSubRealizedState,
    application: AuthorOutboxPlanApplication<'_>,
) -> (RouteWorkResult, bool, ScopedSubOutboxOps, ScopedSubEffects) {
    let AuthorOutboxPlanApplication {
        context,
        scoped,
        previous,
        next,
    } = application;

    let advance_result = author_outbox_plans.advance(AuthorOutboxPlanAdvanceRequest {
        account_pubkey: context.selected_account_pubkey,
        scoped: scoped.clone(),
        account_read_relays: context.account_read_relays,
        spec: next,
    });
    let (plan, plan_pending) =
        build_scoped_sub_plan(next, context.account_read_relays, advance_result.advance);
    let (result, ops) = apply_scoped_sub_plan(
        realized,
        context.ids,
        ActivePlanApplication {
            account_read_relays: context.account_read_relays,
            scoped,
            previous,
            spec: next,
            plan,
        },
    );
    let mut outbox_ops = advance_result.pre_realization_ops;
    outbox_ops.extend(ops);
    let effects = advance_result.effects;
    (result, plan_pending, outbox_ops, effects)
}

impl ScopedSubRuntime {
    /// Create a runtime that allocates outbox ids from the bridge-owned outbox
    /// service namespace.
    pub(crate) fn with_ids(ids: OutboxIdRegistry) -> Self {
        Self {
            ids,
            ..Self::default()
        }
    }

    fn ids(&self) -> OutboxIdRegistry {
        self.ids.clone()
    }

    /// Return the next bridge wake deadline for retained author-outbox
    /// relay-list discovery retry work.
    pub(crate) fn next_author_outbox_retry_deadline(&self) -> Option<Instant> {
        self.author_outbox_plans.next_deadline()
    }

    /// Apply one committed relay request status event to retained author-outbox
    /// relay-list discovery state.
    pub(crate) fn apply_author_outbox_relay_req_status(
        &mut self,
        id: OutboxSubId,
        relay: &NormRelayUrl,
        status: Option<RelayReqStatus>,
    ) -> ScopedSubDelta {
        let (outbox_ops, effects) = self
            .author_outbox_plans
            .apply_relay_req_status(id, relay, status);
        ScopedSubDelta::new_with_effects(ScopedSubOutput::default(), outbox_ops, effects)
    }

    /// Apply one completed author-outbox plan job and realize the scoped
    /// subscriptions owned by that plan slot.
    pub(crate) fn apply_author_outbox_plan_completed(
        &mut self,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        completion: AuthorOutboxPlanJobCompletion,
    ) -> ScopedSubDelta {
        let ids = self.ids();
        self.apply_author_outbox_plan_completed_with_ids(
            &ids,
            selected_account_pubkey,
            account_read_relays,
            completion,
        )
    }

    fn apply_author_outbox_plan_completed_with_ids(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        completion: AuthorOutboxPlanJobCompletion,
    ) -> ScopedSubDelta {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let (scoped_keys, ops) =
            self.author_outbox_plans
                .apply_plan_slot_ready(ids, completion, account_read_relays);
        outbox_ops.extend(ops);
        if scoped_keys.is_empty() {
            return ScopedSubDelta::new(ScopedSubOutput::default(), outbox_ops);
        }

        let (ops, effects) = self.apply_author_outbox_plans_for_scoped_keys_with_effects(
            ids,
            selected_account_pubkey,
            scoped_keys.clone(),
            account_read_relays,
        );
        outbox_ops.extend(ops);
        let output = self.readiness_facts(scoped_keys);
        ScopedSubDelta::new_with_effects(output, outbox_ops, effects)
    }

    /// Apply retained author-outbox relay-list discovery retry deadlines.
    pub(crate) fn apply_author_outbox_discovery_retry_due(
        &mut self,
        now: Instant,
    ) -> ScopedSubDelta {
        let (outbox_ops, effects) = self
            .author_outbox_plans
            .apply_relay_list_discovery_retry_due(now);
        ScopedSubDelta::new_with_effects(ScopedSubOutput::default(), outbox_ops, effects)
    }

    /// Apply one committed live-sub EOSE aggregate fact and emit readiness
    /// facts for scoped subscriptions backed by that outbox sub id.
    pub(crate) fn apply_outbox_sub_relay_eose(
        &mut self,
        id: OutboxSubId,
        relay_eose: Option<OutboxSubRelayEose>,
    ) -> ScopedSubDelta {
        if let Some(relay_eose) = relay_eose {
            self.committed_relay_eose.insert(id, relay_eose);
        } else {
            self.committed_relay_eose.remove(&id);
        }

        let scoped_keys = self.realized.scoped_keys_for_live_id(id);
        let output = self.readiness_facts(scoped_keys);
        ScopedSubDelta::new(output, ScopedSubOutboxOps::default())
    }

    /// Return exact readiness facts for scoped keys affected by the current
    /// transition.
    fn readiness_facts(
        &mut self,
        scoped_keys: impl IntoIterator<Item = ScopedSubKey>,
    ) -> ScopedSubOutput {
        let mut output = ScopedSubOutput::default();
        let scoped_keys = scoped_keys.into_iter().collect::<HashSet<_>>();
        for scoped in scoped_keys {
            let readiness = self.committed_scoped_readiness(&scoped);
            let previous = self
                .published_readiness
                .get(&scoped)
                .copied()
                .unwrap_or(ScopedSubReadiness::Missing);
            if previous == readiness {
                continue;
            }

            if readiness == ScopedSubReadiness::Missing {
                self.published_readiness.remove(&scoped);
            } else {
                self.published_readiness.insert(scoped.clone(), readiness);
            }
            output.push(ScopedSubFact::ReadinessChanged { scoped, readiness });
        }
        output
    }

    fn committed_scoped_readiness(&self, scoped: &ScopedSubKey) -> ScopedSubReadiness {
        self.scoped_readiness(&|id| self.committed_relay_eose.get(id).copied(), scoped)
    }

    pub(super) fn scoped_key(scope: ResolvedSubScope, key: SubKey) -> ScopedSubKey {
        ScopedSubKey { scope, key }
    }

    fn scoped_key_for_account(
        account_pubkey: Pubkey,
        scope: SubScope,
        key: SubKey,
    ) -> ScopedSubKey {
        Self::scoped_key(resolve_scope_for_account(&scope, account_pubkey), key)
    }

    /// Apply the first selected-account snapshot observed by the bridge.
    pub(crate) fn apply_account_initialized(
        &mut self,
        selected_account_pubkey: Pubkey,
    ) -> ScopedSubDelta {
        self.apply_selected_account(selected_account_pubkey);
        ScopedSubDelta::default()
    }

    /// Apply selected-account activation to retained author-outbox demand.
    fn apply_selected_account(&mut self, selected_account_pubkey: Pubkey) {
        self.author_outbox
            .apply_selected_account(selected_account_pubkey);
    }

    /// Apply one bridge-owned scoped-sub command and emit readiness facts for
    /// scoped keys whose command-visible state may have changed.
    pub(crate) fn apply_command(
        &mut self,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        command: ScopedSubCommand,
    ) -> ScopedSubDelta {
        let ids = self.ids();
        self.apply_command_with_ids(&ids, selected_account_pubkey, account_read_relays, command)
    }

    fn apply_command_with_ids(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        command: ScopedSubCommand,
    ) -> ScopedSubDelta {
        let context =
            ScopedSubRuntimeContext::new(ids, selected_account_pubkey, account_read_relays);
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let mut effects = ScopedSubEffects::default();
        let changed_scoped = match command {
            ScopedSubCommand::SetOwnerConfig {
                account_pubkey,
                owner,
                scope,
                key,
                config,
            } => {
                let scoped = Self::scoped_key_for_account(account_pubkey, scope, key);
                let (_, ops, command_effects) = self.set_owner_config_for_account(
                    context.ids,
                    context.selected_account_pubkey,
                    context.account_read_relays,
                    account_pubkey,
                    owner,
                    scope,
                    key,
                    config,
                );
                outbox_ops.extend(ops);
                effects.extend(command_effects);
                vec![scoped]
            }
            ScopedSubCommand::EnsureOwnerConfig {
                account_pubkey,
                owner,
                scope,
                key,
                config,
            } => {
                let scoped = Self::scoped_key_for_account(account_pubkey, scope, key);
                let (changed, ops, command_effects) = self.ensure_owner_config_for_account(
                    context.ids,
                    context.selected_account_pubkey,
                    context.account_read_relays,
                    account_pubkey,
                    owner,
                    scope,
                    key,
                    config,
                );
                outbox_ops.extend(ops);
                effects.extend(command_effects);
                if changed {
                    vec![scoped]
                } else {
                    Vec::new()
                }
            }
            ScopedSubCommand::ClearOwnerConfig {
                account_pubkey,
                owner,
                scope,
                key,
            } => {
                let scoped = Self::scoped_key_for_account(account_pubkey, scope, key);
                let (_, ops, command_effects) = self.clear_owner_config_for_account(
                    context.ids,
                    context.selected_account_pubkey,
                    context.account_read_relays,
                    account_pubkey,
                    owner,
                    key,
                    scope,
                );
                outbox_ops.extend(ops);
                effects.extend(command_effects);
                vec![scoped]
            }
            ScopedSubCommand::DropOwner { owner } => {
                let (changed, ops, command_effects) = self.drop_owner_with_relays_collect(
                    context.ids,
                    context.selected_account_pubkey,
                    context.account_read_relays,
                    owner,
                );
                outbox_ops.extend(ops);
                effects.extend(command_effects);
                changed
            }
            ScopedSubCommand::PurgeAccount { account_pubkey } => {
                let changed = self.readiness_scoped_keys_for_account(account_pubkey);
                let ops = self.purge_account_scope(
                    context.ids,
                    context.selected_account_pubkey,
                    context.account_read_relays,
                    account_pubkey,
                );
                outbox_ops.extend(ops);
                changed
            }
        };

        let output = self.readiness_facts(changed_scoped);
        ScopedSubDelta::new_with_effects(output, outbox_ops, effects)
    }

    /// Apply `SetOwnerConfig` for the command account while realizing against
    /// the selected account.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn set_owner_config_for_account(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        scope: SubScope,
        key: SubKey,
        config: SubConfig,
    ) -> (SetSubResult, ScopedSubOutboxOps, ScopedSubEffects) {
        match scope {
            SubScope::Account if account_pubkey != selected_account_pubkey => self
                .set_inactive_account_sub_with_effects(
                    ids,
                    selected_account_pubkey,
                    account_pubkey,
                    owner,
                    key,
                    config,
                ),
            SubScope::Account | SubScope::Global => self.set_sub_with_relays_with_effects(
                ids,
                account_read_relays,
                selected_account_pubkey,
                owner,
                scope,
                key,
                config,
            ),
        }
    }

    /// Ensure one owner is attached, creating desired state only when absent.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn ensure_owner_config_for_account(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        scope: SubScope,
        key: SubKey,
        config: SubConfig,
    ) -> (bool, ScopedSubOutboxOps, ScopedSubEffects) {
        let scoped = Self::scoped_key(resolve_scope_for_account(&scope, account_pubkey), key);
        if self.store.owner_owns(owner, &scoped) {
            return (
                false,
                ScopedSubOutboxOps::default(),
                ScopedSubEffects::default(),
            );
        }
        if self.store.contains_desired(&scoped) {
            self.store.register_ownership(owner, &scoped);
            return (
                true,
                ScopedSubOutboxOps::default(),
                ScopedSubEffects::default(),
            );
        }

        let (_, ops, effects) = self.set_owner_config_for_account(
            ids,
            selected_account_pubkey,
            account_read_relays,
            account_pubkey,
            owner,
            scope,
            key,
            config,
        );
        (true, ops, effects)
    }

    /// Create-or-update desired state for one `(owner, key)`.
    ///
    /// Store updates produce one effective `SubConfig` transition for the
    /// resolved `ScopedSubKey`; transition code decides active vs. inactive vs.
    /// removed, planner builds active remote shape, and realized state owns ids.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn set_sub_with_relays_with_effects(
        &mut self,
        ids: &OutboxIdRegistry,
        account_read_relays: &HashSet<NormRelayUrl>,
        selected_account_pubkey: Pubkey,
        owner: SubOwnerKey,
        scope: SubScope,
        key: SubKey,
        config: SubConfig,
    ) -> (SetSubResult, ScopedSubOutboxOps, ScopedSubEffects) {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);

        self.store.register_ownership(owner, &scoped);

        let change = self
            .store
            .set_owner_config(scoped.clone(), owner, config.clone());
        let Some(config) = change.next else {
            return (
                SetSubResult::Unchanged,
                ScopedSubOutboxOps::default(),
                ScopedSubEffects::default(),
            );
        };
        let previous = change.previous;
        if previous.as_ref() == Some(&config) {
            return (
                SetSubResult::Unchanged,
                ScopedSubOutboxOps::default(),
                ScopedSubEffects::default(),
            );
        }

        let result = if previous.is_some() {
            SetSubResult::Updated
        } else {
            SetSubResult::Created
        };
        let context =
            ScopedSubRuntimeContext::new(ids, selected_account_pubkey, account_read_relays);
        let (_, outbox_ops, effects) =
            self.apply_desired_config_transition(context, scoped, previous.as_ref(), Some(&config));
        (result, outbox_ops, effects)
    }

    /// Apply one store-derived desired-state transition for a scoped key.
    fn apply_desired_config_transition(
        &mut self,
        context: ScopedSubRuntimeContext<'_>,
        scoped: ScopedSubKey,
        previous: Option<&SubConfig>,
        next: Option<&SubConfig>,
    ) -> (RouteWorkResult, ScopedSubOutboxOps, ScopedSubEffects) {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        match effective_sub_transition(context.selected_account_pubkey, scoped, previous, next) {
            EffectiveSubTransition::Removed { scoped, previous } => {
                let was_active = scoped.is_active_for_account(context.selected_account_pubkey);
                self.author_outbox
                    .release_transition(&scoped, previous, was_active);
                outbox_ops.extend(self.author_outbox_plans.remove_scoped(&scoped));
                outbox_ops.extend(self.realized.remove_scoped(context.ids, &scoped));
                (
                    RouteWorkResult::Complete,
                    outbox_ops,
                    ScopedSubEffects::default(),
                )
            }
            EffectiveSubTransition::Inactive {
                scoped,
                previous,
                next,
            } => {
                self.author_outbox
                    .retain_transition(scoped.clone(), previous, Some(next), false);
                if previous != Some(next) {
                    outbox_ops.extend(self.author_outbox_plans.remove_scoped(&scoped));
                }
                outbox_ops.extend(self.realized.remove_scoped(context.ids, &scoped));
                (
                    RouteWorkResult::Complete,
                    outbox_ops,
                    ScopedSubEffects::default(),
                )
            }
            EffectiveSubTransition::Active(transition) => {
                let scoped = transition.scoped.clone();
                self.author_outbox.retain_transition(
                    transition.scoped.clone(),
                    transition.previous,
                    Some(transition.next),
                    true,
                );
                let (result, outbox_ops, effects) = self.realize_active_config(context, transition);
                Self::trace_deferred_route_work(&scoped, result);
                (result, outbox_ops, effects)
            }
        }
    }

    /// Realize one retained active scoped config from current runtime inputs.
    ///
    /// Event paths use this when selected account, account-read relays, or
    /// author-outbox plan state changes without a desired `SubConfig` change.
    fn realize_retained_active_config(
        &mut self,
        context: ScopedSubRuntimeContext<'_>,
        scoped: ScopedSubKey,
        previous: Option<&SubConfig>,
        spec: &SubConfig,
    ) -> (RouteWorkResult, ScopedSubOutboxOps, ScopedSubEffects) {
        if !scoped.is_active_for_account(context.selected_account_pubkey) {
            let outbox_ops = self.realized.remove_scoped(context.ids, &scoped);
            return (
                RouteWorkResult::Complete,
                outbox_ops,
                ScopedSubEffects::default(),
            );
        }

        self.realize_active_config(
            context,
            ActiveSubTransition {
                scoped,
                previous,
                next: spec,
            },
        )
    }

    /// Apply one active scoped config to retained live/full-history state.
    ///
    /// Store owns the effective `SubConfig`; author demand is retained by the
    /// desired-transition path; this method only advances author-outbox planning
    /// and applies the planned live/full-history state to retained outbox ids.
    fn realize_active_config(
        &mut self,
        context: ScopedSubRuntimeContext<'_>,
        transition: ActiveSubTransition<'_>,
    ) -> (RouteWorkResult, ScopedSubOutboxOps, ScopedSubEffects) {
        let ActiveSubTransition {
            scoped,
            previous,
            next,
        } = transition;
        let (result, plan_pending, outbox_ops, effects) = advance_author_outbox_plan(
            &mut self.author_outbox_plans,
            &mut self.realized,
            AuthorOutboxPlanApplication {
                context,
                scoped,
                previous,
                next,
            },
        );

        (
            Self::route_work_result_after_plan_pending(result, plan_pending),
            outbox_ops,
            effects,
        )
    }

    fn route_work_result_after_plan_pending(
        result: RouteWorkResult,
        plan_pending: bool,
    ) -> RouteWorkResult {
        if result != RouteWorkResult::Complete {
            return result;
        }

        if plan_pending {
            RouteWorkResult::PlanPending
        } else {
            RouteWorkResult::Complete
        }
    }

    fn trace_deferred_route_work(scoped: &ScopedSubKey, result: RouteWorkResult) {
        match result {
            RouteWorkResult::Complete | RouteWorkResult::PlanPending => {}
            RouteWorkResult::FullRefreshRequired | RouteWorkResult::RebuildRequired => {
                tracing::debug!(
                    target: "outbox_perf",
                    ?scoped,
                    "author_outbox_route_work_deferred_without_transition"
                );
            }
        }
    }

    /// Apply `ClearOwnerConfig` for the command account while realizing against
    /// the selected account.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn clear_owner_config_for_account(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        key: SubKey,
        scope: SubScope,
    ) -> (ClearSubResult, ScopedSubOutboxOps, ScopedSubEffects) {
        let scoped = Self::scoped_key(resolve_scope_for_account(&scope, account_pubkey), key);
        let release = self.store.clear_owner_binding(owner, &scoped);
        self.apply_store_release(
            ids,
            selected_account_pubkey,
            account_read_relays,
            &scoped,
            release,
        )
    }

    /// Query readiness for one `(owner, key)` using an explicit selected account.
    #[cfg(test)]
    pub(super) fn sub_readiness_with_selected(
        &self,
        outbox_sub_relay_eose: &impl Fn(&OutboxSubId) -> Option<OutboxSubRelayEose>,
        selected_account_pubkey: Pubkey,
        owner: SubOwnerKey,
        key: SubKey,
        scope: SubScope,
    ) -> ScopedSubReadiness {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);

        if !self.store.owner_owns(owner, &scoped) {
            return ScopedSubReadiness::Missing;
        }

        self.scoped_readiness(outbox_sub_relay_eose, &scoped)
    }

    /// Query readiness for one scoped key after ownership has already been
    /// resolved by the caller.
    fn scoped_readiness(
        &self,
        outbox_sub_relay_eose: &impl Fn(&OutboxSubId) -> Option<OutboxSubRelayEose>,
        scoped: &ScopedSubKey,
    ) -> ScopedSubReadiness {
        if let Some(live_state) = self.realized.live_state(scoped) {
            let mut relay_eose = None;
            match live_state {
                LiveSubState::Single(live_id) => {
                    append_live_id_relay_eose(&mut relay_eose, outbox_sub_relay_eose, live_id);
                }
                LiveSubState::AccountsReadPlusExplicit(state) => {
                    if let Some(baseline) = state.baseline_id() {
                        append_live_id_relay_eose(
                            &mut relay_eose,
                            outbox_sub_relay_eose,
                            &baseline,
                        );
                    }
                    if let Some(shared_live_id) = state.shared_id() {
                        append_live_id_relay_eose(
                            &mut relay_eose,
                            outbox_sub_relay_eose,
                            &shared_live_id,
                        );
                    }
                }
                LiveSubState::AccountsReadWithAuthorOutbox(state) => {
                    if let Some(baseline) = state.baseline_id() {
                        append_live_id_relay_eose(
                            &mut relay_eose,
                            outbox_sub_relay_eose,
                            &baseline,
                        );
                    }
                    if let Some(routed) = state.routed() {
                        for leg in routed.legs.values() {
                            if let Some(live_id) = leg.live_id {
                                append_routed_leg_relay_eose(
                                    &mut relay_eose,
                                    outbox_sub_relay_eose,
                                    &live_id,
                                );
                            } else {
                                merge_relay_eose(&mut relay_eose, pending_relay_eose());
                            }
                        }
                    }
                }
            }

            let relay_eose = relay_eose.unwrap_or_else(empty_relay_eose);
            return ScopedSubReadiness::Live(ScopedSubLiveReadiness { relay_eose });
        }

        if self.store.contains_desired(scoped) {
            ScopedSubReadiness::Inactive
        } else {
            ScopedSubReadiness::Missing
        }
    }

    fn readiness_scoped_keys_for_scope(&self, scope: &ResolvedSubScope) -> Vec<ScopedSubKey> {
        self.store.owned_desired_keys_for_scope(scope)
    }

    fn readiness_scoped_keys_for_account(&self, account_pubkey: Pubkey) -> Vec<ScopedSubKey> {
        self.readiness_scoped_keys_for_scope(&ResolvedSubScope::Account(account_pubkey))
    }

    fn readiness_scoped_keys_for_selected_or_global(
        &self,
        selected_account_pubkey: Pubkey,
    ) -> Vec<ScopedSubKey> {
        self.store
            .owned_desired_keys_for_selected_or_global(selected_account_pubkey)
    }

    /// Drop one owner and return scoped keys whose realized/readiness state may
    /// have changed.
    pub(super) fn drop_owner_with_relays_collect(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        owner: SubOwnerKey,
    ) -> (Vec<ScopedSubKey>, ScopedSubOutboxOps, ScopedSubEffects) {
        let Some(scoped_keys) = self.store.take_owner(owner) else {
            return (
                Vec::new(),
                ScopedSubOutboxOps::default(),
                ScopedSubEffects::default(),
            );
        };

        let mut changed = Vec::new();
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let mut effects = ScopedSubEffects::default();
        for scoped in scoped_keys {
            let release = self.store.release_owner(owner, &scoped);
            let (_, ops, release_effects) = self.apply_store_release(
                ids,
                selected_account_pubkey,
                account_read_relays,
                &scoped,
                release,
            );
            outbox_ops.extend(ops);
            effects.extend(release_effects);
            changed.push(scoped);
        }

        (changed, outbox_ops, effects)
    }

    /// Apply selected-account scoped-sub lifecycle changes and emit readiness
    /// facts for account/global scoped keys affected by the switch.
    pub(crate) fn apply_account_switched(
        &mut self,
        old_pubkey: Pubkey,
        new_pubkey: Pubkey,
        new_account_read_relays: &HashSet<NormRelayUrl>,
    ) -> ScopedSubDelta {
        let ids = self.ids();
        self.apply_account_switched_with_ids(&ids, old_pubkey, new_pubkey, new_account_read_relays)
    }

    fn apply_account_switched_with_ids(
        &mut self,
        ids: &OutboxIdRegistry,
        old_pubkey: Pubkey,
        new_pubkey: Pubkey,
        new_account_read_relays: &HashSet<NormRelayUrl>,
    ) -> ScopedSubDelta {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        if old_pubkey == new_pubkey {
            return self.apply_account_read_relays_changed_with_ids(
                ids,
                new_pubkey,
                new_account_read_relays,
            );
        }

        self.apply_selected_account(new_pubkey);
        let mut changed_scoped = self.readiness_scoped_keys_for_account(old_pubkey);
        let (ops, effects) = self.on_account_switched_with_relays_with_effects(
            ids,
            old_pubkey,
            new_pubkey,
            new_account_read_relays,
        );
        outbox_ops.extend(ops);
        changed_scoped.extend(self.readiness_scoped_keys_for_selected_or_global(new_pubkey));
        let output = self.readiness_facts(changed_scoped);
        ScopedSubDelta::new_with_effects(output, outbox_ops, effects)
    }

    /// Apply selected-account read-relay retargeting and emit readiness facts
    /// for account/global scoped keys affected by the relay set change.
    pub(crate) fn apply_account_read_relays_changed(
        &mut self,
        account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
    ) -> ScopedSubDelta {
        let ids = self.ids();
        self.apply_account_read_relays_changed_with_ids(&ids, account_pubkey, account_read_relays)
    }

    fn apply_account_read_relays_changed_with_ids(
        &mut self,
        ids: &OutboxIdRegistry,
        account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
    ) -> ScopedSubDelta {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let mut effects = ScopedSubEffects::default();
        self.apply_selected_account(account_pubkey);
        let mut changed_scoped = self.readiness_scoped_keys_for_selected_or_global(account_pubkey);
        let (ops, retarget_effects) = self.retarget_selected_account_read_relays_with_effects(
            ids,
            account_pubkey,
            account_read_relays,
        );
        outbox_ops.extend(ops);
        effects.extend(retarget_effects);
        changed_scoped.extend(self.readiness_scoped_keys_for_selected_or_global(account_pubkey));
        let output = self.readiness_facts(changed_scoped);
        ScopedSubDelta::new_with_effects(output, outbox_ops, effects)
    }

    /// Handle centralized account switching with pre-resolved new account relays.
    ///
    /// Account-scoped live state for `old_pk` is removed first. New-account
    /// scoped subscriptions that do not depend on `AccountsRead` are restored
    /// directly; `AccountsRead`-dependent account/global subscriptions are then
    /// retargeted through the shared selected-account relay transition.
    pub(super) fn on_account_switched_with_relays_with_effects(
        &mut self,
        ids: &OutboxIdRegistry,
        old_pk: Pubkey,
        new_pk: Pubkey,
        new_account_read_relays: &HashSet<NormRelayUrl>,
    ) -> (ScopedSubOutboxOps, ScopedSubEffects) {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let mut effects = ScopedSubEffects::default();
        if old_pk == new_pk {
            return (outbox_ops, effects);
        }

        self.apply_selected_account(new_pk);
        let context = ScopedSubRuntimeContext::new(ids, new_pk, new_account_read_relays);
        let new_scope = ResolvedSubScope::Account(new_pk);

        outbox_ops.extend(self.deactivate_account_scoped_subs(ids, old_pk));
        outbox_ops.extend(self.author_outbox_plans.deactivate_account(old_pk));

        let new_desired_keys = self.store.owned_desired_keys_for_scope(&new_scope);

        for scoped in new_desired_keys {
            if self.realized.contains_live(&scoped) {
                continue;
            }

            let Some(spec) = self.store.desired(&scoped).cloned() else {
                continue;
            };

            if spec.depends_on_accounts_read() {
                continue;
            }

            let scoped_for_log = scoped.clone();
            let (result, ops, restore_effects) =
                self.realize_retained_active_config(context, scoped, None, &spec);
            outbox_ops.extend(ops);
            effects.extend(restore_effects);
            Self::trace_deferred_route_work(&scoped_for_log, result);
        }

        let (ops, retarget_effects) = self.retarget_selected_account_read_relays_with_effects(
            ids,
            new_pk,
            new_account_read_relays,
        );
        outbox_ops.extend(ops);
        effects.extend(retarget_effects);
        (outbox_ops, effects)
    }

    /// Permanently remove desired and live scoped-sub state for a deleted account.
    ///
    /// Account switching only pauses account-scoped live state. Deletion is a
    /// terminal transition: retained desired state for `account_pk` is removed
    /// so re-adding the same pubkey cannot restore subscriptions from the old
    /// account lifecycle.
    pub(super) fn purge_account_scope(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        account_pk: Pubkey,
    ) -> ScopedSubOutboxOps {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let context =
            ScopedSubRuntimeContext::new(ids, selected_account_pubkey, account_read_relays);
        let scope = ResolvedSubScope::Account(account_pk);
        outbox_ops.extend(self.author_outbox_plans.purge_account(account_pk));
        let purge = self.store.purge_scope(&scope);

        for removal in purge.removed {
            let previous = removal.removed_config;
            let (_, ops, _) = self.apply_desired_config_transition(
                context,
                removal.scoped,
                previous.as_ref(),
                None,
            );
            outbox_ops.extend(ops);
        }
        outbox_ops
    }

    fn deactivate_account_scoped_subs(
        &mut self,
        ids: &OutboxIdRegistry,
        old_pubkey: Pubkey,
    ) -> ScopedSubOutboxOps {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let old_scope = ResolvedSubScope::Account(old_pubkey);
        let scoped_keys = self.store.owned_desired_keys_for_scope(&old_scope);

        for scoped in scoped_keys {
            outbox_ops.extend(self.realized.remove_scoped(ids, &scoped));
        }
        outbox_ops
    }

    /// Retarget selected-account-dependent live subscriptions with pre-resolved read relays.
    pub(super) fn retarget_selected_account_read_relays_with_effects(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
    ) -> (ScopedSubOutboxOps, ScopedSubEffects) {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let mut effects = ScopedSubEffects::default();
        let context =
            ScopedSubRuntimeContext::new(ids, selected_account_pubkey, account_read_relays);
        self.apply_selected_account(selected_account_pubkey);
        let scoped_keys = self
            .store
            .owned_desired_keys_for_selected_or_global(selected_account_pubkey);

        for scoped in scoped_keys {
            let Some(spec) = self.store.desired(&scoped).cloned() else {
                continue;
            };

            if !spec.depends_on_accounts_read() {
                continue;
            }

            if spec.uses_author_outbox() {
                continue;
            }

            let scoped_for_log = scoped.clone();
            let (result, ops, realize_effects) =
                self.realize_retained_active_config(context, scoped, Some(&spec), &spec);
            outbox_ops.extend(ops);
            effects.extend(realize_effects);
            Self::trace_deferred_route_work(&scoped_for_log, result);
        }

        let (ops, author_effects) = self
            .apply_author_outbox_plans_for_active_scoped_keys_with_effects(
                ids,
                selected_account_pubkey,
                account_read_relays,
            );
        outbox_ops.extend(ops);
        effects.extend(author_effects);
        (outbox_ops, effects)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn set_inactive_account_sub_with_effects(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        key: SubKey,
        config: SubConfig,
    ) -> (SetSubResult, ScopedSubOutboxOps, ScopedSubEffects) {
        debug_assert_ne!(account_pubkey, selected_account_pubkey);

        let scoped = Self::scoped_key(ResolvedSubScope::Account(account_pubkey), key);
        self.store.register_ownership(owner, &scoped);
        let empty_account_read_relays = HashSet::new();
        let context =
            ScopedSubRuntimeContext::new(ids, selected_account_pubkey, &empty_account_read_relays);

        let change = self
            .store
            .set_owner_config(scoped.clone(), owner, config.clone());
        let Some(config) = change.next else {
            let (_, outbox_ops, effects) = self.apply_desired_config_transition(
                context,
                scoped,
                change.previous.as_ref(),
                None,
            );
            return (SetSubResult::Unchanged, outbox_ops, effects);
        };
        let previous = change.previous;
        if previous.as_ref() == Some(&config) {
            return (
                SetSubResult::Unchanged,
                ScopedSubOutboxOps::default(),
                ScopedSubEffects::default(),
            );
        }

        let result = if previous.is_some() {
            SetSubResult::Updated
        } else {
            SetSubResult::Created
        };
        let (_, outbox_ops, effects) =
            self.apply_desired_config_transition(context, scoped, previous.as_ref(), Some(&config));
        (result, outbox_ops, effects)
    }

    /// Apply retained author-outbox plans for every active scoped key.
    ///
    /// Concrete transition:
    /// - `scoped_keys: Vec<ScopedSubKey>` names retained requests owned by the
    ///   selected account/global scope;
    /// - each `ScopedSubKey` advances its frozen author-outbox plan inputs through
    ///   NDB lookup, relay-list discovery, and route application;
    /// - for `AccountsReadWithAuthorOutbox`, empty planned output removes only
    ///   the author-outbox live legs and keeps the baseline live sub;
    /// - non-empty planned output is diffed into the author-outbox live state
    ///   so relay changes do not churn unrelated legs;
    /// - full-history packages are applied from the same planned additive
    ///   coverage so live and history retain the same author relay projection.
    pub(super) fn apply_author_outbox_plans_for_active_scoped_keys_with_effects(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        baseline_relays: &HashSet<NormRelayUrl>,
    ) -> (ScopedSubOutboxOps, ScopedSubEffects) {
        let scoped_keys = self.active_author_outbox_scoped_keys(selected_account_pubkey);

        if scoped_keys.is_empty() {
            return (ScopedSubOutboxOps::default(), ScopedSubEffects::default());
        }

        self.apply_author_outbox_plans_for_scoped_keys_with_effects(
            ids,
            selected_account_pubkey,
            scoped_keys,
            baseline_relays,
        )
    }

    fn active_author_outbox_scoped_keys(
        &self,
        selected_account_pubkey: Pubkey,
    ) -> Vec<ScopedSubKey> {
        self.store
            .owned_desired_keys_for_selected_or_global(selected_account_pubkey)
            .into_iter()
            .filter(|scoped| {
                self.store
                    .desired(scoped)
                    .is_some_and(SubConfig::uses_author_outbox)
            })
            .collect()
    }

    fn apply_author_outbox_plans_for_scoped_keys_with_effects(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        scoped_keys: Vec<ScopedSubKey>,
        baseline_relays: &HashSet<NormRelayUrl>,
    ) -> (ScopedSubOutboxOps, ScopedSubEffects) {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let mut effects = ScopedSubEffects::default();
        if scoped_keys.is_empty() {
            return (outbox_ops, effects);
        }

        for scoped in scoped_keys {
            if !self.store.contains_desired(&scoped) {
                continue;
            }

            let (result, ops, scoped_effects) = self.apply_author_outbox_plan_for_scoped_key(
                ids,
                selected_account_pubkey,
                baseline_relays,
                scoped.clone(),
            );
            outbox_ops.extend(ops);
            effects.extend(scoped_effects);
            match result {
                RouteWorkResult::Complete | RouteWorkResult::PlanPending => {}
                RouteWorkResult::FullRefreshRequired | RouteWorkResult::RebuildRequired => {
                    tracing::debug!(
                        target: "outbox_perf",
                        ?scoped,
                        "author_outbox_route_work_deferred_without_transition"
                    );
                }
            }
        }
        (outbox_ops, effects)
    }

    fn apply_author_outbox_plan_for_scoped_key(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        baseline_relays: &HashSet<NormRelayUrl>,
        scoped: ScopedSubKey,
    ) -> (RouteWorkResult, ScopedSubOutboxOps, ScopedSubEffects) {
        let Some(spec) = self.store.desired(&scoped).cloned() else {
            return (
                RouteWorkResult::Complete,
                ScopedSubOutboxOps::default(),
                ScopedSubEffects::default(),
            );
        };

        debug_assert!(
            spec.uses_author_outbox(),
            "author-outbox plan application received non-author-outbox scoped key: {scoped:?}",
        );
        if !spec.uses_author_outbox() {
            return (
                RouteWorkResult::Complete,
                ScopedSubOutboxOps::default(),
                ScopedSubEffects::default(),
            );
        }

        let context = ScopedSubRuntimeContext::new(ids, selected_account_pubkey, baseline_relays);
        self.realize_retained_active_config(context, scoped, Some(&spec), &spec)
    }

    fn apply_store_release(
        &mut self,
        ids: &OutboxIdRegistry,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        scoped: &ScopedSubKey,
        release: ScopedSubStoreRelease,
    ) -> (ClearSubResult, ScopedSubOutboxOps, ScopedSubEffects) {
        let context =
            ScopedSubRuntimeContext::new(ids, selected_account_pubkey, account_read_relays);
        match release {
            ScopedSubStoreRelease::NotFound => (
                ClearSubResult::NotFound,
                ScopedSubOutboxOps::default(),
                ScopedSubEffects::default(),
            ),
            ScopedSubStoreRelease::StillInUse {
                previous_config,
                next_config,
            } => {
                if previous_config != next_config {
                    let (_, outbox_ops, effects) = self.apply_desired_config_transition(
                        context,
                        scoped.clone(),
                        previous_config.as_ref(),
                        next_config.as_ref(),
                    );
                    return (ClearSubResult::StillInUse, outbox_ops, effects);
                }
                (
                    ClearSubResult::StillInUse,
                    ScopedSubOutboxOps::default(),
                    ScopedSubEffects::default(),
                )
            }
            ScopedSubStoreRelease::Cleared { removed_config } => {
                let (_, outbox_ops, effects) = self.apply_desired_config_transition(
                    context,
                    scoped.clone(),
                    removed_config.as_ref(),
                    None,
                );
                (ClearSubResult::Cleared, outbox_ops, effects)
            }
        }
    }
}

#[cfg(test)]
impl ScopedSubRuntime {
    pub(super) fn live_sub_ids_for_test(
        &self,
        selected_account_pubkey: Pubkey,
        key: SubKey,
        scope: SubScope,
    ) -> Vec<OutboxSubId> {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);
        self.realized.live_sub_ids_for_test(&scoped)
    }

    pub(super) fn routed_live_legs_for_test(
        &self,
        selected_account_pubkey: Pubkey,
        key: SubKey,
        scope: SubScope,
    ) -> Vec<(NormRelayUrl, OutboxSubId)> {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);
        self.realized.routed_live_legs_for_test(&scoped)
    }

    pub(super) fn routed_live_author_sets_for_test(
        &self,
        selected_account_pubkey: Pubkey,
        key: SubKey,
        scope: SubScope,
    ) -> Vec<(NormRelayUrl, OutboxSubId, Vec<Vec<Pubkey>>)> {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);
        self.realized.routed_live_author_sets_for_test(&scoped)
    }

    pub(super) fn single_live_id_for_scoped_for_test(&self, scoped: &ScopedSubKey) -> OutboxSubId {
        self.realized.single_live_id_for_scoped_for_test(scoped)
    }

    pub(super) fn has_live_for_scoped_for_test(&self, scoped: &ScopedSubKey) -> bool {
        self.realized.contains_live(scoped)
    }

    pub(super) fn full_history_id_for_test(
        &self,
        scoped: &ScopedSubKey,
    ) -> Option<enostr::FullHistorySubId> {
        self.realized.full_history_id_for_test(scoped)
    }

    pub(super) fn full_history_targets_for_test(
        &self,
        scoped: &ScopedSubKey,
    ) -> Option<&[enostr::FullHistoryTarget]> {
        self.realized.full_history_targets_for_test(scoped)
    }

    pub(super) fn live_id_relay_url_source_for_test(
        &self,
        scoped: &ScopedSubKey,
        live_id: OutboxSubId,
    ) -> Option<enostr::RelayUrlSource> {
        self.realized
            .live_id_relay_url_source_for_test(scoped, live_id)
    }

    pub(super) fn desired_len(&self) -> usize {
        self.store.desired_len()
    }

    pub(super) fn desired_for_test(&self, scoped: &ScopedSubKey) -> Option<&SubConfig> {
        self.store.desired_for_test(scoped)
    }

    pub(super) fn live_len(&self) -> usize {
        self.realized.live_len()
    }

    pub(super) fn owner_len(&self) -> usize {
        self.store.owner_len()
    }

    pub(crate) fn live_id_with_selected(
        &self,
        selected_account_pubkey: Pubkey,
        key: SubKey,
        scope: SubScope,
    ) -> Option<OutboxSubId> {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);
        self.realized.baseline_live_id_for_test(&scoped)
    }

    pub(super) fn author_outbox_active_authors_for_test(&self) -> HashSet<Pubkey> {
        self.author_outbox.active_authors_for_test()
    }
}

fn resolve_scope(scope: &SubScope, selected_account_pubkey: Pubkey) -> ResolvedSubScope {
    resolve_scope_for_account(scope, selected_account_pubkey)
}

fn resolve_scope_for_account(scope: &SubScope, account_pubkey: Pubkey) -> ResolvedSubScope {
    match scope {
        SubScope::Account => ResolvedSubScope::Account(account_pubkey),
        SubScope::Global => ResolvedSubScope::Global,
    }
}
