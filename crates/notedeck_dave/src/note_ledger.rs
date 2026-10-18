//! A session's kind-1988 note bookkeeping (headway:notedeck/hammer-evolve-knee).
//!
//! Three things track a session's notes, and every reader and writer of them
//! goes through [`NoteLedger`]:
//! - which notes are marked seen, so the conversation poll skips them;
//! - which notes this host published that nostrdb hasn't handed back yet
//!   ([`UnindexedNotes`]), which the reconcile at rest waits on;
//! - the newest note the poll has handed over or a load has claimed
//!   ([`NoteLedger::through`]), which caps what a rebuild folds.
//!
//! A note gets marked seen three ways: this host published it
//! ([`NoteLedger::record_own`]), the poll delivered it
//! ([`NoteLedger::deliver`]), or a rebuild or restore loaded it as history
//! ([`NoteLedger::claim`]). The seen set doesn't yet say which, so a reader
//! asking one of those questions gets the union.

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
    /// Not seen before: the poll processes it.
    New,
    /// The echo of a note this host published and still has marked seen. Its
    /// side effects already ran when it was published, so the poll skips it.
    Own,
    /// Already seen, delivered again or loaded as history: the poll skips it.
    Again,
}

/// Which of a session's notes are seen, which of its own are still to come
/// back from nostrdb, and how far the poll has got.
#[derive(Debug, Default)]
pub(crate) struct NoteLedger {
    /// Notes the poll skips: every note this host published, the poll
    /// delivered, or a load claimed as history.
    seen: HashSet<[u8; 32]>,
    /// Notes this host published that nostrdb hasn't handed back through the
    /// conversation subscription yet.
    unindexed: UnindexedNotes,
    /// Newest note the conversation poll has handed this session, or a load
    /// claimed into its chat.
    ///
    /// nostrdb delivers notes in the order it stores them, and a restore
    /// loads only notes the poll has already passed, so every note of this
    /// session stored at or before this key has been through the poll or that
    /// restore. A rebuild folds only up to here: a note stored since is still
    /// on its way through the poll, which must see it unseen to run its side
    /// effects (a remote user message is dispatched and fanned out from
    /// there). `None` until the first note arrives.
    through: Option<NoteKey>,
}

impl NoteLedger {
    /// Record a note this host just published at `now`: marked seen, so its
    /// echo through the poll isn't processed again, and held as unindexed
    /// until that echo arrives.
    pub fn record_own(&mut self, id: [u8; 32], now: Instant) {
        self.seen.insert(id);
        self.unindexed.insert(id, now);
    }

    /// Take a note the poll handed over, and say whether the poll should
    /// process it.
    ///
    /// Seen or not, its arrival means nostrdb has indexed it, and that the
    /// poll has passed it, so a rebuild may fold it. A note of this host's
    /// that a rebuild unmarked ([`unshow_unfolded`](Self::unshow_unfolded))
    /// is [`Delivery::New`], so the poll shows it.
    pub fn deliver(&mut self, note: Delivered) -> Delivery {
        let own = self.unindexed.remove(&note.id);
        self.through = self.through.max(Some(note.key));
        if self.seen.insert(note.id) {
            Delivery::New
        } else if own {
            Delivery::Own
        } else {
            Delivery::Again
        }
    }

    /// Whether this host published the note, so the outbound fan-out has
    /// already sent its envelope. For now this answers from every seen note,
    /// own or not.
    pub fn is_own(&self, id: &[u8; 32]) -> bool {
        self.seen.contains(id)
    }

    /// Every note marked seen, for the rebuild's cap exception: one stored
    /// past [`through`](Self::through) is folded anyway when it's in here.
    pub fn seen_set(&self) -> &HashSet<[u8; 32]> {
        &self.seen
    }

    /// Unmark each note this host published that nostrdb hasn't stored yet
    /// and so isn't among `folded`: a rebuild can't show it, so the poll
    /// treats it as new when it arrives and shows it then.
    pub fn unshow_unfolded(&mut self, folded: &HashSet<[u8; 32]>) {
        for id in self.unindexed.ids() {
            if !folded.contains(id) {
                self.seen.remove(id);
            }
        }
    }

    /// Claim what a rebuild or restore loaded into the chat: its notes are
    /// marked seen, and [`through`](Self::through) covers them.
    pub fn claim(&mut self, loaded: &LoadedSession) {
        self.seen.extend(loaded.note_ids.iter().copied());
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

    /// Newest note the poll has handed over or a load has claimed. `None`
    /// until the first.
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

    /// An unseen note is new, an own note's echo is own, and any note seen
    /// before is again; every delivery advances `through`, never backwards.
    #[test]
    fn deliver_says_new_own_or_again() {
        let mut ledger = NoteLedger::default();
        ledger.record_own([1; 32], Instant::now());

        assert_eq!(ledger.deliver(delivered(5, 2)), Delivery::New);
        assert_eq!(ledger.deliver(delivered(3, 1)), Delivery::Own);
        assert!(ledger.all_indexed(), "the echo released the own note");
        assert_eq!(ledger.through(), Some(NoteKey::new(5)), "never backwards");

        assert_eq!(ledger.deliver(delivered(5, 2)), Delivery::Again);
        assert_eq!(
            ledger.deliver(delivered(3, 1)),
            Delivery::Again,
            "an own note's second echo"
        );
        assert_eq!(ledger.deliver(delivered(7, 3)), Delivery::New);
        assert_eq!(ledger.through(), Some(NoteKey::new(7)));
    }

    /// An own note a rebuild couldn't fold is unmarked, so its echo is new;
    /// one it folded stays marked.
    #[test]
    fn an_unfolded_own_note_is_new_on_arrival() {
        let mut ledger = NoteLedger::default();
        let now = Instant::now();
        ledger.record_own([1; 32], now);
        ledger.record_own([2; 32], now);

        ledger.unshow_unfolded(&HashSet::from([[2; 32]]));

        assert!(!ledger.is_own(&[1; 32]));
        assert_eq!(ledger.deliver(delivered(1, 1)), Delivery::New);
        assert_eq!(ledger.deliver(delivered(2, 2)), Delivery::Own);
    }
}
