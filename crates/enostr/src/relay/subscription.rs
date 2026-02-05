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

    pub fn get(&self, id: &OutboxSubId) -> Option<&OutboxSubscription> {
        self.subs.get(id)
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
