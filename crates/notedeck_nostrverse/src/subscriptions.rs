//! Local nostrdb subscription management for nostrverse rooms.
//!
//! Subscribes to room events (kind 37555) in the local nostrdb and
//! polls for updates each frame. No remote relay subscriptions — rooms
//! are local-only for now.

use nostrdb::{Filter, Ndb, Note, Subscription, Transaction};

use crate::kinds;

/// Manages a local nostrdb subscription for room events.
pub struct RoomSubscription {
    /// Local nostrdb subscription handle
    sub: Subscription,
}

impl RoomSubscription {
    /// Subscribe to all room events (kind 37555) in the local nostrdb.
    pub fn new(ndb: &Ndb) -> Self {
        let filter = Filter::new().kinds([kinds::ROOM as u64]).build();
        let sub = ndb.subscribe(&[filter]).expect("room subscription");
        Self { sub }
    }

    /// Subscribe to room events from a specific author.
    #[allow(dead_code)]
    pub fn for_author(ndb: &Ndb, author: &[u8; 32]) -> Self {
        let filter = Filter::new()
            .kinds([kinds::ROOM as u64])
            .authors([author])
            .build();
        let sub = ndb.subscribe(&[filter]).expect("room subscription");
        Self { sub }
    }

    /// Poll for new room events. Returns parsed notes.
    pub fn poll<'a>(&self, ndb: &'a Ndb, txn: &'a Transaction) -> Vec<Note<'a>> {
        let note_keys = ndb.poll_for_notes(self.sub, 50);
        note_keys
            .into_iter()
            .filter_map(|nk| ndb.get_note_by_key(txn, nk).ok())
            .collect()
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
    sub: Subscription,
}

impl PresenceSubscription {
    /// Subscribe to presence events in the local nostrdb.
    pub fn new(ndb: &Ndb) -> Self {
        let filter = Filter::new().kinds([kinds::PRESENCE as u64]).build();
        let sub = ndb.subscribe(&[filter]).expect("presence subscription");
        Self { sub }
    }

    /// Poll for new presence events.
    pub fn poll<'a>(&self, ndb: &'a Ndb, txn: &'a Transaction) -> Vec<Note<'a>> {
        let note_keys = ndb.poll_for_notes(self.sub, 50);
        note_keys
            .into_iter()
            .filter_map(|nk| ndb.get_note_by_key(txn, nk).ok())
            .collect()
    }
}
