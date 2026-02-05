use std::collections::HashSet;

use enostr::OutboxSubId;

/// Keeps track of subs which should follow a modifiable set of relays (the user's relay list)
#[derive(Default, Clone)]
pub struct RelayListDependents {
    subs: HashSet<OutboxSubId>,
}

impl RelayListDependents {
    pub fn add(&mut self, id: OutboxSubId) {
        self.subs.insert(id);
    }

    pub fn remove(&mut self, id: &OutboxSubId) {
        self.subs.remove(id);
    }

    pub fn get_all(&self) -> &HashSet<OutboxSubId> {
        &self.subs
    }
}
