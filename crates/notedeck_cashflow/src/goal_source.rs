//! Where the app's goals come from: the `goal`-labelled cards on the
//! Headway boards `config.toml` names, read from the host's nostrdb.
//!
//! [`GoalSource`] is polled once a frame from `App::update`. Finding the
//! goals ([`cashflow::goals::discover`]) folds every board it reads, so it
//! runs on the host's job pool, never on the UI thread. The source
//! subscribes to headway's kinds and to key-shares, and finds the goals
//! again when one of those arrives, at most once every
//! [`REDISCOVER_EVERY`]; an account switch finds them straight away. A
//! key-share means a board was shared with the account, and reloading the
//! [`Roster`] in the job registers its root so the board's sealed edits
//! peel.
//!
//! [`crate::boards::BoardSource`] folds the boards projects link the same
//! way, for the Projects view.

use std::time::{Duration, Instant};

use cashflow::goals::{self, Goal};
use headway::boards::{Identity, Roster};
use headway::event::HEADWAY_KINDS;
use headway::teams::RootRegistry;
use nostrdb::{Filter, Ndb, Subscription};
use nostrdb_net::Pubkey;
use notedeck::AppContext;
use tokio::sync::oneshot;

/// The least time between two discoveries Headway activity starts. Agents
/// comment on cards many times a minute, and a goal's card rarely changes
/// in a way that moves the goal.
pub const REDISCOVER_EVERY: Duration = Duration::from_secs(5);

/// How many delivered keys one poll takes at most: as many as fit in
/// nostrdb's stack buffer, so a quiet poll never allocates.
const POLL_BATCH: u32 = 64;

/// A finished discovery, and the registry it borrowed.
struct Found {
    registry: RootRegistry,
    goals: Vec<Goal>,
}

/// The goals on the configured boards, kept current. See the module docs.
#[derive(Default)]
pub struct GoalSource {
    /// Every headway note and key-share, so the goals follow the boards.
    sub: Option<Subscription>,
    /// The sealed boards' roots registered with nostrdb, here between
    /// discoveries and away while one runs.
    registry: Option<RootRegistry>,
    /// The running discovery.
    job: Option<oneshot::Receiver<Found>>,
    /// The account the running discovery is for.
    job_for: Option<Pubkey>,
    /// The account the goals in hand were found for.
    found_for: Option<Pubkey>,
    /// Whether a headway note arrived since the last discovery started.
    dirty: bool,
    /// When the last discovery started.
    last_found: Option<Instant>,
}

impl GoalSource {
    /// One frame's worth of work: take a finished discovery, note any
    /// headway activity, and start the next discovery of the goals on
    /// `boards` if one is due. Returns the goals when a discovery finished.
    #[profiling::function]
    pub fn update(&mut self, ctx: &mut AppContext<'_>, boards: &[String]) -> Option<Vec<Goal>> {
        let found = self.take_job();
        self.poll(ctx.ndb);
        if self.job.is_some() {
            return found;
        }
        let account = *ctx.accounts.selected_account_pubkey();
        let stale = self.found_for != Some(account);
        let due = self.dirty
            && self
                .last_found
                .is_none_or(|at| at.elapsed() >= REDISCOVER_EVERY);
        if !stale && !due {
            return found;
        }
        self.dirty = false;
        let ndb = ctx.ndb.clone();
        let boards = boards.to_vec();
        let mut registry = self.registry.take().unwrap_or_default();
        let waker = ctx.waker.clone();
        self.job = Some(ctx.job_pool.schedule_receivable(move || {
            let roster = Roster::load(&ndb, Identity::own(account), &mut registry);
            let goals = goals::discover(&ndb, &roster, &boards);
            waker.wake();
            Found { registry, goals }
        }));
        self.job_for = Some(account);
        self.last_found = Some(Instant::now());
        found
    }

    /// Whether a discovery is running.
    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    /// Note whether any headway note arrived, subscribing on the first
    /// call.
    fn poll(&mut self, ndb: &Ndb) {
        if self.sub.is_none() {
            let kinds = HEADWAY_KINDS
                .iter()
                .map(|k| u64::from(*k))
                .chain([u64::from(nostrdb_net::sns::KEYSHARE_KIND)]);
            self.sub = ndb.subscribe(&[Filter::new().kinds(kinds).build()]).ok();
        }
        let Some(sub) = self.sub else {
            return;
        };
        loop {
            let keys = ndb.poll_for_notes(sub, POLL_BATCH);
            self.dirty |= !keys.is_empty();
            if keys.len() < POLL_BATCH as usize {
                return;
            }
        }
    }

    /// The running discovery's goals, if it has finished.
    fn take_job(&mut self) -> Option<Vec<Goal>> {
        let done = match self.job.as_mut().map(oneshot::Receiver::try_recv) {
            None | Some(Err(oneshot::error::TryRecvError::Empty)) => return None,
            Some(Ok(done)) => done,
            Some(Err(oneshot::error::TryRecvError::Closed)) => {
                // The pool dropped it (shutting down): find them again later.
                self.job = None;
                self.job_for = None;
                return None;
            }
        };
        self.job = None;
        self.registry = Some(done.registry);
        self.found_for = self.job_for.take();
        Some(done.goals)
    }
}
