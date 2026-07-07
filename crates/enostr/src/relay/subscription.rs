use hashbrown::{HashMap, HashSet};
use nostrdb::Filter;

use crate::relay::{
    same_canonical_filter_set, FullHistorySubId, MetadataFilters, NormRelayUrl, OutboxSubId,
    RelayDemandPriority, RelayRoutingPreference, RelayUrlPkgs, RelayUrlPolicy, RelayUrlSource,
};

/// Filter set used for background full-history reconciliation.
#[derive(Clone, Debug)]
pub struct FullHistoryConfig {
    pub(crate) filters: Vec<Filter>,
}

impl FullHistoryConfig {
    /// Create an explicit full-history declaration with its own non-empty
    /// filter set.
    pub fn new(filters: Vec<Filter>) -> Self {
        Self {
            filters: filters
                .into_iter()
                .filter(|filter| filter.num_elements() != 0)
                .collect(),
        }
    }

    /// Returns the full-history filter set.
    pub fn filters(&self) -> &[Filter] {
        &self.filters
    }

    /// Consume this config and return its full-history filter set.
    pub fn into_filters(self) -> Vec<Filter> {
        self.filters
    }

    /// Returns whether this config contains no meaningful history filters.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

/// Relay packages paired with the full-history filters that apply to them.
#[derive(Clone, Debug)]
pub struct FullHistoryTarget {
    pub(crate) filters: Vec<Filter>,
    pub(crate) relay_pkgs: Vec<RelayUrlPkgs>,
}

impl FullHistoryTarget {
    /// Create one relay-scoped full-history target.
    pub fn new(filters: Vec<Filter>, relay_pkgs: Vec<RelayUrlPkgs>) -> Self {
        Self {
            filters: filters
                .into_iter()
                .filter(|filter| filter.num_elements() != 0)
                .collect(),
            relay_pkgs,
        }
    }

    /// Returns the filters that apply to this target's relay packages.
    pub fn filters(&self) -> &[Filter] {
        &self.filters
    }

    /// Returns the relay packages that share this target's filters.
    pub fn relay_pkgs(&self) -> &[RelayUrlPkgs] {
        &self.relay_pkgs
    }

    /// Returns whether this target has either no filters or no relay packages.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty() || self.relay_pkgs.is_empty()
    }
}

/// Return whether targets normalize to at least one relay/filter entry.
pub fn full_history_targets_have_work(targets: &[FullHistoryTarget]) -> bool {
    targets.iter().any(|target| {
        target
            .filters
            .iter()
            .any(|filter| filter.num_elements() != 0)
            && target.relay_pkgs.iter().any(|relay_pkgs| {
                let source = relay_pkgs.source();
                relay_pkgs
                    .urls
                    .iter()
                    .any(|relay| relay.allowed_for_source(source))
            })
    })
}

/// One relay/filter/policy target for a full-history subscription.
#[derive(Clone, Debug)]
pub(in crate::relay) struct FullHistoryRelayFilter {
    pub(in crate::relay) relay: NormRelayUrl,
    pub(in crate::relay) relay_policy: RelayUrlPolicy,
    pub(in crate::relay) filter: Filter,
}

impl FullHistoryRelayFilter {
    /// Construct one relay-local full-history target.
    pub(in crate::relay) fn new(
        relay: NormRelayUrl,
        relay_policy: RelayUrlPolicy,
        filter: Filter,
    ) -> Self {
        Self {
            relay,
            relay_policy,
            filter,
        }
    }

    /// Whether two entries address the same relay and canonical filter.
    pub(in crate::relay) fn has_same_relay_filter(&self, other: &Self) -> bool {
        self.relay == other.relay && self.filter.same_canonical_attributes(&other.filter)
    }

    /// Merge another entry's relay package policy into this relay/filter pair.
    pub(in crate::relay) fn merge_policy_from(&mut self, other: &Self) {
        self.relay_policy.merge_from(other.relay_policy);
    }

    /// Return the single-relay package represented by this target.
    pub(in crate::relay) fn relay_pkgs(&self) -> RelayUrlPkgs {
        RelayUrlPkgs::single(self.relay.clone(), self.relay_policy)
    }

    /// Return the relay connection demand priority for this target.
    pub(in crate::relay) fn demand_priority(&self) -> RelayDemandPriority {
        self.relay_policy.demand_priority()
    }

    /// Whether two relay/filter pairs target the same relay and canonical filter.
    ///
    /// Relay package policy is transport metadata and may change without
    /// creating new full-history work.
    pub(in crate::relay) fn semantically_matches(&self, other: &Self) -> bool {
        self.has_same_relay_filter(other)
    }
}

/// Return the distinct filter set represented by expanded full-history targets.
pub(in crate::relay) fn full_history_filters_from_relay_targets(
    targets: &[FullHistoryRelayFilter],
) -> Vec<Filter> {
    let mut filters = Vec::<Filter>::new();
    for target in targets {
        if filters
            .iter()
            .any(|filter| filter.same_canonical_attributes(&target.filter))
        {
            continue;
        }
        filters.push(target.filter.clone());
    }
    filters
}

/// Return relay packages represented by expanded full-history targets.
pub(in crate::relay) fn full_history_relay_pkgs_from_relay_targets(
    targets: &[FullHistoryRelayFilter],
) -> Vec<RelayUrlPkgs> {
    let mut relay_pkgs = Vec::<RelayUrlPkgs>::new();
    for target in targets {
        if let Some(existing) = relay_pkgs
            .iter_mut()
            .find(|relay_pkgs| relay_pkgs.policy() == target.relay_policy)
        {
            existing.urls.insert(target.relay.clone());
            continue;
        }
        relay_pkgs.push(target.relay_pkgs());
    }
    relay_pkgs
}

/// Canonicalize caller full-history targets into relay/filter/policy entries.
pub(in crate::relay) fn normalize_full_history_targets(
    targets: Vec<FullHistoryTarget>,
) -> Vec<FullHistoryRelayFilter> {
    let mut relay_filters = Vec::<FullHistoryRelayFilter>::new();
    for mut target in targets {
        target.filters.retain(|filter| filter.num_elements() != 0);
        target.relay_pkgs = normalize_relay_pkgs(target.relay_pkgs);
        if target.is_empty() {
            continue;
        }

        for filter in target.filters {
            for relay_pkgs in &target.relay_pkgs {
                for relay in relay_pkgs.urls.iter().cloned() {
                    let candidate =
                        FullHistoryRelayFilter::new(relay, relay_pkgs.policy(), filter.clone());
                    if let Some(existing) = relay_filters
                        .iter_mut()
                        .find(|existing| existing.has_same_relay_filter(&candidate))
                    {
                        existing.merge_policy_from(&candidate);
                        continue;
                    }
                    relay_filters.push(candidate);
                }
            }
        }
    }
    relay_filters
}

/// Remove blocked URLs and merge duplicate relay package policy.
fn normalize_relay_pkgs(relay_pkgs: Vec<RelayUrlPkgs>) -> Vec<RelayUrlPkgs> {
    let mut relays = Vec::<RelayPackageEntry>::new();

    for mut relay_pkg in relay_pkgs {
        relay_pkg.retain_allowed();
        let source = relay_pkg.source();
        let demand_priority = relay_pkg.demand_priority();
        let routing_preference = relay_pkg.routing_preference();
        let connection_weight = relay_pkg.connection_weight();
        for relay in relay_pkg.urls {
            if let Some(existing) = relays.iter_mut().find(|existing| existing.relay == relay) {
                existing.merge_policy(
                    source,
                    demand_priority,
                    routing_preference,
                    connection_weight,
                );
                continue;
            }

            relays.push(RelayPackageEntry {
                relay,
                source,
                demand_priority,
                routing_preference,
                connection_weight,
            });
        }
    }

    let mut normalized = Vec::<RelayUrlPkgs>::new();
    for relay in relays {
        if let Some(existing) = normalized.iter_mut().find(|existing| {
            existing.source() == relay.source
                && existing.demand_priority() == relay.demand_priority
                && existing.routing_preference() == relay.routing_preference
                && existing.connection_weight() == relay.connection_weight
        }) {
            existing.urls.insert(relay.relay);
            continue;
        }

        normalized.push(RelayUrlPkgs::single(
            relay.relay,
            RelayUrlPolicy::new(
                relay.source,
                relay.demand_priority,
                relay.routing_preference,
            )
            .with_connection_weight(relay.connection_weight),
        ));
    }

    normalized
}

/// One relay URL plus merged policy while normalizing relay packages.
struct RelayPackageEntry {
    relay: NormRelayUrl,
    source: RelayUrlSource,
    demand_priority: RelayDemandPriority,
    routing_preference: RelayRoutingPreference,
    connection_weight: u32,
}

impl RelayPackageEntry {
    fn merge_policy(
        &mut self,
        source: RelayUrlSource,
        demand_priority: RelayDemandPriority,
        routing_preference: RelayRoutingPreference,
        connection_weight: u32,
    ) {
        self.source = self.source.strongest(source);
        self.demand_priority = self.demand_priority.max(demand_priority);
        self.routing_preference = self.routing_preference.strongest(routing_preference);
        self.connection_weight = self.connection_weight.max(connection_weight);
    }
}

pub struct OutboxSubscription {
    pub relays: HashSet<NormRelayUrl>,
    pub filters: MetadataFilters,
    wire_filter_revision: u64,
    json_size: usize,
    pub is_oneshot: bool,
    pub demand_priority: RelayDemandPriority,
    pub connection_weight: u32,
    /// Trust source for the relay URL set attached to this subscription.
    pub relay_url_source: RelayUrlSource,
    full_history_fetch: Option<FullHistoryFetchOrigin>,
    pub routing_preference: RelayRoutingPreference,
}

/// Source full-history relay/filter pair that produced an internal fetch
/// subscription.
struct FullHistoryFetchOrigin {
    owner: FullHistorySubId,
    filter: Filter,
}

impl OutboxSubscription {
    /// Returns the filter set that compaction should send for this
    /// subscription, applying any synthetic `since` cursor from metadata.
    pub fn filters_for_compaction(&self) -> Vec<Filter> {
        self.filters.projected_filters()
    }

    fn see_all(&mut self, at: u64) {
        for (_, meta) in self.filters.iter_mut() {
            meta.last_seen = Some(at);
        }
    }

    fn ingest_task(&mut self, task: ModifyTask) -> bool {
        match task {
            ModifyTask::FullRelayPkgs(full_modification_task) => {
                let transport_demand_changed = self.relays != full_modification_task.relays.urls
                    || !self.relay_policy_matches(&full_modification_task.relays);
                if !same_canonical_filter_set(
                    self.filters.get_filters(),
                    full_modification_task.filters.as_slice(),
                ) {
                    self.bump_wire_filter_revision();
                }
                self.filters = MetadataFilters::new(full_modification_task.filters);
                self.json_size = self.filters.json_size_sum();
                self.refresh_relay_policy(&full_modification_task.relays);
                self.relays = full_modification_task.relays.urls;
                transport_demand_changed
            }
        }
    }

    pub(in crate::relay) fn relay_policy_matches(&self, relay_pkgs: &RelayUrlPkgs) -> bool {
        self.demand_priority == relay_pkgs.demand_priority()
            && self.connection_weight == relay_pkgs.connection_weight()
            && self.relay_url_source == relay_pkgs.source()
            && self.routing_preference == relay_pkgs.routing_preference()
    }
}

#[derive(Default)]
pub struct OutboxSubscriptions {
    subs: HashMap<OutboxSubId, OutboxSubscription>,
    /// Monotonic counter for mutable subscription access and owned mutations.
    version: u64,
}

impl OutboxSubscriptions {
    fn bump_version(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    #[cfg(test)]
    pub(in crate::relay) fn version(&self) -> u64 {
        self.version
    }

    pub fn stored_ref(&self, id: &OutboxSubId) -> Option<StoredSubscriptionRef<'_>> {
        let sub = self.subs.get(id)?;

        Some(StoredSubscriptionRef {
            id: *id,
            filters: &sub.filters,
            wire_filter_revision: sub.wire_filter_revision,
            demand_priority: sub.demand_priority,
            connection_weight: sub.connection_weight,
            relay_url_source: sub.relay_url_source,
            routing_preference: sub.routing_preference,
        })
    }

    #[cfg(test)]
    pub fn json_size(&self, id: &OutboxSubId) -> Option<usize> {
        self.subs.get(id).map(|s| s.json_size)
    }

    pub fn is_oneshot(&self, id: &OutboxSubId) -> bool {
        self.subs.get(id).is_some_and(|s| s.is_oneshot)
    }

    /// Remove one relay leg from a retained transient fetch.
    ///
    /// Returns whether the fetch subscription was removed because that was its
    /// last retained relay leg.
    pub(in crate::relay) fn remove_oneshot_relay(
        &mut self,
        id: OutboxSubId,
        relay: &NormRelayUrl,
    ) -> Option<bool> {
        let removed_sub = {
            let sub = self.subs.get_mut(&id)?;
            if !sub.is_oneshot || !sub.relays.remove(relay) {
                return None;
            }
            sub.relays.is_empty()
        };

        if removed_sub {
            self.subs.remove(&id);
        }
        self.bump_version();
        Some(removed_sub)
    }

    /// Return retained internal full-history fetch ids owned by one history subscription.
    pub(in crate::relay) fn full_history_fetch_ids(
        &self,
        owner: FullHistorySubId,
    ) -> Vec<OutboxSubId> {
        self.subs
            .iter()
            .filter_map(|(id, sub)| {
                (sub.full_history_fetch.as_ref()?.owner == owner).then_some(*id)
            })
            .collect()
    }

    /// Remove relay legs from internal full-history fetches when their source
    /// relay/filter pair no longer belongs to the owning full-history snapshot.
    pub(in crate::relay) fn remove_full_history_fetch_relays_matching<F>(
        &mut self,
        owner: FullHistorySubId,
        mut matches: F,
    ) -> Vec<FullHistoryFetchCancellation>
    where
        F: FnMut(&NormRelayUrl, &Filter) -> bool,
    {
        let mut cancellations = Vec::new();
        let mut empty_subs = Vec::new();
        for (id, sub) in &mut self.subs {
            let Some(origin) = sub.full_history_fetch.as_ref() else {
                continue;
            };
            if origin.owner != owner {
                continue;
            }

            let mut relays = Vec::new();
            sub.relays.retain(|relay| {
                if matches(relay, &origin.filter) {
                    relays.push(relay.clone());
                    false
                } else {
                    true
                }
            });
            if relays.is_empty() {
                continue;
            }

            let removed_sub = sub.relays.is_empty();
            if removed_sub {
                empty_subs.push(*id);
            }
            cancellations.push(FullHistoryFetchCancellation {
                id: *id,
                relays,
                removed_sub,
            });
        }

        for id in empty_subs {
            self.subs.remove(&id);
        }
        if !cancellations.is_empty() {
            self.bump_version();
        }

        cancellations
    }

    /// Refresh retained internal full-history fetch subscriptions from the
    /// current relay/filter policy owned by the full-history snapshot.
    pub(in crate::relay) fn refresh_full_history_fetch_policies<F>(
        &mut self,
        owner: FullHistorySubId,
        mut relay_pkgs_for: F,
    ) -> Vec<FullHistoryFetchPolicyRefresh>
    where
        F: FnMut(&NormRelayUrl, &Filter) -> Option<RelayUrlPkgs>,
    {
        let mut changed = false;
        let mut refreshes = Vec::new();
        for (id, sub) in &mut self.subs {
            let Some(origin) = sub.full_history_fetch.as_ref() else {
                continue;
            };
            if origin.owner != owner {
                continue;
            }

            let mut merged_relay_pkgs = None::<RelayUrlPkgs>;
            for relay in &sub.relays {
                let Some(relay_pkgs) = relay_pkgs_for(relay, &origin.filter) else {
                    continue;
                };
                let relay_pkgs = relay_pkgs.single_relay_with_same_policy(relay.clone());
                if let Some(merged) = &mut merged_relay_pkgs {
                    merged.merge_policy_from(&relay_pkgs);
                    continue;
                }
                merged_relay_pkgs = Some(relay_pkgs);
            }

            if let Some(relay_pkgs) = merged_relay_pkgs {
                if sub.refresh_relay_policy(&relay_pkgs) {
                    changed = true;
                    refreshes.push(FullHistoryFetchPolicyRefresh {
                        id: *id,
                        relays: sub.relays.iter().cloned().collect(),
                    });
                }
            }
        }
        if changed {
            self.bump_version();
        }
        refreshes
    }

    /// Returns the dedicated/compaction routing preference for the subscription, if present.
    pub fn routing_preference(&self, id: &OutboxSubId) -> Option<RelayRoutingPreference> {
        self.subs.get(id).map(|s| s.routing_preference)
    }

    /// Returns the effective relay connection priority derived from all demand
    /// currently targeting `relay`.
    #[cfg(test)]
    pub(crate) fn relay_connection_priority_for_relay(
        &self,
        relay: &NormRelayUrl,
    ) -> Option<crate::relay::RelayConnectionPriority> {
        self.subs
            .values()
            .filter(|sub| sub.relays.contains(relay))
            .fold(None, |priority, sub| {
                let next =
                    crate::relay::RelayConnectionPriority::from_demand(sub.demand_priority, 1);
                match (priority, next) {
                    (Some(priority), Some(next)) => Some(priority.merge(next)),
                    (Some(priority), None) | (None, Some(priority)) => Some(priority),
                    (None, None) => None,
                }
            })
    }

    #[cfg(test)]
    pub fn json_size_sum(&self, ids: &HashSet<OutboxSubId>) -> usize {
        ids.iter()
            .map(|id| self.subs.get(id).map_or(0, |s| s.json_size))
            .sum()
    }

    /// Returns the compaction-projected filters for one subscription.
    pub fn filters_for_compaction(&self, id: &OutboxSubId) -> Option<Vec<Filter>> {
        self.subs
            .get(id)
            .map(OutboxSubscription::filters_for_compaction)
    }

    /// Apply a received event high-water mark without changing relay demand.
    pub(in crate::relay) fn see_all(&mut self, id: &OutboxSubId, at: u64) -> bool {
        let Some(sub) = self.subs.get_mut(id) else {
            return false;
        };
        sub.see_all(at);
        true
    }

    /// Apply one stored subscription mutation and bump transport-demand version
    /// only if relay demand changed.
    pub(in crate::relay) fn ingest_task(&mut self, id: &OutboxSubId, task: ModifyTask) -> bool {
        let Some(sub) = self.subs.get_mut(id) else {
            return false;
        };
        if sub.ingest_task(task) {
            self.bump_version();
        }
        true
    }

    pub fn get(&self, id: &OutboxSubId) -> Option<&OutboxSubscription> {
        self.subs.get(id)
    }

    pub fn remove(&mut self, id: &OutboxSubId) {
        if self.subs.remove(id).is_some() {
            self.bump_version();
        }
    }

    pub fn new_subscription(&mut self, id: OutboxSubId, task: SubscribeTask, is_oneshot: bool) {
        let relays = task.relays;
        self.insert_subscription(id, task.filters, relays, is_oneshot, None);
    }

    pub(in crate::relay) fn new_full_history_fetch_subscription(
        &mut self,
        id: OutboxSubId,
        task: SubscribeTask,
        owner: FullHistorySubId,
        filter: Filter,
    ) {
        let relays = task.relays;
        self.insert_subscription(
            id,
            task.filters,
            relays,
            true,
            Some(FullHistoryFetchOrigin { owner, filter }),
        );
    }

    fn insert_subscription(
        &mut self,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relays: RelayUrlPkgs,
        is_oneshot: bool,
        full_history_fetch: Option<FullHistoryFetchOrigin>,
    ) {
        let demand_priority = relays.demand_priority();
        let connection_weight = relays.connection_weight();
        let relay_url_source = relays.source();
        let routing_preference = relays.routing_preference();
        let filters = MetadataFilters::new(filters);
        let json_size = filters.json_size_sum();
        self.subs.insert(
            id,
            OutboxSubscription {
                relays: relays.urls,
                filters,
                wire_filter_revision: 0,
                json_size,
                is_oneshot,
                demand_priority,
                connection_weight,
                relay_url_source,
                full_history_fetch,
                routing_preference,
            },
        );
        self.bump_version();
    }
}

/// Relay work that should be unsubscribed after trimming internal fetch legs.
pub(in crate::relay) struct FullHistoryFetchCancellation {
    pub(in crate::relay) id: OutboxSubId,
    pub(in crate::relay) relays: Vec<NormRelayUrl>,
    pub(in crate::relay) removed_sub: bool,
}

/// Relay work that should be replaced after refreshing internal fetch policy.
pub(in crate::relay) struct FullHistoryFetchPolicyRefresh {
    pub(in crate::relay) id: OutboxSubId,
    pub(in crate::relay) relays: Vec<NormRelayUrl>,
}

impl OutboxSubscription {
    fn bump_wire_filter_revision(&mut self) {
        self.wire_filter_revision = self.wire_filter_revision.wrapping_add(1);
    }

    fn refresh_relay_policy(&mut self, relay_pkgs: &RelayUrlPkgs) -> bool {
        let previous_demand_priority = self.demand_priority;
        let previous_connection_weight = self.connection_weight;
        let previous_relay_url_source = self.relay_url_source;
        let previous_routing_preference = self.routing_preference;

        self.demand_priority = relay_pkgs.demand_priority();
        self.connection_weight = relay_pkgs.connection_weight();
        self.relay_url_source = relay_pkgs.source();
        self.routing_preference = relay_pkgs.routing_preference();

        self.demand_priority != previous_demand_priority
            || self.connection_weight != previous_connection_weight
            || self.relay_url_source != previous_relay_url_source
            || self.routing_preference != previous_routing_preference
    }
}

pub struct StoredSubscriptionRef<'a> {
    pub id: OutboxSubId,
    pub filters: &'a MetadataFilters,
    pub wire_filter_revision: u64,
    pub demand_priority: RelayDemandPriority,
    pub connection_weight: u32,
    pub relay_url_source: RelayUrlSource,
    pub routing_preference: RelayRoutingPreference,
}

pub enum ModifyTask {
    /// Replace filters, relays, and relay policy metadata together.
    FullRelayPkgs(FullRelayPkgsModificationTask),
}

/// Full live-subscription replacement including relay package policy metadata.
pub struct FullRelayPkgsModificationTask {
    pub filters: Vec<Filter>,
    pub relays: RelayUrlPkgs,
}

pub struct SubscribeTask {
    pub filters: Vec<Filter>,
    pub relays: RelayUrlPkgs,
}

pub(in crate::relay) enum FullHistoryTask {
    Upsert(FullHistoryUpsertTask),
    Remove,
}

pub(in crate::relay) struct FullHistoryUpsertTask {
    pub(in crate::relay) targets: Vec<FullHistoryRelayFilter>,
}

impl FullHistoryUpsertTask {
    pub(in crate::relay) fn relay_pkgs(&self) -> Vec<RelayUrlPkgs> {
        full_history_relay_pkgs_from_relay_targets(&self.targets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::RelayUrlPkgs;

    fn subscribe_task(filters: Vec<Filter>, urls: RelayUrlPkgs) -> SubscribeTask {
        SubscribeTask {
            filters,
            relays: urls,
        }
    }

    fn relay_urls(url: &str) -> HashSet<NormRelayUrl> {
        let mut urls = HashSet::new();
        let relay = NormRelayUrl::new(url).unwrap();
        urls.insert(relay);
        urls
    }

    fn relay_pkgs(
        urls: HashSet<NormRelayUrl>,
        demand_priority: RelayDemandPriority,
        routing_preference: RelayRoutingPreference,
    ) -> RelayUrlPkgs {
        RelayUrlPkgs::new(
            urls,
            crate::relay::RelayUrlPolicy::explicit(demand_priority, routing_preference),
        )
    }

    fn important_relay_pkgs(urls: HashSet<NormRelayUrl>) -> RelayUrlPkgs {
        relay_pkgs(
            urls,
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        )
    }

    /// new_subscription should persist relay metadata and expose it via stored_ref().
    #[test]
    fn new_subscription_records_metadata() {
        let mut subs = OutboxSubscriptions::default();
        let pkgs = relay_pkgs(
            relay_urls("wss://relay-meta.example.com"),
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        );
        let filters = vec![Filter::new().kinds(vec![1]).limit(4).build()];
        let id = OutboxSubId(7);

        subs.new_subscription(id, subscribe_task(filters.clone(), pkgs), true);

        let view = subs.stored_ref(&id).expect("subscription ref");
        assert_eq!(view.id, id);
        assert_eq!(view.filters.get_filters().len(), filters.len());
        assert!(subs.json_size(&id).expect("subscription json size") > 0);

        let sub = subs.get(&id).expect("subscription metadata");
        assert!(sub.is_oneshot);
        assert_eq!(sub.relays.len(), 1);
        assert_eq!(
            sub.routing_preference,
            RelayRoutingPreference::PreferDedicated
        );
    }

    #[test]
    fn relay_connection_priority_prefers_strongest_demand_and_tracks_request_count() {
        let mut subs = OutboxSubscriptions::default();
        let relay = NormRelayUrl::new("wss://relay-priority.example.com").unwrap();
        let relay_urls = HashSet::from([relay.clone()]);

        subs.new_subscription(
            OutboxSubId(1),
            subscribe_task(
                vec![Filter::new().kinds(vec![1]).build()],
                relay_pkgs(
                    relay_urls.clone(),
                    RelayDemandPriority::Opportunistic,
                    RelayRoutingPreference::NoPreference,
                ),
            ),
            false,
        );
        subs.new_subscription(
            OutboxSubId(2),
            subscribe_task(
                vec![Filter::new().kinds(vec![2]).build()],
                relay_pkgs(
                    relay_urls,
                    RelayDemandPriority::Critical,
                    RelayRoutingPreference::RequireDedicated,
                ),
            ),
            false,
        );

        assert_eq!(
            subs.relay_connection_priority_for_relay(&relay),
            Some(crate::relay::RelayConnectionPriority {
                strongest_demand: RelayDemandPriority::Critical,
                request_count: 2,
            })
        );
    }

    /// json_size_sum aggregates the JSON payload size for the requested subscriptions.
    #[test]
    fn json_size_sum_accumulates_sizes() {
        let mut subs = OutboxSubscriptions::default();
        let filters = vec![Filter::new().kinds(vec![1]).build()];
        let id_a = OutboxSubId(1);
        let id_b = OutboxSubId(2);
        subs.new_subscription(
            id_a,
            subscribe_task(
                filters.clone(),
                important_relay_pkgs(relay_urls("wss://relay-json-a.example")),
            ),
            false,
        );
        subs.new_subscription(
            id_b,
            subscribe_task(
                filters,
                important_relay_pkgs(relay_urls("wss://relay-json-b.example")),
            ),
            false,
        );

        let mut ids = HashSet::new();
        ids.insert(id_a);
        ids.insert(id_b);

        let sum = subs.json_size_sum(&ids);
        let expected = subs.json_size(&id_a).unwrap() + subs.json_size(&id_b).unwrap();
        assert_eq!(sum, expected);
    }

    /// see_all should mark every filter as seen at the provided timestamp.
    #[test]
    fn see_all_marks_filters() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(8);
        subs.new_subscription(
            id,
            subscribe_task(
                vec![
                    Filter::new().kinds(vec![1]).limit(2).build(),
                    Filter::new().kinds(vec![4]).limit(1).build(),
                ],
                important_relay_pkgs(relay_urls("wss://relay-see.example")),
            ),
            false,
        );

        let timestamp = 12345;
        assert!(subs.see_all(&id, timestamp));

        assert!(subs
            .get(&id)
            .expect("subscription metadata")
            .filters
            .iter()
            .all(|(_, meta)| meta.last_seen == Some(timestamp)));
    }

    #[test]
    fn metadata_only_updates_do_not_bump_transport_demand_version() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(18);
        let relays = relay_urls("wss://relay-metadata.example");
        subs.new_subscription(
            id,
            subscribe_task(
                vec![Filter::new().kinds(vec![1]).limit(2).build()],
                important_relay_pkgs(relays.clone()),
            ),
            false,
        );
        let version = subs.version();

        assert!(!subs.see_all(&OutboxSubId(19), 12345));
        assert_eq!(subs.version(), version);

        assert!(subs.see_all(&id, 12345));
        assert_eq!(subs.version(), version);

        assert!(subs.ingest_task(
            &id,
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters: vec![Filter::new().kinds(vec![1]).limit(4).build()],
                relays: important_relay_pkgs(relays),
            })
        ));
        assert_eq!(subs.version(), version);
    }

    #[test]
    fn relay_demand_updates_bump_transport_demand_version() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(20);
        let filters = vec![Filter::new().kinds(vec![1]).limit(2).build()];
        subs.new_subscription(
            id,
            subscribe_task(
                filters.clone(),
                important_relay_pkgs(relay_urls("wss://relay-demand-old.example")),
            ),
            false,
        );
        let version = subs.version();

        assert!(!subs.ingest_task(
            &OutboxSubId(21),
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters: filters.clone(),
                relays: important_relay_pkgs(relay_urls("wss://relay-demand-missing.example")),
            })
        ));
        assert_eq!(subs.version(), version);

        assert!(subs.ingest_task(
            &id,
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters,
                relays: important_relay_pkgs(relay_urls("wss://relay-demand-new.example")),
            })
        ));
        assert_ne!(subs.version(), version);
    }

    /// ingest_task should update json_size when filters are modified.
    #[test]
    fn ingest_task_updates_json_size_on_filter_change() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(9);
        let small_filters = vec![Filter::new().kinds(vec![1]).build()];
        subs.new_subscription(
            id,
            subscribe_task(
                small_filters,
                important_relay_pkgs(relay_urls("wss://relay-ingest.example")),
            ),
            false,
        );

        let original_size = subs.json_size(&id).unwrap();

        // Modify with larger filters
        let large_filters = vec![
            Filter::new().kinds(vec![1, 2, 3, 4, 5]).limit(100).build(),
            Filter::new().kinds(vec![6, 7, 8]).limit(50).build(),
        ];
        assert!(subs.ingest_task(
            &id,
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters: large_filters,
                relays: important_relay_pkgs(relay_urls("wss://relay-ingest.example")),
            })
        ));

        let new_size = subs.json_size(&id).unwrap();
        assert_ne!(
            original_size, new_size,
            "json_size should change after filter modification"
        );
        assert!(
            new_size > original_size,
            "larger filters should have larger json_size"
        );
    }

    /// ingest_task with Full modification should update json_size.
    #[test]
    fn ingest_task_updates_json_size_on_full_change() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(10);
        let small_filters = vec![Filter::new().kinds(vec![1]).build()];
        subs.new_subscription(
            id,
            subscribe_task(
                small_filters,
                important_relay_pkgs(relay_urls("wss://relay-full.example")),
            ),
            false,
        );

        let original_size = subs.json_size(&id).unwrap();

        // Full modification with larger filters
        let large_filters = vec![
            Filter::new().kinds(vec![1, 2, 3, 4, 5]).limit(100).build(),
            Filter::new().kinds(vec![6, 7, 8]).limit(50).build(),
        ];
        assert!(subs.ingest_task(
            &id,
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters: large_filters,
                relays: important_relay_pkgs(relay_urls("wss://new-relay.example")),
            })
        ));

        let new_size = subs.json_size(&id).unwrap();
        assert_ne!(
            original_size, new_size,
            "json_size should change after full modification"
        );
        assert!(
            new_size > original_size,
            "larger filters should have larger json_size"
        );
    }

    fn filter_has_since(filter: &Filter, expected: u64) -> bool {
        let json = filter.json().expect("filter json");
        json.contains(&format!("\"since\":{}", expected))
    }

    /// Full-history config should preserve explicit history filters.
    #[test]
    fn full_history_config_preserves_limit_and_since() {
        let filter = Filter::new().kinds(vec![1]).since(123).limit(500).build();

        let config = FullHistoryConfig::new(vec![filter]);
        let json = config.filters()[0].json().expect("filter json");

        assert!(json.contains("\"since\":123"));
        assert!(json.contains("\"limit\":500"));
    }

    /// Full flow: see_all sets last_seen, then compaction projection applies it to filters.
    #[test]
    fn see_all_then_compaction_projection_applies_since_to_filters() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(11);
        let filters = vec![
            Filter::new().kinds(vec![1]).build(),
            Filter::new().kinds(vec![2]).build(),
        ];
        subs.new_subscription(
            id,
            subscribe_task(
                filters,
                important_relay_pkgs(relay_urls("wss://relay-since.example")),
            ),
            false,
        );

        // Verify filters don't have since initially
        let view = subs.stored_ref(&id).unwrap();
        for filter in view.filters.get_filters() {
            let json = filter.json().expect("filter json");
            assert!(
                !json.contains("\"since\""),
                "filter should not have since initially"
            );
        }

        let timestamp = 1700000000u64;
        assert!(subs.see_all(&id, timestamp));

        // Verify filters now have since
        let filters = subs.filters_for_compaction(&id).unwrap();
        for filter in &filters {
            assert!(
                filter_has_since(filter, timestamp),
                "filter should have since after see_all + compaction projection"
            );
        }
    }

    /// Stored filters remain pristine while compaction projection applies since.
    #[test]
    fn stored_ref_keeps_pristine_filters_after_projection() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(12);
        let filters = vec![Filter::new().kinds(vec![1]).build()];
        subs.new_subscription(
            id,
            subscribe_task(
                filters,
                important_relay_pkgs(relay_urls("wss://relay-view.example")),
            ),
            false,
        );

        let timestamp = 1234567890u64;
        assert!(subs.see_all(&id, timestamp));

        let view = subs.stored_ref(&id).unwrap();
        let stored_filter = &view.filters.get_filters()[0];
        assert!(
            !stored_filter
                .json()
                .expect("stored filter json")
                .contains("\"since\""),
            "stored filters should remain pristine"
        );

        let projected = subs.filters_for_compaction(&id).unwrap();
        assert!(
            filter_has_since(&projected[0], timestamp),
            "projection should return filters with since applied"
        );
    }
}
