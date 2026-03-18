use enostr::Pubkey;
use hashbrown::HashSet;
use nostrdb::NoteKey;
use notedeck::NoteRef;
use std::cmp::Ordering;

/// Maintains a strictly ordered list of message references for a single
/// conversation. It mirrors the lightweight ordering guarantees that
/// `TimelineCache` and `Threads` rely on so UI code can assume the
/// backing data is already sorted from newest to oldest.
#[derive(Default)]
pub struct MessageStore {
    pub messages_ordered: Vec<NotePkg>,
    seen: HashSet<NoteKey>,
}

impl MessageStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a new `NoteRef` while keeping the store sorted. Returns
    /// `true` when the reference was new to the conversation.
    pub fn insert(&mut self, note: NotePkg) -> bool {
        if !self.seen.insert(note.note_ref.key) {
            return false;
        }

        match self.messages_ordered.binary_search(&note) {
            Ok(_) => {
                debug_assert!(
                    false,
                    "MessageStore::insert was asked to insert a duplicate NoteRef"
                );
                false
            }
            Err(idx) => {
                self.messages_ordered.insert(idx, note);
                true
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.messages_ordered.is_empty()
    }

    pub fn len(&self) -> usize {
        self.messages_ordered.len()
    }

    pub fn latest(&self) -> Option<&NoteRef> {
        self.messages_ordered.first().map(|p| &p.note_ref)
    }

    pub fn newest_timestamp(&self) -> Option<u64> {
        self.latest().map(|n| n.created_at)
    }
}

pub struct NotePkg {
    pub note_ref: NoteRef,
    pub author: Pubkey,
}

impl Ord for NotePkg {
    fn cmp(&self, other: &Self) -> Ordering {
        self.note_ref
            .cmp(&other.note_ref)
            .then_with(|| self.author.cmp(&other.author))
    }
}

impl PartialOrd for NotePkg {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for NotePkg {
    fn eq(&self, other: &Self) -> bool {
        self.note_ref == other.note_ref && self.author == other.author
    }
}

impl Eq for NotePkg {}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::NoteKey;

    fn note_pkg(key: u64, created_at: u64, author: [u8; 32]) -> NotePkg {
        NotePkg {
            note_ref: NoteRef {
                key: NoteKey::new(key),
                created_at,
            },
            author: Pubkey::new(author),
        }
    }

    /// Verifies insertion order stays newest-first even when notes arrive out of order.
    #[test]
    fn insert_orders_out_of_order_messages_by_newest_first() {
        let mut store = MessageStore::new();

        assert!(store.insert(note_pkg(10, 100, [0x10; 32])));
        assert!(store.insert(note_pkg(11, 300, [0x11; 32])));
        assert!(store.insert(note_pkg(12, 200, [0x12; 32])));

        let ordered = store
            .messages_ordered
            .iter()
            .map(|pkg| pkg.note_ref.created_at)
            .collect::<Vec<_>>();
        assert_eq!(ordered, vec![300, 200, 100]);
    }

    /// Verifies ties on `created_at` remain stable and dedupe by `NoteKey` only.
    #[test]
    fn insert_orders_timestamp_ties_by_note_key_and_rejects_duplicates() {
        let mut store = MessageStore::new();

        assert!(store.insert(note_pkg(20, 500, [0x20; 32])));
        assert!(store.insert(note_pkg(10, 500, [0x21; 32])));
        assert!(!store.insert(note_pkg(20, 500, [0x22; 32])));

        let ordered = store
            .messages_ordered
            .iter()
            .map(|pkg| pkg.note_ref.key.as_u64())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![10, 20],
            "expected equal timestamps to order by NoteRef tie-breaker"
        );
        assert_eq!(store.len(), 2);
    }

    /// Verifies mixed timestamp ties sort deterministically regardless of arrival order.
    #[test]
    fn insert_orders_mixed_timestamp_pathologies_independent_of_arrival_order() {
        let mut store = MessageStore::new();

        for note in [
            note_pkg(40, 999, [0x40; 32]),
            note_pkg(20, 1_000, [0x20; 32]),
            note_pkg(50, 1_001, [0x50; 32]),
            note_pkg(10, 1_000, [0x10; 32]),
            note_pkg(30, 1_000, [0x30; 32]),
        ] {
            assert!(store.insert(note));
        }

        let ordered = store
            .messages_ordered
            .iter()
            .map(|pkg| (pkg.note_ref.created_at, pkg.note_ref.key.as_u64()))
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![
                (1_001, 50),
                (1_000, 10),
                (1_000, 20),
                (1_000, 30),
                (999, 40)
            ],
            "expected newest-first order with NoteKey tie-breaks inside equal timestamps"
        );
    }

    /// Verifies older backfills do not perturb the latest timestamp after startup catch-up.
    #[test]
    fn newest_timestamp_stays_on_latest_message_after_older_backfills() {
        let mut store = MessageStore::new();

        assert!(store.insert(note_pkg(90, 1_200, [0x90; 32])));
        assert_eq!(store.newest_timestamp(), Some(1_200));

        assert!(store.insert(note_pkg(80, 1_100, [0x80; 32])));
        assert!(store.insert(note_pkg(70, 1_000, [0x70; 32])));

        assert_eq!(
            store.newest_timestamp(),
            Some(1_200),
            "expected historical backfill to leave the newest timestamp unchanged"
        );
        assert_eq!(
            store.latest().map(|note| note.key.as_u64()),
            Some(90),
            "expected the newest note reference to remain first after backfill"
        );
    }
}
