//! Which specs' triggers fire where ([`card_rules`], [`firings`]), the
//! decision ([`plan`]) over a command log, and the claim and spawn notes it
//! publishes read back through a real store as the log a later scan, a
//! restarted host or a second host sees.

use super::*;
use crate::test_support::{test_config, test_secret_key};
use agentium_core::agent_spec::builtin;
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

/// The butler's own trigger, as a rule: its spec, its label and its prompt.
fn butler_rules() -> Vec<CardRule> {
    card_rules(&[builtin::find("butler").unwrap()])
}

/// The butler's card trigger is config: one rule, read from its spec.
#[test]
fn the_butlers_trigger_is_one_card_rule() {
    let rules = butler_rules();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].spec, "butler");
    assert_eq!(rules[0].label, "butler");
    assert!(rules[0].prompt.contains("{card}"));
}

/// Only specs with triggers give rules, the account's record replaces the
/// built-in of its slug (here dropping the butler's trigger), and a spec of
/// the account's own can add one with no code.
#[test]
fn an_accounts_records_decide_which_triggers_run() {
    let record = |spec: AgentSpec| SpecRecord {
        slug: spec.slug.clone(),
        updated_at: 1,
        spec: Ok(spec),
    };
    let mut quiet_butler = builtin::find("butler").unwrap();
    quiet_butler.triggers.clear();
    let mut triage = builtin::find("butler").unwrap();
    triage.slug = "triage".into();
    triage.triggers = vec![Trigger::CardDue {
        label: "bug".into(),
        prompt: "Re-check {card}.".into(),
    }];
    let records: BTreeMap<String, SpecRecord> = [record(quiet_butler), record(triage)]
        .into_iter()
        .map(|r| (r.slug.clone(), r))
        .collect();
    let rules = card_rules(&triggered_specs(&records));
    assert_eq!(
        rules,
        [CardRule {
            spec: "triage".into(),
            label: "bug".into(),
            prompt: "Re-check {card}.".into(),
        }]
    );
    assert_eq!(
        card_rules(&triggered_specs(&BTreeMap::new())),
        butler_rules()
    );
}

/// Only a labelled card whose date arrived, outside a terminal column,
/// fires; it is named by its spec, card and date, and its prompt names them.
#[test]
fn only_a_labelled_card_whose_date_arrived_fires() {
    let today = date("2026-11-18");
    let boards = [board(
        vec![
            card(1, &["butler"], Some("2026-11-18")),
            card(2, &["butler"], Some("2026-11-02")),
            card(3, &["butler"], Some("2026-11-19")),
            card(4, &["idea"], Some("2026-11-18")),
            card(5, &["butler"], None),
        ],
        vec![card(6, &["butler"], Some("2026-11-01"))],
    )];
    let fired = firings(&boards, &butler_rules(), today);
    let refs: Vec<&str> = fired.iter().map(|f| f.card_ref.as_str()).collect();
    assert_eq!(
        refs,
        [
            headway::wordid::card_ref("ideas", &[1; 32]),
            headway::wordid::card_ref("ideas", &[2; 32]),
        ]
    );
    assert_eq!(fired[0].spec, "butler");
    assert_eq!(
        fired[0].key,
        format!("trigger:butler:{}:2026-11-18", hex::encode([1u8; 32]))
    );
    assert!(fired[0].prompt.contains(&fired[0].card_ref));
    assert!(fired[0].prompt.contains("2026-11-18"));
    assert!(!fired[0].prompt.contains("{card}"));
}

/// One card two specs trigger on fires once for each, under separate keys.
#[test]
fn a_card_two_specs_watch_fires_for_each() {
    let mut rules = butler_rules();
    rules.push(CardRule {
        spec: "triage".into(),
        label: "bug".into(),
        prompt: "Re-check {card}.".into(),
    });
    let boards = [board(
        vec![card(1, &["butler", "bug"], Some("2026-11-18"))],
        vec![],
    )];
    let fired = firings(&boards, &rules, date("2026-11-18"));
    let specs: Vec<&str> = fired.iter().map(|f| f.spec.as_str()).collect();
    assert_eq!(specs, ["butler", "triage"]);
    assert_ne!(fired[0].key, fired[1].key);
}

/// The butler's firing on card 1, due today.
fn one_firing() -> Firing {
    let boards = [board(
        vec![card(1, &["butler"], Some("2026-11-18"))],
        vec![],
    )];
    firings(&boards, &butler_rules(), date("2026-11-18")).remove(0)
}

/// One host: claim, wait out the settle window, spawn once, then nothing.
#[test]
fn one_host_claims_settles_and_spawns_once() {
    let firing = one_firing();
    let fired = [firing.clone()];
    let t0 = 1_000_000;
    let mut log = TriggerLog::default();

    let claim = TriggerAction::Claim {
        key: firing.key.clone(),
    };
    assert_eq!(plan(&fired, &log, "monad", t0), [claim]);

    log.claims.push(Claim {
        key: firing.key.clone(),
        host: "monad".into(),
        at: t0,
    });
    assert_eq!(plan(&fired, &log, "monad", t0 + 1), []);
    assert_eq!(
        plan(&fired, &log, "monad", t0 + SETTLE_SECS),
        [TriggerAction::Spawn(firing.clone())]
    );

    log.spawned.insert(firing.key.clone());
    assert_eq!(plan(&fired, &log, "monad", t0 + SETTLE_SECS + 1), []);
}

/// Two hosts that claim at once both wait out the window, then agree: only
/// the earlier claim (the lower host on a tie) spawns.
#[test]
fn of_two_hosts_claiming_at_once_only_one_spawns() {
    let firing = one_firing();
    let fired = [firing.clone()];
    let t0 = 1_000_000;
    let claim = |host: &str, at| Claim {
        key: firing.key.clone(),
        host: host.into(),
        at,
    };
    let after = t0 + SETTLE_SECS + 1;

    let tie = TriggerLog {
        claims: vec![claim("monad", t0), claim("hydra", t0)],
        ..Default::default()
    };
    let spawn = [TriggerAction::Spawn(firing.clone())];
    assert_eq!(plan(&fired, &tie, "hydra", after), spawn);
    assert_eq!(plan(&fired, &tie, "monad", after), []);

    let earlier = TriggerLog {
        claims: vec![claim("monad", t0), claim("hydra", t0 + 3)],
        ..Default::default()
    };
    assert_eq!(plan(&fired, &earlier, "monad", after + 3), spawn);
    assert_eq!(plan(&fired, &earlier, "hydra", after + 3), []);
}

/// A host that sees another's live claim doesn't claim too; once that claim
/// goes stale with nothing spawned (its host went away), it claims afresh.
#[test]
fn a_stale_claim_is_taken_over() {
    let firing = one_firing();
    let fired = [firing.clone()];
    let t0 = 1_000_000;
    let log = TriggerLog {
        claims: vec![Claim {
            key: firing.key.clone(),
            host: "hydra".into(),
            at: t0,
        }],
        ..Default::default()
    };
    assert_eq!(plan(&fired, &log, "monad", t0 + SETTLE_SECS), []);
    assert_eq!(
        plan(&fired, &log, "monad", t0 + STALE_SECS),
        [TriggerAction::Claim { key: firing.key }]
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
    fn publish(&self, me: &str, spawned: &mut HashSet<String>, actions: Vec<TriggerAction>) {
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

    fn log(&self) -> TriggerLog {
        let txn = Transaction::new(&self.ndb).unwrap();
        TriggerLog::load(&self.ndb, &txn, &self.account, 0)
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
/// only the log) finds the firing done and does nothing.
#[test]
fn two_hosts_spawn_exactly_one_command_and_a_restart_does_not_refire() {
    let store = Store::new();
    let firing = one_firing();
    let fired = [firing.clone()];
    let now = session_events::now_secs();
    let mut monad_spawned = HashSet::new();
    let mut hydra_spawned = HashSet::new();

    // Both scan before either claim has synced.
    let empty = store.log();
    let monad = plan(&fired, &empty, "monad", now);
    let hydra = plan(&fired, &empty, "hydra", now);
    store.publish("monad", &mut monad_spawned, monad);
    store.publish("hydra", &mut hydra_spawned, hydra);

    let log = store.log();
    assert_eq!(log.claims.len(), 2, "both claims read back from the store");
    assert!(log.claims.iter().all(|c| c.key == firing.key));

    // After the settle window both read the same log and agree on one winner.
    let later = now + SETTLE_SECS + 5;
    let monad = plan(&fired, &log, "monad", later);
    let hydra = plan(&fired, &log, "hydra", later);
    assert_eq!(monad.len() + hydra.len(), 1, "exactly one host spawns");
    store.publish("monad", &mut monad_spawned, monad);
    store.publish("hydra", &mut hydra_spawned, hydra);
    assert_eq!(store.spawns(&firing.key), 1);

    // A restart: a fresh scan over only what the store holds.
    let log = store.log();
    assert!(log.spawned.contains(&firing.key));
    for host in ["monad", "hydra", "fresh-host"] {
        assert_eq!(plan(&fired, &log, host, later + 3600), [], "{host} refires");
    }
    assert_eq!(store.spawns(&firing.key), 1);
}

/// The spawn a firing publishes is addressed to its host and runs the
/// firing's spec on the card: titled and linked to it, with the trigger's
/// filled prompt and the trigger key for idempotency.
#[test]
fn a_firings_spawn_runs_its_spec_on_its_card() {
    let store = Store::new();
    let firing = one_firing();
    let mut spawned = HashSet::new();
    store.publish(
        "monad",
        &mut spawned,
        vec![TriggerAction::Spawn(firing.clone())],
    );
    assert!(spawned.contains(&firing.key));

    // Already spawned in this process: a plan made before the spawn reached
    // the log publishes nothing.
    let engine = agentium_core::Engine::embedded(store.ndb.clone(), store.sk).unwrap();
    publish(
        &engine,
        "monad",
        "/home/me",
        &mut spawned,
        vec![TriggerAction::Spawn(firing.clone())],
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
    assert_eq!(tag(agentium_core::agent_spec::SPEC_TAG), Some("butler"));
    assert_eq!(tag("custom_title"), Some(firing.card_ref.as_str()));
    assert_eq!(tag("issue_url"), Some(firing.card_ref.as_str()));
    assert_eq!(tag("idempotency_key"), Some(firing.key.as_str()));
    assert_eq!(tag("prompt"), Some(firing.prompt.as_str()));
}

/// A claim targets no host, so no host's command decoder takes it for a
/// spawn or resume addressed to it.
#[test]
fn a_claim_targets_no_host() {
    let store = Store::new();
    let firing = one_firing();
    store.publish(
        "monad",
        &mut HashSet::new(),
        vec![TriggerAction::Claim {
            key: firing.key.clone(),
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
    assert_eq!(tag("command"), Some(TRIGGER_CLAIM_COMMAND));
    assert_eq!(tag("target_host"), Some(""));
    assert_eq!(tag(TRIGGER_KEY_TAG), Some(firing.key.as_str()));
    assert_eq!(tag(TRIGGER_CLAIMANT_TAG), Some("monad"));
}
