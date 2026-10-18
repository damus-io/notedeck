//! A session's kind-1988 note bookkeeping (headway:notedeck/hammer-evolve-knee,
//! headway:notedeck/cushion-audit-embark).
//!
//! Every reader and writer of a session's notes goes through [`NoteLedger`],
//! which keeps two questions apart:
//! - **own**: did this host publish the note ([`NoteLedger::record_own`])? Its
//!   side effects ran when it was published, so the poll skips its echo and the
//!   outbound fan-out has already sent its envelope;
//! - **delivered**: has the note been processed, because the poll handed it
//!   over ([`NoteLedger::deliver`]) or a restore installed it as history
//!   ([`NoteLedger::install_history`])?
//!
//! A rebuild answers neither. It folds what the poll has passed, and the one
//! thing it records is which of this host's notes it couldn't show
//! ([`NoteLedger::unshow_unfolded`]), so the poll shows them on arrival. A note
//! the rebuild folds before the poll reaches it is still undelivered, and the
//! poll still processes it.
//!
//! Beside those, the ledger holds the host's notes nostrdb hasn't handed back
//! yet ([`UnindexedNotes`]), which the reconcile at rest waits on, and how far
//! the poll has got ([`NoteLedger::through`]), which caps what a rebuild folds.

use crate::reconcile::{Stall, UnindexedNotes};
use agentium_core::session_loader::LoadedSession;
use nostrdb::{Note, NoteKey};
use std::collections::HashSet;
use std::time::Instant;

/// A note the conversation poll handed a session: where nostrdb stored it,
/// and its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Delivered {
    /// Where nostrdb stored the note.
    pub key: NoteKey,
    /// The note's id.
    pub id: [u8; 32],
}

impl Delivered {
    /// The delivery record for a note read from nostrdb. `None` for a note
    /// that has no key, which can't have come through the poll.
    pub fn of(note: &Note) -> Option<Self> {
        Some(Delivered {
            key: note.key()?,
            id: *note.id(),
        })
    }
}

/// What [`NoteLedger::deliver`] made of a note the poll handed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Not processed before: the poll processes it.
    New,
    /// The echo of a note this host published. Its side effects already ran
    /// when it was published, so the poll skips it.
    Own,
    /// Delivered before, or installed as history: the poll skips it.
    Again,
}

/// Which of a session's notes this host published, which have been processed,
/// which of its own are still to come back from nostrdb, and how far the poll
/// has got.
#[derive(Debug, Default)]
pub(crate) struct NoteLedger {
    /// Notes this host published, for the life of the session. A restored
    /// session starts with none: what it published before is history.
    own: HashSet<[u8; 32]>,
    /// Notes this host published that nostrdb hasn't handed back through the
    /// conversation subscription yet. Always a subset of `own`.
    unindexed: UnindexedNotes,
    /// Own notes the last rebuild couldn't fold, because nostrdb hadn't stored
    /// them: the poll treats their arrival as new, so it shows them.
    unshown_own: HashSet<[u8; 32]>,
    /// Notes the poll handed over, or a restore installed as history.
    delivered: HashSet<[u8; 32]>,
    /// Newest note the conversation poll has handed this session, or a
    /// restore installed into its chat.
    ///
    /// nostrdb delivers notes in the order it stores them, and a restore
    /// loads only notes the poll has already passed, so every note of this
    /// session stored at or before this key has been through the poll or that
    /// restore. A rebuild folds only up to here: a note stored since is still
    /// on its way through the poll, which runs its side effects (a remote user
    /// message is dispatched and fanned out from there). `None` until the
    /// first note arrives.
    through: Option<NoteKey>,
}

impl NoteLedger {
    /// Record a note this host just published at `now`: own, so its echo
    /// through the poll isn't processed again, and held as unindexed until
    /// that echo arrives.
    pub fn record_own(&mut self, id: [u8; 32], now: Instant) {
        self.own.insert(id);
        self.unindexed.insert(id, now);
    }

    /// Take a note the poll handed over, and say whether the poll should
    /// process it.
    ///
    /// Whatever it is, its arrival means nostrdb has indexed it and the poll
    /// has passed it, so it is delivered and a rebuild may fold it. An own
    /// note the last rebuild couldn't show
    /// ([`unshow_unfolded`](Self::unshow_unfolded)) arrives as
    /// [`Delivery::New`], so the poll shows it.
    pub fn deliver(&mut self, note: Delivered) -> Delivery {
        self.unindexed.remove(&note.id);
        self.through = self.through.max(Some(note.key));
        let first = self.delivered.insert(note.id);
        if self.own.contains(&note.id) && !self.unshown_own.remove(&note.id) {
            Delivery::Own
        } else if first {
            Delivery::New
        } else {
            Delivery::Again
        }
    }

    /// Whether this host published the note, so the outbound fan-out has
    /// already sent its envelope.
    pub fn is_own(&self, id: &[u8; 32]) -> bool {
        self.own.contains(id)
    }

    /// Every note this host published, for the rebuild's cap exception: one
    /// stored past [`through`](Self::through) is folded anyway when it's in
    /// here, since it has no side effect left for the poll to run.
    pub fn own(&self) -> &HashSet<[u8; 32]> {
        &self.own
    }

    /// Notes the poll handed over or a restore installed.
    #[cfg(test)]
    pub fn delivered(&self) -> &HashSet<[u8; 32]> {
        &self.delivered
    }

    /// Note which of this host's notes nostrdb hasn't handed back are among
    /// `folded`, what a rebuild just showed: one that isn't, the poll shows
    /// when it arrives; one that is, the poll skips as its own.
    pub fn unshow_unfolded(&mut self, folded: &HashSet<[u8; 32]>) {
        for id in self.unindexed.ids() {
            if folded.contains(id) {
                self.unshown_own.remove(id);
            } else {
                self.unshown_own.insert(*id);
            }
        }
    }

    /// Install what a restore loaded as the session's history: its notes are
    /// delivered, so the poll skips them, and [`through`](Self::through)
    /// covers them. A restore loads only notes the poll has passed (see
    /// [`ConversationFeed`](crate::conversation_feed::ConversationFeed)), so
    /// none of them is still to come through the poll. A rebuild never calls
    /// this.
    pub fn install_history(&mut self, loaded: &LoadedSession) {
        self.delivered.extend(loaded.note_ids.iter().copied());
        self.through = self.through.max(loaded.max_key);
    }

    /// Every note this host published has been handed back by nostrdb.
    pub fn all_indexed(&self) -> bool {
        self.unindexed.is_empty()
    }

    /// How many notes this host published that nostrdb hasn't handed back.
    #[cfg(test)]
    pub fn unindexed_count(&self) -> usize {
        self.unindexed.ids().count()
    }

    /// The unindexed notes, once the oldest has waited too long. Reported
    /// once per stall (see [`UnindexedNotes::take_stall`]).
    pub fn take_stall(&mut self, now: Instant) -> Option<Stall> {
        self.unindexed.take_stall(now)
    }

    /// Newest note the poll has handed over or a restore has installed.
    /// `None` until the first.
    pub fn through(&self) -> Option<NoteKey> {
        self.through
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivered(key: u64, id: u8) -> Delivered {
        Delivered {
            key: NoteKey::new(key),
            id: [id; 32],
        }
    }

    /// A note never processed is new, an own note's echo is own however often
    /// it comes, and any other note delivered before is again; every delivery
    /// advances `through`, never backwards.
    #[test]
    fn deliver_says_new_own_or_again() {
        let mut ledger = NoteLedger::default();
        ledger.record_own([1; 32], Instant::now());

        assert_eq!(ledger.deliver(delivered(5, 2)), Delivery::New);
        assert_eq!(ledger.deliver(delivered(3, 1)), Delivery::Own);
        assert!(ledger.all_indexed(), "the echo released the own note");
        assert_eq!(ledger.through(), Some(NoteKey::new(5)), "never backwards");

        assert_eq!(ledger.deliver(delivered(5, 2)), Delivery::Again);
        assert_eq!(ledger.deliver(delivered(3, 1)), Delivery::Own);
        assert_eq!(ledger.deliver(delivered(7, 3)), Delivery::New);
        assert_eq!(ledger.through(), Some(NoteKey::new(7)));
        assert_eq!(
            *ledger.delivered(),
            HashSet::from([[1; 32], [2; 32], [3; 32]])
        );
    }

    /// An own note a rebuild couldn't fold is new on arrival, so the poll
    /// shows it; one it folded is own. Neither is delivered by the rebuild,
    /// and a later rebuild that folds the first takes it back off the unshown.
    #[test]
    fn an_unfolded_own_note_is_new_on_arrival() {
        let mut ledger = NoteLedger::default();
        let now = Instant::now();
        ledger.record_own([1; 32], now);
        ledger.record_own([2; 32], now);
        ledger.record_own([3; 32], now);

        ledger.unshow_unfolded(&HashSet::from([[2; 32]]));
        assert!(ledger.is_own(&[1; 32]), "still the host's own");
        assert!(ledger.delivered().is_empty(), "the rebuild claims nothing");
        ledger.unshow_unfolded(&HashSet::from([[2; 32], [3; 32]]));

        assert_eq!(ledger.deliver(delivered(1, 1)), Delivery::New);
        assert_eq!(ledger.deliver(delivered(2, 2)), Delivery::Own);
        assert_eq!(ledger.deliver(delivered(3, 3)), Delivery::Own);
        assert_eq!(ledger.through(), Some(NoteKey::new(3)));
    }

    /// A restore's history is delivered and moves `through`; none of it is
    /// own, so its notes would be fanned out by a poll that met them.
    #[test]
    fn installed_history_is_delivered_not_own() {
        let mut ledger = NoteLedger::default();
        let loaded = LoadedSession {
            messages: Vec::new(),
            orders: Vec::new(),
            root_note_id: None,
            last_note_id: None,
            permissions: crate::session::PermissionTracker::new(),
            note_ids: HashSet::from([[4; 32]]),
            max_order: None,
            max_key: Some(NoteKey::new(9)),
            turns: Vec::new(),
        };
        ledger.install_history(&loaded);

        assert_eq!(ledger.through(), Some(NoteKey::new(9)));
        assert!(!ledger.is_own(&[4; 32]));
        assert_eq!(ledger.deliver(delivered(9, 4)), Delivery::Again);
    }
}
