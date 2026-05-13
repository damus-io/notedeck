use super::*;
use crate::{
    jobs::{JobOutput, JobPackage, JobRun, RunType},
    relay_limits::{Nip11FetchError, RelayLimitJobKind, RelayLimitJobResult},
    Accounts, FullHistoryConfig, JobPool, ScopedSubIdentity, SubConfig, SubKey, SubOwnerKey,
    UnknownIds, FALLBACK_PUBKEY,
};
use enostr::{FullKeypair, NormRelayUrl, NoteId, OutboxRecvBudget, OutboxSubId, RelayUrlPkgs};
use futures_util::StreamExt;
use hashbrown::HashSet;
use nostr::{Event, JsonUtil};
use nostr_relay_builder::{
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use nostrdb::{Config, Filter, NoteBuilder, Transaction};
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime},
};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::Notify};
use tokio_tungstenite::{accept_async, tungstenite::Message};

fn test_ndb() -> (TempDir, Ndb) {
    let tmp = TempDir::new().expect("tmp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    (tmp, ndb)
}

fn test_accounts(ndb: &mut Ndb, txn: &Transaction) -> Accounts {
    let mut unknown_ids = UnknownIds::default();
    Accounts::new(
        None,
        Vec::new(),
        FALLBACK_PUBKEY(),
        ndb,
        txn,
        &mut unknown_ids,
    )
}

fn explicit_full_history_config(relay: &str) -> SubConfig {
    let mut relays = HashSet::new();
    relays.insert(NormRelayUrl::new(relay).expect("relay"));
    let filter = vec![Filter::new().kinds(vec![1]).limit(10).build()];
    SubConfig::live(filter.clone())
        .explicit_relays(relays)
        .full_history(FullHistoryConfig::new(filter))
        .build()
}
fn explicit_since_history_config(relay: &str) -> SubConfig {
    let mut relays = HashSet::new();
    relays.insert(NormRelayUrl::new(relay).expect("relay"));
    let filter = vec![Filter::new()
        .kinds(vec![1])
        .since(1_700_000_000)
        .limit(25)
        .build()];
    SubConfig::live(filter.clone())
        .explicit_relays(relays)
        .full_history(FullHistoryConfig::new(filter))
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

async fn create_text_capture_relay() -> (
    tokio::task::JoinHandle<()>,
    String,
    Arc<Mutex<Vec<String>>>,
    Arc<Notify>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind text capture relay");
    let addr = listener.local_addr().expect("text capture relay addr");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_task = Arc::clone(&captured);
    let notify = Arc::new(Notify::new());
    let notify_task = Arc::clone(&notify);

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let captured_task = Arc::clone(&captured_task);
            let notify_task = Arc::clone(&notify_task);
            tokio::spawn(async move {
                let Ok(mut websocket) = accept_async(stream).await else {
                    return;
                };

                while let Some(msg) = websocket.next().await {
                    let Ok(Message::Text(text)) = msg else {
                        continue;
                    };

                    captured_task
                        .lock()
                        .expect("lock captured text frames")
                        .push(text.to_string());
                    notify_task.notify_one();
                }
            });
        }
    });

    (handle, format!("ws://{addr}"), captured, notify)
}

fn captured_neg_open(captured: &Arc<Mutex<Vec<String>>>) -> Option<String> {
    captured
        .lock()
        .expect("lock captured text frames")
        .iter()
        .find(|text| text.starts_with("[\"NEG-OPEN\","))
        .cloned()
}

async fn wait_for_remote_condition<T>(
    remote: &mut RemoteState,
    ctx: &egui::Context,
    ndb: &Ndb,
    timeout: Duration,
    notify: Option<&Arc<Notify>>,
    context: &str,
    mut condition: impl FnMut(&mut RemoteState) -> Option<T>,
) -> T {
    let deadline = Instant::now() + timeout;

    loop {
        remote.process_events(ctx, ndb);
        if let Some(value) = condition(remote) {
            return value;
        }

        let now = Instant::now();
        assert!(now < deadline, "timed out waiting for {context}");
        let remaining = deadline
            .checked_duration_since(now)
            .expect("remaining remote wait");

        if let Some(notify) = notify {
            let _ =
                tokio::time::timeout(remaining.min(Duration::from_millis(20)), notify.notified())
                    .await;
        } else {
            tokio::time::sleep(remaining.min(Duration::from_millis(20))).await;
        }
    }
}

async fn wait_for_neg_open_after_remote_api_drop(
    remote: &mut RemoteState,
    ctx: &egui::Context,
    ndb: &Ndb,
    captured: &Arc<Mutex<Vec<String>>>,
    notify: &Arc<Notify>,
    timeout: Duration,
) -> String {
    wait_for_remote_condition(remote, ctx, ndb, timeout, Some(notify), "NEG-OPEN", |_| {
        captured_neg_open(captured)
    })
    .await
}
async fn wait_for_note_ingest_after_process_events(
    remote: &mut RemoteState,
    ctx: &egui::Context,
    ndb: &Ndb,
    note_id: NoteId,
    notify: &Arc<Notify>,
    timeout: Duration,
) {
    wait_for_remote_condition(
        remote,
        ctx,
        ndb,
        timeout,
        Some(notify),
        "note ingest",
        |_| {
            if let Ok(txn) = Transaction::new(ndb) {
                if ndb.get_note_by_id(&txn, note_id.bytes()).is_ok() {
                    return Some(());
                }
            }

            None
        },
    )
    .await
}
async fn wait_for_websocket_relay(
    remote: &mut RemoteState,
    ctx: &egui::Context,
    ndb: &Ndb,
    relay: &NormRelayUrl,
    timeout: Duration,
) {
    wait_for_remote_condition(
        remote,
        ctx,
        ndb,
        timeout,
        None,
        "relay websocket",
        |remote| {
            remote
                .pool
                .websocket_statuses()
                .into_keys()
                .any(|url| *url == *relay)
                .then_some(())
        },
    )
    .await
}
async fn wait_for_routed_sub_status(
    remote: &mut RemoteState,
    ctx: &egui::Context,
    ndb: &Ndb,
    sub_id: OutboxSubId,
    timeout: Duration,
) {
    wait_for_remote_condition(
        remote,
        ctx,
        ndb,
        timeout,
        None,
        "routed sub status",
        |remote| (!remote.pool.status(&sub_id).is_empty()).then_some(()),
    )
    .await
}
async fn create_seeded_local_relay(event_json: String) -> (LocalRelay, String) {
    create_seeded_local_relay_events(vec![event_json]).await
}

async fn create_seeded_local_relay_events(event_jsons: Vec<String>) -> (LocalRelay, String) {
    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });
    for event_json in event_jsons {
        let event = Event::from_json(event_json).expect("parse relay event");
        relay_db.save_event(&event).await.expect("save relay event");
    }
    let relay = LocalRelay::run(RelayBuilder::default().database(relay_db))
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();
    (relay, relay_url)
}
#[tokio::test]
async fn remote_api_drop_starts_negentropy_during_session_finalize() {
    let (_relay_task, relay_url, captured, notify) = create_text_capture_relay().await;
    let (_tmp, mut ndb) = test_ndb();
    let txn = Transaction::new(&ndb).expect("txn");
    let accounts = test_accounts(&mut ndb, &txn);
    let job_pool = JobPool::default();
    let mut remote = RemoteState::new(&ndb, job_pool.spawner());
    let ctx = egui::Context::default();
    let identity = ScopedSubIdentity::global(
        SubOwnerKey::new("remote/full-history/drop"),
        SubKey::new("home"),
    );

    {
        let mut api = remote.api(&ctx);
        let _ = api
            .scoped_subs(&accounts)
            .ensure_sub(identity, explicit_full_history_config(&relay_url));
    }

    let _ = wait_for_neg_open_after_remote_api_drop(
        &mut remote,
        &ctx,
        &ndb,
        &captured,
        &notify,
        Duration::from_secs(2),
    )
    .await;
}
#[tokio::test]
async fn remote_api_drop_preserves_explicit_bounds_for_full_history() {
    let (_relay_task, relay_url, captured, notify) = create_text_capture_relay().await;
    let (_tmp, mut ndb) = test_ndb();
    let txn = Transaction::new(&ndb).expect("txn");
    let accounts = test_accounts(&mut ndb, &txn);
    let job_pool = JobPool::default();
    let mut remote = RemoteState::new(&ndb, job_pool.spawner());
    let ctx = egui::Context::default();
    let identity = ScopedSubIdentity::global(
        SubOwnerKey::new("remote/full-history/drop"),
        SubKey::new("bounded-home"),
    );

    {
        let mut api = remote.api(&ctx);
        let _ = api
            .scoped_subs(&accounts)
            .ensure_sub(identity, explicit_since_history_config(&relay_url));
    }

    let frame = wait_for_neg_open_after_remote_api_drop(
        &mut remote,
        &ctx,
        &ndb,
        &captured,
        &notify,
        Duration::from_secs(2),
    )
    .await;
    let frame: Value = serde_json::from_str(&frame).expect("parse neg-open frame");
    let filter = &frame[2];
    assert_eq!(filter["limit"].as_u64(), Some(25));
    assert_eq!(filter["since"].as_u64(), Some(1_700_000_000));
}
#[tokio::test]
async fn process_events_ingests_received_relay_events_into_ndb() {
    let (event_json, note_id) = signed_text_note_json("remote-control ingest", 1_776_000_200);
    let (_relay, relay_url) = create_seeded_local_relay(event_json).await;
    let (_tmp, ndb) = test_ndb();
    let job_pool = JobPool::default();
    let mut remote = RemoteState::new(&ndb, job_pool.spawner());
    let ctx = egui::Context::default();
    let relay = NormRelayUrl::new(&relay_url).expect("relay");
    let sub_id = {
        let mut relays = HashSet::new();
        relays.insert(relay.clone());
        let mut session = remote
            .pool
            .start_session(crate::EguiWakeup::new(ctx.clone()));
        session.subscribe(
            vec![Filter::new().kinds(vec![1]).limit(10).build()],
            RelayUrlPkgs::new(relays),
        )
    };

    wait_for_websocket_relay(&mut remote, &ctx, &ndb, &relay, Duration::from_secs(2)).await;
    wait_for_routed_sub_status(&mut remote, &ctx, &ndb, sub_id, Duration::from_secs(2)).await;
    wait_for_note_ingest_after_process_events(
        &mut remote,
        &ctx,
        &ndb,
        note_id,
        &Arc::new(Notify::new()),
        Duration::from_secs(2),
    )
    .await;
}

#[tokio::test]
async fn process_events_requests_repaint_after_time_budget_exhaustion() {
    let (first_event_json, _first_note_id) =
        signed_text_note_json("remote-control budget first", 1_776_000_201);
    let (second_event_json, _second_note_id) =
        signed_text_note_json("remote-control budget second", 1_776_000_202);
    let (_relay, relay_url) =
        create_seeded_local_relay_events(vec![first_event_json, second_event_json]).await;
    let (_tmp, ndb) = test_ndb();
    let job_pool = JobPool::default();
    let mut remote = RemoteState::new(&ndb, job_pool.spawner());
    let ctx = egui::Context::default();
    let relay = NormRelayUrl::new(&relay_url).expect("relay");
    let sub_id = {
        let mut relays = HashSet::new();
        relays.insert(relay.clone());
        let mut session = remote
            .pool
            .start_session(crate::EguiWakeup::new(ctx.clone()));
        session.subscribe(
            vec![Filter::new().kinds(vec![1]).limit(10).build()],
            RelayUrlPkgs::new(relays),
        )
    };

    wait_for_websocket_relay(&mut remote, &ctx, &ndb, &relay, Duration::from_secs(2)).await;
    wait_for_routed_sub_status(&mut remote, &ctx, &ndb, sub_id, Duration::from_secs(2)).await;

    let repaint_count = Arc::new(AtomicUsize::new(0));
    let repaint_count_for_callback = Arc::clone(&repaint_count);
    ctx.set_request_repaint_callback(move |_| {
        repaint_count_for_callback.fetch_add(1, Ordering::SeqCst);
    });

    wait_for_remote_condition(
        &mut remote,
        &ctx,
        &ndb,
        Duration::from_secs(2),
        None,
        "time-budget repaint",
        |remote| {
            remote.process_events_for_test(
                &ctx,
                &ndb,
                OutboxRecvBudget::until(Instant::now() - Duration::from_millis(1)),
            );
            (repaint_count.load(Ordering::SeqCst) > 0).then_some(())
        },
    )
    .await;
}

#[tokio::test]
async fn service_relays_records_completed_nip11_failures_and_retries_later() {
    let _relay_backend = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay = NormRelayUrl::new(&_relay_backend.url()).expect("relay");
    let (_tmp, ndb) = test_ndb();
    let ctx = egui::Context::default();
    let mut job_pool = JobPool::default();
    let mut remote = RemoteState::new(&ndb, job_pool.spawner());

    {
        let mut session = remote
            .pool
            .start_session(crate::EguiWakeup::new(ctx.clone()));
        let mut relays = HashSet::new();
        relays.insert(relay.clone());
        session.subscribe(
            vec![Filter::new().kinds(vec![1]).limit(10).build()],
            RelayUrlPkgs::new(relays),
        );
    }

    wait_for_websocket_relay(&mut remote, &ctx, &ndb, &relay, Duration::from_secs(2)).await;

    let requested_at = SystemTime::now();
    let initial = remote.pool.take_nip11_fetch_requests(1, requested_at);
    assert_eq!(
        initial.len(),
        1,
        "relay should be ready for an initial NIP-11 fetch"
    );

    let package = JobPackage::new(
        relay.to_string(),
        RelayLimitJobKind::Nip11Fetch,
        RunType::Output(JobRun::Sync(Box::new({
            let relay = relay.clone();
            move || {
                JobOutput::complete(RelayLimitJobResult {
                    relay,
                    result: Err(Nip11FetchError::Json("invalid".to_owned())),
                })
            }
        }))),
    );
    remote
        .relay_limit_jobs
        .sender()
        .send(package)
        .expect("enqueue relay-limit completion");

    for _ in 0..20 {
        remote.service_relays(&mut job_pool);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let retry_ready = remote
        .pool
        .take_nip11_fetch_requests(1, requested_at + Duration::from_secs(20));
    assert_eq!(retry_ready.len(), 1);
    assert_eq!(retry_ready[0].relay, relay);
    assert_eq!(retry_ready[0].attempt, initial[0].attempt + 1);
}
