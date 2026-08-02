use super::*;
use crate::{
    Accounts, EnsureSubResult, FullHistoryConfig, JobPool, Notedeck, ScopedSubCommand,
    ScopedSubIdentity, ScopedSubReadiness, ScopedSubsState, SubConfig, SubKey, SubOwnerKey,
    SubRelayPolicy, UnknownIds, FALLBACK_PUBKEY,
};
use enostr::{
    FullKeypair, NormRelayUrl, NoteId, RelayDemandPriority, RelayId, RelayRoutingPreference,
};
use enostr_test_support::relay::{
    create_filtered_capture_relay_with_handler,
    create_text_capture_relay as create_shared_text_capture_relay, CaptureRelayResponse,
};
use nostrdb::{Config, Filter, Ndb, NoteBuilder, Transaction};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio::sync::Notify;

fn test_ndb() -> (TempDir, Ndb) {
    let tmp = TempDir::new().expect("tmp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    (tmp, ndb)
}

fn test_remote_state(ndb: &Ndb, job_pool: &JobPool) -> RemoteState {
    crate::app::install_crypto();
    RemoteState::new_with_config(
        ndb,
        job_pool.spawner(),
        || {},
        RemoteBridgeConfig::default(),
    )
}

fn test_accounts(ndb: &mut Ndb, txn: &Transaction, forced_relays: Vec<String>) -> Accounts {
    let mut unknown_ids = UnknownIds::default();
    Accounts::new(
        None,
        forced_relays,
        Vec::new(),
        FALLBACK_PUBKEY(),
        ndb,
        txn,
        &mut unknown_ids,
    )
}

fn important_policy() -> SubRelayPolicy {
    SubRelayPolicy::new(
        RelayDemandPriority::Important,
        RelayRoutingPreference::PreferDedicated,
    )
}

fn explicit_config(relay: NormRelayUrl, filter: Filter) -> SubConfig {
    SubConfig::builder(vec![filter])
        .explicit([relay], important_policy())
        .build()
}

fn explicit_full_history_config(relay: NormRelayUrl) -> SubConfig {
    let filter = vec![Filter::new().kinds(vec![1]).limit(10).build()];
    SubConfig::builder(filter.clone())
        .full_history(FullHistoryConfig::new(filter))
        .explicit([relay], important_policy())
        .build()
}

fn signed_text_note_json(content: &str, created_at: u64) -> (String, NoteId) {
    let keypair = FullKeypair::generate();
    let note = NoteBuilder::new()
        .kind(1)
        .content(content)
        .created_at(created_at)
        .sign(&keypair.secret_key.secret_bytes())
        .build()
        .expect("signed text note");
    let json = note.json().expect("text note json");
    let id = NoteId::new(*note.id());
    (json, id)
}

fn signed_text_note(content: &str, created_at: u64) -> nostrdb::Note<'static> {
    let keypair = FullKeypair::generate();
    NoteBuilder::new()
        .kind(1)
        .content(content)
        .created_at(created_at)
        .sign(&keypair.secret_key.secret_bytes())
        .build()
        .expect("signed text note")
}

#[test]
fn repeated_scoped_sub_ensure_emits_one_ensure_owner_config_command() {
    let (_tmp, mut ndb) = test_ndb();
    let txn = Transaction::new(&ndb).expect("txn");
    let accounts = test_accounts(&mut ndb, &txn, Vec::new());
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let read_model = RemoteOutboxReadModel::default();
    let mut scoped_sub_state = ScopedSubsState::default();
    let relay = NormRelayUrl::new("wss://bridge-command.example.com").expect("relay");
    let identity = ScopedSubIdentity::global(
        SubOwnerKey::new("bridge-command-owner"),
        SubKey::new("bridge-command-key"),
    );
    let config = explicit_config(relay, Filter::new().kinds([1]).limit(10).build());

    {
        let mut remote = crate::RemoteApi::new(sender, &read_model, &mut scoped_sub_state);
        remote.on_selected_account_changed(&accounts);
        {
            let mut scoped_subs = remote.scoped_subs(&accounts);

            assert_eq!(
                scoped_subs.ensure_sub(identity, config.clone()),
                EnsureSubResult::Created
            );
            assert_eq!(
                scoped_subs.ensure_sub(identity, config),
                EnsureSubResult::AlreadyExists
            );
        }
        remote.flush();
    }

    let mut ensure_commands = 0;
    let mut set_commands = 0;
    while let Ok(input) = receiver.try_recv() {
        let RemoteBridgeInput::Ui(batch) = input else {
            continue;
        };
        let sections = batch.sections();
        assert_eq!(
            sections.len(),
            1,
            "account snapshot and scoped-sub commands should share one section"
        );
        assert!(
            sections[0].account_changed().is_some(),
            "scoped-sub command batch should carry account snapshot changes explicitly"
        );
        for intent in sections[0].intents() {
            match intent {
                RemoteIntent::ScopedSub(ScopedSubCommand::EnsureOwnerConfig {
                    owner, key, ..
                }) => {
                    assert_eq!(*owner, identity.owner);
                    assert_eq!(*key, identity.key);
                    ensure_commands += 1;
                }
                RemoteIntent::ScopedSub(ScopedSubCommand::SetOwnerConfig { .. }) => {
                    set_commands += 1;
                }
                _ => {}
            }
        }
    }

    assert_eq!(ensure_commands, 1);
    assert_eq!(set_commands, 0);
}

#[test]
fn ensure_existing_scoped_sub_new_owner_emits_ensure_owner_config_command() {
    let (_tmp, mut ndb) = test_ndb();
    let txn = Transaction::new(&ndb).expect("txn");
    let accounts = test_accounts(&mut ndb, &txn, Vec::new());
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let read_model = RemoteOutboxReadModel::default();
    let mut scoped_sub_state = ScopedSubsState::default();
    let relay = NormRelayUrl::new("wss://bridge-add-owner.example.com").expect("relay");
    let key = SubKey::new("bridge-add-owner-key");
    let first_owner = SubOwnerKey::new("bridge-add-owner-first");
    let second_owner = SubOwnerKey::new("bridge-add-owner-second");
    let first_identity = ScopedSubIdentity::global(first_owner, key);
    let second_identity = ScopedSubIdentity::global(second_owner, key);
    let config = explicit_config(relay, Filter::new().kinds([1]).limit(10).build());

    {
        let mut remote = crate::RemoteApi::new(sender, &read_model, &mut scoped_sub_state);
        remote.on_selected_account_changed(&accounts);
        {
            let mut scoped_subs = remote.scoped_subs(&accounts);

            assert_eq!(
                scoped_subs.ensure_sub(first_identity, config.clone()),
                EnsureSubResult::Created
            );
            assert_eq!(
                scoped_subs.ensure_sub(second_identity, config),
                EnsureSubResult::AlreadyExists
            );
        }
        remote.flush();
    }

    let mut set_commands = 0;
    let mut ensure_commands = 0;
    while let Ok(input) = receiver.try_recv() {
        let RemoteBridgeInput::Ui(batch) = input else {
            continue;
        };
        let sections = batch.sections();
        assert_eq!(
            sections.len(),
            1,
            "account snapshot and scoped-sub commands should share one section"
        );
        assert!(
            sections[0].account_changed().is_some(),
            "scoped-sub command batch should carry account snapshot changes explicitly"
        );
        for intent in sections[0].intents() {
            match intent {
                RemoteIntent::ScopedSub(ScopedSubCommand::EnsureOwnerConfig {
                    owner,
                    key: command_key,
                    ..
                }) => {
                    assert!([first_owner, second_owner].contains(owner));
                    assert_eq!(*command_key, key);
                    ensure_commands += 1;
                }
                RemoteIntent::ScopedSub(ScopedSubCommand::SetOwnerConfig {
                    key: command_key,
                    ..
                }) => {
                    assert_eq!(*command_key, key);
                    set_commands += 1;
                }
                _ => {}
            }
        }
    }

    assert_eq!(ensure_commands, 2);
    assert_eq!(set_commands, 0);
}

#[test]
fn account_change_after_queued_publish_stays_in_one_frame_batch() {
    let (_tmp, mut ndb) = test_ndb();
    let txn = Transaction::new(&ndb).expect("txn");
    let accounts = test_accounts(&mut ndb, &txn, Vec::new());
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let read_model = RemoteOutboxReadModel::default();
    let mut scoped_sub_state = ScopedSubsState::default();
    let publish_relay =
        NormRelayUrl::new("wss://pre-account-publish.example.com").expect("publish relay");

    {
        let mut remote = crate::RemoteApi::new(sender, &read_model, &mut scoped_sub_state);
        remote
            .publisher_explicit()
            .publish_event_json("{}".to_owned(), vec![RelayId::Websocket(publish_relay)]);
        remote.on_selected_account_changed(&accounts);
        assert!(
            receiver.try_recv().is_err(),
            "account change should not flush the frame-local batch"
        );
        remote.flush();
    }

    let RemoteBridgeInput::Ui(batch) = receiver.try_recv().expect("frame batch") else {
        panic!("expected UI batch");
    };
    assert!(
        receiver.try_recv().is_err(),
        "frame work should remain one bridge input"
    );

    let sections = batch.sections();
    assert_eq!(sections.len(), 2);
    assert!(sections[0].account_changed().is_none());
    assert!(matches!(
        sections[0].intents(),
        [RemoteIntent::Publish(RemotePublishCommand::Explicit { .. })]
    ));
    assert!(sections[1].account_changed().is_some());
    assert!(sections[1].intents().is_empty());
}

async fn create_text_capture_relay() -> (String, Arc<Mutex<Vec<String>>>, Arc<Notify>) {
    let (_handle, relay, captured, notify) = create_shared_text_capture_relay().await;
    (relay.to_string(), captured, notify)
}

async fn wait_for_frame(
    remote: &mut RemoteState,
    captured: &Arc<Mutex<Vec<String>>>,
    notify: &Arc<Notify>,
    context: &str,
    mut predicate: impl FnMut(&str) -> bool,
) -> String {
    wait_for_condition(
        remote,
        Duration::from_secs(2),
        Some(notify),
        context,
        |_| {
            captured
                .lock()
                .expect("lock captured text frames")
                .iter()
                .find(|text| predicate(text))
                .cloned()
        },
    )
    .await
}

fn captured_neg_open(captured: &Arc<Mutex<Vec<String>>>) -> Option<String> {
    captured
        .lock()
        .expect("lock captured text frames")
        .iter()
        .find(|text| text.starts_with("[\"NEG-OPEN\","))
        .cloned()
}

async fn create_event_relay(event_json: String) -> (String, Arc<Mutex<Vec<String>>>, Arc<Notify>) {
    let (_handle, relay, captured, notify) = create_filtered_capture_relay_with_handler(
        |_| true,
        move || {
            let event_json = event_json.clone();
            move |text: &str| {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
                    return CaptureRelayResponse::none();
                };
                if value.get(0).and_then(serde_json::Value::as_str) != Some("REQ") {
                    return CaptureRelayResponse::none();
                }
                let Some(sub_id) = value.get(1).and_then(serde_json::Value::as_str) else {
                    return CaptureRelayResponse::none();
                };
                CaptureRelayResponse {
                    send_text: vec![
                        format!(r#"["EVENT","{sub_id}",{event_json}]"#),
                        format!(r#"["EOSE","{sub_id}"]"#),
                    ],
                    close: false,
                }
            }
        },
    )
    .await;
    (relay.to_string(), captured, notify)
}

async fn wait_for_condition<T>(
    remote: &mut RemoteState,
    timeout: Duration,
    notify: Option<&Arc<Notify>>,
    context: &str,
    mut condition: impl FnMut(&mut RemoteState) -> Option<T>,
) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        remote.poll_bridge();
        if let Some(value) = condition(remote) {
            return value;
        }

        let now = Instant::now();
        assert!(now < deadline, "timed out waiting for {context}");
        let sleep_for = deadline
            .checked_duration_since(now)
            .expect("remaining wait")
            .min(Duration::from_millis(20));
        if let Some(notify) = notify {
            let _ = tokio::time::timeout(sleep_for, notify.notified()).await;
        } else {
            tokio::time::sleep(sleep_for).await;
        }
    }
}

#[tokio::test]
async fn notedeck_init_seeds_selected_account_before_immediate_scoped_sub() {
    let (relay_url, captured, notify) = create_text_capture_relay().await;
    let tmp = TempDir::new().expect("tmp dir");
    let ui_ctx = egui::Context::default();
    let args = vec![
        "notedeck-test".to_owned(),
        "--testrunner".to_owned(),
        "--relay".to_owned(),
        relay_url,
    ];
    let mut notedeck = Notedeck::init(&ui_ctx, tmp.path(), &args);
    let identity = ScopedSubIdentity::global(
        SubOwnerKey::new("remote/startup-seed"),
        SubKey::new("immediate-home"),
    );
    let config = SubConfig::builder(vec![Filter::new().kinds([1]).limit(1).build()])
        .accounts_read(important_policy())
        .build();

    {
        let mut app_ctx = notedeck.app_context();
        {
            let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);
            assert_eq!(
                scoped_subs.ensure_sub(identity, config),
                EnsureSubResult::Created
            );
        }
        app_ctx.remote.flush();
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        notedeck.tick(&ui_ctx);
        if let Some(frame) = captured
            .lock()
            .expect("lock captured startup relay frames")
            .iter()
            .find(|text| text.starts_with("[\"REQ\",") && text.contains("\"kinds\":[1]"))
            .cloned()
        {
            assert!(frame.contains("\"limit\":1"));
            break;
        }

        let now = Instant::now();
        assert!(
            now < deadline,
            "timed out waiting for immediate scoped-sub startup REQ"
        );
        let sleep_for = deadline
            .checked_duration_since(now)
            .expect("remaining wait")
            .min(Duration::from_millis(20));
        let _ = tokio::time::timeout(sleep_for, notify.notified()).await;
    }
}

#[tokio::test]
async fn scoped_sub_full_history_runs_inside_bridge() {
    let (relay_url, captured, notify) = create_text_capture_relay().await;
    let (_tmp, mut ndb) = test_ndb();
    let txn = Transaction::new(&ndb).expect("txn");
    let accounts = test_accounts(&mut ndb, &txn, Vec::new());
    drop(txn);
    let job_pool = JobPool::new(2);
    let mut remote = test_remote_state(&ndb, &job_pool);
    let relay = NormRelayUrl::new(&relay_url).expect("relay");
    let identity = ScopedSubIdentity::global(
        SubOwnerKey::new("remote/full-history/bridge-drop"),
        SubKey::new("home"),
    );

    {
        let mut api = remote.api();
        api.on_selected_account_changed(&accounts);
        let _ = api
            .scoped_subs(&accounts)
            .ensure_sub(identity, explicit_full_history_config(relay));
        api.flush();
    }

    let _ = wait_for_condition(
        &mut remote,
        Duration::from_secs(2),
        Some(&notify),
        "bridge NEG-OPEN",
        |_| captured_neg_open(&captured),
    )
    .await;
}

#[tokio::test]
async fn bridge_relay_ingest_writes_events_into_ndb() {
    let (event_json, note_id) = signed_text_note_json("bridge ingest", 1_776_000_200);
    let (relay_url, captured, notify) = create_event_relay(event_json).await;
    let (_tmp, mut ndb) = test_ndb();
    let txn = Transaction::new(&ndb).expect("txn");
    let accounts = test_accounts(&mut ndb, &txn, Vec::new());
    drop(txn);
    let job_pool = JobPool::new(2);
    let mut remote = test_remote_state(&ndb, &job_pool);
    let relay = NormRelayUrl::new(&relay_url).expect("relay");
    let identity = ScopedSubIdentity::global(
        SubOwnerKey::new("remote/live/bridge-ingest"),
        SubKey::new("home"),
    );

    {
        let mut api = remote.api();
        api.on_selected_account_changed(&accounts);
        let _ = api.scoped_subs(&accounts).ensure_sub(
            identity,
            explicit_config(relay, Filter::new().kinds(vec![1]).limit(10).build()),
        );
        api.flush();
    }

    wait_for_condition(
        &mut remote,
        Duration::from_secs(2),
        Some(&notify),
        "bridge live REQ",
        |_| {
            captured
                .lock()
                .expect("lock captured event relay frames")
                .iter()
                .any(|text| text.starts_with("[\"REQ\","))
                .then_some(())
        },
    )
    .await;

    wait_for_condition(
        &mut remote,
        Duration::from_secs(2),
        Some(&notify),
        "bridge note ingest",
        |_| {
            let Ok(txn) = Transaction::new(&ndb) else {
                return None;
            };
            ndb.get_note_by_id(&txn, note_id.bytes()).ok().map(|_| ())
        },
    )
    .await;
}

#[tokio::test]
async fn bridge_eose_fact_updates_scoped_readiness() {
    let (event_json, _) = signed_text_note_json("bridge readiness eose", 1_776_000_250);
    let (relay_url, captured, notify) = create_event_relay(event_json).await;
    let (_tmp, mut ndb) = test_ndb();
    let txn = Transaction::new(&ndb).expect("txn");
    let accounts = test_accounts(&mut ndb, &txn, Vec::new());
    drop(txn);
    let job_pool = JobPool::new(2);
    let mut remote = test_remote_state(&ndb, &job_pool);
    let relay = NormRelayUrl::new(&relay_url).expect("relay");
    let identity = ScopedSubIdentity::global(
        SubOwnerKey::new("remote/live/eose-readiness"),
        SubKey::new("home"),
    );

    {
        let mut api = remote.api();
        api.on_selected_account_changed(&accounts);
        let _ = api.scoped_subs(&accounts).ensure_sub(
            identity,
            explicit_config(relay, Filter::new().kinds(vec![1]).limit(10).build()),
        );
        api.flush();
    }

    wait_for_condition(
        &mut remote,
        Duration::from_secs(2),
        Some(&notify),
        "bridge scoped readiness EOSE",
        |remote| {
            let mut api = remote.api();
            let status = api.scoped_subs(&accounts).sub_readiness(identity);
            matches!(
                status,
                ScopedSubReadiness::Live(live) if live.relay_eose.all_eosed
            )
            .then_some(())
        },
    )
    .await;

    assert!(captured
        .lock()
        .expect("lock captured event relay frames")
        .iter()
        .any(|text| text.starts_with("[\"REQ\",")));
}

#[tokio::test]
async fn publish_explicit_broadcasts_to_requested_relay() {
    let (relay_url, captured, notify) = create_text_capture_relay().await;
    let (_tmp, ndb) = test_ndb();
    let job_pool = JobPool::new(2);
    let mut remote = test_remote_state(&ndb, &job_pool);
    let note = signed_text_note("publish explicit", 1_776_000_300);
    let relay = NormRelayUrl::new(&relay_url).expect("relay");

    {
        let mut api = remote.api();
        api.publisher_explicit()
            .publish_note(&note, vec![RelayId::Websocket(relay)]);
        api.flush();
    }

    let note_json = note.json().expect("note json");
    let frame = wait_for_frame(
        &mut remote,
        &captured,
        &notify,
        "explicit publish EVENT",
        |text| text.starts_with("[\"EVENT\",") && text.contains(&note_json),
    )
    .await;
    assert!(frame.contains("publish explicit"));
}

#[tokio::test]
async fn publish_accounts_write_broadcasts_to_selected_account_write_relays() {
    let (relay_url, captured, notify) = create_text_capture_relay().await;
    let (_tmp, mut ndb) = test_ndb();
    let txn = Transaction::new(&ndb).expect("txn");
    let accounts = test_accounts(&mut ndb, &txn, vec![relay_url.clone()]);
    drop(txn);
    let job_pool = JobPool::new(2);
    let mut remote = test_remote_state(&ndb, &job_pool);
    let note = signed_text_note("publish account-write", 1_776_000_301);

    {
        let mut api = remote.api();
        api.on_selected_account_changed(&accounts);
        api.publisher().accounts_write().publish_note(&note);
        api.flush();
    }

    let note_json = note.json().expect("note json");
    let frame = wait_for_frame(
        &mut remote,
        &captured,
        &notify,
        "accounts-write publish EVENT",
        |text| text.starts_with("[\"EVENT\",") && text.contains(&note_json),
    )
    .await;
    assert!(frame.contains("publish account-write"));
}

#[tokio::test]
async fn oneshot_uses_selected_account_read_relays() {
    let (relay_url, captured, notify) = create_text_capture_relay().await;
    let (_tmp, mut ndb) = test_ndb();
    let txn = Transaction::new(&ndb).expect("txn");
    let accounts = test_accounts(&mut ndb, &txn, vec![relay_url]);
    drop(txn);
    let job_pool = JobPool::new(2);
    let mut remote = test_remote_state(&ndb, &job_pool);

    {
        let mut api = remote.api();
        api.on_selected_account_changed(&accounts);
        api.oneshot()
            .oneshot(vec![Filter::new().kinds(vec![1]).limit(1).build()]);
        api.flush();
    }

    let frame = wait_for_frame(&mut remote, &captured, &notify, "oneshot REQ", |text| {
        text.starts_with("[\"REQ\",") && text.contains("\"kinds\":[1]")
    })
    .await;
    assert!(frame.contains("\"limit\":1"));
}
