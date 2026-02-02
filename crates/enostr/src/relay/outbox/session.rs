use hashbrown::{hash_map::Entry, HashMap, HashSet};
use nostrdb::Filter;

use crate::relay::{
    FullModificationTask, ModifyFiltersTask, ModifyRelaysTask, ModifyTask, NormRelayUrl,
    OutboxSubId, OutboxTask, RelayUrlPkgs, SubscribeTask,
};

/// OutboxSession records subscription mutations for the current frame before they
/// are applied to the relay coordinators.
#[derive(Default)]
pub struct OutboxSession {
    pub tasks: HashMap<OutboxSubId, OutboxTask>,
}

impl OutboxSession {
    #[profiling::function]
    pub fn new_filters(&mut self, id: OutboxSubId, mut new_filters: Vec<Filter>) {
        filters_prune_empty(&mut new_filters);
        if new_filters.is_empty() {
            self.unsubscribe(id);
            return;
        }

        let entry = self.tasks.entry(id);

        let mut entry = match entry {
            Entry::Occupied(occupied_entry) => {
                if matches!(occupied_entry.get(), OutboxTask::Oneshot(_)) {
                    // we don't modify oneshots
                    return;
                }
                occupied_entry
            }
            Entry::Vacant(vacant_entry) => {
                vacant_entry.insert(OutboxTask::Modify(ModifyTask::Filters(ModifyFiltersTask(
                    new_filters,
                ))));
                return;
            }
        };

        match entry.get_mut() {
            OutboxTask::Modify(modify) => match modify {
                ModifyTask::Filters(_) => {
                    self.tasks.insert(
                        id,
                        OutboxTask::Modify(ModifyTask::Filters(ModifyFiltersTask(new_filters))),
                    );
                }
                ModifyTask::Relays(modify_relays_task) => {
                    let relays = std::mem::take(&mut modify_relays_task.0);
                    *entry.get_mut() = OutboxTask::Modify(ModifyTask::Full(FullModificationTask {
                        filters: new_filters,
                        relays,
                    }));
                }
                ModifyTask::Full(full) => {
                    full.filters = new_filters;
                }
            },
            OutboxTask::Unsubscribe => {
                self.tasks.insert(
                    id,
                    OutboxTask::Modify(ModifyTask::Filters(ModifyFiltersTask(new_filters))),
                );
            }
            OutboxTask::Oneshot(oneshot) => {
                oneshot.filters = new_filters;
            }
            OutboxTask::Subscribe(subscribe_task) => {
                subscribe_task.filters = new_filters;
            }
        }
    }
    #[profiling::function]
    pub fn new_relays(&mut self, id: OutboxSubId, new_urls: HashSet<NormRelayUrl>) {
        let entry = self.tasks.entry(id);

        let mut entry = match entry {
            Entry::Occupied(occupied_entry) => {
                let task = occupied_entry.get();

                if matches!(task, OutboxTask::Oneshot(_)) {
                    // we don't modify oneshots
                    return;
                }

                occupied_entry
            }
            Entry::Vacant(vacant_entry) => {
                vacant_entry.insert(OutboxTask::Modify(ModifyTask::Relays(ModifyRelaysTask(
                    new_urls,
                ))));
                return;
            }
        };

        match entry.get_mut() {
            OutboxTask::Modify(modify) => {
                match modify {
                    ModifyTask::Filters(modify_filters_task) => {
                        let filters = std::mem::take(&mut modify_filters_task.0); // moves out, leaves empty/default
                        *entry.get_mut() =
                            OutboxTask::Modify(ModifyTask::Full(FullModificationTask {
                                filters,
                                relays: new_urls,
                            }));
                    }
                    ModifyTask::Relays(_) => {
                        self.tasks.insert(
                            id,
                            OutboxTask::Modify(ModifyTask::Relays(ModifyRelaysTask(new_urls))),
                        );
                    }
                    ModifyTask::Full(full_modification_task) => {
                        full_modification_task.relays = new_urls;
                    }
                }
            }
            OutboxTask::Unsubscribe => {
                self.tasks.insert(
                    id,
                    OutboxTask::Modify(ModifyTask::Relays(ModifyRelaysTask(new_urls))),
                );
            }
            OutboxTask::Oneshot(oneshot) => {
                oneshot.relays.urls = new_urls;
            }
            OutboxTask::Subscribe(subscribe_task) => {
                subscribe_task.relays.urls = new_urls;
            }
        }
    }

    pub fn subscribe(&mut self, id: OutboxSubId, mut filters: Vec<Filter>, urls: RelayUrlPkgs) {
        filters_prune_empty(&mut filters);
        if filters.is_empty() {
            return;
        }

        self.tasks.insert(
            id,
            OutboxTask::Subscribe(SubscribeTask {
                filters,
                relays: urls,
            }),
        );
    }

    pub fn oneshot(&mut self, id: OutboxSubId, mut filters: Vec<Filter>, urls: RelayUrlPkgs) {
        filters_prune_empty(&mut filters);
        if filters.is_empty() {
            return;
        }

        self.tasks.insert(
            id,
            OutboxTask::Oneshot(SubscribeTask {
                filters,
                relays: urls,
            }),
        );
    }

    pub fn unsubscribe(&mut self, id: OutboxSubId) {
        self.tasks.insert(id, OutboxTask::Unsubscribe);
    }
}

fn filters_prune_empty(filters: &mut Vec<Filter>) {
    filters.retain(|f| f.num_elements() != 0);
}
