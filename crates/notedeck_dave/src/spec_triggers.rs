//! A spec's triggers, run: spawn a session under a spec when one of its
//! [`Trigger`]s fires.
//!
//! What fires and what the session is told are the spec's config
//! (`agent_spec::trigger`), so nothing here knows about any one agent. The
//! one event source so far, [`CARD_DUE_SOURCE`], is scanned here: a card on
//! a board the account owns reaching its due date is an [`Event`], and a
//! trigger bound to that source whose `label` match the card's labels meet
//! fires.
//! The butler uses it for its weekly idea review: it labels each idea card
//! `butler` and sets the card's due date to the next review, and when that
//! date arrives it runs on the card again.
//!
//! **Exactly once per firing, on one host, across restarts.** A firing is
//! named by its spec, card and due date ([`trigger_key`]), and that key rides
//! the spawn command as its `idempotency_key`. The account's command log in
//! nostrdb is the only ledger: a key already spawned is done, so a restart
//! finds it there and does nothing. Every host running Dave sees a card fall
//! due at the same moment, though, so before spawning a host publishes a
//! claim (a kind-31989 `trigger_claim`, which syncs like any command) and
//! waits [`SETTLE_SECS`] for the others' claims to arrive. The earliest claim
//! wins, ties broken by host name, so every host picks the same winner and
//! only it spawns. A claim older than [`STALE_SECS`] with no spawn behind it
//! (its host went away) is ignored, and the firing is claimed afresh.
//!
//! Only boards the account owns are watched. The log is per account, so a
//! card on a board shared between two accounts would otherwise fire once on
//! each account's hosts.
//!
//! Two halves, so the decision is testable without a host: [`plan`] is pure
//! over the firings, the command log, the host and the clock, and
//! [`TriggerScheduler`] runs it on a worker thread every [`TICK`], since
//! folding every board is too slow for the frame. The main thread only
//! publishes what the worker plans ([`Dave::poll_spec_triggers`]).
//!
//! [`Dave::poll_spec_triggers`]: crate::Dave::poll_spec_triggers

use std::collections::{BTreeMap, HashSet};
use std::thread;
use std::time::Duration;

use agentium_core::agent_spec::record::{self, load_agent_spec_records_from_ndb, SpecRecord};
use agentium_core::agent_spec::{AgentSpec, Trigger};
use agentium_core::events::Event;
use agentium_core::session_events::{
    self, AI_SESSION_COMMAND_KIND, TRIGGER_CLAIMANT_TAG, TRIGGER_CLAIM_COMMAND, TRIGGER_KEY_TAG,
};
use chrono::Datelike;
use crossbeam_channel as chan;
use headway::boards::{self, Identity, Roster};
use headway::event::{BoardView, Date};
use nostrdb::{Filter, Ndb, Note, Transaction};
use nostrdb_net::{NoteId, Pubkey};
use notedeck::Waker;

/// What every trigger key starts with, so the command log's trigger spawns
/// can be told from other idempotency keys.
const TRIGGER_KEY_PREFIX: &str = "trigger:";

/// How often the worker re-checks the boards for cards that fell due. A due
/// date is a day, so minutes late is on time.
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

/// The name of one firing: the spec, the card and the due date that fired
/// it, so the card's next due date is a new firing and the same one never
/// runs twice. The card id is prefixed, so the tag value never reads as a
/// 64-hex id that nostrdb would store as bytes.
pub(crate) fn trigger_key(spec: &str, card: &NoteId, due: Date) -> String {
    format!("{TRIGGER_KEY_PREFIX}{spec}:{}:{due}", card.hex())
}

/// The event source this module scans for: a headway card whose due date
/// has arrived. Its events carry `card` (the card's ref), `due`
/// (`YYYY-MM-DD`), `board` and `label` (every label of the card) as meta,
/// and the card's title as content.
pub(crate) const CARD_DUE_SOURCE: &str = "headway/card_due";

/// The meta key a [`CARD_DUE_SOURCE`] trigger must match on to be scanned
/// for, so no spec runs on every due card of every board.
const LABEL_KEY: &str = "label";

/// One [`CARD_DUE_SOURCE`] trigger of one spec: what the scan looks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardRule {
    /// The spec the session runs under.
    pub(crate) spec: String,
    /// The trigger: its match and its prompt.
    pub(crate) trigger: Trigger,
}

/// The card rules of `specs`, in their order: their triggers bound to
/// [`CARD_DUE_SOURCE`] that match on a label. Other sources have no
/// emitter yet.
pub(crate) fn card_rules<'a>(specs: impl IntoIterator<Item = &'a AgentSpec>) -> Vec<CardRule> {
    let mut rules = Vec::new();
    for spec in specs {
        for trigger in &spec.triggers {
            if trigger.source != CARD_DUE_SOURCE || !trigger.r#match.contains_key(LABEL_KEY) {
                continue;
            }
            rules.push(CardRule {
                spec: spec.slug.clone(),
                trigger: trigger.clone(),
            });
        }
    }
    rules
}

/// Every spec the account can run with a trigger: its records over the
/// built-ins, as a session would look each one up. A record that doesn't
/// read is skipped: a session under it couldn't run either.
fn triggered_specs(records: &BTreeMap<String, SpecRecord>) -> Vec<AgentSpec> {
    record::slugs(records)
        .into_iter()
        .filter_map(|slug| record::lookup(slug, records).ok())
        .filter(|spec| !spec.triggers.is_empty())
        .collect()
}

/// A trigger that has fired: a card carrying a rule's label whose due date
/// is today or earlier, on a live column that isn't terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Firing {
    /// [`trigger_key`] of the spec, the card and its due date.
    pub(crate) key: String,
    /// The spec to run.
    pub(crate) spec: String,
    /// The card's `headway:<board>/<word-id>` ref: the session's title and
    /// issue.
    pub(crate) card_ref: String,
    /// The due date that fired it.
    pub(crate) due: Date,
    /// The session's first message, its placeholders filled.
    pub(crate) prompt: String,
}

/// Every firing of `rules` on `boards` by `today`. Archived cards aren't in
/// a column, and a card in a terminal column (an idea killed or finished) is
/// left alone.
pub(crate) fn firings(boards: &[BoardView], rules: &[CardRule], today: Date) -> Vec<Firing> {
    let mut fired = Vec::new();
    for board in boards {
        for column in &board.columns {
            if board.column_is_terminal(&column.id) {
                continue;
            }
            for card in &column.cards {
                let Some(due) = card.due.filter(|d| *d <= today) else {
                    continue;
                };
                let card_ref = headway::wordid::card_ref(&board.id, card.id.bytes());
                let event = Event {
                    source: CARD_DUE_SOURCE.to_owned(),
                    key: format!("{}:{due}", card.id.hex()),
                    content: card.title.clone(),
                    meta: [
                        ("card".to_owned(), vec![card_ref.clone()]),
                        ("due".to_owned(), vec![due.to_string()]),
                        ("board".to_owned(), vec![board.id.clone()]),
                        (LABEL_KEY.to_owned(), card.labels.clone()),
                    ]
                    .into(),
                };
                for rule in rules {
                    if !rule.trigger.matches(&event) {
                        continue;
                    }
                    fired.push(Firing {
                        key: trigger_key(&rule.spec, &card.id, due),
                        spec: rule.spec.clone(),
                        card_ref: card_ref.clone(),
                        due,
                        prompt: rule.trigger.prompt_for(&event),
                    });
                }
            }
        }
    }
    fired
}

/// One host's claim on a firing, read back from the command log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Claim {
    /// The firing claimed.
    pub(crate) key: String,
    /// The host that claimed it.
    pub(crate) host: String,
    /// When, in unix seconds.
    pub(crate) at: u64,
}

/// What the account's command log says about trigger firings: who claimed
/// which, and which have been spawned.
#[derive(Debug, Default)]
pub(crate) struct TriggerLog {
    /// Every claim read.
    pub(crate) claims: Vec<Claim>,
    /// The firings some host has spawned a session for.
    pub(crate) spawned: HashSet<String>,
}

impl TriggerLog {
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

    /// Fold one command note in: a claim, a firing's spawn, or neither.
    pub(crate) fn record(&mut self, note: &Note) {
        let tag = |name| session_events::get_tag_value(note, name);
        match tag("command") {
            Some(TRIGGER_CLAIM_COMMAND) => {
                let (Some(key), Some(host)) = (tag(TRIGGER_KEY_TAG), tag(TRIGGER_CLAIMANT_TAG))
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
                    tag("idempotency_key").filter(|k| k.starts_with(TRIGGER_KEY_PREFIX))
                {
                    self.spawned.insert(key.to_string());
                }
            }
            _ => {}
        }
    }
}

/// What one host should do about one firing, now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nobody holds a live claim: claim it.
    Claim,
    /// Settling, or another host's claim is live: do nothing yet.
    Wait,
    /// Our claim has settled and won: spawn the session.
    Spawn,
    /// A session was spawned for it already.
    Done,
}

/// Decide [`Step`] for the firing `key` on host `me` at `now`.
pub(crate) fn step(key: &str, me: &str, now: u64, log: &TriggerLog) -> Step {
    if log.spawned.contains(key) {
        return Step::Done;
    }
    let live = log
        .claims
        .iter()
        .filter(|c| c.key == key && now.saturating_sub(c.at) < STALE_SECS);
    let Some(winner) = live
        .clone()
        .min_by(|a, b| (a.at, &a.host).cmp(&(b.at, &b.host)))
    else {
        return Step::Claim;
    };
    // Our earliest live claim: the one the settle window runs from.
    let Some(mine) = live.filter(|c| c.host == me).min_by_key(|c| c.at) else {
        // Another host is on it. If it never spawns, its claim goes stale
        // and the firing is claimed afresh.
        return Step::Wait;
    };
    if now.saturating_sub(mine.at) < SETTLE_SECS {
        return Step::Wait;
    }
    if winner.host == me {
        Step::Spawn
    } else {
        Step::Wait
    }
}

/// What the main thread publishes for one firing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TriggerAction {
    /// Publish this host's claim on the firing.
    Claim {
        /// The firing claimed.
        key: String,
    },
    /// Spawn the firing's session on this host.
    Spawn(Firing),
}

/// The actions host `me` takes at `now` for `fired`, given `log`.
pub(crate) fn plan(fired: &[Firing], log: &TriggerLog, me: &str, now: u64) -> Vec<TriggerAction> {
    fired
        .iter()
        .filter_map(|firing| match step(&firing.key, me, now, log) {
            Step::Claim => Some(TriggerAction::Claim {
                key: firing.key.clone(),
            }),
            Step::Spawn => Some(TriggerAction::Spawn(firing.clone())),
            Step::Wait | Step::Done => None,
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

/// The unix second the command log is read from for firings due on
/// `oldest`: a day before that date's UTC midnight, since a host east of UTC
/// reaches the date before UTC does.
fn log_since(oldest: Date) -> u64 {
    chrono::NaiveDate::from_ymd_opt(oldest.year, oldest.month.into(), oldest.day.into())
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|t| t.and_utc().timestamp().saturating_sub(86_400).max(0) as u64)
        .unwrap_or(0)
}

/// One scan: find the specs with triggers, fold the account's own boards,
/// find what fired, read the command log back to the oldest due date, and
/// plan.
fn scan(ndb: &Ndb, account: &Pubkey, me: &str) -> Vec<TriggerAction> {
    // In its own transaction, closed before the board fold opens its own:
    // LMDB refuses a second read transaction on one thread.
    let rules = {
        let Ok(txn) = Transaction::new(ndb) else {
            return Vec::new();
        };
        card_rules(&triggered_specs(&load_agent_spec_records_from_ndb(
            ndb, &txn, account,
        )))
    };
    if rules.is_empty() {
        return Vec::new();
    }
    let roster = Roster::read(ndb, Identity::own(*account));
    let fired = firings(&boards::list_boards(ndb, &roster), &rules, today());
    let Some(oldest) = fired.iter().map(|f| f.due).min() else {
        return Vec::new();
    };
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };
    let log = TriggerLog::load(ndb, &txn, account, log_since(oldest));
    plan(&fired, &log, me, session_events::now_secs())
}

/// A scan's plan, for the account it was made for.
pub(crate) struct TriggerPlan {
    /// The account whose specs, boards and log were read.
    pub(crate) account: Pubkey,
    /// What to publish.
    pub(crate) actions: Vec<TriggerAction>,
}

/// The worker thread that scans for fired triggers every [`TICK`], and the
/// main thread's handle on it.
///
/// Started on a host's first update with an account. Re-pointed with
/// [`watch`](Self::watch) when the account changes; the first scan runs at
/// once, so a host that was off when a card fell due runs it on start.
pub(crate) struct TriggerScheduler {
    /// Tells the worker which account to scan. Dropping it stops the worker.
    watch_tx: Option<chan::Sender<Pubkey>>,
    /// The worker's plans.
    plan_rx: chan::Receiver<TriggerPlan>,
    /// The account the worker scans.
    account: Pubkey,
    /// Firings this process already spawned, so a plan computed before the
    /// spawn reached nostrdb can't spawn it again.
    pub(crate) spawned: HashSet<String>,
    worker: Option<thread::JoinHandle<()>>,
}

impl TriggerScheduler {
    /// Start the worker scanning `account`'s specs and boards as host `me`.
    pub(crate) fn start(ndb: Ndb, waker: Waker, me: String, account: Pubkey) -> Option<Self> {
        let (watch_tx, watch_rx) = chan::unbounded::<Pubkey>();
        let (plan_tx, plan_rx) = chan::unbounded::<TriggerPlan>();
        let worker = thread::Builder::new()
            .name("dave-spec-triggers".to_string())
            .spawn(move || {
                let mut account = account;
                loop {
                    let actions = scan(&ndb, &account, &me);
                    if !actions.is_empty() {
                        if plan_tx.send(TriggerPlan { account, actions }).is_err() {
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
                tracing::warn!("spec triggers: failed to start its worker: {err}");
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
    pub(crate) fn next_plan(&mut self) -> Option<Vec<TriggerAction>> {
        while let Ok(plan) = self.plan_rx.try_recv() {
            if plan.account == self.account {
                return Some(plan.actions);
            }
        }
        None
    }
}

impl Drop for TriggerScheduler {
    fn drop(&mut self) {
        self.watch_tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Publish one plan as host `me`: each claim, and the spawn of each firing
/// won, skipping a firing this process already spawned (`spawned`, which a
/// spawn joins once it is published).
///
/// A spawn goes out as an ordinary kind-31989 spawn command addressed to
/// `me`, so the host's command poll materializes it like any other: under
/// the firing's spec, titled and linked to its card, in `cwd`, with the
/// trigger key as its idempotency key, which is also what marks the firing
/// done in the command log.
pub(crate) fn publish(
    engine: &agentium_core::Engine,
    me: &str,
    cwd: &str,
    spawned: &mut HashSet<String>,
    actions: Vec<TriggerAction>,
) {
    for action in actions {
        match action {
            TriggerAction::Claim { key } => {
                tracing::info!("spec triggers: claiming {key}");
                if let Err(err) = engine.prepare_trigger_claim(&key, me) {
                    tracing::warn!("spec triggers: failed to claim {key}: {err:?}");
                }
            }
            TriggerAction::Spawn(firing) => {
                if spawned.contains(&firing.key) {
                    continue;
                }
                tracing::info!("spec triggers: spawning {}", firing.key);
                let opts = session_events::SpawnOptions {
                    title: Some(&firing.card_ref),
                    prompt: Some(&firing.prompt),
                    idempotency_key: Some(&firing.key),
                    issue_url: Some(&firing.card_ref),
                    spec: Some(&firing.spec),
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
                    tracing::warn!("spec triggers: failed to spawn {}: {err:?}", firing.key);
                    continue;
                }
                spawned.insert(firing.key);
            }
        }
    }
}

impl crate::Dave {
    /// Keep the trigger worker scanning the selected account, and
    /// [`publish`] what it plans.
    pub(crate) fn poll_spec_triggers(&mut self, ctx: &mut notedeck::AppContext<'_>) {
        let Some(account) = self
            .pns_local_state
            .as_ref()
            .filter(|state| state.has_secret_key)
            .map(|state| state.account)
        else {
            return;
        };
        let Some(scheduler) = self.trigger_scheduler.as_mut() else {
            self.trigger_scheduler = TriggerScheduler::start(
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
