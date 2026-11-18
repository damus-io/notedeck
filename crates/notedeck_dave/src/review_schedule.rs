//! Scheduled butler reviews: re-run the butler on an idea card when the card's
//! review date arrives.
//!
//! The butler (the `butler` built-in spec) labels each idea card
//! [`REVIEW_LABEL`] and sets its due date to the next review. On or after that
//! date, a Dave host spawns a butler session on the card, which reads its last
//! template, re-checks, grades the last review and proposes the next date. The
//! loop is the card's due date: approving the new date schedules the next run.
//!
//! **Exactly once per due date, on one host, across restarts.** A review is
//! named by its card and due date ([`review_key`]), and that key rides the
//! spawn command as its `idempotency_key`. The account's command log in
//! nostrdb is the only ledger: a key already spawned is done, so a restart
//! finds it there and does nothing. Every host running Dave sees the same card
//! fall due at the same moment, though, so before spawning a host publishes a
//! claim (a kind-31989 `review_claim`, which syncs like any command) and waits
//! [`SETTLE_SECS`] for the others' claims to arrive. The earliest claim wins,
//! ties broken by host name, so every host picks the same winner and only it
//! spawns. A claim older than [`STALE_SECS`] with no spawn behind it (its host
//! died) is ignored, and the review is claimed afresh.
//!
//! Two halves, so the decision is testable without a host: [`plan`] is pure
//! over the due cards, the command log, the host and the clock, and
//! [`ReviewScheduler`] runs it on a worker thread every [`TICK`], since
//! folding every board is too slow for the frame. The main thread only
//! publishes what the worker plans ([`Dave::poll_review_schedule`]).
//!
//! [`Dave::poll_review_schedule`]: crate::Dave::poll_review_schedule

use std::collections::HashSet;
use std::thread;
use std::time::Duration;

use agentium_core::session_events::{
    self, AI_SESSION_COMMAND_KIND, REVIEW_CLAIMANT_TAG, REVIEW_CLAIM_COMMAND, REVIEW_KEY_TAG,
};
use chrono::Datelike;
use crossbeam_channel as chan;
use headway::boards::{self, Identity, Roster};
use headway::event::{BoardView, Date};
use nostrdb::{Filter, Ndb, Note, Transaction};
use nostrdb_net::{NoteId, Pubkey};
use notedeck::Waker;

pub(crate) use agentium_core::agent_spec::builtin::REVIEW_LABEL;

/// The spec a scheduled review runs under.
pub(crate) const REVIEW_SPEC: &str = "butler";

/// What every review key starts with, so the command log's spawns can be told
/// from other idempotency keys.
const REVIEW_KEY_PREFIX: &str = "butler-review:";

/// How often the worker re-checks the boards for reviews that fell due. A
/// due date is a day, so minutes late is on time.
const TICK: Duration = Duration::from_secs(300);

/// How long a host waits after its claim before deciding whether it won,
/// so a claim made on another host at the same moment has time to sync.
pub(crate) const SETTLE_SECS: u64 = 120;

/// How old a claim with no spawn behind it gets before it is ignored: its
/// host claimed and then went away.
pub(crate) const STALE_SECS: u64 = 900;

/// The most kind-31989 notes one scan reads back. A scan reads only those
/// since the oldest due date, so this bounds a pathological log, not a normal
/// one.
const LOG_LIMIT: i32 = 4096;

/// The name of one scheduled review: its card and the due date that
/// scheduled it, so the next due date is a new review and the same one is
/// never run twice. The card id is prefixed, so the tag value never reads as
/// a 64-hex id that nostrdb would store as bytes.
pub(crate) fn review_key(card: &NoteId, due: Date) -> String {
    format!("{REVIEW_KEY_PREFIX}{}:{due}", card.hex())
}

/// A review that has fallen due: a [`REVIEW_LABEL`] card whose due date is
/// today or earlier, on a live column that isn't terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DueReview {
    /// [`review_key`] of the card and its due date.
    pub(crate) key: String,
    /// The card's `headway:<board>/<word-id>` ref: the session's title and
    /// issue, and what its prompt names.
    pub(crate) card_ref: String,
    /// The due date that scheduled it.
    pub(crate) due: Date,
}

impl DueReview {
    /// The scheduled session's first message.
    pub(crate) fn prompt(&self) -> String {
        format!(
            "Scheduled review of {card}: its review date, {due}, has arrived. \
             Read the card and the last idea template in its comments, re-check \
             what has changed since, grade the last review (evidence moved / no \
             change / killed), post the updated template, and propose the next \
             review date.",
            card = self.card_ref,
            due = self.due,
        )
    }
}

/// Every review due on `boards` by `today`: cards labelled [`REVIEW_LABEL`]
/// with a due date on or before it. Archived cards aren't in a column, and a
/// card in a terminal column (an idea killed or finished) is left alone.
pub(crate) fn due_reviews(boards: &[BoardView], today: Date) -> Vec<DueReview> {
    let mut due = Vec::new();
    for board in boards {
        for column in &board.columns {
            if board.column_is_terminal(&column.id) {
                continue;
            }
            for card in &column.cards {
                let Some(date) = card.due.filter(|d| *d <= today) else {
                    continue;
                };
                if !card.labels.iter().any(|l| l == REVIEW_LABEL) {
                    continue;
                }
                due.push(DueReview {
                    key: review_key(&card.id, date),
                    card_ref: headway::wordid::card_ref(&board.id, card.id.bytes()),
                    due: date,
                });
            }
        }
    }
    due
}

/// One host's claim on a review, read back from the command log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Claim {
    /// The review claimed.
    pub(crate) key: String,
    /// The host that claimed it.
    pub(crate) host: String,
    /// When, in unix seconds.
    pub(crate) at: u64,
}

/// What the account's command log says about scheduled reviews: who claimed
/// which, and which have been spawned.
#[derive(Debug, Default)]
pub(crate) struct ReviewLog {
    /// Every claim read.
    pub(crate) claims: Vec<Claim>,
    /// The review keys some host has spawned a session for.
    pub(crate) spawned: HashSet<String>,
}

impl ReviewLog {
    /// Read the account's kind-31989 notes since `since` (unix seconds).
    pub(crate) fn load(ndb: &Ndb, txn: &Transaction, account: &Pubkey, since: u64) -> Self {
        let filter = Filter::new()
            .kinds([AI_SESSION_COMMAND_KIND as u64])
            .authors([account.bytes()])
            .since(since)
            .limit(LOG_LIMIT as u64)
            .build();
        let mut log = Self::default();
        let Ok(results) = ndb.query(txn, &[filter], LOG_LIMIT) else {
            return log;
        };
        for result in results {
            log.record(&result.note);
        }
        log
    }

    /// Fold one command note in: a claim, a review's spawn, or neither.
    pub(crate) fn record(&mut self, note: &Note) {
        let tag = |name| session_events::get_tag_value(note, name);
        match tag("command") {
            Some(REVIEW_CLAIM_COMMAND) => {
                let (Some(key), Some(host)) = (tag(REVIEW_KEY_TAG), tag(REVIEW_CLAIMANT_TAG))
                else {
                    return;
                };
                self.claims.push(Claim {
                    key: key.to_string(),
                    host: host.to_string(),
                    at: note.created_at(),
                });
            }
            Some("spawn_session") => {
                if let Some(key) =
                    tag("idempotency_key").filter(|k| k.starts_with(REVIEW_KEY_PREFIX))
                {
                    self.spawned.insert(key.to_string());
                }
            }
            _ => {}
        }
    }
}

/// What one host should do about one due review, now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReviewStep {
    /// Nobody holds a live claim: claim it.
    Claim,
    /// Settling, or another host's claim is live: do nothing yet.
    Wait,
    /// Our claim has settled and won: spawn the review.
    Spawn,
    /// A session was spawned for it already.
    Done,
}

/// Decide [`ReviewStep`] for the review `key` on host `me` at `now`.
pub(crate) fn step(key: &str, me: &str, now: u64, log: &ReviewLog) -> ReviewStep {
    if log.spawned.contains(key) {
        return ReviewStep::Done;
    }
    let live = log
        .claims
        .iter()
        .filter(|c| c.key == key && now.saturating_sub(c.at) < STALE_SECS);
    let Some(winner) = live
        .clone()
        .min_by(|a, b| (a.at, &a.host).cmp(&(b.at, &b.host)))
    else {
        return ReviewStep::Claim;
    };
    // Our earliest live claim: the one the settle window runs from.
    let Some(mine) = live.filter(|c| c.host == me).min_by_key(|c| c.at) else {
        // Another host is on it. If it never spawns, its claim goes stale
        // and the review is claimed afresh.
        return ReviewStep::Wait;
    };
    if now.saturating_sub(mine.at) < SETTLE_SECS {
        return ReviewStep::Wait;
    }
    if winner.host == me {
        ReviewStep::Spawn
    } else {
        ReviewStep::Wait
    }
}

/// What the main thread publishes for one review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReviewAction {
    /// Publish this host's claim on the review.
    Claim {
        /// The review claimed.
        key: String,
    },
    /// Spawn the review's butler session on this host.
    Spawn(DueReview),
}

/// The actions host `me` takes at `now` for `due` reviews, given `log`.
pub(crate) fn plan(due: &[DueReview], log: &ReviewLog, me: &str, now: u64) -> Vec<ReviewAction> {
    due.iter()
        .filter_map(|review| match step(&review.key, me, now, log) {
            ReviewStep::Claim => Some(ReviewAction::Claim {
                key: review.key.clone(),
            }),
            ReviewStep::Spawn => Some(ReviewAction::Spawn(review.clone())),
            ReviewStep::Wait | ReviewStep::Done => None,
        })
        .collect()
}

/// The local calendar day, as headway's [`Date`].
pub(crate) fn today() -> Date {
    let now = chrono::Local::now().date_naive();
    Date {
        year: now.year(),
        month: now.month() as u8,
        day: now.day() as u8,
    }
}

/// The unix second the command log is read from for reviews due on `oldest`:
/// a day before that date's UTC midnight, since a host east of UTC reaches
/// the date before UTC does.
fn log_since(oldest: Date) -> u64 {
    chrono::NaiveDate::from_ymd_opt(oldest.year, oldest.month.into(), oldest.day.into())
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|t| t.and_utc().timestamp().saturating_sub(86_400).max(0) as u64)
        .unwrap_or(0)
}

/// One scan: fold the account's boards, find what fell due, read the
/// command log back to the oldest due date, and plan.
fn scan(ndb: &Ndb, account: &Pubkey, me: &str) -> Vec<ReviewAction> {
    let roster = Roster::read(ndb, Identity::own(*account));
    let mut views = boards::list_boards(ndb, &roster);
    views.extend(boards::list_shared_with_me(ndb, &roster));
    let due = due_reviews(&views, today());
    let Some(oldest) = due.iter().map(|r| r.due).min() else {
        return Vec::new();
    };
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };
    let log = ReviewLog::load(ndb, &txn, account, log_since(oldest));
    plan(&due, &log, me, session_events::now_secs())
}

/// A scan's plan, for the account it was made for.
pub(crate) struct ReviewPlan {
    /// The account whose boards and log were read.
    pub(crate) account: Pubkey,
    /// What to publish.
    pub(crate) actions: Vec<ReviewAction>,
}

/// The worker thread that scans for due reviews every [`TICK`], and the
/// main thread's handle on it.
///
/// Started on a host's first update with an account. Re-pointed with
/// [`watch`](Self::watch) when the account changes; the first scan runs at
/// once, so a host that was off when a review fell due runs it on start.
pub(crate) struct ReviewScheduler {
    /// Tells the worker which account to scan. Dropping it stops the worker.
    watch_tx: Option<chan::Sender<Pubkey>>,
    /// The worker's plans.
    plan_rx: chan::Receiver<ReviewPlan>,
    /// The account the worker scans.
    account: Pubkey,
    /// Reviews this process already spawned, so a plan computed before the
    /// spawn reached nostrdb can't spawn it again.
    pub(crate) spawned: HashSet<String>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ReviewScheduler {
    /// Start the worker scanning `account`'s boards as host `me`.
    pub(crate) fn start(ndb: Ndb, waker: Waker, me: String, account: Pubkey) -> Option<Self> {
        let (watch_tx, watch_rx) = chan::unbounded::<Pubkey>();
        let (plan_tx, plan_rx) = chan::unbounded::<ReviewPlan>();
        let worker = thread::Builder::new()
            .name("dave-review-schedule".to_string())
            .spawn(move || {
                let mut account = account;
                loop {
                    let actions = scan(&ndb, &account, &me);
                    if !actions.is_empty() {
                        if plan_tx.send(ReviewPlan { account, actions }).is_err() {
                            return;
                        }
                        waker.wake();
                    }
                    match watch_rx.recv_timeout(TICK) {
                        Ok(next) => account = next,
                        Err(chan::RecvTimeoutError::Timeout) => {}
                        Err(chan::RecvTimeoutError::Disconnected) => return,
                    }
                }
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(err) => {
                tracing::warn!("review schedule: failed to start its worker: {err}");
                return None;
            }
        };
        Some(Self {
            watch_tx: Some(watch_tx),
            plan_rx,
            account,
            spawned: HashSet::new(),
            worker: Some(worker),
        })
    }

    /// Scan `account` from now on, at once if it changed.
    pub(crate) fn watch(&mut self, account: Pubkey) {
        if self.account == account {
            return;
        }
        self.account = account;
        if let Some(tx) = &self.watch_tx {
            let _ = tx.send(account);
        }
    }

    /// The next plan for the watched account, skipping any made for an
    /// account no longer watched.
    pub(crate) fn next_plan(&mut self) -> Option<Vec<ReviewAction>> {
        while let Ok(plan) = self.plan_rx.try_recv() {
            if plan.account == self.account {
                return Some(plan.actions);
            }
        }
        None
    }
}

impl Drop for ReviewScheduler {
    fn drop(&mut self) {
        self.watch_tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Publish one plan as host `me`: each claim, and the spawn of each review
/// won, skipping a review this process already spawned (`spawned`, which a
/// spawn joins once it is published).
///
/// A spawn goes out as an ordinary kind-31989 spawn command addressed to
/// `me`, so the host's command poll materializes it like any other: under the
/// butler spec, titled and linked to its card, in `cwd`, with the review key
/// as its idempotency key, which is also what marks the review done in the
/// command log.
pub(crate) fn publish(
    engine: &agentium_core::Engine,
    me: &str,
    cwd: &str,
    spawned: &mut HashSet<String>,
    actions: Vec<ReviewAction>,
) {
    for action in actions {
        match action {
            ReviewAction::Claim { key } => {
                tracing::info!("review schedule: claiming {key}");
                if let Err(err) = engine.prepare_review_claim(&key, me) {
                    tracing::warn!("review schedule: failed to claim {key}: {err:?}");
                }
            }
            ReviewAction::Spawn(review) => {
                if spawned.contains(&review.key) {
                    continue;
                }
                tracing::info!("review schedule: spawning {}", review.key);
                let prompt = review.prompt();
                let opts = session_events::SpawnOptions {
                    title: Some(&review.card_ref),
                    prompt: Some(&prompt),
                    idempotency_key: Some(&review.key),
                    issue_url: Some(&review.card_ref),
                    spec: Some(REVIEW_SPEC),
                    ..Default::default()
                };
                let spawn_id = uuid::Uuid::new_v4().to_string();
                if let Err(err) = engine.prepare_spawn_command(
                    me,
                    cwd,
                    crate::backend::BackendType::OpenAI.as_str(),
                    &opts,
                    &spawn_id,
                ) {
                    tracing::warn!("review schedule: failed to spawn {}: {err:?}", review.key);
                    continue;
                }
                spawned.insert(review.key);
            }
        }
    }
}

impl crate::Dave {
    /// Keep the review worker scanning the selected account, and [`publish`]
    /// what it plans.
    pub(crate) fn poll_review_schedule(&mut self, ctx: &mut notedeck::AppContext<'_>) {
        let Some(account) = self
            .pns_local_state
            .as_ref()
            .filter(|state| state.has_secret_key)
            .map(|state| state.account)
        else {
            return;
        };
        let Some(scheduler) = self.review_scheduler.as_mut() else {
            self.review_scheduler = ReviewScheduler::start(
                ctx.ndb.clone(),
                ctx.waker.clone(),
                self.hostname.clone(),
                account,
            );
            return;
        };
        scheduler.watch(account);
        let Some(actions) = scheduler.next_plan() else {
            return;
        };
        let Some(engine) = crate::secret_key_bytes(ctx.accounts.get_selected_account().keypair())
            .and_then(|sk| crate::embedded_engine(ctx.ndb, &sk))
        else {
            return;
        };
        let cwd = dirs::home_dir()
            .map(|home| home.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        publish(
            &engine,
            &self.hostname,
            &cwd,
            &mut scheduler.spawned,
            actions,
        );
    }
}

#[cfg(test)]
mod tests;
