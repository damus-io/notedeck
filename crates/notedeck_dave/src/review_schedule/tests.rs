//! The review schedule's decision ([`plan`]) over a command log, and the
//! claim and spawn notes it publishes read back through a real store as the
//! log a later scan, a restarted host or a second host sees.

use super::*;
use crate::test_support::{test_config, test_secret_key};
use headway::event::{CardView, ColumnView, Priority};
use std::time::Duration as StdDuration;
use tempfile::TempDir;

fn date(s: &str) -> Date {
    Date::parse(s).unwrap()
}

/// A card `n` with `labels` and an optional due date.
fn card(n: u8, labels: &[&str], due: Option<&str>) -> CardView {
    CardView {
        id: NoteId::new([n; 32]),
        author: [0; 32],
        title: format!("idea {n}"),
        description: String::new(),
        labels: labels.iter().map(|l| l.to_string()).collect(),
        priority: Priority::None,
        due: due.map(date),
        estimate: None,
        rank: String::new(),
        seq: None,
        placed_at: 0,
        created_at: 0,
        updated_at: 0,
        comments: Vec::new(),
        reviews: Vec::new(),
        activity: Vec::new(),
        parent: None,
        subissues: Vec::new(),
        blocked_by: Vec::new(),
        blocks: Vec::new(),
        related: Vec::new(),
    }
}

/// A board `ideas` with a Todo column and a terminal Done column.
fn board(todo: Vec<CardView>, done: Vec<CardView>) -> BoardView {
    let column = |id: &str, terminal, cards| ColumnView {
        id: id.to_string(),
        name: id.to_string(),
        terminal,
        cards,
    };
    BoardView {
        id: "ideas".to_string(),
        author: [0; 32],
        title: "Ideas".to_string(),
        description: String::new(),
        created_at: 0,
        columns: vec![column("todo", false, todo), column("done", true, done)],
        archived: Vec::new(),
        deleted: false,
    }
}

/// Only a butler card whose date arrived, outside a terminal column, is due;
/// it is named by its card and that date.
#[test]
fn only_a_labelled_card_whose_date_arrived_is_due() {
    let today = date("2026-11-18");
    let boards = [board(
        vec![
            card(1, &[REVIEW_LABEL], Some("2026-11-18")),
            card(2, &[REVIEW_LABEL], Some("2026-11-02")),
            card(3, &[REVIEW_LABEL], Some("2026-11-19")),
            card(4, &["idea"], Some("2026-11-18")),
            card(5, &[REVIEW_LABEL], None),
        ],
        vec![card(6, &[REVIEW_LABEL], Some("2026-11-01"))],
    )];
    let due = due_reviews(&boards, today);
    let refs: Vec<&str> = due.iter().map(|r| r.card_ref.as_str()).collect();
    assert_eq!(
        refs,
        [
            headway::wordid::card_ref("ideas", &[1; 32]),
            headway::wordid::card_ref("ideas", &[2; 32]),
        ]
    );
    assert_eq!(due[0].key, review_key(&NoteId::new([1; 32]), today));
    assert_eq!(
        due[0].key,
        format!("butler-review:{}:2026-11-18", hex::encode([1u8; 32]))
    );
    assert!(due[0].prompt().contains(&due[0].card_ref));
}

/// A review due on `card` 1, today.
fn one_review() -> DueReview {
    let today = date("2026-11-18");
    due_reviews(
        &[board(
            vec![card(1, &[REVIEW_LABEL], Some("2026-11-18"))],
            vec![],
        )],
        today,
    )
    .remove(0)
}

/// One host: claim, wait out the settle window, spawn once, then nothing.
#[test]
fn one_host_claims_settles_and_spawns_once() {
    let review = one_review();
    let due = [review.clone()];
    let t0 = 1_000_000;
    let mut log = ReviewLog::default();

    let claim = ReviewAction::Claim {
        key: review.key.clone(),
    };
    assert_eq!(plan(&due, &log, "monad", t0), [claim]);

    log.claims.push(Claim {
        key: review.key.clone(),
        host: "monad".into(),
        at: t0,
    });
    assert_eq!(plan(&due, &log, "monad", t0 + 1), []);
    assert_eq!(
        plan(&due, &log, "monad", t0 + SETTLE_SECS),
        [ReviewAction::Spawn(review.clone())]
    );

    log.spawned.insert(review.key.clone());
    assert_eq!(plan(&due, &log, "monad", t0 + SETTLE_SECS + 1), []);
}

/// Two hosts that claim at once both wait out the window, then agree: only
/// the earlier claim (the lower host on a tie) spawns.
#[test]
fn of_two_hosts_claiming_at_once_only_one_spawns() {
    let review = one_review();
    let due = [review.clone()];
    let t0 = 1_000_000;
    let claim = |host: &str, at| Claim {
        key: review.key.clone(),
        host: host.into(),
        at,
    };
    let after = t0 + SETTLE_SECS + 1;

    let tie = ReviewLog {
        claims: vec![claim("monad", t0), claim("hydra", t0)],
        ..Default::default()
    };
    assert_eq!(
        plan(&due, &tie, "hydra", after),
        [ReviewAction::Spawn(review.clone())]
    );
    assert_eq!(plan(&due, &tie, "monad", after), []);

    let earlier = ReviewLog {
        claims: vec![claim("monad", t0), claim("hydra", t0 + 3)],
        ..Default::default()
    };
    assert_eq!(
        plan(&due, &earlier, "monad", after + 3),
        [ReviewAction::Spawn(review.clone())]
    );
    assert_eq!(plan(&due, &earlier, "hydra", after + 3), []);
}

/// A host that sees another's live claim doesn't claim too; once that claim
/// goes stale with nothing spawned (its host went away), it claims afresh.
#[test]
fn a_stale_claim_is_taken_over() {
    let review = one_review();
    let due = [review.clone()];
    let t0 = 1_000_000;
    let log = ReviewLog {
        claims: vec![Claim {
            key: review.key.clone(),
            host: "hydra".into(),
            at: t0,
        }],
        ..Default::default()
    };
    assert_eq!(plan(&due, &log, "monad", t0 + SETTLE_SECS), []);
    assert_eq!(
        plan(&due, &log, "monad", t0 + STALE_SECS),
        [ReviewAction::Claim { key: review.key }]
    );
}

/// A store with the account's PNS key, so the engine's wrapped notes unwrap.
struct Store {
    _dir: TempDir,
    ndb: Ndb,
    sk: [u8; 32],
    account: Pubkey,
}

impl Store {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let sk = test_secret_key();
        assert!(ndb.add_key(&sk), "ndb must accept the PNS key");
        let account = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        Self {
            _dir: dir,
            ndb,
            sk,
            account,
        }
    }

    /// Publish `actions` as host `me`, waiting until each note is readable.
    fn publish(&self, me: &str, spawned: &mut HashSet<String>, actions: Vec<ReviewAction>) {
        let filter = Filter::new()
            .kinds([AI_SESSION_COMMAND_KIND as u64])
            .authors([self.account.bytes()])
            .build();
        let sub = self.ndb.subscribe(&[filter]).unwrap();
        let count = actions.len() as u32;
        let engine = agentium_core::Engine::embedded(self.ndb.clone(), self.sk).unwrap();
        publish(&engine, me, "/home/me", spawned, actions);
        if count == 0 {
            return;
        }
        let ready = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(
                self.ndb
                    .wait_for_all_notes_within(sub, count, StdDuration::from_secs(5)),
            );
        assert!(ready.is_ok(), "{count} command notes ingested");
    }

    fn log(&self) -> ReviewLog {
        let txn = Transaction::new(&self.ndb).unwrap();
        ReviewLog::load(&self.ndb, &txn, &self.account, 0)
    }

    /// The spawn commands carrying `key`.
    fn spawns(&self, key: &str) -> usize {
        let txn = Transaction::new(&self.ndb).unwrap();
        let filter = Filter::new()
            .kinds([AI_SESSION_COMMAND_KIND as u64])
            .authors([self.account.bytes()])
            .build();
        let results = self.ndb.query(&txn, &[filter], 64).unwrap();
        results
            .iter()
            .filter(|r| session_events::get_tag_value(&r.note, "idempotency_key") == Some(key))
            .count()
    }
}

/// Two hosts over one synced log, the way the account's hosts see it: both
/// claim, one spawns exactly one command, and a restarted host (no memory,
/// only the log) finds the review done and does nothing.
#[test]
fn two_hosts_spawn_exactly_one_command_and_a_restart_does_not_refire() {
    let store = Store::new();
    let review = one_review();
    let due = [review.clone()];
    let now = session_events::now_secs();
    let mut monad_spawned = HashSet::new();
    let mut hydra_spawned = HashSet::new();

    // Both scan before either claim has synced.
    let empty = store.log();
    let monad = plan(&due, &empty, "monad", now);
    let hydra = plan(&due, &empty, "hydra", now);
    store.publish("monad", &mut monad_spawned, monad);
    store.publish("hydra", &mut hydra_spawned, hydra);

    let log = store.log();
    assert_eq!(log.claims.len(), 2, "both claims read back from the store");
    assert!(log.claims.iter().all(|c| c.key == review.key));

    // After the settle window both read the same log and agree on one winner.
    let later = now + SETTLE_SECS + 5;
    let monad = plan(&due, &log, "monad", later);
    let hydra = plan(&due, &log, "hydra", later);
    assert_eq!(monad.len() + hydra.len(), 1, "exactly one host spawns");
    store.publish("monad", &mut monad_spawned, monad);
    store.publish("hydra", &mut hydra_spawned, hydra);
    assert_eq!(store.spawns(&review.key), 1);

    // A restart: a fresh scan over only what the store holds.
    let log = store.log();
    assert!(log.spawned.contains(&review.key));
    for host in ["monad", "hydra", "fresh-host"] {
        assert_eq!(plan(&due, &log, host, later + 3600), [], "{host} refires");
    }
    assert_eq!(store.spawns(&review.key), 1);
}

/// The spawn a review publishes is addressed to its host and runs the butler
/// on the card: titled and linked to it, with the review key for idempotency.
#[test]
fn a_review_spawn_runs_the_butler_on_its_card() {
    let store = Store::new();
    let review = one_review();
    let mut spawned = HashSet::new();
    store.publish(
        "monad",
        &mut spawned,
        vec![ReviewAction::Spawn(review.clone())],
    );
    assert!(spawned.contains(&review.key));

    // Already spawned in this process: a plan made before the spawn reached
    // the log publishes nothing.
    let engine = agentium_core::Engine::embedded(store.ndb.clone(), store.sk).unwrap();
    publish(
        &engine,
        "monad",
        "/home/me",
        &mut spawned,
        vec![ReviewAction::Spawn(review.clone())],
    );

    let txn = Transaction::new(&store.ndb).unwrap();
    let filter = Filter::new()
        .kinds([AI_SESSION_COMMAND_KIND as u64])
        .authors([store.account.bytes()])
        .build();
    let results = store.ndb.query(&txn, &[filter], 8).unwrap();
    assert_eq!(results.len(), 1);
    let note = &results[0].note;
    let tag = |name| session_events::get_tag_value(note, name);
    assert_eq!(tag("command"), Some("spawn_session"));
    assert_eq!(tag("target_host"), Some("monad"));
    assert_eq!(tag(agentium_core::agent_spec::SPEC_TAG), Some(REVIEW_SPEC));
    assert_eq!(tag("custom_title"), Some(review.card_ref.as_str()));
    assert_eq!(tag("issue_url"), Some(review.card_ref.as_str()));
    assert_eq!(tag("idempotency_key"), Some(review.key.as_str()));
    assert_eq!(tag("prompt"), Some(review.prompt().as_str()));
}

/// A claim targets no host, so no host's command decoder takes it for a
/// spawn or resume addressed to it.
#[test]
fn a_claim_targets_no_host() {
    let store = Store::new();
    let review = one_review();
    store.publish(
        "monad",
        &mut HashSet::new(),
        vec![ReviewAction::Claim {
            key: review.key.clone(),
        }],
    );
    let txn = Transaction::new(&store.ndb).unwrap();
    let filter = Filter::new()
        .kinds([AI_SESSION_COMMAND_KIND as u64])
        .authors([store.account.bytes()])
        .build();
    let results = store.ndb.query(&txn, &[filter], 8).unwrap();
    assert_eq!(results.len(), 1);
    let note = &results[0].note;
    let tag = |name| session_events::get_tag_value(note, name);
    assert_eq!(tag("command"), Some(REVIEW_CLAIM_COMMAND));
    assert_eq!(tag("target_host"), Some(""));
    assert_eq!(tag(REVIEW_KEY_TAG), Some(review.key.as_str()));
    assert_eq!(tag(REVIEW_CLAIMANT_TAG), Some("monad"));
}
