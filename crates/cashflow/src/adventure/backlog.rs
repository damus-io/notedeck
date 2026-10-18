//! The triage backlog through time: how many transactions were waiting to
//! be categorized on each day, worked out from the books alone.
//!
//! A transaction joins the backlog the day its segment was imported and
//! leaves it the day the vault note that now decides it (an override,
//! split, link or vault rule, [`DeriveInputs::written_at`]) was written,
//! when that is after the import. One still on the importer's default
//! ([`Why::Default`]) never leaves. One a note decided before it arrived,
//! or that the importer or a rules file categorized, never joins. Days are
//! dated in the synced zone, so every machine with the same journal and
//! notes sees the same backlog.
//!
//! Only the note deciding a transaction now is known, so a transaction
//! recategorized later leaves the backlog on the later note's day: the
//! backlog reads as it would have, had that note been the first.
//!
//! The weekly triage quest reads what left in a week and what was still
//! waiting at its end, and the inbox-zero achievement reads the first day
//! nothing was.

use std::collections::BTreeMap;

use ledgerdb::{Date, TxnId, Why};

use crate::zone::Zone;
use crate::{Books, DeriveInputs};

/// One day the backlog changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BacklogDay {
    /// The day.
    pub day: Date,
    /// Transactions triaged out of the backlog that day.
    pub triaged: u32,
    /// What was still waiting once the day was over.
    pub left: u32,
}

/// The backlog's changes, oldest first.
#[derive(Clone, Debug, Default)]
pub(super) struct Backlog {
    days: Vec<BacklogDay>,
}

/// One day's arrivals and departures while they are added up.
#[derive(Clone, Copy, Default)]
struct Moves {
    arrived: u32,
    triaged: u32,
}

impl Backlog {
    /// The backlog of `books`, derived with `derive`, dated in `zone`.
    #[profiling::function]
    pub fn gather(books: &Books, derive: &DeriveInputs, zone: Zone) -> Backlog {
        let ledger = books.ledger();
        let mut moves: BTreeMap<Date, Moves> = BTreeMap::new();
        for i in 0..ledger.txns().len() {
            let txn = TxnId(i as u32);
            let why = books.derived.why(txn);
            let imported = ledger.segment_of(txn).header.imported;
            let decided = derive.written_at(why);
            let waited = why == Why::Default || decided.is_some_and(|at| at > imported);
            if !waited {
                continue;
            }
            moves.entry(zone.date(imported)).or_default().arrived += 1;
            if let Some(at) = decided {
                moves.entry(zone.date(at)).or_default().triaged += 1;
            }
        }
        let mut left = 0u32;
        let days = moves
            .into_iter()
            .map(|(day, m)| {
                left = (left + m.arrived).saturating_sub(m.triaged);
                BacklogDay {
                    day,
                    triaged: m.triaged,
                    left,
                }
            })
            .collect();
        Backlog { days }
    }

    /// The days the backlog changed on in `from..until`.
    pub fn days_in(&self, from: Date, until: Date) -> &[BacklogDay] {
        let start = self.days.partition_point(|d| d.day < from);
        let end = self.days.partition_point(|d| d.day < until);
        &self.days[start..end.max(start)]
    }

    /// What was waiting once `day` was over.
    pub fn left_after(&self, day: Date) -> u32 {
        let at = self.days.partition_point(|d| d.day <= day);
        at.checked_sub(1).map_or(0, |at| self.days[at].left)
    }

    /// Whether nothing ever waited.
    pub fn is_empty(&self) -> bool {
        self.days.is_empty()
    }

    /// The first day anything joined the backlog.
    pub fn first_day(&self) -> Option<Date> {
        self.days.first().map(|d| d.day)
    }

    /// The first day that ended with nothing waiting, once anything had.
    pub fn first_zero(&self) -> Option<Date> {
        self.days.iter().find(|d| d.left == 0).map(|d| d.day)
    }
}
