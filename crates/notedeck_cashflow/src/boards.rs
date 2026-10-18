//! The Headway boards each project's work is tracked on, and how that work
//! is moving: what the Projects view shows beside a project's revenue.
//!
//! The vault links a project to boards (`cashflow settings set
//! boards:<project> <slugs>`, [`cashflow::settings::set_project_boards`]);
//! the journal source reads the links whenever the vault changes and the
//! app hands them to its [`Boards`]. A [`BoardSource`] folds the linked
//! boards' [`BoardProgress`] off the UI thread, on the host's job pool,
//! through the `headway` core ([`cashflow::board_progress`]), and folds
//! again when the links, the account, or any Headway note moves.
//!
//! A test hands the app its links and progress instead, with
//! [`crate::CashflowApp::set_project_links`] and
//! [`crate::CashflowApp::set_board_progress`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use cashflow::board_progress::{self, BoardProgress};
use cashflow::settings::Settings;
use headway::boards::{Identity, Roster};
use headway::event::HEADWAY_KINDS;
use headway::teams::RootRegistry;
use nostrdb::{Filter, Ndb, Subscription};
use nostrdb_net::Pubkey;
use notedeck::AppContext;
use tokio::sync::oneshot;

/// How many delivered keys one poll takes at most: as many as fit in
/// nostrdb's stack buffer, so a quiet poll never allocates.
const POLL_BATCH: u32 = 64;

/// The least time between two folds Headway activity starts. Agents comment
/// on cards many times a minute, and a board's counts don't need to follow
/// every one; a change of links or account folds straight away.
const REFOLD_EVERY: Duration = Duration::from_secs(10);

/// One project's boards, owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectLink {
    /// The project, as the chart of accounts names it.
    pub project: String,
    /// Its boards' slugs, in the order they were linked.
    pub boards: Vec<String>,
}

/// The links in `settings`, owned, sorted by project.
pub fn links_of(settings: &Settings<'_>) -> Vec<ProjectLink> {
    settings
        .project_boards()
        .map(|links| ProjectLink {
            project: links.project.to_owned(),
            boards: links.boards().map(str::to_owned).collect(),
        })
        .collect()
}

/// Which boards each project links, and their progress as last folded.
#[derive(Debug, Default)]
pub struct Boards {
    /// Every linked project, sorted by project.
    links: Arc<[ProjectLink]>,
    /// Every slug [`Self::links`] names, sorted, once each.
    slugs: Vec<String>,
    /// Bumped when [`Self::links`] changes.
    links_generation: u64,
    /// The linked boards' progress, in [`Self::slugs`] order.
    progress: Vec<BoardProgress>,
    /// Bumped when the links or the progress change, so a view's cached
    /// figures know to recompute.
    generation: u64,
}

impl Boards {
    /// Every linked project, sorted by project.
    pub fn links(&self) -> &Arc<[ProjectLink]> {
        &self.links
    }

    /// The linked boards' progress as last folded.
    pub fn progress(&self) -> &[BoardProgress] {
        &self.progress
    }

    /// Bumped whenever the links or the progress change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Link projects as `links` says from now on.
    pub fn set_links(&mut self, links: Arc<[ProjectLink]>) {
        if self.links == links {
            return;
        }
        let mut slugs: Vec<String> = links.iter().flat_map(|l| l.boards.clone()).collect();
        slugs.sort_unstable();
        slugs.dedup();
        self.slugs = slugs;
        self.links = links;
        self.links_generation += 1;
        self.generation += 1;
    }

    /// Show `progress` for the linked boards from now on.
    pub fn set_progress(&mut self, progress: Vec<BoardProgress>) {
        if self.progress == progress {
            return;
        }
        self.progress = progress;
        self.generation += 1;
    }
}

/// What a fold was for: whose boards, under which links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FoldedFor {
    author: Pubkey,
    links_generation: u64,
}

/// A finished fold, and the registry it borrowed.
struct FoldDone {
    registry: RootRegistry,
    progress: Vec<BoardProgress>,
}

/// Folds the linked boards' progress off the UI thread. See the module
/// docs.
#[derive(Default)]
pub struct BoardSource {
    /// Every Headway note and key-share, so a fold follows the boards.
    sub: Option<Subscription>,
    /// The sealed boards' roots registered with nostrdb, here between
    /// folds and away while one runs.
    registry: Option<RootRegistry>,
    /// The running fold.
    job: Option<oneshot::Receiver<FoldDone>>,
    /// What the running fold is for.
    job_for: Option<FoldedFor>,
    /// What the progress in hand was folded for.
    folded_for: Option<FoldedFor>,
    /// Whether a Headway note arrived since the last fold started.
    dirty: bool,
    /// When the last fold started.
    last_fold: Option<Instant>,
}

impl BoardSource {
    /// One frame's worth of work: take a finished fold into `boards`, note
    /// any Headway activity, and start the next fold if one is due.
    #[profiling::function]
    pub fn update(&mut self, ctx: &mut AppContext<'_>, boards: &mut Boards) {
        self.take_job(boards);
        self.poll(ctx.ndb);
        if self.job.is_some() {
            return;
        }
        let wanted = FoldedFor {
            author: *ctx.accounts.selected_account_pubkey(),
            links_generation: boards.links_generation,
        };
        let stale = self.folded_for != Some(wanted);
        let due = self.dirty && self.last_fold.is_none_or(|at| at.elapsed() >= REFOLD_EVERY);
        if !stale && !due {
            return;
        }
        self.dirty = false;
        if boards.slugs.is_empty() {
            boards.set_progress(Vec::new());
            self.folded_for = Some(wanted);
            return;
        }
        let ndb = ctx.ndb.clone();
        let slugs = boards.slugs.clone();
        let mut registry = self.registry.take().unwrap_or_default();
        let waker = ctx.waker.clone();
        let author = wanted.author;
        self.job = Some(ctx.job_pool.schedule_receivable(move || {
            let roster = Roster::load(&ndb, Identity::own(author), &mut registry);
            let progress = board_progress::load(&ndb, &roster, &slugs);
            waker.wake();
            FoldDone { registry, progress }
        }));
        self.job_for = Some(wanted);
        self.last_fold = Some(Instant::now());
    }

    /// Whether a fold is running.
    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    /// Note whether any Headway note arrived, subscribing on the first
    /// call.
    fn poll(&mut self, ndb: &Ndb) {
        if self.sub.is_none() {
            let kinds = HEADWAY_KINDS
                .iter()
                .map(|k| *k as u64)
                .chain([u64::from(nostrdb_net::sns::KEYSHARE_KIND)]);
            self.sub = ndb.subscribe(&[Filter::new().kinds(kinds).build()]).ok();
            // Nothing has been folded against this subscription yet.
            self.dirty = true;
        }
        let Some(sub) = self.sub else {
            return;
        };
        loop {
            let keys = ndb.poll_for_notes(sub, POLL_BATCH);
            self.dirty |= !keys.is_empty();
            if keys.len() < POLL_BATCH as usize {
                break;
            }
        }
    }

    /// Take the running fold's progress if it has finished.
    fn take_job(&mut self, boards: &mut Boards) {
        let done = match self.job.as_mut().map(oneshot::Receiver::try_recv) {
            None | Some(Err(oneshot::error::TryRecvError::Empty)) => return,
            Some(Ok(done)) => done,
            Some(Err(oneshot::error::TryRecvError::Closed)) => {
                // The pool dropped it (shutting down): fold again later.
                self.job = None;
                self.job_for = None;
                return;
            }
        };
        self.job = None;
        self.registry = Some(done.registry);
        self.folded_for = self.job_for.take();
        // Links that moved while it ran make it stale; the next frame
        // folds again, and until then the old links' progress is close.
        boards.set_progress(done.progress);
    }
}
