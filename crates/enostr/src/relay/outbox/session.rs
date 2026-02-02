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

#[cfg(test)]
mod tests {
    use crate::relay::test_utils::{expect_task, trivial_filter};

    use super::*;

    // ==================== OutboxSession tests ====================

    /// Verifies a freshly created session has no pending tasks.
    #[test]
    fn outbox_session_default_empty() {
        let session = OutboxSession::default();
        assert!(session.tasks.is_empty());
    }

    /// Drops subscribe/oneshot requests that lack meaningful filters/relays.
    #[test]
    fn outbox_session_subscribe_empty() {
        let mut session = OutboxSession::default();
        let urls = RelayUrlPkgs::new(HashSet::new());

        session.subscribe(OutboxSubId(0), vec![Filter::new().build()], urls.clone());
        assert!(session.tasks.is_empty());

        session.subscribe(OutboxSubId(0), vec![], urls.clone());
        assert!(session.tasks.is_empty());

        session.oneshot(OutboxSubId(0), vec![Filter::new().build()], urls.clone());
        assert!(session.tasks.is_empty());

        session.oneshot(OutboxSubId(0), vec![], urls);
        assert!(session.tasks.is_empty());
    }

    /// Stores subscribe tasks when filters and relays are provided.
    #[test]
    fn outbox_session_subscribe() {
        let mut session = OutboxSession::default();
        let urls = RelayUrlPkgs::new(HashSet::new());

        session.subscribe(OutboxSubId(0), trivial_filter(), urls);

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            OutboxTask::Subscribe(_)
        ));
    }

    /// Stores oneshot tasks when filters and relays are provided.
    #[test]
    fn outbox_session_oneshot() {
        let mut session = OutboxSession::default();
        let urls = RelayUrlPkgs::new(HashSet::new());

        session.oneshot(OutboxSubId(0), trivial_filter(), urls);

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            OutboxTask::Oneshot(_)
        ));
    }

    /// Records unsubscribe operations on demand.
    #[test]
    fn outbox_session_unsubscribe() {
        let mut session = OutboxSession::default();

        session.unsubscribe(OutboxSubId(42));

        assert!(matches!(
            expect_task(&session, OutboxSubId(42)),
            OutboxTask::Unsubscribe
        ));
    }

    /// Pushing filters first results in a Modify(Filters) task.
    #[test]
    fn outbox_session_new_filters_creates_modify_filters() {
        let mut session = OutboxSession::default();

        session.new_filters(OutboxSubId(0), trivial_filter());

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            OutboxTask::Modify(ModifyTask::Filters(_))
        ));
    }

    /// Pushing relays first results in a Modify(Relays) task.
    #[test]
    fn outbox_session_new_relays_creates_modify_relays() {
        let mut session = OutboxSession::default();

        session.new_relays(OutboxSubId(0), HashSet::new());

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            OutboxTask::Modify(ModifyTask::Relays(_))
        ));
    }

    /// Mixing filters then relays converges to a Modify(Full) task.
    #[test]
    fn outbox_session_merges_filters_and_relays_to_full_modification() {
        let mut session = OutboxSession::default();

        // First add filters
        session.new_filters(OutboxSubId(0), trivial_filter());

        // Then add relays - should merge to Full modification
        session.new_relays(OutboxSubId(0), HashSet::new());

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            OutboxTask::Modify(ModifyTask::Full(_))
        ));
    }

    /// Mixing relays then filters also converges to a Modify(Full) task.
    #[test]
    fn outbox_session_merges_relays_and_filters_to_full_modification() {
        let mut session = OutboxSession::default();

        // First add relays
        session.new_relays(OutboxSubId(0), HashSet::new());

        // Then add filters - should merge to Full modification
        session.new_filters(OutboxSubId(0), trivial_filter());

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            OutboxTask::Modify(ModifyTask::Full(_))
        ));
    }

    // this should never happen in practice though
    /// Subscribe commands override previously staged filter changes.
    #[test]
    fn outbox_session_subscribe_overwrites_modify_filters() {
        let mut session = OutboxSession::default();
        let urls = RelayUrlPkgs::new(HashSet::new());

        session.new_filters(OutboxSubId(0), trivial_filter());
        session.subscribe(
            OutboxSubId(0),
            vec![Filter::new().kinds(vec![3]).build()],
            urls,
        );

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            OutboxTask::Subscribe(_)
        ));
    }

    /// Unsubscribe issued after subscribe should take precedence.
    #[test]
    fn outbox_session_unsubscribe_after_subscribe() {
        let mut session = OutboxSession::default();
        let urls = RelayUrlPkgs::new(HashSet::new());

        session.subscribe(OutboxSubId(0), trivial_filter(), urls);
        session.unsubscribe(OutboxSubId(0));

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            OutboxTask::Unsubscribe
        ));
    }

    /// Adding filters after an unsubscribe restarts the task as Modify(Filters).
    #[test]
    fn outbox_session_new_filters_after_unsubscribe() {
        let mut session = OutboxSession::default();

        session.unsubscribe(OutboxSubId(0));
        session.new_filters(OutboxSubId(0), trivial_filter());

        // Filters should overwrite unsubscribe
        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            OutboxTask::Modify(ModifyTask::Filters(_))
        ));
    }

    /// Updating filters of a Full modification replaces its filter list.
    #[test]
    fn outbox_session_update_full_modification_filters() {
        let mut session = OutboxSession::default();

        // Create full modification
        session.new_filters(OutboxSubId(0), trivial_filter());
        session.new_relays(OutboxSubId(0), HashSet::new());

        // Update filters on the full modification
        session.new_filters(
            OutboxSubId(0),
            vec![
                Filter::new().kinds(vec![3]).build(),
                Filter::new().kinds(vec![1]).build(),
            ],
        );

        match expect_task(&session, OutboxSubId(0)) {
            OutboxTask::Modify(ModifyTask::Full(fm)) => {
                assert_eq!(fm.filters.len(), 2);
            }
            _ => panic!("Expected Modify(Full)"),
        }
    }

    /// Updating relays of a Full modification replaces its relay set.
    #[test]
    fn outbox_session_update_full_modification_relays() {
        let mut session = OutboxSession::default();

        // Create full modification
        session.new_filters(OutboxSubId(0), trivial_filter());
        session.new_relays(OutboxSubId(0), HashSet::new());

        // Update relays on the full modification
        let mut new_urls = HashSet::new();
        new_urls.insert(NormRelayUrl::new("wss://relay.example.com").unwrap());
        session.new_relays(OutboxSubId(0), new_urls);

        match expect_task(&session, OutboxSubId(0)) {
            OutboxTask::Modify(ModifyTask::Full(fm)) => {
                assert!(!fm.relays.is_empty());
            }
            _ => panic!("Expected Modify(Full)"),
        }
    }

    /// Attempting to modify oneshot filters leaves them unchanged.
    #[test]
    fn outbox_session_update_oneshot_filters() {
        let mut session = OutboxSession::default();
        let urls = RelayUrlPkgs::new(HashSet::new());

        session.oneshot(OutboxSubId(0), trivial_filter(), urls);
        session.new_filters(
            OutboxSubId(0),
            vec![
                Filter::new().kinds([1]).build(),
                Filter::new().kinds([3]).build(),
            ],
        );

        match expect_task(&session, OutboxSubId(0)) {
            OutboxTask::Oneshot(task) => {
                assert_eq!(task.filters.len(), 1);
            }
            _ => panic!("Expected Oneshot task"),
        }
    }

    /// Updating filters on a Subscribe task replaces the stored filters.
    #[test]
    fn outbox_session_update_subscribe_filters() {
        let mut session = OutboxSession::default();
        let urls = RelayUrlPkgs::new(HashSet::new());

        session.subscribe(OutboxSubId(0), trivial_filter(), urls);
        session.new_filters(
            OutboxSubId(0),
            vec![
                Filter::new().kinds([1]).build(),
                Filter::new().kinds([3]).build(),
            ],
        );

        match expect_task(&session, OutboxSubId(0)) {
            OutboxTask::Subscribe(task) => {
                assert_eq!(task.filters.len(), 2);
            }
            _ => panic!("Expected Subscribe task"),
        }
    }

    /// Updating relays on a Subscribe task replaces the stored relays.
    #[test]
    fn outbox_session_update_subscribe_relays() {
        let mut session = OutboxSession::default();
        let urls = RelayUrlPkgs::new(HashSet::new());

        session.subscribe(OutboxSubId(0), trivial_filter(), urls);

        let mut new_urls = HashSet::new();
        new_urls.insert(NormRelayUrl::new("wss://relay.example.com").unwrap());
        session.new_relays(OutboxSubId(0), new_urls);

        match expect_task(&session, OutboxSubId(0)) {
            OutboxTask::Subscribe(task) => {
                assert!(!task.relays.urls.is_empty());
            }
            _ => panic!("Expected Subscribe task"),
        }
    }

    /// Attempting to modify oneshot relays leaves them unchanged.
    #[test]
    fn outbox_session_update_oneshot_relays() {
        let mut session = OutboxSession::default();
        let urls = RelayUrlPkgs::new(HashSet::new());

        session.oneshot(OutboxSubId(0), trivial_filter(), urls);

        let mut new_urls = HashSet::new();
        new_urls.insert(NormRelayUrl::new("wss://relay.example.com").unwrap());
        session.new_relays(OutboxSubId(0), new_urls);

        match expect_task(&session, OutboxSubId(0)) {
            OutboxTask::Oneshot(task) => {
                assert!(
                    task.relays.urls.is_empty(),
                    "cannot make modifications on oneshot"
                );
            }
            _ => panic!("Expected Oneshot task"),
        }
    }
}
