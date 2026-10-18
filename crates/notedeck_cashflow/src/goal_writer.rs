//! Reports each goal's progress on its Headway card, as the account: the
//! app's half of [`cashflow::goals::writeback`].
//!
//! [`GoalWriter::update`] runs from `App::update`. When the books, the goals
//! or today change it measures every goal and [plans](writeback::plan) what
//! its card is owed, on the UI thread like the Goals view's report. The
//! writes themselves (a fold of each board, then signing and ingesting) go
//! to the host's job pool. Their notes are ingested into the host's nostrdb
//! with [`NoPublish`]: the host fans a sealed board's envelopes out to the
//! account's private relays, as it does for the Headway app's own edits.
//!
//! A write's notes take a moment to fold back into the goals (a sealed
//! board's envelope is unwrapped on nostrdb's writer thread), and the
//! next plan may run before they do. So the writer remembers what it
//! wrote this session and never plans it again, on top of the card's own
//! markers that make it idempotent across sessions.

use std::collections::HashSet;

use cashflow::Books;
use cashflow::goals::writeback::{self, Posted, Write};
use cashflow::goals::{Goal, progress};
use cashflow::projects::Projects;
use headway::store::NoPublish;
use headway::teams::RootRegistry;
use ledgerdb::Date;
use notedeck::AppContext;
use tokio::sync::oneshot;

/// What the goals were last planned against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    books: u64,
    goals: u64,
    today: Date,
}

/// What [`GoalWriter::update`] reads.
pub struct Inputs<'a> {
    /// The derived books.
    pub books: &'a Books,
    /// Bumped whenever the books or projects change.
    pub books_generation: u64,
    /// The chart's project attribution, for a goal scoped to a project.
    pub projects: &'a Projects,
    /// The goals, as found on the boards.
    pub goals: &'a [Goal],
    /// Bumped whenever the goals change.
    pub goals_generation: u64,
    /// The day the goals are measured as of.
    pub today: Date,
    /// Whether an achieved goal's card moves to done (`[goals] auto_done`).
    pub auto_done: bool,
}

/// A finished batch of writes, and the registry it borrowed.
struct Done {
    registry: RootRegistry,
}

/// Writes the goals' progress to their cards. See the module docs.
#[derive(Default)]
pub struct GoalWriter {
    /// What the last plan was made from.
    key: Option<Key>,
    /// The reports commented this session, by card.
    commented: HashSet<([u8; 32], Posted)>,
    /// The cards moved to done this session.
    moved: HashSet<[u8; 32]>,
    /// The sealed boards' roots registered with nostrdb, here between
    /// batches and away while one runs.
    registry: Option<RootRegistry>,
    /// The running batch.
    job: Option<oneshot::Receiver<Done>>,
}

impl GoalWriter {
    /// Take a finished batch, and plan and start the next if the books, the
    /// goals or today moved since the last plan. An account with no secret
    /// key writes nothing.
    #[profiling::function]
    pub fn update(&mut self, ctx: &mut AppContext<'_>, inputs: Inputs<'_>) {
        self.take_job();
        if self.job.is_some() {
            return;
        }
        let key = Key {
            books: inputs.books_generation,
            goals: inputs.goals_generation,
            today: inputs.today,
        };
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        let Some(secret) = ctx
            .accounts
            .selected_filled()
            .map(|full| full.secret_key.secret_bytes())
        else {
            return;
        };
        let writes = self.plan(&inputs);
        if writes.is_empty() {
            return;
        }
        let ndb = ctx.ndb.clone();
        let me = *ctx.accounts.selected_account_pubkey();
        let mut registry = self.registry.take().unwrap_or_default();
        self.job = Some(ctx.job_pool.schedule_receivable(move || {
            let results =
                writeback::write(&ndb, me, &secret, &mut registry, &writes, &mut NoPublish);
            for (write, result) in writes.iter().zip(results) {
                match result {
                    Ok(written) => tracing::info!(
                        "cashflow: goal {} reported {} ({written:?})",
                        write.card_ref,
                        write.posted.status.name()
                    ),
                    Err(err) => tracing::warn!("cashflow: goal write-back failed: {err}"),
                }
            }
            Done { registry }
        }));
    }

    /// What every measurable goal's card is owed that this session hasn't
    /// written yet, noted as written.
    fn plan(&mut self, inputs: &Inputs<'_>) -> Vec<Write> {
        let eval = progress::Inputs {
            books: inputs.books,
            projects: inputs.projects,
            today: inputs.today,
        };
        let commodities = &inputs.books.ledger().journal().commodities;
        let mut writes = Vec::new();
        for goal in inputs.goals {
            let Ok(spec) = &goal.spec else {
                continue;
            };
            let Ok(measured) = progress::evaluate(&eval, spec) else {
                continue;
            };
            let Some(mut write) =
                writeback::plan(goal, &measured, inputs.today, inputs.auto_done, commodities)
            else {
                continue;
            };
            if write.comment.is_some() && !self.commented.insert((write.card, write.posted)) {
                write.comment = None;
            }
            if write.done && !self.moved.insert(write.card) {
                write.done = false;
            }
            if write.comment.is_some() || write.done {
                writes.push(write);
            }
        }
        writes
    }

    /// Take the running batch's registry back once it has finished.
    fn take_job(&mut self) {
        let done = match self.job.as_mut().map(oneshot::Receiver::try_recv) {
            None | Some(Err(oneshot::error::TryRecvError::Empty)) => return,
            Some(Ok(done)) => Some(done),
            Some(Err(oneshot::error::TryRecvError::Closed)) => None,
        };
        self.job = None;
        if let Some(done) = done {
            self.registry = Some(done.registry);
        }
    }
}
