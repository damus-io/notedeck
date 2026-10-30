//! Plans for the writes a view is about to make, worked out off the UI
//! thread.
//!
//! A view that wants to say what a write would do before it makes it
//! ("moves 12 transactions"), or to refuse one derive wouldn't apply, asks
//! for a [`cashflow::plan::plan`] of its [`Action`] with
//! [`PlanJob::request`] and reads it with [`PlanJob::result`]. A plan
//! derives the raw ledger twice, about 90 ms each on a large journal, so it
//! runs on the host's job pool, from the raw ledger and the
//! [`DeriveInputs`] the books in hand were derived with.
//!
//! Each [`PlanKey`] holds one plan. Asking for another action under the
//! same key drops the one in hand (a running job's result is thrown away
//! when it lands), and when new books arrive every plan is made again
//! with them, so a result always describes the books on screen.
//!
//! [`PlanJob::update`] runs once a frame from the app's `render`, after
//! the views have asked: it takes finished plans and starts wanted ones.
//! A quiet frame looks at each key and allocates nothing.

use std::sync::Arc;

use cashflow::DeriveInputs;
use cashflow::plan::{self, Plan};
use cashflow::triage::{Action, WriteError};
use ledgerdb::Ledger;
use notedeck::JobPool;
use tokio::sync::oneshot;

/// Who a plan is for: each holds one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanKey {
    /// The triage inbox's "Categorize all…", before its Apply.
    Inbox,
}

/// What a plan is made from: the books in hand's raw ledger and edits.
#[derive(Clone, Copy)]
pub struct PlanInputs<'a> {
    /// The raw ledger the books were derived from.
    pub raw: &'a Arc<Ledger>,
    /// The edits they were derived with.
    pub inputs: &'a Arc<DeriveInputs>,
    /// The books' generation: a plan made with older books is made again.
    pub generation: u64,
}

/// A finished plan: what the action would move, or its refusal.
pub type PlanResult = Result<Plan, WriteError>;

/// One key's plan.
struct Slot {
    key: PlanKey,
    action: Action,
    /// The books' generation the plan running or in hand was made with.
    generation: u64,
    state: State,
}

/// Where a [`Slot`]'s plan is.
enum State {
    /// Asked for, not started: waiting for the raw ledger, or for new
    /// books to replan with.
    Wanted,
    /// Running on the job pool.
    Running(oneshot::Receiver<PlanResult>),
    /// In hand.
    Done(PlanResult),
}

/// The plans the views asked for. See the module docs.
#[derive(Default)]
pub struct PlanJob {
    /// One per key asked for; there are only a few keys.
    slots: Vec<Slot>,
}

impl PlanJob {
    /// Plan `action` for `key`, unless that is what `key` already plans.
    /// Call it when the action is picked, not every frame: the action is
    /// moved in.
    pub fn request(&mut self, key: PlanKey, action: Action) {
        if let Some(slot) = self.slot_mut(key) {
            if slot.action != action {
                slot.action = action;
                slot.state = State::Wanted;
            }
            return;
        }
        self.slots.push(Slot {
            key,
            action,
            generation: 0,
            state: State::Wanted,
        });
    }

    /// Forget `key`'s plan; a running job's result is dropped when it
    /// lands.
    pub fn cancel(&mut self, key: PlanKey) {
        self.slots.retain(|slot| slot.key != key);
    }

    /// `key`'s plan, once it is made with the books in hand.
    pub fn result(&self, key: PlanKey) -> Option<&PlanResult> {
        let slot = self.slots.iter().find(|slot| slot.key == key)?;
        match &slot.state {
            State::Done(result) => Some(result),
            State::Wanted | State::Running(_) => None,
        }
    }

    /// Whether any plan asked for isn't in hand yet.
    pub fn is_busy(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| !matches!(slot.state, State::Done(_)))
    }

    /// One frame's worth of work: take the plans that finished, replan
    /// the ones made with older books, and start the wanted ones on `pool`
    /// when `inputs` gives the raw ledger. Returns whether any is still
    /// running or waiting, so the caller polls again soon.
    #[profiling::function]
    pub fn update(&mut self, pool: &JobPool, inputs: Option<PlanInputs<'_>>) -> bool {
        let generation = inputs.map(|i| i.generation);
        for slot in &mut self.slots {
            slot.take_finished();
            let stale = generation.is_some_and(|g| g != slot.generation);
            if stale && matches!(slot.state, State::Done(_)) {
                slot.state = State::Wanted;
            }
            if let (State::Wanted, Some(inputs)) = (&slot.state, inputs) {
                slot.start(pool, inputs);
            }
        }
        self.is_busy()
    }

    fn slot_mut(&mut self, key: PlanKey) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|slot| slot.key == key)
    }
}

impl Slot {
    /// Take the running plan's result if it has finished. A job the pool
    /// dropped (shutting down) is wanted again.
    fn take_finished(&mut self) {
        let State::Running(rx) = &mut self.state else {
            return;
        };
        self.state = match rx.try_recv() {
            Ok(result) => State::Done(result),
            Err(oneshot::error::TryRecvError::Empty) => return,
            Err(oneshot::error::TryRecvError::Closed) => State::Wanted,
        };
    }

    /// Start planning this slot's action from `inputs` on `pool`.
    fn start(&mut self, pool: &JobPool, inputs: PlanInputs<'_>) {
        let raw = inputs.raw.clone();
        let derive = inputs.inputs.clone();
        let action = self.action.clone();
        let rx = pool.schedule_receivable(move || plan::plan(&raw, &derive, &action));
        self.generation = inputs.generation;
        self.state = State::Running(rx);
    }
}
