//! Local nostrdb subscription management for nostrverse rooms.
//!
//! Subscribes to room events (kind 37555) and presence events (kind 10555)
//! in the local nostrdb and polls for updates each frame.

use nostrdb::{Filter, Ndb, Note, Subscription, Transaction};

use crate::kinds;

/// A local nostrdb subscription that polls for notes of a given kind.
struct KindSubscription {
    sub: Subscription,
}

impl KindSubscription {
    fn new(ndb: &Ndb, kind: u16) -> Self {
        let filter = Filter::new().kinds([kind as u64]).build();
        let sub = ndb.subscribe(&[filter]).expect("kind subscription");
        Self { sub }
    }

    fn poll<'a>(&self, ndb: &'a Ndb, txn: &'a Transaction) -> Vec<Note<'a>> {
        ndb.poll_for_notes(self.sub, 50)
            .into_iter()
            .filter_map(|nk| ndb.get_note_by_key(txn, nk).ok())
            .collect()
    }
}

/// Manages a local nostrdb subscription for room events.
pub struct RoomSubscription {
    inner: KindSubscription,
}

impl RoomSubscription {
    /// Subscribe to all room events (kind 37555) in the local nostrdb.
    pub fn new(ndb: &Ndb) -> Self {
        Self {
            inner: KindSubscription::new(ndb, kinds::ROOM),
        }
    }

    /// Subscribe to room events from a specific author.
    #[allow(dead_code)]
    pub fn for_author(ndb: &Ndb, author: &[u8; 32]) -> Self {
        let filter = Filter::new()
            .kinds([kinds::ROOM as u64])
            .authors([author])
            .build();
        let sub = ndb.subscribe(&[filter]).expect("room subscription");
        Self {
            inner: KindSubscription { sub },
        }
    }

    /// Poll for new room events. Returns parsed notes.
    pub fn poll<'a>(&self, ndb: &'a Ndb, txn: &'a Transaction) -> Vec<Note<'a>> {
        self.inner.poll(ndb, txn)
    }

    /// Query for existing room events (e.g. on startup).
    pub fn query_existing<'a>(ndb: &'a Ndb, txn: &'a Transaction) -> Vec<Note<'a>> {
        let filter = Filter::new().kinds([kinds::ROOM as u64]).limit(50).build();
        ndb.query(txn, &[filter], 50)
            .unwrap_or_default()
            .into_iter()
            .map(|qr| qr.note)
            .collect()
    }
}

/// Manages a local nostrdb subscription for presence events (kind 10555).
pub struct PresenceSubscription {
    inner: KindSubscription,
}

impl PresenceSubscription {
    /// Subscribe to presence events in the local nostrdb.
    pub fn new(ndb: &Ndb) -> Self {
        Self {
            inner: KindSubscription::new(ndb, kinds::PRESENCE),
        }
    }

    /// Poll for new presence events.
    pub fn poll<'a>(&self, ndb: &'a Ndb, txn: &'a Transaction) -> Vec<Note<'a>> {
        self.inner.poll(ndb, txn)
    }
}
