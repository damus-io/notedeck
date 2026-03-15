use std::hash::Hash;

use hashbrown::HashMap;
use slab::Slab;

type EntryId = usize;

/// IndexedQueue is a FIFO queue with O(1) deduped insertion and removal by id.
pub(crate) struct IndexedQueue<T>
where
    T: Copy + Eq + Hash,
{
    entry_by_id: HashMap<T, EntryId>,
    entries: Slab<QueueEntry<T>>,
    head: Option<EntryId>,
    tail: Option<EntryId>,
}

impl<T> Default for IndexedQueue<T>
where
    T: Copy + Eq + Hash,
{
    fn default() -> Self {
        Self {
            entry_by_id: HashMap::default(),
            entries: Slab::default(),
            head: None,
            tail: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct QueueEntry<T> {
    id: T,
    prev: Option<EntryId>,
    next: Option<EntryId>,
}

impl<T> IndexedQueue<T>
where
    T: Copy + Eq + Hash,
{
    /// Returns an iterator over IDs in FIFO order without consuming the queue.
    pub(crate) fn iter(&self) -> IndexedQueueIter<'_, T> {
        IndexedQueueIter {
            queue: self,
            next: self.head,
        }
    }

    /// Appends `id` at the tail when it is not already present.
    pub(crate) fn push_back_if_missing(&mut self, id: T) {
        if self.entry_by_id.contains_key(&id) {
            return;
        }

        let entry_id = self.entries.insert(QueueEntry {
            id,
            prev: self.tail,
            next: None,
        });

        if let Some(tail_id) = self.tail {
            self.entries[tail_id].next = Some(entry_id);
        } else {
            self.head = Some(entry_id);
        }

        self.tail = Some(entry_id);
        self.entry_by_id.insert(id, entry_id);
    }

    /// Removes `id` from the queue in O(1) when present.
    pub(crate) fn remove(&mut self, id: T) -> bool {
        let Some(entry_id) = self.entry_by_id.remove(&id) else {
            return false;
        };
        self.unlink_and_remove(entry_id);
        true
    }

    /// Removes and returns the head of the queue.
    pub(crate) fn pop_front(&mut self) -> Option<T> {
        let head_id = self.head?;
        let id = self.entries[head_id].id;
        self.entry_by_id.remove(&id);
        Some(self.unlink_and_remove(head_id))
    }

    pub(crate) fn clear(&mut self) {
        self.entry_by_id.clear();
        self.entries.clear();
        self.head = None;
        self.tail = None;
    }

    pub(crate) fn len(&self) -> usize {
        self.entry_by_id.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entry_by_id.is_empty()
    }

    fn unlink_and_remove(&mut self, entry_id: EntryId) -> T {
        let entry = self
            .entries
            .try_remove(entry_id)
            .expect("indexed queue entry should exist");

        match entry.prev {
            Some(prev_id) => self.entries[prev_id].next = entry.next,
            None => self.head = entry.next,
        }

        match entry.next {
            Some(next_id) => self.entries[next_id].prev = entry.prev,
            None => self.tail = entry.prev,
        }

        entry.id
    }
}

pub(crate) struct IndexedQueueIter<'a, T>
where
    T: Copy + Eq + Hash,
{
    queue: &'a IndexedQueue<T>,
    next: Option<EntryId>,
}

impl<T> Iterator for IndexedQueueIter<'_, T>
where
    T: Copy + Eq + Hash,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        let entry_id = self.next?;
        let entry = &self.queue.entries[entry_id];
        self.next = entry.next;
        Some(entry.id)
    }
}

pub(crate) struct IndexedQueueIntoIter<T>
where
    T: Copy + Eq + Hash,
{
    queue: IndexedQueue<T>,
}

impl<T> Iterator for IndexedQueueIntoIter<T>
where
    T: Copy + Eq + Hash,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.queue.pop_front()
    }
}

impl<T> IntoIterator for IndexedQueue<T>
where
    T: Copy + Eq + Hash,
{
    type Item = T;
    type IntoIter = IndexedQueueIntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        IndexedQueueIntoIter { queue: self }
    }
}

#[cfg(test)]
mod tests {
    use super::IndexedQueue;

    #[test]
    fn push_back_preserves_fifo_order() {
        let mut queue = IndexedQueue::default();
        queue.push_back_if_missing(1u64);
        queue.push_back_if_missing(2u64);
        queue.push_back_if_missing(3u64);

        assert_eq!(queue.pop_front(), Some(1));
        assert_eq!(queue.pop_front(), Some(2));
        assert_eq!(queue.pop_front(), Some(3));
        assert!(queue.is_empty());
    }

    #[test]
    fn push_back_dedupes_existing_ids() {
        let mut queue = IndexedQueue::default();
        queue.push_back_if_missing(7u64);
        queue.push_back_if_missing(7u64);

        assert_eq!(queue.len(), 1);
        assert_eq!(queue.pop_front(), Some(7));
        assert_eq!(queue.pop_front(), None);
    }

    #[test]
    fn remove_unlinks_middle_entry() {
        let mut queue = IndexedQueue::default();
        queue.push_back_if_missing(1u64);
        queue.push_back_if_missing(2u64);
        queue.push_back_if_missing(3u64);

        assert!(queue.remove(2));

        assert_eq!(queue.pop_front(), Some(1));
        assert_eq!(queue.pop_front(), Some(3));
        assert_eq!(queue.pop_front(), None);
    }

    #[test]
    fn remove_allows_reenqueue_at_tail() {
        let mut queue = IndexedQueue::default();
        queue.push_back_if_missing(1u64);
        queue.push_back_if_missing(2u64);
        queue.remove(1);
        queue.push_back_if_missing(1u64);

        assert_eq!(queue.pop_front(), Some(2));
        assert_eq!(queue.pop_front(), Some(1));
        assert_eq!(queue.pop_front(), None);
    }
}
