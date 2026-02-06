use hashbrown::{HashMap, HashSet};
use nostrdb::Filter;

use crate::relay::{MetadataFilters, NormRelayUrl, OutboxSubId, RelayType, RelayUrlPkgs};

pub struct OutboxSubscription {
    pub relays: HashSet<NormRelayUrl>,
    pub filters: MetadataFilters,
    json_size: usize,
    pub is_oneshot: bool,
    pub relay_type: RelayType,
}

impl OutboxSubscription {
    pub fn see_all(&mut self, at: u64) {
        for (_, meta) in self.filters.iter_mut() {
            meta.last_seen = Some(at);
        }
    }

    pub fn ingest_task(&mut self, task: ModifyTask) {
        match task {
            ModifyTask::Filters(modify_filters_task) => {
                self.filters = MetadataFilters::new(modify_filters_task.0);
                self.json_size = self.filters.json_size_sum();
            }
            ModifyTask::Relays(modify_relays_task) => {
                self.relays = modify_relays_task.0;
            }
            ModifyTask::Full(full_modification_task) => {
                self.filters = MetadataFilters::new(full_modification_task.filters);
                self.json_size = self.filters.json_size_sum();
                self.relays = full_modification_task.relays;
            }
        }
    }
}

#[derive(Default)]
pub struct OutboxSubscriptions {
    subs: HashMap<OutboxSubId, OutboxSubscription>,
}

impl OutboxSubscriptions {
    pub fn view(&self, id: &OutboxSubId) -> Option<SubscriptionView<'_>> {
        let sub = self.subs.get(id)?;

        Some(SubscriptionView {
            id: *id,
            filters: &sub.filters,
            json_size: sub.json_size,
            is_oneshot: sub.is_oneshot,
        })
    }

    pub fn json_size(&self, id: &OutboxSubId) -> Option<usize> {
        self.subs.get(id).map(|s| s.json_size)
    }

    pub fn subset_oneshot(&self, ids: &HashSet<OutboxSubId>) -> HashSet<OutboxSubId> {
        ids.iter()
            .filter(|id| self.subs.get(*id).is_some_and(|s| s.is_oneshot))
            .copied()
            .collect()
    }

    pub fn is_oneshot(&self, id: &OutboxSubId) -> bool {
        self.subs.get(id).is_some_and(|s| s.is_oneshot)
    }

    pub fn json_size_sum(&self, ids: &HashSet<OutboxSubId>) -> usize {
        ids.iter()
            .map(|id| self.subs.get(id).map_or(0, |s| s.json_size))
            .sum()
    }

    pub fn filters_all(&self, ids: &HashSet<OutboxSubId>) -> Vec<Filter> {
        ids.iter()
            .filter_map(|id| self.subs.get(id))
            .flat_map(|sub| sub.filters.filters.iter().cloned())
            .collect()
    }

    pub fn get_mut(&mut self, id: &OutboxSubId) -> Option<&mut OutboxSubscription> {
        self.subs.get_mut(id)
    }

    pub fn remove(&mut self, id: &OutboxSubId) {
        self.subs.remove(id);
    }

    pub fn new_subscription(&mut self, id: OutboxSubId, task: SubscribeTask, is_oneshot: bool) {
        let filters = MetadataFilters::new(task.filters);
        let json_size = filters.json_size_sum();
        self.subs.insert(
            id,
            OutboxSubscription {
                relays: task.relays.urls,
                filters,
                json_size,
                is_oneshot,
                relay_type: if task.relays.use_transparent {
                    RelayType::Transparent
                } else {
                    RelayType::Compaction
                },
            },
        );
    }
}

pub struct SubscriptionView<'a> {
    pub id: OutboxSubId,
    pub filters: &'a MetadataFilters,
    pub json_size: usize,
    pub is_oneshot: bool,
}

pub enum OutboxTask {
    Modify(ModifyTask),
    Subscribe(SubscribeTask),
    Unsubscribe,
    Oneshot(SubscribeTask),
}

pub enum ModifyTask {
    Filters(ModifyFiltersTask),
    Relays(ModifyRelaysTask),
    Full(FullModificationTask),
}

#[derive(Default)]
pub struct ModifyFiltersTask(pub Vec<Filter>);

pub struct ModifyRelaysTask(pub HashSet<NormRelayUrl>);

pub struct FullModificationTask {
    pub filters: Vec<Filter>,
    pub relays: HashSet<NormRelayUrl>,
}

pub struct SubscribeTask {
    pub filters: Vec<Filter>,
    pub relays: RelayUrlPkgs,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::RelayUrlPkgs;
    use crate::relay::{FullModificationTask, ModifyFiltersTask};

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

    /// new_subscription should persist relay metadata and expose it via view().
    #[test]
    fn new_subscription_records_metadata() {
        let mut subs = OutboxSubscriptions::default();
        let mut pkgs = RelayUrlPkgs::new(relay_urls("wss://relay-meta.example.com"));
        pkgs.use_transparent = true;
        let filters = vec![Filter::new().kinds(vec![1]).limit(4).build()];
        let id = OutboxSubId(7);

        subs.new_subscription(id, subscribe_task(filters.clone(), pkgs), true);

        let view = subs.view(&id).expect("subscription view");
        assert_eq!(view.id, id);
        assert!(view.is_oneshot);
        assert_eq!(view.filters.get_filters().len(), filters.len());
        assert!(view.json_size > 0);

        let sub = subs.get_mut(&id).expect("subscription metadata");
        assert_eq!(sub.relays.len(), 1);
        assert_eq!(sub.relay_type, RelayType::Transparent);
    }

    /// subset_oneshot should only return IDs corresponding to oneshot subscriptions.
    #[test]
    fn subset_oneshot_filters_ids() {
        let mut subs = OutboxSubscriptions::default();
        let filters = vec![Filter::new().kinds(vec![1]).build()];
        let id_a = OutboxSubId(1);
        let id_b = OutboxSubId(2);
        subs.new_subscription(
            id_a,
            subscribe_task(
                filters.clone(),
                RelayUrlPkgs::new(relay_urls("wss://relay-a.example")),
            ),
            false,
        );
        subs.new_subscription(
            id_b,
            subscribe_task(
                filters,
                RelayUrlPkgs::new(relay_urls("wss://relay-b.example")),
            ),
            true,
        );

        let mut ids = HashSet::new();
        ids.insert(id_a);
        ids.insert(id_b);

        let oneshots = subs.subset_oneshot(&ids);
        let expected = {
            let mut s = HashSet::new();
            s.insert(id_b);
            s
        };
        assert_eq!(oneshots, expected);
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
                RelayUrlPkgs::new(relay_urls("wss://relay-json-a.example")),
            ),
            false,
        );
        subs.new_subscription(
            id_b,
            subscribe_task(
                filters,
                RelayUrlPkgs::new(relay_urls("wss://relay-json-b.example")),
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
                RelayUrlPkgs::new(relay_urls("wss://relay-see.example")),
            ),
            false,
        );

        let timestamp = 12345;
        let sub = subs.get_mut(&id).expect("subscription metadata");
        sub.see_all(timestamp);

        assert!(sub
            .filters
            .iter()
            .all(|(_, meta)| meta.last_seen == Some(timestamp)));
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
                RelayUrlPkgs::new(relay_urls("wss://relay-ingest.example")),
            ),
            false,
        );

        let original_size = subs.json_size(&id).unwrap();

        // Modify with larger filters
        let large_filters = vec![
            Filter::new().kinds(vec![1, 2, 3, 4, 5]).limit(100).build(),
            Filter::new().kinds(vec![6, 7, 8]).limit(50).build(),
        ];
        let sub = subs.get_mut(&id).unwrap();
        sub.ingest_task(ModifyTask::Filters(ModifyFiltersTask(large_filters)));

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
                RelayUrlPkgs::new(relay_urls("wss://relay-full.example")),
            ),
            false,
        );

        let original_size = subs.json_size(&id).unwrap();

        // Full modification with larger filters
        let large_filters = vec![
            Filter::new().kinds(vec![1, 2, 3, 4, 5]).limit(100).build(),
            Filter::new().kinds(vec![6, 7, 8]).limit(50).build(),
        ];
        let sub = subs.get_mut(&id).unwrap();
        sub.ingest_task(ModifyTask::Full(FullModificationTask {
            filters: large_filters,
            relays: relay_urls("wss://new-relay.example"),
        }));

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

    /// Full flow: see_all sets last_seen, then since_optimize applies it to filters.
    #[test]
    fn see_all_then_since_optimize_applies_since_to_filters() {
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
                RelayUrlPkgs::new(relay_urls("wss://relay-since.example")),
            ),
            false,
        );

        // Verify filters don't have since initially
        let view = subs.view(&id).unwrap();
        for filter in view.filters.get_filters() {
            let json = filter.json().expect("filter json");
            assert!(
                !json.contains("\"since\""),
                "filter should not have since initially"
            );
        }

        let timestamp = 1700000000u64;
        let sub = subs.get_mut(&id).unwrap();
        sub.see_all(timestamp);
        sub.filters.since_optimize();

        // Verify filters now have since
        let view = subs.view(&id).unwrap();
        for filter in view.filters.get_filters() {
            assert!(
                filter_has_since(filter, timestamp),
                "filter should have since after see_all + since_optimize"
            );
        }
    }

    /// Filters accessed via view() should have since after optimization.
    #[test]
    fn view_returns_optimized_filters() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(12);
        let filters = vec![Filter::new().kinds(vec![1]).build()];
        subs.new_subscription(
            id,
            subscribe_task(
                filters,
                RelayUrlPkgs::new(relay_urls("wss://relay-view.example")),
            ),
            false,
        );

        let timestamp = 1234567890u64;
        {
            let sub = subs.get_mut(&id).unwrap();
            sub.see_all(timestamp);
            sub.filters.since_optimize();
        }

        // Access via view - should see the optimized filters
        let view = subs.view(&id).unwrap();
        let filter = &view.filters.get_filters()[0];
        assert!(
            filter_has_since(filter, timestamp),
            "view should return filters with since applied"
        );
    }
}
