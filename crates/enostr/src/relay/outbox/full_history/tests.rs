use futures_util::{SinkExt, StreamExt};
use hashbrown::HashSet;
use negentropy::{Id, Negentropy, NegentropyStorageVector};
use nostrdb::Filter;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
    sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use tokio::{net::TcpListener, sync::Notify};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use super::full_history::{
    FullHistoryFetchRetryState, FULL_HISTORY_PRESENCE_CHECK_BUDGET,
    MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID,
};
use super::*;
use crate::relay::{
    negentropy::{EventChecker, NegSetProvider},
    test_utils::{filters_json, trivial_filter, MockWakeup},
    FullHistoryConfig, RelayUrlPkgs,
};
use crate::NoteId;
use crate::Wakeup;

const NEG_OPEN_PREFIX: &str = r#"["NEG-OPEN","#;
const NEG_CLOSE_PREFIX: &str = r#"["NEG-CLOSE","#;

struct ReadyNegSetProvider;

impl NegSetProvider for ReadyNegSetProvider {
    fn provide(&self, _filter: &Filter) -> oneshot::Receiver<NegentropyStorageVector> {
        let (tx, rx) = oneshot::channel();
        let mut storage = NegentropyStorageVector::new();
        storage.seal().expect("test negentropy storage should seal");
        tx.send(storage)
            .expect("test negentropy receiver should accept one storage");
        rx
    }
}

struct CountingReadyNegSetProvider {
    calls: Arc<AtomicUsize>,
}

impl NegSetProvider for CountingReadyNegSetProvider {
    fn provide(&self, _filter: &Filter) -> oneshot::Receiver<NegentropyStorageVector> {
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        ReadyNegSetProvider.provide(_filter)
    }
}

struct PendingNegSetProvider {
    senders: Arc<Mutex<Vec<oneshot::Sender<NegentropyStorageVector>>>>,
}

impl NegSetProvider for PendingNegSetProvider {
    fn provide(&self, _filter: &Filter) -> oneshot::Receiver<NegentropyStorageVector> {
        let (tx, rx) = oneshot::channel();
        self.senders
            .lock()
            .expect("lock pending neg set senders")
            .push(tx);
        rx
    }
}

struct SelectiveEventChecker {
    present: HashSet<NoteId>,
}

impl EventChecker for SelectiveEventChecker {
    fn retain_missing(&self, ids: &mut HashSet<NoteId>) {
        ids.retain(|id| !self.present.contains(id));
    }
}

struct BatchRecordingEventChecker {
    present: HashSet<NoteId>,
    batches: Arc<Mutex<Vec<Vec<NoteId>>>>,
}

impl EventChecker for BatchRecordingEventChecker {
    fn retain_missing(&self, ids: &mut HashSet<NoteId>) {
        self.batches
            .lock()
            .expect("lock recorded checker batches")
            .push(ids.iter().copied().collect());
        ids.retain(|id| !self.present.contains(id));
    }
}

#[derive(Clone, Debug)]
enum CaptureRelayMode {
    Silent,
    NegErrOnOpen(&'static str),
    NoticeOnOpen(&'static str),
    InvalidNegMsgOnOpen(&'static str),
    DisconnectOnOpenOnce(Arc<AtomicBool>),
    DelayedValidNegMsgOnClose([u8; 32]),
}

async fn create_capture_relay_with_mode(
    mode: CaptureRelayMode,
) -> (
    tokio::task::JoinHandle<()>,
    NormRelayUrl,
    Arc<Mutex<Vec<String>>>,
    Arc<Notify>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind text capture relay");
    let addr = listener.local_addr().expect("text capture relay addr");
    let url = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid text capture relay url");
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
            let mode = mode.clone();
            tokio::spawn(async move {
                let Ok(mut websocket) = accept_async(stream).await else {
                    return;
                };
                let mut delayed_replies = HashMap::<String, String>::new();

                while let Some(msg) = websocket.next().await {
                    let Ok(Message::Text(text)) = msg else {
                        continue;
                    };
                    let text = text.to_string();

                    captured_task
                        .lock()
                        .expect("lock captured text frames")
                        .push(text.clone());
                    notify_task.notify_one();

                    let parsed: Value =
                        serde_json::from_str(&text).expect("parse captured relay frame");
                    let Some(frame) = parsed.as_array() else {
                        continue;
                    };
                    let Some(kind) = frame.first().and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(session_id) = frame.get(1).and_then(Value::as_str) else {
                        continue;
                    };

                    match mode {
                        CaptureRelayMode::Silent => {}
                        CaptureRelayMode::NegErrOnOpen(reason) if kind == "NEG-OPEN" => {
                            let err = format!(r#"["NEG-ERR","{session_id}","{reason}"]"#);
                            websocket
                                .send(Message::Text(err))
                                .await
                                .expect("send NEG-ERR");
                        }
                        CaptureRelayMode::NoticeOnOpen(reason) if kind == "NEG-OPEN" => {
                            let notice = format!(r#"["NOTICE","{reason}"]"#);
                            websocket
                                .send(Message::Text(notice))
                                .await
                                .expect("send NOTICE");
                        }
                        CaptureRelayMode::InvalidNegMsgOnOpen(payload) if kind == "NEG-OPEN" => {
                            let reply = format!(r#"["NEG-MSG","{session_id}","{payload}"]"#);
                            websocket
                                .send(Message::Text(reply))
                                .await
                                .expect("send invalid NEG-MSG");
                        }
                        CaptureRelayMode::DisconnectOnOpenOnce(ref did_disconnect)
                            if kind == "NEG-OPEN"
                                && !did_disconnect.swap(true, Ordering::SeqCst) =>
                        {
                            let _ = websocket.close(None).await;
                            break;
                        }
                        CaptureRelayMode::DelayedValidNegMsgOnClose(id) if kind == "NEG-OPEN" => {
                            let initial_hex = frame
                                .get(3)
                                .and_then(Value::as_str)
                                .expect("neg-open initial hex");
                            let mut storage = NegentropyStorageVector::new();
                            storage
                                .insert(1, Id::from_byte_array(id))
                                .expect("insert relay negentropy item");
                            storage.seal().expect("seal relay negentropy storage");
                            let mut session =
                                Negentropy::owned(storage, 0).expect("owned relay negentropy");
                            let initial_bytes =
                                hex::decode(initial_hex).expect("decode neg-open initial hex");
                            let reply = session
                                .reconcile(&initial_bytes)
                                .expect("reconcile neg-open");
                            delayed_replies.insert(session_id.to_owned(), hex::encode(reply));
                        }
                        CaptureRelayMode::DelayedValidNegMsgOnClose(_) if kind == "NEG-CLOSE" => {
                            if let Some(reply_hex) = delayed_replies.remove(session_id) {
                                let reply = format!(r#"["NEG-MSG","{session_id}","{reply_hex}"]"#);
                                websocket
                                    .send(Message::Text(reply))
                                    .await
                                    .expect("send delayed NEG-MSG");
                            }
                        }
                        _ => {}
                    }
                }
            });
        }
    });

    (handle, url, captured, notify)
}

async fn create_text_capture_relay() -> (
    tokio::task::JoinHandle<()>,
    NormRelayUrl,
    Arc<Mutex<Vec<String>>>,
    Arc<Notify>,
) {
    create_capture_relay_with_mode(CaptureRelayMode::Silent).await
}

async fn wait_for_captured_text<F>(
    captured: &Arc<Mutex<Vec<String>>>,
    notify: &Arc<Notify>,
    timeout: Duration,
    context: &str,
    predicate: F,
) -> String
where
    F: Fn(&str) -> bool,
{
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(frame) = captured
            .lock()
            .expect("lock captured text frames")
            .iter()
            .find(|text| predicate(text))
            .cloned()
        {
            return frame;
        }

        let snapshot = captured.lock().expect("lock captured text frames").clone();
        let now = Instant::now();
        assert!(
            now < deadline,
            "timed out waiting for {context}; captured {snapshot:?}"
        );

        let remaining = deadline
            .checked_duration_since(now)
            .expect("remaining text capture wait");
        if tokio::time::timeout(remaining, notify.notified())
            .await
            .is_err()
        {
            let snapshot = captured.lock().expect("lock captured text frames").clone();
            panic!("timed out waiting for {context}; captured {snapshot:?}");
        }
    }
}

async fn wait_for_websocket_connected(
    pool: &mut OutboxPool,
    relay: &NormRelayUrl,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;

    loop {
        pool.try_recv(|_| {});
        let connected = pool
            .websocket_statuses()
            .into_iter()
            .find_map(|(relay_url, status)| (*relay_url == *relay).then_some(status))
            == Some(RelayStatus::Connected);
        if connected {
            return;
        }

        let now = Instant::now();
        assert!(
            now < deadline,
            "relay should connect before the test continues"
        );

        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_pool_condition<F>(
    pool: &mut OutboxPool,
    timeout: Duration,
    context: &str,
    predicate: F,
) where
    F: Fn(&OutboxPool) -> bool,
{
    let deadline = Instant::now() + timeout;

    loop {
        pool.try_recv(|_| {});
        if predicate(pool) {
            return;
        }

        assert!(Instant::now() < deadline, "timed out waiting for {context}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn drive_transport_once(pool: &mut OutboxPool, wakeup: &MockWakeup) {
    let wake = wakeup.clone();
    pool.keepalive_ping(move || wake.wake());
    pool.try_recv(|_| {});
}

fn force_full_history_retries_due(pool: &mut OutboxPool, history_id: FullHistorySubId) {
    let Some(tracked) = pool.full_history.tracked_subs.get_mut(&history_id) else {
        return;
    };
    for retry in &mut tracked.progress.retry_states {
        if retry.next_retry_at.is_some() {
            retry.next_retry_at = Some(Instant::now());
        }
    }
}

fn ready_pool() -> OutboxPool {
    let mut pool = OutboxPool::default();
    pool.set_neg_set_provider(Box::new(ReadyNegSetProvider));
    pool
}

fn counting_ready_pool(calls: Arc<AtomicUsize>) -> OutboxPool {
    let mut pool = OutboxPool::default();
    pool.set_neg_set_provider(Box::new(CountingReadyNegSetProvider { calls }));
    pool
}

fn note_id(byte: u8) -> NoteId {
    NoteId::new([byte; 32])
}

fn unique_note_id(index: usize) -> NoteId {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&(index as u64).to_be_bytes());
    NoteId::new(bytes)
}

fn relay_url(name: &str) -> NormRelayUrl {
    NormRelayUrl::new(&format!("wss://relay-full-history-{name}.invalid")).unwrap()
}

fn filter_larger_than_default_json_buffer() -> Filter {
    let mut ids = Vec::new();
    for index in 0..18_000u64 {
        let mut id = [0u8; 32];
        id[..8].copy_from_slice(&index.to_be_bytes());
        ids.push(id);
    }
    let filter = Filter::new_with_capacity(512).ids(ids.iter()).build();
    assert!(
        filter.json().is_err(),
        "test filter should exceed Filter::json default buffer"
    );
    filter
}

fn pending_ingestion(relay: NormRelayUrl, started_at: Instant) -> PendingIngestion {
    PendingIngestion {
        relay,
        filter: trivial_filter()[0].clone(),
        started_at,
        retries_started: 0,
    }
}

fn fetch_retry_state(
    id: NoteId,
    relay: NormRelayUrl,
    next_retry_at: Instant,
) -> FullHistoryFetchRetryState {
    FullHistoryFetchRetryState {
        id,
        relay,
        filter: trivial_filter()[0].clone(),
        next_retries_started: 1,
        next_retry_at,
    }
}

fn full_history_need(
    history_id: FullHistorySubId,
    relay: NormRelayUrl,
    id: NoteId,
) -> FullHistoryNeed {
    FullHistoryNeed {
        history_id,
        relay,
        filter: trivial_filter()[0].clone(),
        id,
    }
}

fn tracked_sub(pool: &OutboxPool, history_id: FullHistorySubId) -> &TrackedFullHistorySub {
    pool.full_history
        .tracked_subs
        .get(&history_id)
        .expect("full-history sub should be tracked")
}

fn tracked_sub_mut(
    pool: &mut OutboxPool,
    history_id: FullHistorySubId,
) -> &mut TrackedFullHistorySub {
    pool.full_history
        .tracked_subs
        .get_mut(&history_id)
        .expect("full-history sub should be tracked")
}

fn queued_need_id_count(tracked: &TrackedFullHistorySub) -> usize {
    tracked
        .progress
        .pending_needs
        .iter()
        .map(|needs| needs.ids.len())
        .sum()
}

fn is_tracked(pool: &OutboxPool, history_id: FullHistorySubId) -> bool {
    pool.full_history.tracked_subs.contains_key(&history_id)
}

fn relay_data<'a>(pool: &'a OutboxPool, relay: &NormRelayUrl) -> &'a CoordinationData {
    pool.relays.get(relay).expect("relay tracked")
}

fn relay_data_mut<'a>(pool: &'a mut OutboxPool, relay: &NormRelayUrl) -> &'a mut CoordinationData {
    pool.relays.get_mut(relay).expect("relay tracked")
}

fn clear_pending_neg_sets(pool: &mut OutboxPool, history_id: FullHistorySubId) {
    let progress = &mut tracked_sub_mut(pool, history_id).progress;
    progress.pending_neg_sets.clear();
}

fn pending_neg_set_relays(
    pool: &OutboxPool,
    history_id: FullHistorySubId,
) -> HashSet<NormRelayUrl> {
    tracked_sub(pool, history_id)
        .progress
        .pending_neg_sets
        .iter()
        .flat_map(|pending| pending.relays.iter().cloned())
        .collect()
}

fn seed_relay_need(
    pool: &mut OutboxPool,
    relay: &NormRelayUrl,
    history_id: FullHistorySubId,
    id: NoteId,
) {
    relay_data_mut(pool, relay)
        .negentropy_data
        .seed_need_for_test(history_id, trivial_filter()[0].clone(), id);
}

fn seed_relay_retry(pool: &mut OutboxPool, relay: &NormRelayUrl, history_id: FullHistorySubId) {
    relay_data_mut(pool, relay)
        .negentropy_data
        .seed_retry_for_test(history_id, trivial_filter()[0].clone());
}

fn assert_active_sessions(pool: &OutboxPool, relay: &NormRelayUrl, count: usize) {
    assert_eq!(
        relay_data(pool, relay)
            .negentropy_data
            .active_session_count(),
        count
    );
}

fn relay_set(relays: impl IntoIterator<Item = NormRelayUrl>) -> HashSet<NormRelayUrl> {
    relays.into_iter().collect()
}

fn subscribe_with_history(
    pool: &mut OutboxPool,
    wakeup: MockWakeup,
    filters: Vec<Filter>,
    history_filters: Vec<Filter>,
    relays: impl IntoIterator<Item = NormRelayUrl>,
) -> FullHistorySubId {
    let mut handler = pool.start_session(wakeup);
    let relays = relay_set(relays);
    handler.subscribe(filters, RelayUrlPkgs::new(relays.clone()));
    handler.subscribe_full_history(FullHistoryConfig::new(history_filters), relays)
}

fn subscribe_history_only(
    pool: &mut OutboxPool,
    wakeup: MockWakeup,
    history_filters: Vec<Filter>,
    relays: impl IntoIterator<Item = NormRelayUrl>,
) -> FullHistorySubId {
    let mut handler = pool.start_session(wakeup);
    handler.subscribe_full_history(FullHistoryConfig::new(history_filters), relay_set(relays))
}

fn subscribe_unbounded(
    pool: &mut OutboxPool,
    wakeup: MockWakeup,
    relays: impl IntoIterator<Item = NormRelayUrl>,
) -> FullHistorySubId {
    let filters = trivial_filter();
    subscribe_with_history(pool, wakeup, filters.clone(), filters, relays)
}

fn counting_retry_fixture(
    relay_name: &str,
) -> (
    Arc<AtomicUsize>,
    OutboxPool,
    NormRelayUrl,
    FullHistorySubId,
    OutboxSession,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pool = counting_ready_pool(Arc::clone(&calls));
    let relay = relay_url(relay_name);
    let sub_id = subscribe_unbounded(&mut pool, MockWakeup::default(), [relay.clone()]);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    clear_pending_neg_sets(&mut pool, sub_id);
    (calls, pool, relay, sub_id, OutboxSession::default())
}

fn modify_relays_for_history(
    pool: &mut OutboxPool,
    wakeup: MockWakeup,
    history_id: FullHistorySubId,
    relays: impl IntoIterator<Item = NormRelayUrl>,
) {
    let mut handler = pool.start_session(wakeup);
    handler.modify_full_history(
        history_id,
        FullHistoryConfig::new(trivial_filter()),
        relay_set(relays),
    );
}

fn modify_unbounded_history(
    pool: &mut OutboxPool,
    wakeup: MockWakeup,
    history_id: FullHistorySubId,
    relays: impl IntoIterator<Item = NormRelayUrl>,
) {
    let mut handler = pool.start_session(wakeup);
    handler.modify_full_history(
        history_id,
        FullHistoryConfig::new(trivial_filter()),
        relay_set(relays),
    );
}

fn remove_full_history(pool: &mut OutboxPool, wakeup: MockWakeup, history_id: FullHistorySubId) {
    let mut handler = pool.start_session(wakeup);
    handler.remove_full_history(history_id);
}

fn full_history_fetch_ids_by_relay(session: &OutboxSession) -> HashMap<NormRelayUrl, OutboxSubId> {
    session
        .tasks
        .iter()
        .filter_map(|(id, task)| {
            let OutboxTask::FullHistoryFetch(fetch) = task else {
                return None;
            };
            let relay = fetch
                .subscribe
                .relays
                .urls
                .iter()
                .next()
                .expect("test fetch should target one relay")
                .clone();
            Some((relay, *id))
        })
        .collect()
}

fn neg_open_count(captured: &Arc<Mutex<Vec<String>>>) -> usize {
    captured_count(captured, NEG_OPEN_PREFIX)
}

fn captured_count(captured: &Arc<Mutex<Vec<String>>>, prefix: &str) -> usize {
    captured
        .lock()
        .expect("lock captured frames")
        .iter()
        .filter(|text| text.starts_with(prefix))
        .count()
}

async fn wait_for_neg_open(
    captured: &Arc<Mutex<Vec<String>>>,
    notify: &Arc<Notify>,
    context: &str,
) -> String {
    wait_for_captured_text(captured, notify, Duration::from_secs(2), context, |text| {
        text.starts_with(NEG_OPEN_PREFIX)
    })
    .await
}

async fn wait_for_neg_close(
    captured: &Arc<Mutex<Vec<String>>>,
    notify: &Arc<Notify>,
    context: &str,
) -> String {
    wait_for_captured_text(captured, notify, Duration::from_secs(2), context, |text| {
        text.starts_with(NEG_CLOSE_PREFIX)
    })
    .await
}

async fn poll_until_neg_open(
    pool: &mut OutboxPool,
    relay: &NormRelayUrl,
    captured: &Arc<Mutex<Vec<String>>>,
    notify: &Arc<Notify>,
    context: &str,
) -> OutboxSession {
    wait_for_websocket_connected(pool, relay, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_neg_open(captured, notify, context).await;
    staged_session
}

fn normalized_history_filter(filter: Filter) -> Filter {
    FullHistoryConfig::new(vec![filter]).filters()[0].clone()
}

async fn neg_open_filter_for_history_filter(history_filter: Filter, context: &str) -> Value {
    let (_relay_task, relay, captured, notify) = create_text_capture_relay().await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let live_filter = Filter::new().kinds(vec![1]).limit(10).since(123).build();
    let _sub_id = subscribe_with_history(
        &mut pool,
        wakeup,
        vec![live_filter],
        vec![history_filter],
        [relay.clone()],
    );

    let mut staged_session = OutboxSession::default();
    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    pool.poll_full_history(&mut staged_session);
    pool.poll_full_history(&mut staged_session);

    let frame = wait_for_neg_open(&captured, &notify, context).await;
    let frame: Value = serde_json::from_str(&frame).expect("parse NEG-OPEN frame");
    frame.as_array().expect("neg-open frame array")[2].clone()
}
#[tokio::test]
async fn subscribe_full_history_tracks_snapshot_and_schedules_round() {
    let mut pool = ready_pool();
    pool.set_keepalive_reconnect_delay(Duration::from_millis(20));
    pool.set_keepalive_reconnect_backoff_base(Duration::from_millis(20));
    let relay = relay_url("track");

    let sub_id = subscribe_history_only(
        &mut pool,
        MockWakeup::default(),
        trivial_filter(),
        [relay.clone()],
    );

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.rounds_started, 1);
    assert_eq!(tracked.snapshot.relays, vec![relay.clone()]);
    assert_eq!(
        filters_json(&tracked.snapshot.filters),
        filters_json(&trivial_filter())
    );

    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    let pending = &tracked.progress.pending_neg_sets[0];
    assert_eq!(pending.relays, vec![relay]);
    assert_eq!(
        filters_json(std::slice::from_ref(&pending.filter)),
        filters_json(&trivial_filter())
    );
}
#[tokio::test]
async fn upsert_full_history_sub_resnapshots_modified_filters_and_relays() {
    let mut pool = ready_pool();
    pool.set_keepalive_reconnect_delay(Duration::from_millis(20));
    pool.set_keepalive_reconnect_backoff_base(Duration::from_millis(20));
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("a");
    let relay_b = relay_url("b");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay_a]);

    let updated_filters = vec![Filter::new().kinds(vec![7]).limit(3).build()];
    {
        let mut handler = pool.start_session(wakeup);
        handler.modify_full_history(
            sub_id,
            FullHistoryConfig::new(updated_filters.clone()),
            relay_set([relay_b.clone()]),
        );
    }

    let tracked = tracked_sub(&pool, sub_id);
    let expected_filters = FullHistoryConfig::new(updated_filters.clone())
        .filters()
        .to_vec();
    assert_eq!(tracked.rounds_started, 1);
    assert_eq!(tracked.snapshot.relays, vec![relay_b.clone()]);
    assert_eq!(
        filters_json(&tracked.snapshot.filters),
        filters_json(&expected_filters)
    );

    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    let pending = &tracked.progress.pending_neg_sets[0];
    assert_eq!(pending.relays, vec![relay_b]);
    assert_eq!(
        filters_json(std::slice::from_ref(&pending.filter)),
        filters_json(&expected_filters)
    );
}
#[tokio::test]
async fn subscribe_full_history_builds_one_local_set_per_filter_across_relays() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pool = counting_ready_pool(Arc::clone(&calls));
    let relay_a = relay_url("count-a");
    let relay_b = relay_url("count-b");
    let sub_id = subscribe_history_only(
        &mut pool,
        MockWakeup::default(),
        trivial_filter(),
        [relay_a.clone(), relay_b.clone()],
    );

    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    assert_eq!(
        pending_neg_set_relays(&pool, sub_id),
        HashSet::from([relay_a, relay_b])
    );
}
#[tokio::test]
async fn full_history_pending_sets_deduplicate_added_relay_and_verification_round() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pool = counting_ready_pool(Arc::clone(&calls));
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("dedup-a");
    let relay_b = relay_url("dedup-b");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay_a.clone()]);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

    clear_pending_neg_sets(&mut pool, sub_id);

    modify_relays_for_history(
        &mut pool,
        wakeup,
        sub_id,
        [relay_a.clone(), relay_b.clone()],
    );
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);

    pool.full_history.schedule_round(sub_id);

    assert_eq!(
        calls.load(AtomicOrdering::SeqCst),
        2,
        "verification round should reuse pending local-set work for the same filter"
    );
    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    assert_eq!(
        pending_neg_set_relays(&pool, sub_id),
        HashSet::from([relay_a, relay_b])
    );
}
#[tokio::test]
async fn full_history_snapshot_uses_explicit_history_filter() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay = relay_url("filter");

    let live_filter = Filter::new().kinds(vec![1]).limit(10).since(123).build();
    let history_filter = Filter::new().kinds(vec![1]).limit(10).build();
    let sub_id = subscribe_with_history(
        &mut pool,
        wakeup,
        vec![live_filter],
        vec![history_filter.clone()],
        [relay],
    );

    let snapshot = pool
        .full_history_snapshot(sub_id)
        .expect("full-history snapshot should exist");

    assert_eq!(snapshot.filters.len(), 1);
    assert_eq!(snapshot.filters[0].limit(), history_filter.limit());
    assert!(snapshot.filters[0].since().is_none());
}
#[tokio::test]
async fn upsert_full_history_sub_preserves_progress_when_history_snapshot_is_unchanged() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("equivalent");

    let sub_id = subscribe_with_history(
        &mut pool,
        wakeup.clone(),
        vec![Filter::new().kinds(vec![1]).limit(10).since(123).build()],
        vec![Filter::new().kinds(vec![1]).build()],
        [relay.clone()],
    );

    let missing_id = note_id(0x44);
    {
        let tracked = tracked_sub_mut(&mut pool, sub_id);
        tracked.rounds_started = 7;
        tracked.progress.pending_neg_sets.clear();
        tracked
            .progress
            .pending_ingestion
            .insert(missing_id, pending_ingestion(relay.clone(), Instant::now()));
    }

    {
        let mut handler = pool.start_session(wakeup);
        handler.modify_full_history(
            sub_id,
            FullHistoryConfig::new(vec![Filter::new().kinds(vec![1]).build()]),
            relay_set([relay.clone()]),
        );
    }

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.rounds_started, 7);
    assert!(
        tracked.progress.pending_neg_sets.is_empty(),
        "equivalent history snapshot should not enqueue a fresh round"
    );
    assert!(
        tracked.progress.pending_ingestion.contains_key(&missing_id),
        "equivalent history snapshot should preserve in-flight fetch tracking"
    );
}
#[tokio::test]
async fn full_history_snapshot_preserves_explicit_bounds() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay = relay_url("bounded");

    let raw_filter = Filter::new().kinds(vec![1]).limit(10).since(123).build();
    let sub_id = subscribe_with_history(
        &mut pool,
        wakeup,
        vec![raw_filter.clone()],
        vec![raw_filter.clone()],
        [relay],
    );

    let snapshot = pool
        .full_history_snapshot(sub_id)
        .expect("full-history snapshot should exist");

    assert_eq!(snapshot.filters.len(), 1);
    assert_eq!(snapshot.filters[0].limit(), raw_filter.limit());
    assert_eq!(snapshot.filters[0].since(), raw_filter.since());
}
#[tokio::test]
async fn remove_full_history_sub_clears_tracker_and_shared_progress() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("remove");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay]);
    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked.progress.pending_ingestion.insert(
        note_id(7),
        pending_ingestion(relay_url("pending"), Instant::now()),
    );
    tracked.progress.fetch_retry_states.push(fetch_retry_state(
        note_id(9),
        relay_url("failed"),
        Instant::now(),
    ));

    remove_full_history(&mut pool, wakeup, sub_id);

    assert!(!is_tracked(&pool, sub_id));
}
#[tokio::test]
async fn poll_negentropy_state_machine_retains_ready_storage_until_relay_available() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pool = counting_ready_pool(Arc::clone(&calls));
    let relay = relay_url("ready");

    let sub_id = subscribe_history_only(
        &mut pool,
        MockWakeup::default(),
        trivial_filter(),
        [relay.clone()],
    );
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    assert_eq!(
        pending_neg_set_relays(&pool, sub_id),
        HashSet::from([relay.clone()])
    );

    pool.poll_negentropy_state_machine();
    pool.poll_negentropy_state_machine();

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    assert_eq!(
        pending_neg_set_relays(&pool, sub_id),
        HashSet::from([relay])
    );
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
}
#[tokio::test]
async fn poll_negentropy_state_machine_retains_unsent_filter_until_relay_available() {
    let mut pool = ready_pool();
    let relay = relay_url("large-filter");
    let filter = filter_larger_than_default_json_buffer();
    let history_id = subscribe_history_only(
        &mut pool,
        MockWakeup::default(),
        vec![filter.clone()],
        [relay.clone()],
    );

    pool.poll_negentropy_state_machine();

    let tracked = tracked_sub(&pool, history_id);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    let pending = &tracked.progress.pending_neg_sets[0];
    assert_eq!(pending.relays, vec![relay]);
    assert!(pending.filter.same_canonical_attributes(&filter));
}
#[tokio::test]
async fn pending_neg_set_starts_when_relay_connects_without_rebuilding() {
    let (_relay_task, relay, captured, notify) = create_text_capture_relay().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pool = counting_ready_pool(Arc::clone(&calls));
    let history_id = subscribe_history_only(
        &mut pool,
        MockWakeup::default(),
        trivial_filter(),
        [relay.clone()],
    );
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

    pool.poll_negentropy_state_machine();
    {
        let tracked = tracked_sub(&pool, history_id);
        assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    }

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let _ = wait_for_neg_open(&captured, &notify, "pending neg-open frame").await;

    let tracked = tracked_sub(&pool, history_id);
    assert!(tracked.progress.pending_neg_sets.is_empty());
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
}

#[tokio::test]
async fn pending_neg_set_starts_startable_relay_when_same_filter_relay_waits() {
    let (_relay_task, startable_relay, captured, notify) = create_text_capture_relay().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pool = counting_ready_pool(Arc::clone(&calls));
    let waiting_relay = relay_url("still-waiting");
    let history_id = subscribe_history_only(
        &mut pool,
        MockWakeup::default(),
        trivial_filter(),
        [waiting_relay.clone(), startable_relay.clone()],
    );
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

    {
        let relays = pending_neg_set_relays(&pool, history_id);
        assert_eq!(
            relays,
            HashSet::from([waiting_relay.clone(), startable_relay.clone()])
        );
    }

    wait_for_websocket_connected(&mut pool, &startable_relay, Duration::from_secs(2)).await;
    let _ = wait_for_neg_open(&captured, &notify, "startable waiting relay").await;

    let relays = pending_neg_set_relays(&pool, history_id);
    assert_eq!(relays, HashSet::from([waiting_relay]));
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
}

#[tokio::test]
async fn poll_negentropy_state_machine_drops_pending_when_full_history_removed() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("live");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay]);
    remove_full_history(&mut pool, wakeup, sub_id);

    pool.poll_negentropy_state_machine();

    assert!(!is_tracked(&pool, sub_id));
}
#[tokio::test]
async fn stage_need_fetches_skips_known_ids_and_batches_by_relay() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::from([note_id(3)]),
    }));
    let wakeup = MockWakeup::default();
    let relay = relay_url("fetch");
    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked
        .progress
        .pending_ingestion
        .insert(note_id(1), pending_ingestion(relay.clone(), Instant::now()));
    tracked.progress.fetch_retry_states.push(fetch_retry_state(
        note_id(2),
        relay.clone(),
        Instant::now() + Duration::from_secs(60),
    ));

    let mut staged_session = OutboxSession::default();
    pool.stage_need_fetches(
        vec![
            full_history_need(sub_id, relay.clone(), note_id(1)),
            full_history_need(sub_id, relay.clone(), note_id(2)),
            full_history_need(sub_id, relay.clone(), note_id(3)),
            full_history_need(sub_id, relay.clone(), note_id(4)),
            full_history_need(sub_id, relay.clone(), note_id(5)),
        ],
        &mut staged_session,
    );

    let tracked = tracked_sub(&pool, sub_id);
    assert!(tracked.progress.pending_ingestion.contains_key(&note_id(1)));
    assert!(tracked.progress.pending_ingestion.contains_key(&note_id(4)));
    assert!(tracked.progress.pending_ingestion.contains_key(&note_id(5)));
    assert!(!tracked.progress.pending_ingestion.contains_key(&note_id(2)));
    assert!(!tracked.progress.pending_ingestion.contains_key(&note_id(3)));
    assert_eq!(pool.registry.next_request_id, 2);

    let oneshot = staged_session
        .tasks
        .get(&OutboxSubId(1))
        .expect("one batched fetch should be staged");
    assert!(matches!(oneshot, OutboxTask::FullHistoryFetch(_)));
}

#[tokio::test]
async fn stage_need_fetches_retains_alternate_relay_while_fetch_is_pending() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let first_relay = relay_url("dedupe-first");
    let second_relay = relay_url("dedupe-second");
    let sub_id = subscribe_unbounded(
        &mut pool,
        wakeup.clone(),
        [first_relay.clone(), second_relay.clone()],
    );
    let missing = note_id(6);

    let mut staged_session = OutboxSession::default();
    pool.stage_need_fetches(
        vec![
            full_history_need(sub_id, first_relay.clone(), missing),
            full_history_need(sub_id, second_relay.clone(), missing),
        ],
        &mut staged_session,
    );

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.progress.pending_ingestion.len(), 1);
    assert_eq!(
        tracked
            .progress
            .pending_ingestion
            .get(&missing)
            .expect("active fetch should be tracked")
            .relay,
        first_relay
    );
    assert!(
        tracked
            .progress
            .fetch_candidate_waiting(&missing, &second_relay),
        "alternate relay should be retained while the first fetch is active"
    );
    assert_eq!(staged_session.tasks.len(), 1);
}

#[tokio::test]
async fn stage_need_fetches_batches_local_presence_checks() {
    let mut pool = OutboxPool::default();
    let batches = Arc::new(Mutex::new(Vec::new()));
    pool.set_event_checker(Box::new(BatchRecordingEventChecker {
        present: HashSet::from([note_id(3)]),
        batches: Arc::clone(&batches),
    }));
    let wakeup = MockWakeup::default();
    let relay = relay_url("batch");
    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked
        .progress
        .pending_ingestion
        .insert(note_id(1), pending_ingestion(relay.clone(), Instant::now()));
    tracked.progress.fetch_retry_states.push(fetch_retry_state(
        note_id(2),
        relay.clone(),
        Instant::now() + Duration::from_secs(60),
    ));

    let mut staged_session = OutboxSession::default();
    pool.stage_need_fetches(
        vec![
            full_history_need(sub_id, relay.clone(), note_id(1)),
            full_history_need(sub_id, relay.clone(), note_id(2)),
            full_history_need(sub_id, relay.clone(), note_id(3)),
            full_history_need(sub_id, relay.clone(), note_id(4)),
            full_history_need(sub_id, relay, note_id(5)),
        ],
        &mut staged_session,
    );

    let recorded = batches.lock().expect("lock recorded checker batches");
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        HashSet::<NoteId>::from_iter(recorded[0].iter().copied()),
        HashSet::from([note_id(2), note_id(3), note_id(4), note_id(5)])
    );
}

#[tokio::test]
async fn stage_need_fetches_limits_local_presence_checks_per_poll() {
    let mut pool = OutboxPool::default();
    let batches = Arc::new(Mutex::new(Vec::new()));
    pool.set_event_checker(Box::new(BatchRecordingEventChecker {
        present: HashSet::new(),
        batches: Arc::clone(&batches),
    }));
    let wakeup = MockWakeup::default();
    let relay = relay_url("presence-budget");
    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);

    let needs: Vec<FullHistoryNeed> = (0..FULL_HISTORY_PRESENCE_CHECK_BUDGET + 3)
        .map(|index| full_history_need(sub_id, relay.clone(), unique_note_id(index)))
        .collect();

    let mut first_session = OutboxSession::default();
    pool.stage_need_fetches(needs, &mut first_session);

    {
        let recorded = batches.lock().expect("lock recorded checker batches");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].len(), FULL_HISTORY_PRESENCE_CHECK_BUDGET);
    }
    assert_eq!(
        queued_need_id_count(tracked_sub(&pool, sub_id)),
        3,
        "unplanned needs should remain queued for the next frame"
    );
    assert!(
        pool.next_full_history_deadline().is_some(),
        "queued needs should request another maintenance frame"
    );

    let mut second_session = OutboxSession::default();
    pool.stage_need_fetches(Vec::new(), &mut second_session);

    let recorded = batches.lock().expect("lock recorded checker batches");
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[1].len(), 3);
}

#[tokio::test]
async fn stage_need_fetches_budget_is_fair_across_full_history_subs() {
    let mut pool = OutboxPool::default();
    let batches = Arc::new(Mutex::new(Vec::new()));
    pool.set_event_checker(Box::new(BatchRecordingEventChecker {
        present: HashSet::new(),
        batches: Arc::clone(&batches),
    }));
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("presence-fair-a");
    let relay_b = relay_url("presence-fair-b");
    let sub_a = subscribe_unbounded(&mut pool, wakeup.clone(), [relay_a.clone()]);
    let sub_b = subscribe_unbounded(&mut pool, wakeup, [relay_b.clone()]);

    let first_history_id = *pool
        .full_history
        .tracked_subs
        .keys()
        .next()
        .expect("at least one tracked full-history sub");
    let (heavy_sub, heavy_relay, light_sub, light_relay) = if first_history_id == sub_a {
        (sub_a, relay_a.clone(), sub_b, relay_b.clone())
    } else {
        (sub_b, relay_b.clone(), sub_a, relay_a.clone())
    };
    let light_id = unique_note_id(FULL_HISTORY_PRESENCE_CHECK_BUDGET + 10);

    let mut needs: Vec<FullHistoryNeed> = (0..FULL_HISTORY_PRESENCE_CHECK_BUDGET + 1)
        .map(|index| full_history_need(heavy_sub, heavy_relay.clone(), unique_note_id(index)))
        .collect();
    needs.push(full_history_need(light_sub, light_relay, light_id));

    let mut session = OutboxSession::default();
    pool.stage_need_fetches(needs, &mut session);

    let recorded = batches.lock().expect("lock recorded checker batches");
    assert_eq!(recorded.len(), 1);
    assert!(
        recorded[0].contains(&light_id),
        "one large queued sub should not consume the entire presence-check budget"
    );
}

#[tokio::test]
async fn poll_full_history_waits_for_queued_needs_before_verification_round() {
    let relay = relay_url("queued-local-needs");
    let ids: Vec<NoteId> = (0..FULL_HISTORY_PRESENCE_CHECK_BUDGET + 3)
        .map(unique_note_id)
        .collect();
    let mut pool = counting_ready_pool(Arc::new(AtomicUsize::new(0)));
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: ids.iter().copied().collect(),
    }));
    let history_id = subscribe_unbounded(&mut pool, MockWakeup::default(), [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);
    assert_eq!(tracked_sub(&pool, history_id).rounds_started, 1);

    for id in &ids {
        seed_relay_need(&mut pool, &relay, history_id, *id);
    }

    let mut first_session = OutboxSession::default();
    pool.poll_full_history(&mut first_session);
    assert_eq!(
        tracked_sub(&pool, history_id).rounds_started,
        1,
        "verification should wait for the remaining queued needs"
    );
    assert_eq!(queued_need_id_count(tracked_sub(&pool, history_id)), 3);

    let mut second_session = OutboxSession::default();
    pool.poll_full_history(&mut second_session);
    assert_eq!(tracked_sub(&pool, history_id).rounds_started, 2);
    assert!(tracked_sub(&pool, history_id)
        .progress
        .pending_needs
        .is_empty());
}

#[tokio::test]
async fn poll_full_history_schedules_fresh_round_when_all_needs_are_already_local() {
    let mut pool = counting_ready_pool(Arc::new(AtomicUsize::new(0)));
    let relay = relay_url("already-local");
    let present = note_id(0x44);
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::from([present]),
    }));
    let history_id = subscribe_unbounded(&mut pool, MockWakeup::default(), [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);
    seed_relay_need(&mut pool, &relay, history_id, present);

    let mut session = OutboxSession::default();
    pool.poll_full_history(&mut session);

    assert!(session.tasks.is_empty());
    assert!(tracked_sub(&pool, history_id)
        .progress
        .pending_ingestion
        .is_empty());
    assert_eq!(tracked_sub(&pool, history_id).rounds_started, 2);
    assert_eq!(
        tracked_sub(&pool, history_id)
            .progress
            .pending_neg_sets
            .len(),
        1,
        "already-local needs should complete fetch planning and schedule fresh verification"
    );
}

#[tokio::test]
async fn poll_full_history_schedules_fresh_round_when_fetch_retry_is_now_local() {
    let mut pool = counting_ready_pool(Arc::new(AtomicUsize::new(0)));
    let relay = relay_url("failed-then-local");
    let present = note_id(0x45);
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::from([present]),
    }));
    let history_id = subscribe_unbounded(&mut pool, MockWakeup::default(), [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);
    tracked_sub_mut(&mut pool, history_id)
        .progress
        .fetch_retry_states
        .push(fetch_retry_state(
            present,
            relay.clone(),
            Instant::now() + Duration::from_secs(60),
        ));
    seed_relay_need(&mut pool, &relay, history_id, present);

    let mut session = OutboxSession::default();
    pool.poll_full_history(&mut session);

    let tracked = tracked_sub(&pool, history_id);
    assert!(session.tasks.is_empty());
    assert!(tracked.progress.pending_ingestion.is_empty());
    assert!(tracked.progress.fetch_retry_states.is_empty());
    assert_eq!(tracked.rounds_started, 2);
    assert_eq!(
        tracked.progress.pending_neg_sets.len(),
        1,
        "already-local needs should not be hidden by stale fetch retry state"
    );
}

#[tokio::test]
async fn poll_full_history_rebuilds_local_set_when_already_local_needs_complete_round() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pool = counting_ready_pool(Arc::clone(&calls));
    let relay_a = relay_url("already-local-a");
    let relay_b = relay_url("already-local-b");
    let present = note_id(0x46);
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::from([present]),
    }));
    let history_id = subscribe_unbounded(
        &mut pool,
        MockWakeup::default(),
        [relay_a.clone(), relay_b.clone()],
    );
    clear_pending_neg_sets(&mut pool, history_id);
    pool.full_history.schedule_round(history_id);
    {
        let tracked = tracked_sub_mut(&mut pool, history_id);
        assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
        tracked.progress.pending_neg_sets[0].relays = vec![relay_b.clone()];
    }
    seed_relay_need(&mut pool, &relay_a, history_id, present);

    let mut session = OutboxSession::default();
    pool.poll_full_history(&mut session);

    let tracked = tracked_sub(&pool, history_id);
    assert!(session.tasks.is_empty());
    assert_eq!(
        calls.load(AtomicOrdering::SeqCst),
        3,
        "fresh verification must rebuild the local set instead of extending stale pending work"
    );
    assert_eq!(tracked.rounds_started, 3);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    let pending_relays: HashSet<NormRelayUrl> = tracked.progress.pending_neg_sets[0]
        .relays
        .iter()
        .cloned()
        .collect();
    assert_eq!(pending_relays, HashSet::from([relay_a, relay_b]));
}

#[tokio::test]
async fn handler_drop_subscribe_uses_explicit_full_history() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("staged-history-filter");
    let live_filter = Filter::new().kinds(vec![1]).limit(500).build();
    let history_filter = Filter::new().kinds(vec![1]).since(123).build();

    let live_id;
    let history_id;
    {
        let mut handler = pool.start_session(wakeup);
        live_id = handler.subscribe(
            vec![live_filter.clone()],
            RelayUrlPkgs::new(relay_set([relay.clone()])),
        );
        history_id = handler.subscribe_full_history(
            FullHistoryConfig::new(vec![history_filter.clone()]),
            relay_set([relay.clone()]),
        );
    }

    let tracked = tracked_sub(&pool, history_id);
    assert_eq!(tracked.snapshot.relays, vec![relay]);
    assert_eq!(
        filters_json(&tracked.snapshot.filters),
        filters_json(std::slice::from_ref(&history_filter))
    );
    assert_eq!(
        filters_json(pool.filters(&live_id).expect("live filters")),
        filters_json(std::slice::from_ref(&live_filter))
    );
}

#[tokio::test]
async fn full_history_can_start_without_live_subscription() {
    let (_relay_task, relay, captured, notify) = create_text_capture_relay().await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    {
        let mut handler = pool.start_session(wakeup);
        handler.subscribe_full_history(
            FullHistoryConfig::new(trivial_filter()),
            relay_set([relay.clone()]),
        );
    }

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    pool.poll_full_history(&mut staged_session);

    let _ = wait_for_neg_open(
        &captured,
        &notify,
        "history-only subscription should open negentropy",
    )
    .await;
}
#[tokio::test]
async fn handler_drop_full_modify_from_none_tracks_full_history() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("staged-enable");

    let sub_id = FullHistorySubId(0);
    assert!(!is_tracked(&pool, sub_id));

    {
        let mut handler = pool.start_session(wakeup);
        let filters = trivial_filter();
        handler.modify_full_history(sub_id, FullHistoryConfig::new(filters), relay_set([relay]));
    }

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.snapshot.id, sub_id);
    assert!(!tracked.progress.pending_neg_sets.is_empty());
}
#[tokio::test]
async fn handler_drop_full_modify_uses_explicit_full_history() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("staged-modify-history-filter");
    let live_id = {
        let mut handler = pool.start_session(wakeup.clone());
        handler.subscribe(
            vec![Filter::new().kinds(vec![1]).limit(500).build()],
            RelayUrlPkgs::new(relay_set([relay.clone()])),
        )
    };
    let history_id = subscribe_with_history(
        &mut pool,
        wakeup.clone(),
        vec![Filter::new().kinds(vec![1]).limit(500).build()],
        vec![Filter::new().kinds(vec![1]).build()],
        [relay.clone()],
    );
    let next_live_filter = Filter::new().kinds(vec![1]).limit(250).build();
    let next_history_filter = Filter::new().kinds(vec![1]).since(456).build();

    {
        let mut handler = pool.start_session(wakeup);
        handler.modify_full(
            live_id,
            vec![next_live_filter.clone()],
            relay_set([relay.clone()]),
        );
        handler.modify_full_history(
            history_id,
            FullHistoryConfig::new(vec![next_history_filter.clone()]),
            relay_set([relay.clone()]),
        );
    }

    let tracked = tracked_sub(&pool, history_id);
    assert_eq!(tracked.snapshot.relays, vec![relay]);
    assert_eq!(
        filters_json(&tracked.snapshot.filters),
        filters_json(std::slice::from_ref(&next_history_filter))
    );
    assert_eq!(
        filters_json(pool.filters(&live_id).expect("live filters")),
        filters_json(std::slice::from_ref(&next_live_filter))
    );
}
#[tokio::test]
async fn handler_drop_filter_modify_preserves_explicit_full_history() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("drop-filter-preserves-history");
    let live_filter = Filter::new().kinds(vec![1]).limit(500).build();
    let history_filter = Filter::new().kinds(vec![1]).since(123).build();
    let live_id = {
        let mut handler = pool.start_session(wakeup.clone());
        handler.subscribe(
            vec![live_filter],
            RelayUrlPkgs::new(relay_set([relay.clone()])),
        )
    };
    let history_id = subscribe_with_history(
        &mut pool,
        wakeup.clone(),
        vec![Filter::new().kinds(vec![1]).limit(500).build()],
        vec![history_filter.clone()],
        [relay],
    );

    {
        let mut handler = pool.start_session(wakeup);
        handler.modify_filters(
            live_id,
            vec![Filter::new().kinds(vec![1]).limit(250).build()],
        );
    }

    let tracked = tracked_sub(&pool, history_id);
    assert_eq!(
        filters_json(&tracked.snapshot.filters),
        filters_json(std::slice::from_ref(&history_filter))
    );
}
#[tokio::test]
async fn handler_drop_tracks_full_history_subscriptions_during_session_ingest() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("drop-track");

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay]);

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.snapshot.id, sub_id);
    assert!(!tracked.progress.pending_neg_sets.is_empty());
}
#[tokio::test]
async fn handler_drop_full_modify_removes_tracked_full_history() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("drop-modify");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);
    assert!(is_tracked(&pool, sub_id));

    {
        let mut handler = pool.start_session(wakeup);
        handler.remove_full_history(sub_id);
    }

    assert!(
        !is_tracked(&pool, sub_id),
        "full modify should remove tracked history when no full-history config is provided"
    );
}
#[tokio::test]
async fn full_history_work_guard_is_false_for_quiescent_tracked_subs() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("idle");

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay]);

    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked.progress.pending_neg_sets.clear();
    tracked.progress.retry_states.clear();
    tracked.progress.pending_ingestion.clear();
    tracked.progress.fetch_retry_states.clear();

    assert!(!pool.has_full_history_work());
}
#[tokio::test]
async fn poll_full_history_stages_auto_fetches_into_live_handler_session() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let relay = relay_url("handler");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    pool.relays.entry(relay.clone()).or_insert_with(|| {
        let mut relay_data = CoordinationData::new(RelayLimitations::default());
        relay_data.connect_websocket(&relay, wakeup.clone(), true);
        relay_data
    });
    seed_relay_need(&mut pool, &relay, sub_id, note_id(7));

    let mut handler = pool.start_session(wakeup);
    handler.outbox.poll_full_history(&mut handler.session);

    assert!(handler.outbox.subs.view(&OutboxSubId(1)).is_none());
    let oneshot = handler
        .session
        .tasks
        .get(&OutboxSubId(1))
        .expect("full-history pass should stage a fetch");
    assert!(matches!(oneshot, OutboxTask::FullHistoryFetch(_)));
}
#[tokio::test]
async fn full_history_work_guard_drains_stale_relay_needs() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay = relay_url("stale-needs");

    pool.relays.entry(relay.clone()).or_insert_with(|| {
        let mut relay_data = CoordinationData::new(RelayLimitations::default());
        relay_data.connect_websocket(&relay, wakeup.clone(), true);
        relay_data
    });
    seed_relay_need(&mut pool, &relay, FullHistorySubId(99), note_id(9));

    assert!(pool.has_full_history_work());

    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);

    assert!(!pool.has_full_history_work());
}
#[tokio::test]
async fn full_history_work_guard_is_true_while_negentropy_session_is_active() {
    let (_relay_task, relay, captured, notify) = create_text_capture_relay().await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let _sub_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);

    let _staged_session =
        poll_until_neg_open(&mut pool, &relay, &captured, &notify, "neg-open frame").await;

    assert!(pool.has_full_history_work());
}
#[tokio::test]
async fn full_history_round_waits_behind_active_negentropy_session() {
    let (_relay_task, relay, captured, notify) = create_text_capture_relay().await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);

    let mut staged_session = poll_until_neg_open(
        &mut pool,
        &relay,
        &captured,
        &notify,
        "initial neg-open frame",
    )
    .await;

    assert_active_sessions(&pool, &relay, 1);

    pool.full_history.schedule_round(sub_id);
    pool.poll_full_history(&mut staged_session);
    pool.poll_full_history(&mut staged_session);

    assert_active_sessions(&pool, &relay, 1);
    assert_eq!(neg_open_count(&captured), 1);

    let tracked = tracked_sub(&pool, sub_id);
    assert!(tracked.progress.pending_neg_sets.is_empty());
}
#[tokio::test]
async fn poll_full_history_timeout_schedules_same_relay_fetch_retry() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let relay = relay_url("timeout");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    let missing_id = note_id(9);
    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked.progress.pending_ingestion.insert(
        missing_id,
        pending_ingestion(
            relay.clone(),
            Instant::now() - INGESTION_TIMEOUT - Duration::from_millis(1),
        ),
    );

    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);

    let tracked = tracked_sub(&pool, sub_id);
    assert!(tracked.progress.pending_ingestion.is_empty());
    assert_eq!(tracked.progress.fetch_retry_states.len(), 1);
    assert!(tracked.progress.fetch_retry_waiting(&missing_id, &relay));
    assert!(pool.next_full_history_deadline().is_some());

    tracked_sub_mut(&mut pool, sub_id)
        .progress
        .fetch_retry_states[0]
        .next_retry_at = Instant::now();
    let retry_request_id = pool.registry.next_request_id;
    pool.poll_full_history(&mut staged_session);

    let retry_oneshot = staged_session
        .tasks
        .get(&OutboxSubId(retry_request_id))
        .expect("same relay fetch retry should stage a fetch");
    let OutboxTask::FullHistoryFetch(retry_task) = retry_oneshot else {
        panic!("expected full-history fetch task for same relay fetch retry");
    };
    assert_eq!(retry_task.subscribe.relays.urls, HashSet::from([relay]));
    assert_eq!(
        tracked_sub(&pool, sub_id)
            .progress
            .pending_ingestion
            .get(&missing_id)
            .expect("retry should be tracked")
            .retries_started,
        1
    );
}

#[tokio::test]
async fn relay_local_fetch_retry_budget_caps_same_relay_fetches() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let relay = relay_url("fetch-budget");
    let history_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);
    let missing_id = note_id(0x91);

    seed_relay_need(&mut pool, &relay, history_id, missing_id);
    let mut session = OutboxSession::default();
    pool.poll_full_history(&mut session);
    assert!(tracked_sub(&pool, history_id)
        .progress
        .pending_ingestion
        .contains_key(&missing_id));

    for expected_retries_started in 1..=MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID {
        tracked_sub_mut(&mut pool, history_id)
            .progress
            .pending_ingestion
            .get_mut(&missing_id)
            .expect("fetch should be tracked")
            .started_at -= INGESTION_TIMEOUT + Duration::from_millis(1);

        pool.poll_full_history(&mut session);
        tracked_sub_mut(&mut pool, history_id)
            .progress
            .fetch_retry_states[0]
            .next_retry_at = Instant::now();

        let retry_request_id = pool.registry.next_request_id;
        pool.poll_full_history(&mut session);
        assert!(
            session.tasks.contains_key(&OutboxSubId(retry_request_id)),
            "retry {expected_retries_started} should stage a same-relay fetch"
        );
        assert_eq!(
            tracked_sub(&pool, history_id)
                .progress
                .pending_ingestion
                .get(&missing_id)
                .expect("retry fetch should be tracked")
                .retries_started,
            expected_retries_started
        );
    }

    tracked_sub_mut(&mut pool, history_id)
        .progress
        .pending_ingestion
        .get_mut(&missing_id)
        .expect("final retry fetch should be tracked")
        .started_at -= INGESTION_TIMEOUT + Duration::from_millis(1);
    pool.poll_full_history(&mut session);

    let tracked = tracked_sub(&pool, history_id);
    assert!(tracked.progress.pending_ingestion.is_empty());
    assert!(tracked.progress.fetch_retry_states.is_empty());
    assert!(tracked.progress.fetch_failed(&missing_id, &relay));

    let request_id_after_budget = pool.registry.next_request_id;
    seed_relay_need(&mut pool, &relay, history_id, missing_id);
    pool.poll_full_history(&mut session);
    assert_eq!(
        pool.registry.next_request_id, request_id_after_budget,
        "exhausted relay/id retry state should suppress further same-relay fetches"
    );
}

#[tokio::test]
async fn due_fetch_retry_preserves_retry_count_when_waiting_behind_active_fetch() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("due-retry-a");
    let relay_b = relay_url("due-retry-b");
    let history_id = subscribe_unbounded(&mut pool, wakeup, [relay_a.clone(), relay_b.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);
    let missing_id = note_id(0x93);

    {
        let progress = &mut tracked_sub_mut(&mut pool, history_id).progress;
        progress
            .fetch_retry_states
            .push(FullHistoryFetchRetryState {
                id: missing_id,
                relay: relay_a.clone(),
                filter: trivial_filter()[0].clone(),
                next_retries_started: 1,
                next_retry_at: Instant::now(),
            });
        progress
            .fetch_retry_states
            .push(FullHistoryFetchRetryState {
                id: missing_id,
                relay: relay_b.clone(),
                filter: trivial_filter()[0].clone(),
                next_retries_started: MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID,
                next_retry_at: Instant::now(),
            });
    }

    let mut session = OutboxSession::default();
    pool.poll_full_history(&mut session);
    assert_eq!(
        tracked_sub(&pool, history_id)
            .progress
            .pending_ingestion
            .len(),
        1
    );
    let active_relay = tracked_sub(&pool, history_id)
        .progress
        .pending_ingestion
        .get(&missing_id)
        .expect("active fetch should be tracked")
        .relay
        .clone();
    let deferred_relay = if active_relay == relay_a {
        relay_b.clone()
    } else {
        relay_a.clone()
    };
    assert!(tracked_sub(&pool, history_id)
        .progress
        .fetch_state_suppresses_need(&missing_id, &deferred_relay));

    tracked_sub_mut(&mut pool, history_id)
        .progress
        .pending_ingestion
        .get_mut(&missing_id)
        .expect("active fetch should be tracked")
        .started_at -= INGESTION_TIMEOUT + Duration::from_millis(1);

    let request_id = pool.registry.next_request_id;
    pool.poll_full_history(&mut session);
    pool.poll_full_history(&mut session);

    let retry_oneshot = session
        .tasks
        .get(&OutboxSubId(request_id))
        .expect("deferred due retry should stage a fetch");
    let OutboxTask::FullHistoryFetch(task) = retry_oneshot else {
        panic!("expected full-history fetch task for deferred due retry");
    };
    let retry_relay = task
        .subscribe
        .relays
        .urls
        .iter()
        .next()
        .expect("retry should target one relay")
        .clone();
    assert!(
        retry_relay == relay_a || retry_relay == relay_b,
        "retry relay should be one of the due retry states"
    );
    assert_eq!(
        tracked_sub(&pool, history_id)
            .progress
            .pending_ingestion
            .get(&missing_id)
            .expect("deferred retry should be tracked")
            .retries_started,
        if retry_relay == relay_b {
            MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID
        } else {
            1
        }
    );
}

#[tokio::test]
async fn timed_out_fetch_on_one_relay_does_not_block_retry_from_another() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("fetch-a");
    let relay_b = relay_url("fetch-b");

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay_a.clone(), relay_b.clone()]);

    let missing_id = note_id(9);
    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked.progress.pending_ingestion.insert(
        missing_id,
        pending_ingestion(
            relay_a.clone(),
            Instant::now() - INGESTION_TIMEOUT - Duration::from_millis(1),
        ),
    );

    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);

    let next_request_id_before = pool.registry.next_request_id;
    pool.stage_need_fetches(
        vec![full_history_need(sub_id, relay_b, missing_id)],
        &mut staged_session,
    );

    assert_eq!(pool.registry.next_request_id, next_request_id_before + 1);
    let oneshot = staged_session
        .tasks
        .get(&OutboxSubId(next_request_id_before))
        .expect("retry from second relay should stage a fetch");
    assert!(matches!(oneshot, OutboxTask::FullHistoryFetch(_)));
}
#[tokio::test]
async fn timed_out_fetch_retries_other_relay_after_real_dispatch_path() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("sequence-a");
    let relay_b = relay_url("sequence-b");

    let sub_id = subscribe_unbounded(
        &mut pool,
        wakeup.clone(),
        [relay_a.clone(), relay_b.clone()],
    );

    let missing_id = note_id(11);
    seed_relay_need(&mut pool, &relay_a, sub_id, missing_id);
    seed_relay_need(&mut pool, &relay_b, sub_id, missing_id);

    let first_request_id = pool.registry.next_request_id;
    let mut initial_session = OutboxSession::default();
    pool.poll_full_history(&mut initial_session);

    let initial_oneshot = initial_session
        .tasks
        .get(&OutboxSubId(first_request_id))
        .expect("first surfaced need should stage a fetch");
    let OutboxTask::FullHistoryFetch(initial_task) = initial_oneshot else {
        panic!("expected full-history fetch task for initial relay fetch");
    };
    let first_relay = initial_task
        .subscribe
        .relays
        .urls
        .iter()
        .next()
        .expect("initial fetch should target one relay")
        .clone();
    let alternate_relay = if first_relay == relay_a {
        relay_b.clone()
    } else {
        relay_a.clone()
    };
    assert_eq!(
        initial_task.subscribe.relays.urls,
        HashSet::from([first_relay.clone()])
    );
    assert!(
        tracked_sub(&pool, sub_id)
            .progress
            .fetch_candidate_waiting(&missing_id, &alternate_relay),
        "alternate relay should wait behind the active first-relay fetch"
    );

    tracked_sub_mut(&mut pool, sub_id)
        .progress
        .pending_ingestion
        .get_mut(&missing_id)
        .expect("initial relay fetch should be tracked")
        .started_at -= INGESTION_TIMEOUT + Duration::from_millis(1);

    let mut timeout_session = OutboxSession::default();
    pool.poll_full_history(&mut timeout_session);
    let tracked = tracked_sub(&pool, sub_id);
    assert!(tracked
        .progress
        .fetch_retry_waiting(&missing_id, &first_relay));
    assert!(tracked
        .progress
        .fetch_candidate_waiting(&missing_id, &alternate_relay));

    let retry_request_id = pool.registry.next_request_id;
    let mut retry_session = OutboxSession::default();
    pool.poll_full_history(&mut retry_session);

    assert_eq!(retry_session.tasks.len(), 1);
    let retry_oneshot = retry_session
        .tasks
        .get(&OutboxSubId(retry_request_id))
        .expect("second relay should stage the retry fetch");
    let OutboxTask::FullHistoryFetch(retry_task) = retry_oneshot else {
        panic!("expected full-history fetch task for retry relay fetch");
    };
    assert_eq!(
        retry_task.subscribe.relays.urls,
        HashSet::from([alternate_relay.clone()])
    );
    let tracked = tracked_sub(&pool, sub_id);
    let pending = tracked
        .progress
        .pending_ingestion
        .get(&missing_id)
        .expect("second relay fetch should be tracked");
    assert_eq!(pending.relay, alternate_relay);
    assert_eq!(pending.retries_started, 0);
    assert!(tracked
        .progress
        .fetch_retry_waiting(&missing_id, &first_relay));
}

#[tokio::test]
async fn full_history_fetch_is_not_deduped_against_active_oneshot() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let relay = relay_url("fetch-dedupe");
    let history_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);

    let missing_id = note_id(0x94);
    let fetch_filter = Filter::new().ids([missing_id.bytes()]).build();
    let mut relay_set = HashSet::new();
    relay_set.insert(relay.clone());

    let mut active_oneshot_session = OutboxSession::default();
    active_oneshot_session.oneshot(
        OutboxSubId(900),
        vec![fetch_filter],
        RelayUrlPkgs::new(relay_set),
    );
    let active_oneshot_delta = pool.collect_sessions(active_oneshot_session);
    assert!(
        active_oneshot_delta.get(&relay).is_some(),
        "initial oneshot should be retained"
    );

    seed_relay_need(&mut pool, &relay, history_id, missing_id);
    let fetch_request_id = pool.registry.next_request_id;
    let mut full_history_session = OutboxSession::default();
    pool.poll_full_history(&mut full_history_session);

    let full_history_delta = pool.collect_sessions(full_history_session);
    assert!(
        full_history_delta
            .get(&relay)
            .is_some_and(|session| session.tasks.contains_key(&OutboxSubId(fetch_request_id))),
        "full-history fetch should bypass generic active-oneshot dedupe"
    );
    assert!(tracked_sub(&pool, history_id)
        .progress
        .pending_ingestion
        .contains_key(&missing_id));
}

#[tokio::test]
async fn app_oneshot_is_not_deduped_against_active_full_history_fetch() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let relay = relay_url("app-oneshot-after-full-history-fetch");
    let history_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);

    let missing_id = note_id(0x95);
    let fetch_filter = Filter::new().ids([missing_id.bytes()]).build();
    let mut relay_set = HashSet::new();
    relay_set.insert(relay.clone());
    let relays = RelayUrlPkgs::new(relay_set);

    seed_relay_need(&mut pool, &relay, history_id, missing_id);
    let fetch_request_id = pool.registry.next_request_id;
    let mut full_history_session = OutboxSession::default();
    pool.poll_full_history(&mut full_history_session);

    let full_history_delta = pool.collect_sessions(full_history_session);
    assert!(
        full_history_delta
            .get(&relay)
            .is_some_and(|session| session.tasks.contains_key(&OutboxSubId(fetch_request_id))),
        "full-history fetch should stage relay work"
    );

    let app_request_id = OutboxSubId(901);
    let mut app_session = OutboxSession::default();
    app_session.oneshot(app_request_id, vec![fetch_filter], relays);
    let app_delta = pool.collect_sessions(app_session);

    assert!(
        app_delta
            .get(&relay)
            .is_some_and(|session| session.tasks.contains_key(&app_request_id)),
        "app oneshot should not be suppressed by active full-history fetch"
    );
}

#[tokio::test]
async fn relay_retarget_fetches_alternate_candidate_when_active_fetch_relay_removed() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("retarget-fetch-a");
    let relay_b = relay_url("retarget-fetch-b");
    let history_id = subscribe_unbounded(
        &mut pool,
        wakeup.clone(),
        [relay_a.clone(), relay_b.clone()],
    );
    let missing_id = note_id(0x92);
    seed_relay_need(&mut pool, &relay_a, history_id, missing_id);
    seed_relay_need(&mut pool, &relay_b, history_id, missing_id);

    let first_request_id = pool.registry.next_request_id;
    let mut initial_session = OutboxSession::default();
    pool.poll_full_history(&mut initial_session);
    let initial_oneshot = initial_session
        .tasks
        .get(&OutboxSubId(first_request_id))
        .expect("first surfaced need should stage a fetch");
    let OutboxTask::FullHistoryFetch(initial_task) = initial_oneshot else {
        panic!("expected full-history fetch task for initial relay fetch");
    };
    let first_relay = initial_task
        .subscribe
        .relays
        .urls
        .iter()
        .next()
        .expect("initial fetch should target one relay")
        .clone();
    let alternate_relay = if first_relay == relay_a {
        relay_b.clone()
    } else {
        relay_a.clone()
    };
    assert!(tracked_sub(&pool, history_id)
        .progress
        .fetch_candidate_waiting(&missing_id, &alternate_relay));

    modify_unbounded_history(&mut pool, wakeup, history_id, [alternate_relay.clone()]);

    let tracked = tracked_sub(&pool, history_id);
    let pending = tracked
        .progress
        .pending_ingestion
        .get(&missing_id)
        .expect("retained relay should fetch after active relay is removed");
    assert_eq!(pending.relay, alternate_relay);
    assert!(!tracked
        .progress
        .fetch_candidate_waiting(&missing_id, &pending.relay));
    assert!(!tracked
        .progress
        .fetch_retry_waiting(&missing_id, &first_relay));
}
#[tokio::test]
async fn completed_ingestion_subs_batches_local_presence_checks() {
    let mut pool = OutboxPool::default();
    let batches = Arc::new(Mutex::new(Vec::new()));
    let first = note_id(7);
    let second = note_id(8);
    pool.set_event_checker(Box::new(BatchRecordingEventChecker {
        present: HashSet::from([first, second]),
        batches: Arc::clone(&batches),
    }));
    let wakeup = MockWakeup::default();
    let relay = relay_url("ingest");

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay]);

    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked.progress.pending_ingestion.insert(
        first,
        pending_ingestion(relay_url("ingest-a"), Instant::now()),
    );
    tracked.progress.pending_ingestion.insert(
        second,
        pending_ingestion(relay_url("ingest-b"), Instant::now()),
    );

    let completed = pool.full_history.completed_ingestion_subs();

    assert_eq!(completed, vec![sub_id]);
    let recorded = batches.lock().expect("lock recorded checker batches");
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        HashSet::<NoteId>::from_iter(recorded[0].iter().copied()),
        HashSet::from([first, second])
    );
}
#[tokio::test]
async fn poll_full_history_timeout_isolated_per_sub() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();

    let sub_a = subscribe_unbounded(&mut pool, wakeup.clone(), [relay_url("timeout-a")]);
    let sub_b = subscribe_unbounded(&mut pool, wakeup.clone(), [relay_url("timeout-b")]);

    let tracked_a = tracked_sub_mut(&mut pool, sub_a);
    tracked_a.progress.pending_ingestion.insert(
        note_id(1),
        pending_ingestion(
            relay_url("timeout-a"),
            Instant::now() - INGESTION_TIMEOUT - Duration::from_millis(1),
        ),
    );

    let tracked_b = tracked_sub_mut(&mut pool, sub_b);
    tracked_b.progress.pending_ingestion.insert(
        note_id(2),
        pending_ingestion(relay_url("timeout-b"), Instant::now()),
    );

    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);

    let tracked_a = tracked_sub(&pool, sub_a);
    assert!(tracked_a
        .progress
        .fetch_retry_waiting(&note_id(1), &relay_url("timeout-a")));
    assert!(tracked_a.progress.pending_ingestion.is_empty());

    let tracked_b = tracked_sub(&pool, sub_b);
    assert!(!tracked_b
        .progress
        .fetch_retry_waiting(&note_id(2), &relay_url("timeout-b")));
    assert!(tracked_b
        .progress
        .pending_ingestion
        .contains_key(&note_id(2)));
}
#[tokio::test]
async fn remove_full_history_sub_cancels_active_negentropy_sessions_and_needs() {
    let (_relay_task, relay, captured, notify) = create_text_capture_relay().await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    let _staged_session =
        poll_until_neg_open(&mut pool, &relay, &captured, &notify, "neg-open frame").await;

    assert_active_sessions(&pool, &relay, 1);
    seed_relay_need(&mut pool, &relay, sub_id, note_id(9));

    remove_full_history(&mut pool, wakeup, sub_id);

    assert_active_sessions(&pool, &relay, 0);
    assert!(pool.drain_full_history_needs().is_empty());
}

#[tokio::test]
async fn remove_full_history_sub_cancels_active_fetch_oneshots() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let relay = relay_url("fetch-owner");
    let history_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);
    seed_relay_need(&mut pool, &relay, history_id, note_id(42));

    let fetch_id = OutboxSubId(pool.registry.next_request_id);
    let mut fetch_session = OutboxSession::default();
    pool.poll_full_history(&mut fetch_session);
    let fetch_delta = pool.collect_sessions(fetch_session);
    assert!(
        fetch_delta
            .get(&relay)
            .is_some_and(|session| session.tasks.contains_key(&fetch_id)),
        "full-history need should stage a fetch oneshot"
    );

    pool.ingest_session_delta(fetch_delta, &wakeup);
    assert!(
        pool.subs.get(&fetch_id).is_some(),
        "fetch oneshot should be active before owner removal"
    );

    remove_full_history(&mut pool, wakeup, history_id);
    assert!(
        pool.subs.get(&fetch_id).is_none(),
        "removing the full-history owner should remove active fetch oneshots"
    );
}

#[tokio::test]
async fn relay_retarget_cancels_only_removed_active_fetch_oneshots() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let retained_relay = relay_url("fetch-retarget-retained");
    let removed_relay = relay_url("fetch-retarget-removed");
    let history_id = subscribe_unbounded(
        &mut pool,
        wakeup.clone(),
        [retained_relay.clone(), removed_relay.clone()],
    );
    clear_pending_neg_sets(&mut pool, history_id);
    seed_relay_need(&mut pool, &retained_relay, history_id, note_id(43));
    seed_relay_need(&mut pool, &removed_relay, history_id, note_id(44));

    let mut fetch_session = OutboxSession::default();
    pool.poll_full_history(&mut fetch_session);
    let fetch_ids = full_history_fetch_ids_by_relay(&fetch_session);
    let retained_fetch_id = *fetch_ids
        .get(&retained_relay)
        .expect("retained relay should have an active fetch");
    let removed_fetch_id = *fetch_ids
        .get(&removed_relay)
        .expect("removed relay should have an active fetch");
    let fetch_delta = pool.collect_sessions(fetch_session);
    pool.ingest_session_delta(fetch_delta, &wakeup);

    modify_unbounded_history(&mut pool, wakeup, history_id, [retained_relay.clone()]);

    assert!(
        pool.subs.get(&removed_fetch_id).is_none(),
        "retargeting away a relay should cancel its active full-history fetch"
    );
    assert!(
        pool.subs.get(&retained_fetch_id).is_some(),
        "retargeting should preserve active fetches for retained relay/filter pairs"
    );
}

#[tokio::test]
async fn unchanged_full_history_snapshot_preserves_active_negentropy_session() {
    let (_relay_task, relay, captured, notify) = create_text_capture_relay().await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let sub_id = subscribe_with_history(
        &mut pool,
        wakeup.clone(),
        vec![Filter::new().kinds(vec![1]).limit(10).since(123).build()],
        vec![Filter::new().kinds(vec![1]).build()],
        [relay.clone()],
    );

    let _staged_session =
        poll_until_neg_open(&mut pool, &relay, &captured, &notify, "neg-open frame").await;

    assert_active_sessions(&pool, &relay, 1);

    {
        let mut handler = pool.start_session(wakeup);
        let live_id = handler.subscribe(
            vec![Filter::new().kinds(vec![1]).limit(10).since(123).build()],
            RelayUrlPkgs::new(relay_set([relay.clone()])),
        );
        handler.modify_filters(
            live_id,
            vec![Filter::new().kinds(vec![1]).limit(20).since(456).build()],
        );
    }

    assert_active_sessions(&pool, &relay, 1);
    assert!(is_tracked(&pool, sub_id));
}
#[tokio::test]
async fn relay_retarget_preserves_active_negentropy_session_for_retained_relay() {
    let (_relay_a_task, relay_a, captured_a, notify_a) = create_text_capture_relay().await;
    let (_relay_b_task, relay_b, captured_b, notify_b) = create_text_capture_relay().await;
    let (_relay_c_task, relay_c, captured_c, notify_c) = create_text_capture_relay().await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let sub_id = subscribe_unbounded(
        &mut pool,
        wakeup.clone(),
        [relay_a.clone(), relay_b.clone()],
    );

    wait_for_websocket_connected(&mut pool, &relay_a, Duration::from_secs(2)).await;
    wait_for_websocket_connected(&mut pool, &relay_b, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_captured_text(
        &captured_a,
        &notify_a,
        Duration::from_secs(2),
        "retained relay initial NEG-OPEN",
        |text| text.starts_with("[\"NEG-OPEN\","),
    )
    .await;
    let _ = wait_for_captured_text(
        &captured_b,
        &notify_b,
        Duration::from_secs(2),
        "removed relay initial NEG-OPEN",
        |text| text.starts_with("[\"NEG-OPEN\","),
    )
    .await;

    modify_relays_for_history(
        &mut pool,
        wakeup,
        sub_id,
        [relay_a.clone(), relay_c.clone()],
    );

    wait_for_websocket_connected(&mut pool, &relay_c, Duration::from_secs(2)).await;
    pool.poll_full_history(&mut staged_session);
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_captured_text(
        &captured_b,
        &notify_b,
        Duration::from_secs(2),
        "removed relay NEG-CLOSE",
        |text| text.starts_with("[\"NEG-CLOSE\","),
    )
    .await;
    let _ = wait_for_captured_text(
        &captured_c,
        &notify_c,
        Duration::from_secs(2),
        "added relay NEG-OPEN",
        |text| text.starts_with("[\"NEG-OPEN\","),
    )
    .await;

    let relay_a_frames = captured_a.lock().expect("lock retained relay frames");
    assert!(
        relay_a_frames
            .iter()
            .all(|text| !text.starts_with("[\"NEG-CLOSE\",")),
        "retained relay should not receive NEG-CLOSE; captured {relay_a_frames:?}"
    );
    assert_active_sessions(&pool, &relay_a, 1);
}
#[tokio::test]
async fn relay_retarget_schedules_added_relay_after_round_budget_exhausted() {
    let (_relay_a_task, relay_a, _captured_a, _notify_a) = create_text_capture_relay().await;
    let (_relay_b_task, relay_b, captured_b, notify_b) = create_text_capture_relay().await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay_a.clone()]);
    tracked_sub_mut(&mut pool, sub_id).rounds_started = MAX_FULL_HISTORY_ROUNDS;

    modify_relays_for_history(&mut pool, wakeup, sub_id, [relay_a, relay_b.clone()]);

    wait_for_websocket_connected(&mut pool, &relay_b, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_captured_text(
        &captured_b,
        &notify_b,
        Duration::from_secs(2),
        "added relay NEG-OPEN after exhausted full-history round budget",
        |text| text.starts_with("[\"NEG-OPEN\","),
    )
    .await;

    assert_eq!(
        tracked_sub(&pool, sub_id).rounds_started,
        MAX_FULL_HISTORY_ROUNDS
    );
}
#[tokio::test]
async fn live_unsubscribe_does_not_cancel_full_history_owner() {
    let (_relay_task, relay, captured, notify) = create_text_capture_relay().await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let live_id = {
        let mut handler = pool.start_session(wakeup.clone());
        handler.subscribe(
            trivial_filter(),
            RelayUrlPkgs::new(relay_set([relay.clone()])),
        )
    };
    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    let _staged_session = poll_until_neg_open(
        &mut pool,
        &relay,
        &captured,
        &notify,
        "neg-open frame before unsubscribe",
    )
    .await;

    assert_active_sessions(&pool, &relay, 1);

    {
        let mut handler = pool.start_session(wakeup);
        handler.unsubscribe(live_id);
    }

    assert_active_sessions(&pool, &relay, 1);
    assert!(is_tracked(&pool, sub_id));
}
#[tokio::test]
async fn poll_full_history_neg_open_uses_explicit_history_filter() {
    let filter = neg_open_filter_for_history_filter(
        Filter::new().kinds(vec![1]).limit(10).build(),
        "unbounded neg-open frame",
    )
    .await;

    assert_eq!(filter.get("limit").and_then(Value::as_u64), Some(10));
    assert!(filter.get("since").is_none());
}
#[tokio::test]
async fn poll_full_history_neg_open_preserves_explicit_limit_and_since() {
    let filter = neg_open_filter_for_history_filter(
        Filter::new().kinds(vec![1]).limit(10).since(123).build(),
        "bounded neg-open frame",
    )
    .await;

    assert_eq!(filter.get("limit").and_then(Value::as_u64), Some(10));
    assert_eq!(filter.get("since").and_then(Value::as_u64), Some(123));
}
#[tokio::test]
async fn blocked_neg_err_marks_filter_blocked_without_marking_relay_unsupported() {
    let (_relay_task, relay, captured, notify) =
        create_capture_relay_with_mode(CaptureRelayMode::NegErrOnOpen("blocked: too many records"))
            .await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let filter = Filter::new().kinds(vec![1]).limit(10).build();
    let history_filter = normalized_history_filter(filter.clone());
    let sub_id = subscribe_with_history(
        &mut pool,
        wakeup.clone(),
        vec![filter.clone()],
        vec![filter],
        [relay.clone()],
    );

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_neg_open(&captured, &notify, "blocked neg-open frame").await;

    wait_for_pool_condition(
        &mut pool,
        Duration::from_secs(2),
        "blocked NEG-ERR processing",
        |pool| {
            pool.relays.get(&relay).is_some_and(|relay_data| {
                relay_data
                    .negentropy_data
                    .is_filter_blocked(&history_filter)
            })
        },
    )
    .await;

    let relay_data = pool.relays.get(&relay).expect("relay tracked");
    assert!(!relay_data.negentropy_data.is_unsupported());

    let neg_open_count_before = neg_open_count(&captured);

    {
        let mut handler = pool.start_session(wakeup.clone());
        handler.modify_full_history(
            sub_id,
            FullHistoryConfig::new(vec![history_filter.clone()]),
            relay_set([relay.clone()]),
        );
    }
    pool.poll_full_history(&mut staged_session);
    wait_for_pool_condition(
        &mut pool,
        Duration::from_millis(200),
        "no second blocked NEG-OPEN",
        |_| true,
    )
    .await;

    let neg_open_count_after = neg_open_count(&captured);
    assert_eq!(neg_open_count_after, neg_open_count_before);
}
#[tokio::test]
async fn closed_neg_err_allows_the_same_sub_to_retry_in_a_later_round() {
    let (_relay_task, relay, captured, notify) =
        create_capture_relay_with_mode(CaptureRelayMode::NegErrOnOpen("closed: session timeout"))
            .await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let filter = Filter::new().kinds(vec![1]).limit(10).build();
    let history_filter = normalized_history_filter(filter.clone());
    let sub_id = subscribe_with_history(
        &mut pool,
        wakeup.clone(),
        vec![filter.clone()],
        vec![filter],
        [relay.clone()],
    );

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_neg_open(&captured, &notify, "first closed neg-open frame").await;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        pool.try_recv(|_| {});
        let relay_data = pool.relays.get(&relay).expect("relay tracked");
        if !relay_data.negentropy_data.is_unsupported()
            && !relay_data
                .negentropy_data
                .is_filter_blocked(&history_filter)
            && relay_data.negentropy_data.active_session_count() == 0
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    {
        let mut handler = pool.start_session(wakeup.clone());
        handler.modify_full_history(
            sub_id,
            FullHistoryConfig::new(vec![history_filter.clone()]),
            relay_set([relay.clone()]),
        );
    }
    pool.poll_full_history(&mut staged_session);
    force_full_history_retries_due(&mut pool, sub_id);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        pool.try_recv(|_| {});
        pool.poll_full_history(&mut staged_session);
        let neg_open_count = neg_open_count(&captured);
        if neg_open_count >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let neg_open_count = neg_open_count(&captured);
    assert!(neg_open_count >= 2, "expected a retry after closed NEG-ERR");
}
#[tokio::test]
async fn transient_retry_does_not_immediately_rebuild_local_set() {
    let (calls, mut pool, relay, sub_id, mut staged_session) = counting_retry_fixture("retry");
    seed_relay_retry(&mut pool, &relay, sub_id);

    pool.poll_full_history(&mut staged_session);

    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
}

#[tokio::test]
async fn next_full_history_deadline_reports_retry_backoff_without_forcing_due() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let relay = relay_url("retry-deadline");
    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    seed_relay_retry(&mut pool, &relay, sub_id);

    let before_poll = Instant::now();
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);

    let deadline = pool
        .next_full_history_deadline()
        .expect("scheduled retry should expose a deadline");
    assert!(deadline > Instant::now());
    assert!(deadline <= before_poll + FULL_HISTORY_RETRY_BACKOFF_BASE + Duration::from_millis(100));
}

#[tokio::test]
async fn next_full_history_deadline_reports_pending_local_set_receiver() {
    let senders = Arc::new(Mutex::new(Vec::new()));
    let mut pool = OutboxPool::default();
    pool.set_neg_set_provider(Box::new(PendingNegSetProvider {
        senders: Arc::clone(&senders),
    }));
    let wakeup = MockWakeup::default();

    let relay = relay_url("local-set-deadline");
    let _sub_id = subscribe_unbounded(&mut pool, wakeup, [relay]);
    assert_eq!(senders.lock().expect("lock pending senders").len(), 1);

    let before_poll = Instant::now();
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);

    let deadline = pool
        .next_full_history_deadline()
        .expect("pending local-set receiver should expose a poll deadline");
    assert!(deadline > Instant::now());
    assert!(deadline <= before_poll + Duration::from_secs(1));
}

#[tokio::test]
async fn next_full_history_deadline_ignores_retry_without_neg_set_provider() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();

    let relay = relay_url("retry-no-provider");
    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    seed_relay_retry(&mut pool, &relay, sub_id);

    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);

    assert!(pool.next_full_history_deadline().is_none());
}

#[tokio::test]
async fn next_full_history_deadline_reports_pending_ingestion_timeout() {
    let mut pool = OutboxPool::default();
    pool.set_event_checker(Box::new(SelectiveEventChecker {
        present: HashSet::new(),
    }));
    let wakeup = MockWakeup::default();
    let relay = relay_url("ingestion-deadline");

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    let started_at = Instant::now();
    tracked_sub_mut(&mut pool, sub_id)
        .progress
        .pending_ingestion
        .insert(note_id(0x77), pending_ingestion(relay, started_at));

    assert_eq!(
        pool.next_full_history_deadline(),
        Some(started_at + INGESTION_TIMEOUT)
    );
}

#[tokio::test]
async fn next_full_history_deadline_ignores_ingestion_without_event_checker() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay = relay_url("ingestion-no-checker");

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    tracked_sub_mut(&mut pool, sub_id)
        .progress
        .pending_ingestion
        .insert(note_id(0x78), pending_ingestion(relay, Instant::now()));

    assert!(pool.next_full_history_deadline().is_none());
}

#[tokio::test]
async fn transient_retry_promotes_after_backoff() {
    let (calls, mut pool, relay, sub_id, mut staged_session) = counting_retry_fixture("retry");
    seed_relay_retry(&mut pool, &relay, sub_id);

    pool.poll_full_history(&mut staged_session);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

    force_full_history_retries_due(&mut pool, sub_id);

    pool.poll_full_history(&mut staged_session);

    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
}

#[tokio::test]
async fn transient_retry_budget_caps_local_set_rebuilds() {
    let (calls, mut pool, relay, sub_id, mut staged_session) = counting_retry_fixture("retry");

    for expected_calls in 2..=(MAX_FULL_HISTORY_RETRIES_PER_RELAY_FILTER + 1) {
        seed_relay_retry(&mut pool, &relay, sub_id);
        pool.poll_full_history(&mut staged_session);
        force_full_history_retries_due(&mut pool, sub_id);
        pool.poll_full_history(&mut staged_session);
        clear_pending_neg_sets(&mut pool, sub_id);

        assert_eq!(calls.load(AtomicOrdering::SeqCst), expected_calls);
    }

    seed_relay_retry(&mut pool, &relay, sub_id);
    pool.poll_full_history(&mut staged_session);
    pool.poll_full_history(&mut staged_session);

    assert_eq!(
        calls.load(AtomicOrdering::SeqCst),
        MAX_FULL_HISTORY_RETRIES_PER_RELAY_FILTER + 1
    );
}
#[tokio::test]
async fn silent_neg_open_timeout_marks_relay_unsupported_and_suppresses_retry() {
    let (_relay_task, relay, captured, notify) =
        create_capture_relay_with_mode(CaptureRelayMode::Silent).await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_neg_open(&captured, &notify, "silent neg-open frame").await;

    relay_data_mut(&mut pool, &relay)
        .negentropy_data
        .age_sessions_for_test(Duration::from_secs(130));

    pool.poll_full_history(&mut staged_session);
    let relay_data = relay_data(&pool, &relay);
    assert!(relay_data.negentropy_data.is_unsupported());

    let neg_open_count_before = neg_open_count(&captured);

    modify_unbounded_history(&mut pool, wakeup.clone(), sub_id, [relay.clone()]);
    pool.poll_full_history(&mut staged_session);
    wait_for_pool_condition(
        &mut pool,
        Duration::from_millis(200),
        "no NEG-OPEN retry after unsupported timeout",
        |_| true,
    )
    .await;

    let neg_open_count_after = neg_open_count(&captured);
    assert_eq!(neg_open_count_after, neg_open_count_before);
}

#[tokio::test]
async fn timed_out_capable_neg_open_sends_neg_close_before_retry() {
    let (_relay_task, relay, captured, notify) =
        create_capture_relay_with_mode(CaptureRelayMode::Silent).await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let _sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    let neg_open = wait_for_neg_open(&captured, &notify, "capable timeout NEG-OPEN frame").await;
    let frame: Value = serde_json::from_str(&neg_open).expect("parse NEG-OPEN frame");
    let session_id = frame
        .get(1)
        .and_then(Value::as_str)
        .expect("NEG-OPEN session id")
        .to_owned();

    let relay_data = relay_data_mut(&mut pool, &relay);
    relay_data
        .negentropy_data
        .set_capability_for_test(Some(true));
    relay_data
        .negentropy_data
        .age_sessions_for_test(Duration::from_secs(130));

    pool.poll_full_history(&mut staged_session);
    let neg_close =
        wait_for_neg_close(&captured, &notify, "timed-out capable NEG-CLOSE frame").await;
    let frame: Value = serde_json::from_str(&neg_close).expect("parse NEG-CLOSE frame");
    assert_eq!(
        frame.get(1).and_then(Value::as_str),
        Some(session_id.as_str())
    );
}

#[tokio::test]
async fn next_full_history_deadline_reports_active_neg_timeout() {
    let (_relay_task, relay, captured, notify) =
        create_capture_relay_with_mode(CaptureRelayMode::Silent).await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let _sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let before_poll = Instant::now();
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_neg_open(&captured, &notify, "deadline neg-open frame").await;

    let deadline = pool
        .next_full_history_deadline()
        .expect("active NEG session should expose a timeout deadline");
    assert!(deadline > Instant::now());
    assert!(deadline >= before_poll + Duration::from_secs(100));
    assert!(deadline <= before_poll + Duration::from_secs(121));
}
#[tokio::test]
async fn negentropy_notice_on_open_does_not_mark_relay_unsupported() {
    let (_relay_task, relay, captured, notify) = create_capture_relay_with_mode(
        CaptureRelayMode::NoticeOnOpen("ERROR: bad msg: negentropy disabled"),
    )
    .await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_neg_open(&captured, &notify, "notice neg-open frame").await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    pool.try_recv(|_| {});

    let relay_data = relay_data(&pool, &relay);
    assert!(!relay_data.negentropy_data.is_unsupported());
    assert_eq!(relay_data.negentropy_data.active_session_count(), 1);

    let neg_open_count_before = neg_open_count(&captured);

    modify_unbounded_history(&mut pool, wakeup.clone(), sub_id, [relay.clone()]);
    pool.poll_full_history(&mut staged_session);
    wait_for_pool_condition(
        &mut pool,
        Duration::from_millis(200),
        "no extra NEG-OPEN while notice leaves the active session intact",
        |_| true,
    )
    .await;

    let neg_open_count_after = neg_open_count(&captured);
    assert_eq!(neg_open_count_after, neg_open_count_before);
}
#[tokio::test]
async fn invalid_neg_msg_on_open_marks_relay_unsupported_immediately() {
    let (_relay_task, relay, captured, notify) =
        create_capture_relay_with_mode(CaptureRelayMode::InvalidNegMsgOnOpen("not-hex")).await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    let neg_open = wait_for_neg_open(&captured, &notify, "invalid-neg-msg neg-open frame").await;
    let frame: Value = serde_json::from_str(&neg_open).expect("parse NEG-OPEN frame");
    let session_id = frame
        .get(1)
        .and_then(Value::as_str)
        .expect("NEG-OPEN session id")
        .to_owned();

    wait_for_pool_condition(
        &mut pool,
        Duration::from_secs(2),
        "invalid NEG-MSG processing",
        |pool| {
            pool.relays.get(&relay).is_some_and(|relay_data| {
                relay_data.negentropy_data.is_unsupported()
                    && relay_data.negentropy_data.active_session_count() == 0
            })
        },
    )
    .await;

    let neg_close = wait_for_neg_close(&captured, &notify, "invalid-neg-msg NEG-CLOSE frame").await;
    let frame: Value = serde_json::from_str(&neg_close).expect("parse NEG-CLOSE frame");
    assert_eq!(
        frame.get(1).and_then(Value::as_str),
        Some(session_id.as_str())
    );

    let neg_open_count_before = neg_open_count(&captured);

    modify_unbounded_history(&mut pool, wakeup.clone(), sub_id, [relay.clone()]);
    pool.poll_full_history(&mut staged_session);
    wait_for_pool_condition(
        &mut pool,
        Duration::from_millis(200),
        "no NEG-OPEN retry after malformed NEG-MSG",
        |_| true,
    )
    .await;

    let neg_open_count_after = neg_open_count(&captured);
    assert_eq!(neg_open_count_after, neg_open_count_before);
}
#[tokio::test]
async fn stale_neg_msg_after_remove_does_not_stage_fetches() {
    let (_relay_task, relay, captured, notify) =
        create_capture_relay_with_mode(CaptureRelayMode::DelayedValidNegMsgOnClose([0xAB; 32]))
            .await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_neg_open(&captured, &notify, "stale-neg-msg neg-open frame").await;

    remove_full_history(&mut pool, wakeup, sub_id);
    let _ = wait_for_neg_close(&captured, &notify, "stale-neg-msg neg-close frame").await;

    wait_for_pool_condition(
        &mut pool,
        Duration::from_secs(2),
        "stale NEG-MSG processing",
        |_| true,
    )
    .await;
    pool.poll_full_history(&mut staged_session);

    assert!(pool.drain_full_history_needs().is_empty());
    assert!(staged_session.tasks.is_empty());
}
#[tokio::test]
async fn disconnect_during_neg_open_allows_later_retry_after_reschedule() {
    let (_relay_task, relay, captured, notify) = create_capture_relay_with_mode(
        CaptureRelayMode::DisconnectOnOpenOnce(Arc::new(AtomicBool::new(false))),
    )
    .await;
    let mut pool = ready_pool();
    pool.set_keepalive_reconnect_delay(Duration::from_millis(20));
    pool.set_keepalive_reconnect_backoff_base(Duration::from_millis(20));
    let wakeup = MockWakeup::default();

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_neg_open(&captured, &notify, "disconnect neg-open frame").await;

    wait_for_pool_condition(
        &mut pool,
        Duration::from_secs(2),
        "disconnect clears active sessions",
        |pool| {
            pool.relays
                .get(&relay)
                .is_some_and(|relay_data| relay_data.negentropy_data.active_session_count() == 0)
        },
    )
    .await;

    modify_unbounded_history(&mut pool, wakeup.clone(), sub_id, [relay.clone()]);
    pool.poll_full_history(&mut staged_session);
    force_full_history_retries_due(&mut pool, sub_id);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        drive_transport_once(&mut pool, &wakeup);
        pool.poll_full_history(&mut staged_session);
        let neg_open_count = neg_open_count(&captured);
        if neg_open_count >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let neg_open_count = neg_open_count(&captured);
    assert!(
        neg_open_count >= 2,
        "expected a later retry after disconnect"
    );
}
#[tokio::test]
async fn stale_pong_during_neg_open_clears_session_and_retries_after_reconnect() {
    let (_relay_task, relay, captured, notify) =
        create_capture_relay_with_mode(CaptureRelayMode::Silent).await;
    let mut pool = ready_pool();
    pool.set_pong_timeout(Duration::from_millis(40));
    pool.set_keepalive_reconnect_delay(Duration::from_millis(200));
    pool.set_keepalive_reconnect_backoff_base(Duration::from_millis(20));
    let wakeup = MockWakeup::default();

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    wait_for_websocket_connected(&mut pool, &relay, Duration::from_secs(2)).await;
    let mut staged_session = OutboxSession::default();
    pool.poll_full_history(&mut staged_session);
    let _ = wait_for_neg_open(&captured, &notify, "stale-pong neg-open frame").await;

    let disconnect_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < disconnect_deadline {
        drive_transport_once(&mut pool, &wakeup);
        if pool.relays.get(&relay).is_some_and(|relay_data| {
            pool.websocket_statuses().get(&relay) == Some(&RelayStatus::Disconnected)
                && relay_data.negentropy_data.active_session_count() == 0
        }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        pool.relays.get(&relay).is_some_and(|relay_data| {
            pool.websocket_statuses().get(&relay) == Some(&RelayStatus::Disconnected)
                && relay_data.negentropy_data.active_session_count() == 0
        }),
        "stale-pong disconnect should clear the active negentropy session"
    );

    pool.poll_full_history(&mut staged_session);
    force_full_history_retries_due(&mut pool, sub_id);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        drive_transport_once(&mut pool, &wakeup);
        pool.poll_full_history(&mut staged_session);
        let neg_open_count = neg_open_count(&captured);
        if neg_open_count >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let neg_open_count = neg_open_count(&captured);
    assert!(
        neg_open_count >= 2,
        "expected reconnect to retry NEG-OPEN after stale-pong disconnect"
    );
}
