use std::collections::{btree_set, BTreeSet};

use crate::relay::{OutboxSubId, RelayTask};

/// QueuedTasks stores subscription work that could not be scheduled immediately.
#[derive(Default)]
pub struct QueuedTasks {
    tasks: BTreeSet<OutboxSubId>,
}

impl QueuedTasks {
    pub fn add(&mut self, id: OutboxSubId, task: RelayTask) {
        match task {
            RelayTask::Unsubscribe => {
                // i guess swap remove is ok here? it's not super important to maintain strict insertion order
                if !self.tasks.contains(&id) {
                    return;
                }
                self.tasks.remove(&id);
            }
            RelayTask::Subscribe => {
                self.tasks.insert(id);
            }
        }
    }

    pub fn pop(&mut self) -> Option<OutboxSubId> {
        self.tasks.pop_last()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }
}

impl IntoIterator for QueuedTasks {
    type Item = OutboxSubId;
    type IntoIter = btree_set::IntoIter<OutboxSubId>;

    fn into_iter(self) -> Self::IntoIter {
        self.tasks.into_iter()
    }
}
