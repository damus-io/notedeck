use enostr::{FullHistoryTarget, NormRelayUrl, Pubkey, RelayUrlPkgs, RelayUrlSource};
use hashbrown::{HashMap, HashSet};
use nostrdb::Filter;

use crate::author_outbox::{
    filter_author_pubkeys, rank_author_outbox_routes, RoutedFilter, RoutedRelayPriority,
};

use super::config::{SubConfig, SubExecution, SubRelayPolicy};

/// Complete desired remote shape for one active scoped subscription.
///
/// This is planning data only: it contains no retained outbox ids and performs
/// no mutation. Runtime code applies this shape to the currently realized state.
pub(super) struct ScopedSubPlan<'a> {
    pub(super) live: LivePlan<'a>,
    pub(super) full_history_targets: Vec<FullHistoryTarget>,
}

/// Frozen author-outbox route filters for one scoped-sub generation.
#[derive(Clone, Debug, Default)]
pub(super) struct PlannedAuthorOutboxRoutes {
    pub(super) live_routed_relays: Vec<PlannedRoutedRelay>,
    pub(super) full_history_routed_relays: Vec<PlannedRoutedRelay>,
}

pub(super) type AuthorOutboxPlanGeneration = u64;

#[derive(Clone, Debug)]
pub(super) struct PlannedRoutedRelay {
    pub(super) relay: NormRelayUrl,
    pub(super) relay_priority: RoutedRelayPriority,
    pub(super) filters: Vec<Filter>,
    pub(super) authors_by_filter_index: HashMap<usize, HashSet<Pubkey>>,
}

impl PlannedAuthorOutboxRoutes {
    pub(super) fn from_routed_filters(
        mut live_routed_filters: Vec<RoutedFilter>,
        mut full_history_routed_filters: Vec<RoutedFilter>,
    ) -> Self {
        let mut route_sets = [&mut live_routed_filters, &mut full_history_routed_filters];
        rank_author_outbox_routes(&mut route_sets);

        Self {
            live_routed_relays: planned_routed_relays(live_routed_filters),
            full_history_routed_relays: planned_routed_relays(full_history_routed_filters),
        }
    }
}

/// Desired live subscription shape for one active scoped subscription.
pub(super) enum LivePlan<'a> {
    Single,
    AccountsReadPlusExplicit,
    AccountsReadWithAuthorOutbox {
        plan_generation: Option<AuthorOutboxPlanGeneration>,
        routed_relays: &'a [PlannedRoutedRelay],
    },
}

/// Plan one active scoped subscription against host relay state.
pub(super) fn plan_scoped_sub<'a>(
    spec: &SubConfig,
    account_read_relays: &HashSet<NormRelayUrl>,
    author_outbox_routes: Option<(&'a PlannedAuthorOutboxRoutes, AuthorOutboxPlanGeneration)>,
) -> ScopedSubPlan<'a> {
    let routes = author_outbox_routes;
    let full_history_targets =
        full_history_targets_with_author_routes(account_read_relays, spec, routes.map(|r| r.0));

    let live = match &spec.execution {
        SubExecution::AccountsRead { .. } | SubExecution::Explicit { .. } => LivePlan::Single,
        SubExecution::AccountsReadPlusExplicit { .. } => LivePlan::AccountsReadPlusExplicit,
        SubExecution::AccountsReadWithAuthorOutbox { .. } => {
            LivePlan::AccountsReadWithAuthorOutbox {
                plan_generation: routes.map(|(_, generation)| generation),
                routed_relays: routes
                    .map(|(routes, _)| routes.live_routed_relays.as_slice())
                    .unwrap_or(&[]),
            }
        }
    };

    ScopedSubPlan {
        live,
        full_history_targets,
    }
}

pub(super) fn full_history_relay_pkgs(
    account_read_relays: &HashSet<NormRelayUrl>,
    spec: &SubConfig,
) -> Vec<RelayUrlPkgs> {
    let mut relay_pkgs = Vec::new();
    match &spec.execution {
        SubExecution::AccountsRead { baseline }
        | SubExecution::AccountsReadWithAuthorOutbox { baseline, .. } => push_relay_pkgs(
            &mut relay_pkgs,
            account_read_relays.clone(),
            RelayUrlSource::Explicit,
            *baseline,
        ),
        SubExecution::Explicit { relays, policy } => push_relay_pkgs(
            &mut relay_pkgs,
            relays.clone(),
            RelayUrlSource::Explicit,
            *policy,
        ),
        SubExecution::AccountsReadPlusExplicit {
            baseline,
            explicit,
            explicit_relays,
            explicit_source,
        } => {
            push_relay_pkgs(
                &mut relay_pkgs,
                account_read_relays.clone(),
                RelayUrlSource::Explicit,
                *baseline,
            );
            let additive_relays = explicit_relays
                .difference(account_read_relays)
                .cloned()
                .collect::<HashSet<_>>();
            push_relay_pkgs(
                &mut relay_pkgs,
                additive_relays,
                *explicit_source,
                *explicit,
            );
        }
    }
    relay_pkgs
}

pub(super) fn full_history_baseline_targets(
    account_read_relays: &HashSet<NormRelayUrl>,
    spec: &SubConfig,
) -> Vec<FullHistoryTarget> {
    let Some(full_history) = spec.full_history_config() else {
        return Vec::new();
    };
    let relay_pkgs = full_history_relay_pkgs(account_read_relays, spec);
    if relay_pkgs.is_empty() {
        return Vec::new();
    }

    vec![FullHistoryTarget::new(
        full_history.owned_filters(),
        relay_pkgs,
    )]
}

pub(super) fn full_history_targets_with_author_routes(
    account_read_relays: &HashSet<NormRelayUrl>,
    spec: &SubConfig,
    author_outbox_routes: Option<&PlannedAuthorOutboxRoutes>,
) -> Vec<FullHistoryTarget> {
    let mut targets = full_history_baseline_targets(account_read_relays, spec);
    targets.extend(full_history_targets_for_author_routes(
        spec,
        author_outbox_routes
            .map(|routes| routes.full_history_routed_relays.as_slice())
            .unwrap_or(&[]),
    ));
    targets
}

fn full_history_targets_for_author_routes(
    spec: &SubConfig,
    routed_relays: &[PlannedRoutedRelay],
) -> Vec<FullHistoryTarget> {
    if spec.full_history_config().is_none() {
        return Vec::new();
    }

    let Some(author_outbox_policy) = spec.author_outbox_policy() else {
        return Vec::new();
    };

    routed_relays
        .iter()
        .filter_map(|routed| {
            let relay_pkgs = RelayUrlPkgs::new(
                HashSet::from([routed.relay.clone()]),
                enostr::RelayUrlPolicy::remote_advertised(
                    author_outbox_policy.demand_priority(),
                    author_outbox_policy.routing_preference(),
                )
                .with_connection_weight(routed.relay_priority.connection_weight),
            );
            let target = FullHistoryTarget::new(routed.filters.clone(), vec![relay_pkgs]);
            (!target.is_empty()).then_some(target)
        })
        .collect()
}

fn planned_routed_relays(routed_filters: Vec<RoutedFilter>) -> Vec<PlannedRoutedRelay> {
    let mut relay_order = Vec::<NormRelayUrl>::new();
    let mut grouped: HashMap<NormRelayUrl, PlannedRoutedRelay> = HashMap::new();
    for routed in routed_filters {
        if routed.is_empty()
            || !routed
                .relay
                .allowed_for_source(RelayUrlSource::RemoteAdvertised)
        {
            continue;
        }

        if !grouped.contains_key(&routed.relay) {
            relay_order.push(routed.relay.clone());
        }
        let authors = filter_author_pubkeys(&routed.filter);
        let relay = routed.relay.clone();
        let entry = grouped
            .entry(relay.clone())
            .or_insert_with(|| PlannedRoutedRelay {
                relay,
                relay_priority: RoutedRelayPriority::default(),
                filters: Vec::new(),
                authors_by_filter_index: HashMap::new(),
            });
        entry.relay_priority =
            merge_routed_relay_priority(entry.relay_priority, routed.relay_priority);
        entry.filters.push(routed.filter);
        entry
            .authors_by_filter_index
            .entry(routed.filter_index)
            .or_default()
            .extend(authors);
    }

    relay_order
        .into_iter()
        .filter_map(|relay| grouped.remove(&relay))
        .collect()
}

fn merge_routed_relay_priority(
    left: RoutedRelayPriority,
    right: RoutedRelayPriority,
) -> RoutedRelayPriority {
    RoutedRelayPriority {
        connection_weight: left.connection_weight.max(right.connection_weight),
        order: left.order.min(right.order),
    }
}

fn push_relay_pkgs(
    relay_pkgs: &mut Vec<RelayUrlPkgs>,
    relays: HashSet<NormRelayUrl>,
    source: RelayUrlSource,
    policy: SubRelayPolicy,
) {
    let relays = relays
        .into_iter()
        .filter(|relay| relay.allowed_for_source(source))
        .collect::<HashSet<_>>();
    if relays.is_empty() {
        return;
    }

    relay_pkgs.push(RelayUrlPkgs::new(
        relays,
        enostr::RelayUrlPolicy::new(
            source,
            policy.demand_priority(),
            policy.routing_preference(),
        ),
    ));
}
