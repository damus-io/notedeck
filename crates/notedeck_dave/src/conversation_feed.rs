//! The shared kind-1988 conversation subscription, and how far it has
//! delivered.
//!
//! A session's notes reach the host two ways: as **history**, folded in one
//! go when the session is installed (startup restore, relay discovery, a
//! resume), and **live**, handed over one poll at a time by the shared
//! subscription, where [`process_conversation_notes`] runs the side effects a
//! note implies (dispatching a remote user message, auto-accepting a
//! permission, advancing compaction). Every note must be exactly one of the
//! two:
//! - a note folded as history *and* delivered by the poll is skipped as
//!   already seen, so its side effects never run: a phone's message shows but
//!   is never dispatched;
//! - a note the poll delivers before its session exists matches no session and
//!   is dropped; if the history it's then installed from predates it, it is
//!   neither.
//!
//! [`ConversationFeed`] draws the line between them. nostrdb keys notes in the
//! order it stores them and a subscription hands its notes over in that
//! order, so [`polled_through`](ConversationFeed::polled_through), the highest
//! key polled, splits the store: every conversation note at or below it has
//! been delivered (or predates the subscription and never will be), and every
//! one above it is still to come. An install that folds only up to there
//! claims nothing the poll will deliver later
//! ([`Dave::load_session_history`]).
//!
//! The background restore reads its history from an older snapshot, so the
//! poll can deliver a restored session's notes before the session exists
//! here. While a restore is in flight the feed keeps those
//! ([`record_drop`](ConversationFeed::record_drop)), and the restore installs
//! only the history before the first of them and hands them to
//! [`process_conversation_notes`] as if polled now (see
//! `Dave::drain_session_restore`).
//!
//! [`process_conversation_notes`]: crate::conversation::process_conversation_notes

use nostrdb::{NoteKey, Subscription, Transaction};
use std::collections::HashMap;

use crate::{session_loader, Dave};

/// The shared per-account subscription for kind-1988 conversation events,
/// with its delivery cursor. See the [module docs](self).
pub(crate) struct ConversationFeed {
    /// Every kind-1988 note authored by the account. Notes are demuxed by
    /// their `d`-tag to the owning session in
    /// `Dave::poll_remote_conversation_events`, so the number of live sessions
    /// is not bounded by nostrdb's per-db subscription cap.
    pub(crate) sub: Subscription,
    /// The highest key the subscription has handed over; `None` until it has
    /// handed over anything.
    polled_through: Option<NoteKey>,
    /// Notes the poll delivered for a session not materialized here, by `d`
    /// tag, in key order. `Some` only while a background restore is in
    /// flight: only that install is read from a snapshot older than the poll,
    /// and only it replays what the poll dropped.
    restore_drops: Option<HashMap<String, Vec<NoteKey>>>,
}

impl ConversationFeed {
    /// Wrap a freshly created conversation subscription, which has delivered
    /// nothing yet.
    pub(crate) fn new(sub: Subscription) -> Self {
        Self {
            sub,
            polled_through: None,
            restore_drops: None,
        }
    }

    /// The highest key the subscription has handed over, or `None` if it has
    /// handed over nothing yet.
    pub(crate) fn polled_through(&self) -> Option<NoteKey> {
        self.polled_through
    }

    /// Advance the cursor past a note the poll just handed over.
    pub(crate) fn mark_polled(&mut self, key: NoteKey) {
        self.polled_through = self.polled_through.max(Some(key));
    }

    /// Start keeping the notes the poll drops, for a background restore that
    /// is about to stream in.
    pub(crate) fn begin_restore(&mut self) {
        self.restore_drops.get_or_insert_with(HashMap::new);
    }

    /// The background restore is done: nothing is left to replay the drops
    /// into, so stop keeping them.
    pub(crate) fn end_restore(&mut self) {
        self.restore_drops = None;
    }

    /// Keep a note the poll delivered for `dtag`, which no session here owns,
    /// if a background restore may still install that session.
    pub(crate) fn record_drop(&mut self, dtag: &str, key: NoteKey) {
        let Some(drops) = &mut self.restore_drops else {
            return;
        };
        drops.entry(dtag.to_owned()).or_default().push(key);
    }

    /// Take the notes the poll dropped for `dtag`, in the order it delivered
    /// them (ascending key).
    pub(crate) fn take_drops(&mut self, dtag: &str) -> Vec<NoteKey> {
        self.restore_drops
            .as_mut()
            .and_then(|drops| drops.remove(dtag))
            .unwrap_or_default()
    }
}

/// Where a background-restored session's history ends: every note of the
/// session at or below the bound is history, and every one above it goes
/// through the poll (or the replay of what the poll dropped).
///
/// `first_drop` is the first note the poll delivered for the session before it
/// existed. The session's notes the subscription delivers come in key order,
/// and it delivered all of them while the session didn't exist, so every note
/// before the first drop predates the subscription and is history. With no
/// drops, everything the poll has passed is history. `None` means nothing has
/// been polled at all, so nothing can have been dropped either: the whole
/// snapshot is history.
pub(crate) fn restored_history_bound(
    polled_through: Option<NoteKey>,
    first_drop: Option<NoteKey>,
) -> Option<NoteKey> {
    match first_drop {
        Some(first) => Some(NoteKey::new(first.as_u64().saturating_sub(1))),
        None => polled_through,
    }
}

impl Dave {
    /// Fold a session's history for an install on the render thread (relay
    /// discovery, a resume, a reopen): only the notes the conversation poll
    /// has already passed, so one it has yet to deliver comes through it and
    /// is processed, rather than claimed as history and skipped.
    ///
    /// Before the poll has handed over anything there's no cursor, and the
    /// whole store is folded.
    pub(crate) fn load_session_history(
        &self,
        ndb: &nostrdb::Ndb,
        txn: &Transaction,
        account: &nostrdb_net::Pubkey,
        claude_sid: &str,
    ) -> session_loader::LoadedSession {
        let through = self
            .conversation_feed
            .as_ref()
            .and_then(ConversationFeed::polled_through);
        match through {
            Some(through) => session_loader::load_session_messages_through(
                ndb, txn, account, claude_sid, through,
            ),
            None => session_loader::load_session_messages_for_author(ndb, txn, account, claude_sid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: u64) -> NoteKey {
        NoteKey::new(k)
    }

    /// History ends just before the first dropped note, whatever the poll has
    /// passed since; with no drops it ends at the cursor.
    #[test]
    fn restored_history_ends_before_the_first_drop() {
        assert_eq!(
            restored_history_bound(Some(key(50)), Some(key(20))),
            Some(key(19))
        );
        assert_eq!(restored_history_bound(Some(key(50)), None), Some(key(50)));
        assert_eq!(restored_history_bound(None, None), None);
    }

    /// Drops are only kept while a restore is in flight, and handed back once.
    #[test]
    fn drops_are_kept_only_while_restoring() {
        let mut feed = ConversationFeed::new(Subscription::new(1));
        feed.record_drop("s", key(1));
        feed.begin_restore();
        assert!(
            feed.take_drops("s").is_empty(),
            "nothing is kept outside a restore"
        );

        feed.record_drop("s", key(2));
        feed.record_drop("s", key(3));
        assert_eq!(feed.take_drops("s"), [key(2), key(3)]);
        assert!(feed.take_drops("s").is_empty(), "handed back once");

        feed.record_drop("s", key(4));
        feed.end_restore();
        assert!(feed.take_drops("s").is_empty(), "dropped with the restore");
    }

    /// The cursor only moves forward.
    #[test]
    fn cursor_is_a_high_water_mark() {
        let mut feed = ConversationFeed::new(Subscription::new(1));
        assert_eq!(feed.polled_through(), None);
        feed.mark_polled(key(7));
        feed.mark_polled(key(3));
        assert_eq!(feed.polled_through(), Some(key(7)));
    }
}
