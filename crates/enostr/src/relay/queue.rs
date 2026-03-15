use crate::relay::{indexed_queue::IndexedQueue, OutboxSubId};

/// QueuedTasks stores subscription work that could not be scheduled immediately.
#[derive(Default)]
pub struct QueuedTasks {
    order: IndexedQueue<OutboxSubId>,
}

impl QueuedTasks {
    /// Enqueues `id` once in FIFO order.
    pub fn enqueue(&mut self, id: OutboxSubId) {
        self.order.push_back_if_missing(id);
    }

    /// Cancels queued work for `id` in O(1).
    pub fn cancel(&mut self, id: OutboxSubId) {
        self.order.remove(id);
    }

    pub fn pop(&mut self) -> Option<OutboxSubId> {
        self.order.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.order.len()
    }
}

impl IntoIterator for QueuedTasks {
    type Item = OutboxSubId;
    type IntoIter = QueuedTasksIntoIter;

    fn into_iter(self) -> Self::IntoIter {
        QueuedTasksIntoIter { queue: self }
    }
}

pub struct QueuedTasksIntoIter {
    queue: QueuedTasks,
}

impl Iterator for QueuedTasksIntoIter {
    type Item = OutboxSubId;

    fn next(&mut self) -> Option<Self::Item> {
        self.queue.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_preserves_fifo_order() {
        let mut queue = QueuedTasks::default();
        queue.enqueue(OutboxSubId(1));
        queue.enqueue(OutboxSubId(2));
        queue.enqueue(OutboxSubId(3));

        assert_eq!(queue.pop(), Some(OutboxSubId(1)));
        assert_eq!(queue.pop(), Some(OutboxSubId(2)));
        assert_eq!(queue.pop(), Some(OutboxSubId(3)));
        assert!(queue.is_empty());
    }

    #[test]
    fn enqueue_dedupes_existing_ids() {
        let mut queue = QueuedTasks::default();
        queue.enqueue(OutboxSubId(7));
        queue.enqueue(OutboxSubId(7));

        assert_eq!(queue.len(), 1);
        assert_eq!(queue.pop(), Some(OutboxSubId(7)));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn cancel_removes_middle_entry_immediately() {
        let mut queue = QueuedTasks::default();
        queue.enqueue(OutboxSubId(1));
        queue.enqueue(OutboxSubId(2));
        queue.enqueue(OutboxSubId(3));

        queue.cancel(OutboxSubId(2));

        assert_eq!(queue.pop(), Some(OutboxSubId(1)));
        assert_eq!(queue.pop(), Some(OutboxSubId(3)));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn cancel_allows_reenqueue_at_tail() {
        let mut queue = QueuedTasks::default();
        queue.enqueue(OutboxSubId(1));
        queue.enqueue(OutboxSubId(2));
        queue.cancel(OutboxSubId(1));
        queue.enqueue(OutboxSubId(1));

        assert_eq!(queue.pop(), Some(OutboxSubId(2)));
        assert_eq!(queue.pop(), Some(OutboxSubId(1)));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn into_iter_yields_live_fifo_order() {
        let mut queue = QueuedTasks::default();
        queue.enqueue(OutboxSubId(4));
        queue.enqueue(OutboxSubId(5));
        queue.cancel(OutboxSubId(4));
        queue.enqueue(OutboxSubId(6));

        let ids: Vec<_> = queue.into_iter().collect();
        assert_eq!(ids, vec![OutboxSubId(5), OutboxSubId(6)]);
    }
}
