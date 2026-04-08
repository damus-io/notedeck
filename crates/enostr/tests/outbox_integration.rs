//! Integration tests for the Outbox relay system
//!
//! These tests use `nostr-relay-builder::LocalRelay` to run a real relay on localhost
//! and test the full subscription lifecycle, EOSE propagation, and multi-relay coordination.

use enostr::{
    FullKeypair, Nip11ApplyOutcome, Nip11LimitationsRaw, NormRelayUrl, OutboxPool,
    OutboxSessionHandler, OutboxSubId, RelayId, RelayReqStatus, RelayRoutingPreference,
    RelayStatus, RelayUrlPkgs, Wakeup,
};
use futures_util::{SinkExt, StreamExt};
use hashbrown::HashSet;
use nostr::{Event, JsonUtil};
use nostr_relay_builder::{
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use nostrdb::{Filter, NoteBuilder};
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Once,
};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::{accept_async, tungstenite::Message};

static TRACING_INIT: Once = Once::new();

/// Initialize tracing for tests (only runs once even if called multiple times)
fn init_tracing() {
    TRACING_INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("enostr=debug".parse().unwrap()),
            )
            .with_test_writer()
            .init();
    });
}

/// A mock Wakeup implementation for integration tests
#[derive(Clone, Default)]
pub struct MockWakeup {}

impl Wakeup for MockWakeup {
    fn wake(&self) {}
}

/// Helper to create a LocalRelay with default settings for tests.
/// Returns the relay handle (must be kept alive) and its normalized URL.
async fn create_test_relay() -> (LocalRelay, NormRelayUrl) {
    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("failed to start relay");

    let url_str = relay.url();
    tracing::info!("LocalRelay listening at {}", url_str);

    let url = NormRelayUrl::new(&url_str).expect("valid relay url");
    (relay, url)
}

/// Helper to create a LocalRelay pre-seeded with one kind-1 note that matches `trivial_filter`.
async fn create_test_relay_with_seeded_note() -> (LocalRelay, NormRelayUrl) {
    let (relay, url, _) = create_test_relay_with_seeded_notes(1).await;
    (relay, url)
}

/// Helper to create a LocalRelay pre-seeded with `count` kind-1 notes that
/// all match `trivial_filter`.
async fn create_test_relay_with_seeded_notes(
    count: usize,
) -> (LocalRelay, NormRelayUrl, Vec<String>) {
    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });

    let signer = FullKeypair::generate();
    let mut event_ids = Vec::with_capacity(count);
    for i in 0..count {
        let note = NoteBuilder::new()
            .kind(1)
            .content(&format!("seeded relay note {i}"))
            .sign(&signer.secret_key.secret_bytes())
            .build()
            .expect("build seeded note");
        let event = Event::from_json(note.json().expect("seeded note json")).expect("parse event");
        event_ids.push(event.id.to_hex());
        relay_db.save_event(&event).await.expect("seed relay event");
    }

    let relay = LocalRelay::run(RelayBuilder::default().database(relay_db))
        .await
        .expect("failed to start seeded relay");

    let url_str = relay.url();
    let url = NormRelayUrl::new(&url_str).expect("valid relay url");
    (relay, url, event_ids)
}

/// Helper to create a tiny relay that sends one NOTICE, then the provided
/// events, then EOSE for the first REQ it receives.
async fn create_notice_then_events_relay(
    events_json: Vec<String>,
) -> (tokio::task::JoinHandle<()>, NormRelayUrl) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind notice relay");
    let addr = listener.local_addr().expect("notice relay addr");
    let url = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid notice relay url");

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept notice relay");
        let mut websocket = accept_async(stream).await.expect("upgrade notice relay");

        while let Some(msg) = websocket.next().await {
            let Message::Text(text) = msg.expect("read notice relay message") else {
                continue;
            };

            let parts: serde_json::Value =
                serde_json::from_str(&text).expect("parse REQ from client");
            if parts[0] != "REQ" {
                continue;
            }

            let sid = parts[1].as_str().expect("REQ sid");
            websocket
                .send(Message::Text(r#"["NOTICE","queued notice"]"#.to_owned()))
                .await
                .expect("send notice");

            for event_json in &events_json {
                websocket
                    .send(Message::Text(
                        serde_json::json!([
                            "EVENT",
                            sid,
                            serde_json::from_str::<serde_json::Value>(event_json)
                                .expect("parse event json for relay frame")
                        ])
                        .to_string(),
                    ))
                    .await
                    .expect("send event frame");
            }

            websocket
                .send(Message::Text(serde_json::json!(["EOSE", sid]).to_string()))
                .await
                .expect("send eose");
            break;
        }
    });

    (handle, url)
}

/// Helper to create a websocket relay that accepts one connection and never
/// sends pong frames.
async fn create_silent_websocket_relay() -> (tokio::task::JoinHandle<()>, NormRelayUrl) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind silent relay");
    let addr = listener.local_addr().expect("silent relay addr");
    let url = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid silent relay url");

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept silent relay");
        let mut websocket = accept_async(stream).await.expect("upgrade silent relay");

        while let Some(msg) = websocket.next().await {
            match msg {
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    (handle, url)
}

/// Helper to create a websocket relay that accepts one connection, sends one
/// pong immediately, then continues sending pong frames at a fixed interval.
async fn create_periodic_pong_relay(
    pong_interval: Duration,
) -> (tokio::task::JoinHandle<()>, NormRelayUrl, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind periodic-pong relay");
    let addr = listener.local_addr().expect("periodic-pong relay addr");
    let url = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid periodic-pong relay url");
    let pong_count = Arc::new(AtomicUsize::new(0));
    let relay_pong_count = Arc::clone(&pong_count);

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept periodic-pong relay");
        let mut websocket = accept_async(stream)
            .await
            .expect("upgrade periodic-pong relay");

        if websocket.send(Message::Pong(Vec::new())).await.is_err() {
            return;
        }
        relay_pong_count.fetch_add(1, Ordering::SeqCst);

        let mut interval = tokio::time::interval(pong_interval);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if websocket.send(Message::Pong(Vec::new())).await.is_err() {
                        break;
                    }
                    relay_pong_count.fetch_add(1, Ordering::SeqCst);
                }
                msg = websocket.next() => {
                    match msg {
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        _ => {}
                    }
                }
            }
        }
    });

    (handle, url, pong_count)
}

/// Helper to create a websocket relay that counts inbound ping frames from the
/// client connection.
async fn create_ping_counting_relay(
) -> (tokio::task::JoinHandle<()>, NormRelayUrl, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ping-counting relay");
    let addr = listener.local_addr().expect("ping-counting relay addr");
    let url = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid ping-counting relay url");
    let ping_count = Arc::new(AtomicUsize::new(0));
    let ping_count_task = ping_count.clone();

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept ping-counting relay");
        let mut websocket = accept_async(stream)
            .await
            .expect("upgrade ping-counting relay");

        while let Some(msg) = websocket.next().await {
            match msg {
                Ok(Message::Ping(_)) => {
                    ping_count_task.fetch_add(1, Ordering::SeqCst);
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    (handle, url, ping_count)
}

/// Helper to create a websocket relay that counts repeated client
/// connections and keeps each upgraded socket alive until the client drops it.
async fn create_reconnect_counting_relay(
) -> (tokio::task::JoinHandle<()>, NormRelayUrl, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind reconnect-counting relay");
    let addr = listener
        .local_addr()
        .expect("reconnect-counting relay addr");
    let url =
        NormRelayUrl::new(&format!("ws://{addr}")).expect("valid reconnect-counting relay url");
    let connection_count = Arc::new(AtomicUsize::new(0));
    let connection_count_task = connection_count.clone();

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            connection_count_task.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let Ok(mut websocket) = accept_async(stream).await else {
                    return;
                };
                while let Some(msg) = websocket.next().await {
                    match msg {
                        Ok(Message::Close(_)) | Err(_) => break,
                        _ => {}
                    }
                }
            });
        }
    });

    (handle, url, connection_count)
}

async fn create_reconnect_counting_relay_at(
    addr: SocketAddr,
) -> (tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(addr)
        .await
        .expect("bind reconnect-counting relay");
    let connection_count = Arc::new(AtomicUsize::new(0));
    let connection_count_task = connection_count.clone();

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            connection_count_task.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let Ok(mut websocket) = accept_async(stream).await else {
                    return;
                };
                while let Some(msg) = websocket.next().await {
                    match msg {
                        Ok(Message::Close(_)) | Err(_) => break,
                        _ => {}
                    }
                }
            });
        }
    });

    (handle, connection_count)
}

async fn create_delayed_close_relay(
    close_after: Duration,
) -> (tokio::task::JoinHandle<()>, NormRelayUrl, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind delayed-close relay");
    let addr = listener.local_addr().expect("delayed-close relay addr");
    let url = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid delayed-close relay url");
    let connection_count = Arc::new(AtomicUsize::new(0));
    let connection_count_task = Arc::clone(&connection_count);

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let connection_number = connection_count_task.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::spawn(async move {
                let Ok(mut websocket) = accept_async(stream).await else {
                    return;
                };

                if connection_number == 1 {
                    tokio::time::sleep(close_after).await;
                    let _ = websocket.close(None).await;
                    return;
                }

                while let Some(msg) = websocket.next().await {
                    match msg {
                        Ok(Message::Close(_)) | Err(_) => break,
                        _ => {}
                    }
                }
            });
        }
    });

    (handle, url, connection_count)
}

async fn create_shutdownable_silent_relay_at(
    addr: SocketAddr,
) -> (tokio::task::JoinHandle<()>, oneshot::Sender<()>) {
    let listener = TcpListener::bind(addr)
        .await
        .expect("bind shutdownable relay");
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

    let handle = tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut websocket) = accept_async(stream).await else {
            return;
        };

        tokio::select! {
            _ = &mut shutdown_rx => {}
            _ = async {
                while let Some(msg) = websocket.next().await {
                    match msg {
                        Ok(Message::Close(_)) | Err(_) => break,
                        _ => {}
                    }
                }
            } => {}
        }
    });

    (handle, shutdown_tx)
}

/// Helper to create a websocket relay that captures inbound EVENT frames from
/// the client connection.
async fn create_event_capture_relay() -> (
    tokio::task::JoinHandle<()>,
    NormRelayUrl,
    Arc<std::sync::Mutex<Vec<String>>>,
) {
    let (listener, addr) = bind_test_tcp_listener().await;
    let url = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid event-capture relay url");
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_task = captured.clone();

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let captured_task = captured_task.clone();
            tokio::spawn(async move {
                let Ok(mut websocket) = accept_async(stream).await else {
                    return;
                };

                while let Some(msg) = websocket.next().await {
                    let Ok(Message::Text(text)) = msg else {
                        continue;
                    };

                    if text.starts_with("[\"EVENT\",") {
                        captured_task
                            .lock()
                            .expect("lock captured events")
                            .push(text);
                    }
                }
            });
        }
    });

    (handle, url, captured)
}

fn reserve_free_socket_addr() -> SocketAddr {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("reserve free socket addr");
    let addr = listener.local_addr().expect("reserved socket addr");
    drop(listener);
    addr
}

async fn bind_test_tcp_listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test tcp listener");
    let addr = listener.local_addr().expect("test tcp listener addr");
    (listener, addr)
}

/// Polls the pool until the provided predicate returns true or the attempt limit is reached.
/// Returns the attempt count and whether the predicate was ultimately satisfied.
async fn pump_pool_until<F>(
    pool: &mut OutboxPool,
    max_attempts: usize,
    sleep_duration: Duration,
    mut predicate: F,
) -> bool
where
    F: FnMut(&mut OutboxPool) -> bool,
{
    let mut attempts = 0;
    for attempt in 0..max_attempts {
        pool.try_recv(10, |_| {});
        if predicate(pool) {
            return true;
        }
        tokio::time::sleep(sleep_duration).await;
        attempts = attempt;
    }

    tracing::trace!("completed pool pump in {attempts} attempts");

    predicate(pool)
}

async fn default_pool_pump<F>(pool: &mut OutboxPool, predicate: F) -> bool
where
    F: FnMut(&mut OutboxPool) -> bool,
{
    pump_pool_until(pool, 100, Duration::from_millis(15), predicate).await
}

fn websocket_status(pool: &OutboxPool, url: &NormRelayUrl) -> Option<RelayStatus> {
    pool.websocket_statuses()
        .into_iter()
        .find_map(|(relay_url, status)| (*relay_url == *url).then_some(status))
}

async fn wait_for_sent_pong(pong_count: &AtomicUsize, observed: usize) -> usize {
    tokio::time::timeout(Duration::from_millis(200), async {
        loop {
            let sent = pong_count.load(Ordering::SeqCst);
            if sent > observed {
                return sent;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("relay should send another pong before timeout")
}

async fn wait_for_last_pong_advance(
    pool: &mut OutboxPool,
    url: &NormRelayUrl,
    previous: Instant,
) -> Instant {
    tokio::time::timeout(Duration::from_millis(200), async {
        loop {
            pool.try_recv(10, |_| {});
            let Some(last_pong) = pool.websocket_last_pong(url) else {
                panic!("relay should remain tracked while pongs are flowing");
            };
            if last_pong > previous {
                return last_pong;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("outbox should ingest the next pong before timeout")
}

async fn pump_pool_until_with_note_budget<F>(
    pool: &mut OutboxPool,
    max_notes: usize,
    max_attempts: usize,
    sleep_duration: Duration,
    mut predicate: F,
) -> bool
where
    F: FnMut(&mut OutboxPool) -> bool,
{
    for _ in 0..max_attempts {
        pool.try_recv(max_notes, |_| {});
        if predicate(pool) {
            return true;
        }
        tokio::time::sleep(sleep_duration).await;
    }

    predicate(pool)
}

// ==================== Full Subscription Lifecycle ====================

#[tokio::test]
async fn full_subscription_lifecycle() {
    init_tracing();

    // Start local relay
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();

    // 1. Subscribe to the local relay
    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::new(urls);

    let id = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), url_pkgs)
    }; // session dropped, REQ sent to relay

    let has_eose = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        pool.has_eose(&id)
    })
    .await;

    assert!(has_eose, "should have received EOSE from relay");

    // 4. Unsubscribe
    {
        let mut session = pool.start_session(wakeup.clone());
        session.unsubscribe(id);
    }

    // 5. Verify cleaned up
    let status = pool.status(&id);
    assert!(
        status.is_empty(),
        "status should be empty after unsubscribe"
    );
}

// ==================== EOSE Flow End-to-End ====================

#[tokio::test]
async fn eose_propagation_from_real_relay() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();

    // Subscribe with transparent mode (faster EOSE)
    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::with_preference(urls, RelayRoutingPreference::PreferDedicated);

    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(
            vec![Filter::new().kinds(vec![1]).limit(10).build()],
            url_pkgs,
        )
    };

    let got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&id)).await;

    assert!(got_eose, "EOSE should propagate from relay to pool",);
}

// ==================== Multi-Relay Coordination ====================

#[tokio::test]
async fn subscribe_to_multiple_relays() {
    // Start two local relays
    let (_relay1, url1) = create_test_relay().await;
    let (_relay2, url2) = create_test_relay().await;

    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();

    // Subscribe to both relays
    let mut urls = HashSet::new();
    urls.insert(url1.clone());
    urls.insert(url2.clone());
    let url_pkgs = RelayUrlPkgs::new(urls);

    let id = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(vec![Filter::new().kinds(vec![1]).build()], url_pkgs)
    };

    let got_eoses = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        pool.all_have_eose(&id)
    })
    .await;

    let status = pool.status(&id);
    assert_eq!(status.len(), 2);
    assert!(got_eoses, "should have eoses from both relays");
}

// ==================== Modify Relays Mid-Subscription ====================

#[tokio::test]
async fn modify_relays_adds_and_removes() {
    init_tracing();

    let (_relay1, url1) = create_test_relay().await;
    let (_relay2, url2) = create_test_relay().await;

    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();

    // Start with relay1 only
    let mut urls1 = HashSet::new();
    urls1.insert(url1.clone());

    let id = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(
            vec![Filter::new().kinds(vec![1]).build()],
            RelayUrlPkgs::new(urls1),
        )
    };

    {
        let status = pool.status(&id);
        assert_eq!(status.len(), 1);
        let (url, res) = status.into_iter().next().unwrap();
        assert_eq!(*url, url1);
        assert_eq!(res, RelayReqStatus::InitialQuery);
    }

    let all_eose = default_pool_pump(&mut pool, |pool| pool.all_have_eose(&id)).await;
    assert!(all_eose);

    {
        let status = pool.status(&id);
        assert_eq!(status.len(), 1);
        let (url, _) = status.into_iter().next().unwrap();
        assert_eq!(*url, url1.clone());
    }

    // Switch to relay2 only
    let mut urls2 = HashSet::new();
    urls2.insert(url2.clone());

    {
        let mut session = pool.start_session(wakeup.clone());
        session.modify_relays(id, urls2);
    }

    {
        let status = pool.status(&id);
        assert_eq!(status.len(), 1);
        let (url, res) = status.into_iter().next().unwrap();
        assert_eq!(*url, url2);
        assert_eq!(res, RelayReqStatus::InitialQuery);
    }

    let all_eose = default_pool_pump(&mut pool, |pool| pool.all_have_eose(&id)).await;
    tracing::info!("pool status: {:?}", pool.status(&id));
    assert!(all_eose);

    let status = pool.status(&id);
    assert_eq!(
        status.len(),
        1,
        "we are replacing relay {:?} with {:?}",
        url1,
        url2
    );
    let (url, _) = status.into_iter().next().unwrap();
    assert_eq!(
        *url, url2,
        "we are replacing relay {:?} with {:?}",
        url1, url2
    );
}

// ==================== Subscription with Filters ====================

#[tokio::test]
async fn subscription_with_complex_filters() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::new(urls);

    // Use a more complex filter
    let filters = vec![
        Filter::new().kinds(vec![1]).build(),
        Filter::new().kinds(vec![0]).build(),
        Filter::new().kinds(vec![3]).build(),
        Filter::new().kinds(vec![4]).limit(100).build(),
    ];

    let id = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(filters, url_pkgs)
    };

    let got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&id)).await;
    assert!(got_eose, "should receive EOSE even with multiple filters");
}

// ==================== Multiple Concurrent Subscriptions ====================

#[tokio::test]
async fn multiple_concurrent_subscriptions() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());

    // Create multiple subscriptions
    let mut ids: Vec<OutboxSubId> = Vec::new();

    {
        let mut session = pool.start_session(wakeup.clone());

        for kind in 0..5 {
            let id = session.subscribe(
                vec![Filter::new().kinds(vec![kind]).build()],
                RelayUrlPkgs::new(urls.clone()),
            );
            ids.push(id);
        }
    }

    assert_eq!(ids.len(), 5);

    let all_eose = default_pool_pump(&mut pool, |pool| {
        ids.iter().filter(|id| pool.has_eose(id)).count() == 5
    })
    .await;

    assert!(all_eose, "at least one subscription should have EOSE");
}

// ==================== Unsubscribe During Processing ====================

#[tokio::test]
async fn unsubscribe_during_processing() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::new(urls);

    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(vec![Filter::new().kinds(vec![1]).build()], url_pkgs)
    };

    // Immediately unsubscribe
    {
        let mut session = pool.start_session(MockWakeup::default());
        session.unsubscribe(id);
    }

    let empty = default_pool_pump(&mut pool, |pool| pool.status(&id).is_empty()).await;

    // Status should be empty after unsubscribe
    assert!(empty, "status should be empty after unsubscribe");
}

// ==================== Routing Preference Modes ====================

/// `PreferDedicated` should receive its initial query response when the relay
/// is not saturated.
#[tokio::test]
async fn prefer_dedicated_subscription_receives_eose_when_unsaturated() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::with_preference(urls, RelayRoutingPreference::PreferDedicated);

    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), url_pkgs)
    };

    let got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&id)).await;
    assert!(
        got_eose,
        "prefer-dedicated subscription should receive EOSE when dedicated capacity is available"
    );
}

/// `NoPreference` still receives its initial query response when the relay is
/// not saturated.
#[tokio::test]
async fn no_preference_subscription_receives_eose_when_unsaturated() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::with_preference(urls, RelayRoutingPreference::NoPreference);

    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), url_pkgs)
    };

    let got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&id)).await;
    assert!(
        got_eose,
        "no-preference subscription should still receive EOSE when dedicated capacity is available"
    );
}

/// When dedicated capacity is saturated, a `NoPreference` request should fall
/// through to compaction rather than displacing an existing dedicated route.
#[tokio::test]
async fn no_preference_request_falls_back_to_compaction_when_dedicated_is_saturated() {
    let (_relay, url) = create_test_relay().await;
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_280);

    let mut dedicated_urls = HashSet::new();
    dedicated_urls.insert(url.clone());
    let preferred_pkg =
        RelayUrlPkgs::with_preference(dedicated_urls, RelayRoutingPreference::PreferDedicated);

    let first_id = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), preferred_pkg)
    };

    let applied = pool.apply_nip11_limits(
        &url,
        Nip11LimitationsRaw {
            max_subscriptions: Some(1),
            ..Default::default()
        },
        now,
    );
    assert!(matches!(
        applied,
        Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
    ));

    let mut fallback_urls = HashSet::new();
    fallback_urls.insert(url.clone());
    let no_preference_pkg =
        RelayUrlPkgs::with_preference(fallback_urls, RelayRoutingPreference::NoPreference);

    let second_id = {
        let mut session = pool.start_session(wakeup);
        session.subscribe(trivial_filter(), no_preference_pkg)
    };

    let first_got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&first_id)).await;
    assert!(
        first_got_eose,
        "existing dedicated subscription should stay active while saturation is in effect"
    );
    assert!(
        !pool.has_eose(&second_id),
        "no-preference request should not displace the existing dedicated route"
    );

    {
        let mut session = pool.start_session(MockWakeup::default());
        session.unsubscribe(first_id);
    }

    let second_got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&second_id)).await;
    assert!(
        second_got_eose,
        "no-preference request should become active once compaction can claim capacity"
    );
}

// ==================== Modify Filters Mid-Subscription ====================

#[tokio::test]
async fn modify_filters_mid_subscription() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::new(urls);

    // Start with kind 1
    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), url_pkgs)
    };

    // Modify to kind 4
    {
        let mut session = pool.start_session(MockWakeup::default());
        session.modify_filters(id, vec![Filter::new().kinds(vec![4]).limit(9).build()]);
    }

    let got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&id)).await;
    assert!(got_eose, "should receive EOSE");
}

// ==================== Connection Resilience ====================

fn trivial_filter() -> Vec<Filter> {
    vec![Filter::new().kinds([1]).build()]
}

#[tokio::test]
async fn websocket_status_tracking() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::new(urls);

    {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), url_pkgs);
    }

    // Check websocket statuses
    let statuses = pool.websocket_statuses();
    // Should have at least one relay tracked
    assert!(!statuses.is_empty(), "should track websocket statuses");
}

// ==================== Failure Paths ====================

/// Subscribing to an unreachable relay should remain disconnected and never report EOSE.
#[tokio::test]
async fn unreachable_relay_reports_disconnected_status() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let unreachable =
        NormRelayUrl::new("wss://127.0.0.1:6555").expect("valid unreachable relay url");

    let mut urls = HashSet::new();
    urls.insert(unreachable.clone());
    let url_pkgs = RelayUrlPkgs::new(urls);

    let id = {
        let mut session = pool.start_session(wakeup);
        session.subscribe(trivial_filter(), url_pkgs)
    };

    // Pump until the relay transitions to Disconnected. Windows TCP
    // connect-to-refused-port can take ~1 s vs near-instant on
    // Linux/macOS, so we poll for the target status directly instead
    // of waiting a fixed duration.
    let became_disconnected = pump_pool_until(&mut pool, 50, Duration::from_millis(100), |pool| {
        pool.websocket_statuses()
            .into_iter()
            .any(|(url, s)| *url == unreachable && s == RelayStatus::Disconnected)
    })
    .await;
    assert!(
        became_disconnected,
        "unreachable relay should report Disconnected"
    );

    assert!(
        !pool.has_eose(&id),
        "unreachable relay should never yield an EOSE signal"
    );

    // Should survive keepalive pings even when no websocket is available.
    pool.keepalive_ping(|| {});
}

/// A connected relay with no fresh pong should be marked disconnected when the
/// pong timeout expires.
#[tokio::test]
async fn keepalive_marks_connected_relay_disconnected_after_pong_timeout() {
    let (_relay_task, url) = create_silent_websocket_relay().await;
    let mut pool = OutboxPool::default();
    pool.set_pong_timeout(Duration::from_millis(20));

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), RelayUrlPkgs::new(urls))
    };

    let connected = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
    })
    .await;
    assert!(connected, "relay should reach Connected before timeout");
    assert!(
        !pool.has_eose(&id),
        "silent relay should not produce EOSE in this setup"
    );

    tokio::time::sleep(Duration::from_millis(25)).await;
    pool.keepalive_ping(|| {});

    assert_eq!(
        websocket_status(&pool, &url),
        Some(RelayStatus::Disconnected),
        "stale pong should force the connected relay to Disconnected"
    );
}

/// A connected relay that keeps sending pong frames should remain connected
/// even after wall-clock time exceeds the configured pong timeout.
#[tokio::test]
async fn keepalive_keeps_connected_relay_alive_when_pongs_continue() {
    let (_relay_task, url, pong_count) = create_periodic_pong_relay(Duration::from_millis(5)).await;
    let mut pool = OutboxPool::default();
    let pong_timeout = Duration::from_millis(20);
    pool.set_pong_timeout(pong_timeout);

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), RelayUrlPkgs::new(urls));
    }

    let connected = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
    })
    .await;
    assert!(connected, "relay should reach Connected before pong checks");

    let mut observed_pongs = pong_count.load(Ordering::SeqCst);
    let mut last_pong = pool
        .websocket_last_pong(&url)
        .expect("connected relay should have an initial pong timestamp");
    let start = Instant::now();

    while start.elapsed() <= pong_timeout {
        observed_pongs = wait_for_sent_pong(&pong_count, observed_pongs).await;
        last_pong = wait_for_last_pong_advance(&mut pool, &url, last_pong).await;
        pool.keepalive_ping(|| {});
        assert_eq!(
            websocket_status(&pool, &url),
            Some(RelayStatus::Connected),
            "keepalive should not disconnect a relay after ingesting a fresh pong"
        );
    }

    assert!(
        start.elapsed() > pong_timeout,
        "test should run long enough to cross the configured pong timeout"
    );
    assert_eq!(
        websocket_status(&pool, &url),
        Some(RelayStatus::Connected),
        "fresh pong frames should keep the relay connected"
    );
}

/// A connected relay should receive websocket ping frames on the configured
/// keepalive schedule.
#[tokio::test]
async fn keepalive_sends_ping_on_configured_schedule() {
    let (_relay_task, url, ping_count) = create_ping_counting_relay().await;
    let mut pool = OutboxPool::default();
    pool.set_keepalive_ping_rate(Duration::from_millis(10));
    pool.set_pong_timeout(Duration::from_secs(1));

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), RelayUrlPkgs::new(urls));
    }

    let connected = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
    })
    .await;
    assert!(connected, "relay should reach Connected before ping checks");

    tokio::time::sleep(Duration::from_millis(15)).await;
    pool.keepalive_ping(|| {});

    let saw_ping = tokio::time::timeout(Duration::from_millis(50), async {
        loop {
            if ping_count.load(Ordering::SeqCst) > 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or(false);

    assert!(
        saw_ping,
        "keepalive should send a websocket ping once the configured interval elapses"
    );
}

/// After a stale-pong timeout, reconnect backoff should be measured from the
/// disconnect moment before a new websocket connection is attempted.
#[tokio::test]
async fn keepalive_reconnects_only_after_configured_backoff_from_timeout() {
    let (_relay_task, url, connection_count) = create_reconnect_counting_relay().await;
    let mut pool = OutboxPool::default();
    pool.set_pong_timeout(Duration::from_millis(20));
    pool.set_keepalive_reconnect_delay(Duration::from_millis(30));

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), RelayUrlPkgs::new(urls));
    }

    let connected = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
    })
    .await;
    assert!(connected, "relay should reach Connected before timeout");
    assert_eq!(
        connection_count.load(Ordering::SeqCst),
        1,
        "test relay should observe exactly one initial websocket connection"
    );

    tokio::time::sleep(Duration::from_millis(25)).await;
    pool.keepalive_ping(|| {});
    assert_eq!(
        websocket_status(&pool, &url),
        Some(RelayStatus::Disconnected),
        "stale pong should mark the relay disconnected before reconnect begins"
    );

    tokio::time::sleep(Duration::from_millis(10)).await;
    pool.keepalive_ping(|| {});
    assert_eq!(
        connection_count.load(Ordering::SeqCst),
        1,
        "reconnect must not happen before the configured backoff elapses"
    );

    tokio::time::sleep(Duration::from_millis(25)).await;
    pool.keepalive_ping(|| {});

    let reconnected = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
            && connection_count.load(Ordering::SeqCst) >= 2
    })
    .await;
    assert!(
        reconnected,
        "relay should reconnect after the configured backoff elapses"
    );
}

/// After a relay reconnects, the new websocket leg should start with a fresh
/// pong timestamp and keep using the configured reconnect delay.
#[tokio::test]
async fn keepalive_reconnect_open_refreshes_pong_and_preserves_configured_delay() {
    let (_relay_task, url, connection_count) = create_reconnect_counting_relay().await;
    let mut pool = OutboxPool::default();
    pool.set_pong_timeout(Duration::from_millis(20));
    pool.set_keepalive_reconnect_delay(Duration::from_millis(30));

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), RelayUrlPkgs::new(urls));
    }

    let connected = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
    })
    .await;
    assert!(connected, "relay should reach Connected before timeout");

    tokio::time::sleep(Duration::from_millis(25)).await;
    pool.keepalive_ping(|| {});

    tokio::time::sleep(Duration::from_millis(35)).await;
    pool.keepalive_ping(|| {});

    let reconnected_once = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
            && connection_count.load(Ordering::SeqCst) >= 2
    })
    .await;
    assert!(
        reconnected_once,
        "relay should reconnect once the first configured backoff elapses"
    );

    tokio::time::sleep(Duration::from_millis(5)).await;
    pool.keepalive_ping(|| {});
    assert_eq!(
        websocket_status(&pool, &url),
        Some(RelayStatus::Connected),
        "reopened websocket should not immediately time out from the previous leg's stale pong"
    );

    tokio::time::sleep(Duration::from_millis(25)).await;
    pool.keepalive_ping(|| {});
    assert_eq!(
        websocket_status(&pool, &url),
        Some(RelayStatus::Disconnected),
        "second stale-pong window should still disconnect the reopened websocket"
    );

    tokio::time::sleep(Duration::from_millis(10)).await;
    pool.keepalive_ping(|| {});
    assert_eq!(
        connection_count.load(Ordering::SeqCst),
        2,
        "second reconnect must not happen before the configured backoff elapses"
    );

    tokio::time::sleep(Duration::from_millis(25)).await;
    pool.keepalive_ping(|| {});

    let reconnected_twice = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
            && connection_count.load(Ordering::SeqCst) >= 3
    })
    .await;
    assert!(
        reconnected_twice,
        "reopened websocket should keep using the configured reconnect delay after later timeouts"
    );
}

/// A server-driven close should start reconnect timing from the disconnect
/// moment, not from the original websocket open.
#[tokio::test]
async fn keepalive_closed_relay_waits_for_configured_reconnect_delay() {
    let (_relay_task, url, connection_count) =
        create_delayed_close_relay(Duration::from_millis(40)).await;
    let mut pool = OutboxPool::default();
    pool.set_pong_timeout(Duration::from_secs(1));
    pool.set_keepalive_reconnect_delay(Duration::from_millis(30));

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), RelayUrlPkgs::new(urls));
    }

    let connected = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
    })
    .await;
    assert!(
        connected,
        "relay should reach Connected before the server closes it"
    );

    let disconnected = pump_pool_until(&mut pool, 100, Duration::from_millis(5), |pool| {
        pool.try_recv(10, |_| {});
        websocket_status(pool, &url) == Some(RelayStatus::Disconnected)
    })
    .await;
    assert!(
        disconnected,
        "server-driven close should transition the relay onto the disconnected reconnect path"
    );

    pool.keepalive_ping(|| {});
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(
        connection_count.load(Ordering::SeqCst),
        1,
        "close-driven reconnect should still honor the configured reconnect delay"
    );

    tokio::time::sleep(Duration::from_millis(25)).await;
    pool.keepalive_ping(|| {});
    let reconnected = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
            && connection_count.load(Ordering::SeqCst) >= 2
    })
    .await;
    assert!(
        reconnected,
        "relay should reconnect once the close-driven reconnect delay has elapsed"
    );
}

/// Broadcasting to a websocket relay should eventually send an `EVENT` frame
/// through the relay's broadcast path.
#[tokio::test]
async fn broadcast_note_sends_event_to_websocket_relay() {
    let (_relay_task, url, captured) = create_event_capture_relay().await;
    let mut pool = OutboxPool::default();
    let signer = FullKeypair::generate();
    let note = NoteBuilder::new()
        .kind(1)
        .content("broadcast websocket test")
        .sign(&signer.secret_key.secret_bytes())
        .build()
        .expect("build websocket broadcast note");
    let note_id_hex = hex::encode(note.id());

    {
        let mut session = pool.start_session(MockWakeup::default());
        session.broadcast_note(&note, vec![RelayId::Websocket(url.clone())]);
    }

    let delivered = pump_pool_until(&mut pool, 100, Duration::from_millis(10), |_pool| {
        captured
            .lock()
            .expect("lock captured events")
            .iter()
            .any(|frame| frame.contains(&note_id_hex))
    })
    .await;

    assert!(
        delivered,
        "websocket broadcast path should eventually send the note as an EVENT frame"
    );
}

/// Broadcasting while the websocket relay is disconnected should queue the
/// event and flush it once the relay reopens.
#[tokio::test]
async fn broadcast_note_queues_while_disconnected_and_flushes_on_reopen() {
    let (_relay_task, url, captured) = create_event_capture_relay().await;
    let mut pool = OutboxPool::default();
    pool.set_pong_timeout(Duration::from_millis(20));
    pool.set_keepalive_reconnect_delay(Duration::from_millis(30));
    let signer = FullKeypair::generate();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), RelayUrlPkgs::new(urls));
    }

    let connected = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
    })
    .await;
    assert!(connected, "relay should connect before disconnecting it");

    tokio::time::sleep(Duration::from_millis(25)).await;
    pool.keepalive_ping(|| {});
    assert_eq!(
        websocket_status(&pool, &url),
        Some(RelayStatus::Disconnected),
        "stale pong should disconnect the websocket before queueing the broadcast"
    );

    let note = NoteBuilder::new()
        .kind(1)
        .content("broadcast queued while disconnected")
        .sign(&signer.secret_key.secret_bytes())
        .build()
        .expect("build queued broadcast note");
    let note_id_hex = hex::encode(note.id());
    {
        let mut session = pool.start_session(MockWakeup::default());
        session.broadcast_note(&note, vec![RelayId::Websocket(url.clone())]);
    }
    assert!(
        !captured
            .lock()
            .expect("lock captured events")
            .iter()
            .any(|frame| frame.contains(&note_id_hex)),
        "disconnected websocket should queue the broadcast instead of sending it immediately"
    );

    tokio::time::sleep(Duration::from_millis(35)).await;
    pool.keepalive_ping(|| {});

    let delivered = pump_pool_until(&mut pool, 100, Duration::from_millis(10), |pool| {
        pool.try_recv(10, |_| {});
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
            && captured
                .lock()
                .expect("lock captured events")
                .iter()
                .any(|frame| frame.contains(&note_id_hex))
    })
    .await;
    assert!(
        delivered,
        "queued websocket broadcast should flush once the relay reopens"
    );
}

/// After an outage and several reconnect opportunities with no server bound,
/// awaiting the old relay teardown should let a later rebind recover cleanly
/// on the same address.
#[tokio::test]
async fn websocket_reconnect_recovers_when_relay_returns_after_outage() {
    let addr = reserve_free_socket_addr();
    let (first_relay_task, shutdown_first_relay) = create_shutdownable_silent_relay_at(addr).await;
    let url = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid delayed relay url");
    let mut pool = OutboxPool::default();
    pool.set_pong_timeout(Duration::from_millis(20));
    pool.set_keepalive_reconnect_delay(Duration::from_millis(5));
    pool.set_keepalive_reconnect_backoff_base(Duration::from_millis(10));

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), RelayUrlPkgs::new(urls));
    }

    let disconnected = pump_pool_until(&mut pool, 100, Duration::from_millis(10), |pool| {
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
    })
    .await;
    assert!(
        disconnected,
        "relay should establish an initial websocket connection"
    );

    tokio::time::sleep(Duration::from_millis(25)).await;
    pool.keepalive_ping(|| {});
    assert_eq!(
        websocket_status(&pool, &url),
        Some(RelayStatus::Disconnected),
        "stale pong should first move the live websocket onto the disconnected reconnect path"
    );

    let _ = shutdown_first_relay.send(());
    tokio::time::timeout(Duration::from_secs(1), first_relay_task)
        .await
        .expect("first relay shutdown should complete before rebinding addr")
        .expect("first relay task should exit cleanly");

    tokio::time::sleep(Duration::from_millis(10)).await;
    pool.keepalive_ping(|| {});
    pool.try_recv(10, |_| {});

    tokio::time::sleep(Duration::from_millis(30)).await;
    pool.keepalive_ping(|| {});
    pool.try_recv(10, |_| {});

    let (_relay_task, connection_count) = create_reconnect_counting_relay_at(addr).await;

    let reconnected = pump_pool_until(&mut pool, 100, Duration::from_millis(10), |pool| {
        pool.keepalive_ping(|| {});
        pool.try_recv(10, |_| {});
        websocket_status(pool, &url) == Some(RelayStatus::Connected)
    })
    .await;
    assert!(
        reconnected,
        "relay should reconnect once the later server is available again"
    );
    assert!(
        connection_count.load(Ordering::SeqCst) > 0,
        "later relay startup should eventually observe the reconnect attempt"
    );
}

// ==================== Oneshot Subscription Removal After EOSE ====================

/// Oneshot subscriptions should be removed from the pool after EOSE is received.
#[tokio::test]
async fn oneshot_subscription_removed_after_eose() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::new(urls);

    // Create a oneshot subscription via the handler, then export to get the ID
    let id = {
        let mut handler = pool.start_session(MockWakeup::default());
        handler.oneshot(trivial_filter(), url_pkgs);
        let session = handler.export();
        // Get the ID from the session's tasks
        let id = *session
            .tasks
            .keys()
            .next()
            .expect("oneshot should create a task");
        OutboxSessionHandler::import(&mut pool, session, MockWakeup::default());
        id
    };

    // Verify subscription exists
    let filters_before = pool.filters(&id);
    assert!(
        filters_before.is_some(),
        "oneshot subscription should exist before EOSE"
    );

    // Receive-path EOSE processing should remove the oneshot immediately.
    let removed = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        pool.filters(&id).is_none() && pool.status(&id).is_empty()
    })
    .await;
    assert!(removed, "oneshot subscription should be removed after EOSE");
}

/// Oneshot subscriptions across multiple relays should fully clean up after all EOSEs.
#[tokio::test]
async fn oneshot_multi_relay_fully_removed_after_eose() {
    let (_relay1, url1) = create_test_relay().await;
    let (_relay2, url2) = create_test_relay().await;

    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(url1.clone());
    urls.insert(url2.clone());
    let url_pkgs = RelayUrlPkgs::new(urls);

    let id = {
        let mut handler = pool.start_session(MockWakeup::default());
        handler.oneshot(trivial_filter(), url_pkgs);
        let session = handler.export();
        let id = *session
            .tasks
            .keys()
            .next()
            .expect("oneshot should create a task");
        OutboxSessionHandler::import(&mut pool, session, MockWakeup::default());
        id
    };

    let removed = pump_pool_until(&mut pool, 100, Duration::from_millis(10), |pool| {
        pool.filters(&id).is_none() && pool.status(&id).is_empty()
    })
    .await;
    assert!(
        removed,
        "oneshot should be fully removed on all relays after all EOSE processing"
    );
}

/// A receive pass that stops at the note budget must not strand oneshot cleanup.
/// The follow-up `try_recv` should consume the queued EOSE and finish removal.
#[tokio::test]
async fn oneshot_cleanup_completes_after_try_recv_stops_at_note_budget() {
    let (_relay, url) = create_test_relay_with_seeded_note().await;

    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::new(urls);

    let id = {
        let mut handler = pool.start_session(MockWakeup::default());
        handler.oneshot(trivial_filter(), url_pkgs);
        let session = handler.export();
        let id = *session
            .tasks
            .keys()
            .next()
            .expect("oneshot should create a task");
        OutboxSessionHandler::import(&mut pool, session, MockWakeup::default());
        id
    };

    assert!(
        pool.filters(&id).is_some(),
        "oneshot should exist before receive processing starts"
    );

    let mut saw_note = false;
    for _ in 0..100 {
        pool.try_recv(1, |_| {
            saw_note = true;
        });
        if saw_note {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        saw_note,
        "first bounded receive pass should consume the note"
    );
    assert!(
        pool.filters(&id).is_some(),
        "oneshot should still exist after the note-budget break"
    );
    assert!(
        !pool.has_eose(&id),
        "EOSE should still be unread after the first bounded receive pass"
    );

    let removed =
        pump_pool_until_with_note_budget(&mut pool, 1, 100, Duration::from_millis(10), |pool| {
            pool.filters(&id).is_none() && pool.status(&id).is_empty()
        })
        .await;
    assert!(
        removed,
        "a later bounded receive pass should flush EOSE effects and remove the oneshot"
    );
}

/// Repeated bounded receive passes should still deliver every queued note and
/// the final EOSE.
#[tokio::test]
async fn bounded_try_recv_eventually_delivers_all_notes() {
    let (_relay, url, expected_ids) = create_test_relay_with_seeded_notes(3).await;

    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::new(urls);

    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), url_pkgs)
    };

    let mut seen_ids = HashSet::new();
    let mut received_all = false;
    for _ in 0..100 {
        pool.try_recv(1, |event| {
            let parsed: serde_json::Value =
                serde_json::from_str(event.event_json).expect("parse delivered seeded note json");
            let id = parsed[2]["id"]
                .as_str()
                .expect("delivered seeded note should include an id");
            seen_ids.insert(id.to_owned());
        });

        if expected_ids.iter().all(|id| seen_ids.contains(id)) && pool.has_eose(&id) {
            received_all = true;
            break;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        received_all,
        "repeated bounded receive passes should eventually deliver all notes and EOSE"
    );
}

/// Repeated bounded receive passes should still drain note delivery and EOSE
/// when non-event relay frames are interleaved ahead of them.
#[tokio::test]
async fn bounded_try_recv_eventually_delivers_notes_after_notice_frame() {
    let signer = FullKeypair::generate();
    let first_note = NoteBuilder::new()
        .kind(1)
        .content("notice relay note 1")
        .sign(&signer.secret_key.secret_bytes())
        .build()
        .expect("build first notice relay note");
    let second_note = NoteBuilder::new()
        .kind(1)
        .content("notice relay note 2")
        .sign(&signer.secret_key.secret_bytes())
        .build()
        .expect("build second notice relay note");
    let expected_ids = [hex::encode(first_note.id()), hex::encode(second_note.id())];
    let (_relay_task, url) = create_notice_then_events_relay(vec![
        first_note.json().expect("first notice relay note json"),
        second_note.json().expect("second notice relay note json"),
    ])
    .await;

    let mut pool = OutboxPool::default();
    let mut urls = HashSet::new();
    urls.insert(url);
    let url_pkgs = RelayUrlPkgs::new(urls);

    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), url_pkgs)
    };

    let mut seen_ids = HashSet::new();
    let mut received_all = false;
    for _ in 0..100 {
        pool.try_recv(1, |event| {
            let parsed: serde_json::Value =
                serde_json::from_str(event.event_json).expect("parse delivered notice-relay frame");
            let id = parsed[2]["id"]
                .as_str()
                .expect("delivered notice-relay event should include an id");
            seen_ids.insert(id.to_owned());
        });

        if expected_ids.iter().all(|id| seen_ids.contains(id)) && pool.has_eose(&id) {
            received_all = true;
            break;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        received_all,
        "bounded receive should not lose later notes or EOSE when a NOTICE frame appears first"
    );
}

// ==================== Since Optimization After EOSE ====================

fn filter_has_since(filter: &Filter) -> bool {
    filter.since().is_some()
}

/// After EOSE is received on a dedicated subscription, filters should keep
/// their original shape; `since` optimization is reserved for compaction.
#[tokio::test]
async fn dedicated_eose_does_not_apply_since_to_filters() {
    let (_relay, url) = create_test_relay().await;
    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let url_pkgs = RelayUrlPkgs::with_preference(urls, RelayRoutingPreference::PreferDedicated);

    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(
            vec![Filter::new().kinds(vec![1]).limit(10).build()],
            url_pkgs,
        )
    };

    let initial_filters = pool.filters(&id).expect("subscription exists");
    assert!(
        !filter_has_since(&initial_filters[0]),
        "filters should not have since before EOSE"
    );

    // Wait for EOSE
    let got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&id)).await;
    assert!(got_eose, "should receive EOSE");

    // Create an empty session to trigger EOSE queue processing
    // (ingest_session is called when the handler is dropped)
    {
        let _ = pool.start_session(MockWakeup::default());
    }

    // After EOSE processing, dedicated filters should remain unchanged.
    let optimized_filters = pool.filters(&id).expect("subscription still exists");

    assert!(
        !filter_has_since(&optimized_filters[0]),
        "dedicated filters should not gain since after EOSE"
    );
}

/// After EOSE is received on a compaction-only subscription, the stored filters
/// should remain pristine while the compaction projection gains a `since`
/// cursor for the next shared catch-up request.
#[tokio::test]
async fn compaction_eose_applies_since_to_filters() {
    let (_relay, url) = create_test_relay_with_seeded_note().await;
    let mut pool = OutboxPool::default();
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_320);

    let mut dedicated_urls = HashSet::new();
    dedicated_urls.insert(url.clone());
    let dedicated_pkgs =
        RelayUrlPkgs::with_preference(dedicated_urls, RelayRoutingPreference::PreferDedicated);

    let dedicated_id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(
            vec![Filter::new().kinds(vec![1]).limit(10).build()],
            dedicated_pkgs,
        )
    };

    let applied = pool.apply_nip11_limits(
        &url,
        Nip11LimitationsRaw {
            max_subscriptions: Some(1),
            ..Default::default()
        },
        now,
    );
    assert!(matches!(
        applied,
        Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
    ));

    let mut compaction_urls = HashSet::new();
    compaction_urls.insert(url.clone());
    let compaction_pkgs =
        RelayUrlPkgs::with_preference(compaction_urls, RelayRoutingPreference::NoPreference);

    let compaction_id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(
            vec![Filter::new().kinds(vec![1]).limit(10).build()],
            compaction_pkgs,
        )
    };

    let initial_filters = pool.filters(&compaction_id).expect("subscription exists");
    assert!(
        !filter_has_since(&initial_filters[0]),
        "filters should not have since before EOSE"
    );

    let dedicated_got_eose =
        default_pool_pump(&mut pool, |pool| pool.has_eose(&dedicated_id)).await;
    assert!(
        dedicated_got_eose,
        "dedicated subscription should stay active while the fallback request is queued"
    );
    assert!(
        !pool.has_eose(&compaction_id),
        "fallback request should not become active until the dedicated slot is released"
    );

    {
        let mut session = pool.start_session(MockWakeup::default());
        session.unsubscribe(dedicated_id);
    }

    let got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&compaction_id)).await;
    assert!(
        got_eose,
        "queued fallback request should receive EOSE once it becomes the active compaction route"
    );

    {
        let _ = pool.start_session(MockWakeup::default());
    }

    let stored_filters = pool
        .filters(&compaction_id)
        .expect("compaction subscription still exists");
    assert!(
        !filter_has_since(&stored_filters[0]),
        "stored filters should remain pristine after compaction EOSE"
    );

    let optimized_filters = pool
        .compaction_filters(&compaction_id)
        .expect("compaction-projected filters");
    assert!(
        filter_has_since(&optimized_filters[0]),
        "compaction projection should gain since after EOSE"
    );
}

/// Since optimization should wait until every relay for the subscription reaches EOSE.
#[tokio::test]
async fn since_optimization_waits_for_all_relays_eose() {
    let (_relay, live_url) = create_test_relay().await;
    let dead_url = NormRelayUrl::new("wss://127.0.0.1:1").expect("valid dead relay url");

    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(live_url);
    urls.insert(dead_url);
    let url_pkgs = RelayUrlPkgs::with_preference(urls, RelayRoutingPreference::PreferDedicated);

    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(
            vec![Filter::new().kinds(vec![1]).limit(10).build()],
            url_pkgs,
        )
    };

    let initial_filters = pool.filters(&id).expect("subscription exists");
    assert!(
        !filter_has_since(&initial_filters[0]),
        "filters should not have since before any EOSE"
    );

    let got_any_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&id)).await;
    assert!(got_any_eose, "live relay should produce EOSE");
    assert!(
        !pool.all_have_eose(&id),
        "all relays should not have EOSE when one relay is unreachable"
    );

    // Trigger EOSE queue processing.
    {
        let _ = pool.start_session(MockWakeup::default());
    }

    let filters = pool.filters(&id).expect("subscription still exists");
    assert!(
        !filter_has_since(&filters[0]),
        "since should not be optimized until every relay reaches EOSE"
    );
}

/// When max subscriptions is saturated, an incoming prefer-dedicated request
/// should not displace an existing preferred dedicated route and should become
/// active once capacity is released.
#[tokio::test]
async fn preferred_request_stays_active_without_displacing_existing_preferred() {
    let (_relay, url) = create_test_relay().await;
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_275);

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let preferred_pkg =
        RelayUrlPkgs::with_preference(urls, RelayRoutingPreference::PreferDedicated);

    let first_id = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), preferred_pkg.clone())
    };

    let applied = pool.apply_nip11_limits(
        &url,
        Nip11LimitationsRaw {
            max_subscriptions: Some(1),
            ..Default::default()
        },
        now,
    );
    assert!(matches!(
        applied,
        Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
    ));

    let second_id = {
        let mut session = pool.start_session(wakeup);
        session.subscribe(trivial_filter(), preferred_pkg)
    };

    let first_got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&first_id)).await;
    assert!(
        first_got_eose,
        "existing preferred subscription should remain active on dedicated routing"
    );
    assert!(
        !pool.has_eose(&second_id),
        "incoming preferred request should not displace the existing preferred dedicated route"
    );

    {
        let mut session = pool.start_session(MockWakeup::default());
        session.unsubscribe(first_id);
    }

    let second_got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&second_id)).await;
    assert!(
        second_got_eose,
        "incoming preferred request should become active once the dedicated slot is released"
    );
}

/// When dedicated capacity is saturated, a `RequireDedicated` request must not
/// fall back to compaction; it should stay queued until a dedicated slot opens.
#[tokio::test]
async fn require_dedicated_request_queues_without_compaction_fallback_when_saturated() {
    let (_relay, url) = create_test_relay().await;
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_285);

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let required_pkg =
        RelayUrlPkgs::with_preference(urls, RelayRoutingPreference::RequireDedicated);

    let first_id = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), required_pkg.clone())
    };

    let applied = pool.apply_nip11_limits(
        &url,
        Nip11LimitationsRaw {
            max_subscriptions: Some(1),
            ..Default::default()
        },
        now,
    );
    assert!(matches!(
        applied,
        Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
    ));

    let second_id = {
        let mut session = pool.start_session(wakeup);
        session.subscribe(trivial_filter(), required_pkg)
    };

    let first_got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&first_id)).await;
    assert!(
        first_got_eose,
        "existing required-dedicated subscription should remain active under saturation"
    );
    assert!(
        pool.status(&second_id).is_empty(),
        "queued required-dedicated request should have no active relay leg while saturated"
    );
    assert!(
        !pool.has_eose(&second_id),
        "queued required-dedicated request should not produce EOSE before a dedicated slot is available"
    );

    {
        let mut session = pool.start_session(MockWakeup::default());
        session.unsubscribe(first_id);
    }

    let second_got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&second_id)).await;
    assert!(
        second_got_eose,
        "queued required-dedicated request should activate once dedicated capacity is released"
    );
}

/// Multiple `RequireDedicated` requests competing for one dedicated slot should
/// stay queued and activate one at a time as capacity is released.
#[tokio::test]
async fn require_dedicated_requests_compete_for_last_slot() {
    let (_relay, url) = create_test_relay().await;
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_286);

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let required_pkg =
        RelayUrlPkgs::with_preference(urls, RelayRoutingPreference::RequireDedicated);

    let first_id = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), required_pkg.clone())
    };

    let applied = pool.apply_nip11_limits(
        &url,
        Nip11LimitationsRaw {
            max_subscriptions: Some(1),
            ..Default::default()
        },
        now,
    );
    assert!(matches!(
        applied,
        Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
    ));

    let first_got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&first_id)).await;
    assert!(
        first_got_eose,
        "first required-dedicated request should claim the only dedicated slot"
    );

    let second_id = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), required_pkg.clone())
    };
    let third_id = {
        let mut session = pool.start_session(wakeup);
        session.subscribe(trivial_filter(), required_pkg)
    };

    assert!(
        pool.status(&second_id).is_empty(),
        "second required-dedicated request should be queued under saturation"
    );
    assert!(
        pool.status(&third_id).is_empty(),
        "third required-dedicated request should be queued under saturation"
    );

    {
        let mut session = pool.start_session(MockWakeup::default());
        session.unsubscribe(first_id);
    }

    let second_got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&second_id)).await;
    assert!(
        second_got_eose,
        "second required-dedicated request should activate after first releases capacity"
    );
    assert!(
        !pool.has_eose(&third_id),
        "third required-dedicated request should still wait while second owns the only dedicated slot"
    );

    {
        let mut session = pool.start_session(MockWakeup::default());
        session.unsubscribe(second_id);
    }

    let third_got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&third_id)).await;
    assert!(
        third_got_eose,
        "third required-dedicated request should activate after second releases capacity"
    );
}

/// Under saturation, an existing `RequireDedicated` subscription must not be
/// displaced by an incoming `PreferDedicated` request.
#[tokio::test]
async fn prefer_dedicated_does_not_displace_existing_require_dedicated() {
    let (_relay, url) = create_test_relay().await;
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_287);

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let required_pkg =
        RelayUrlPkgs::with_preference(urls.clone(), RelayRoutingPreference::RequireDedicated);
    let preferred_pkg =
        RelayUrlPkgs::with_preference(urls, RelayRoutingPreference::PreferDedicated);

    let required_id = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), required_pkg)
    };

    let applied = pool.apply_nip11_limits(
        &url,
        Nip11LimitationsRaw {
            max_subscriptions: Some(1),
            ..Default::default()
        },
        now,
    );
    assert!(matches!(
        applied,
        Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
    ));

    let required_got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&required_id)).await;
    assert!(
        required_got_eose,
        "required-dedicated request should own the only dedicated slot"
    );

    let preferred_id = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), preferred_pkg)
    };

    assert!(
        pool.has_eose(&required_id),
        "required-dedicated request should remain active after incoming preferred request"
    );
    assert!(
        pool.status(&preferred_id).is_empty(),
        "preferred request should not displace required-dedicated under saturation"
    );

    {
        let mut session = pool.start_session(wakeup);
        session.unsubscribe(required_id);
    }

    let preferred_got_eose =
        default_pool_pump(&mut pool, |pool| pool.has_eose(&preferred_id)).await;
    assert!(
        preferred_got_eose,
        "preferred request should activate once required slot is released"
    );
}

/// Production-like mixed policy set on one relay:
/// two `RequireDedicated` and two `PreferDedicated` subscriptions.
/// Required routes should hold dedicated capacity first; preferred routes should
/// wait and then activate as required routes release capacity.
#[tokio::test]
async fn mixed_require_and_prefer_dedicated_on_one_relay_behaves_as_expected() {
    let (_relay, url) = create_test_relay().await;
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_288);

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let required_pkg =
        RelayUrlPkgs::with_preference(urls.clone(), RelayRoutingPreference::RequireDedicated);
    let preferred_pkg =
        RelayUrlPkgs::with_preference(urls, RelayRoutingPreference::PreferDedicated);

    let required_a = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), required_pkg.clone())
    };
    let required_b = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), required_pkg)
    };

    let applied = pool.apply_nip11_limits(
        &url,
        Nip11LimitationsRaw {
            max_subscriptions: Some(2),
            ..Default::default()
        },
        now,
    );
    assert!(matches!(
        applied,
        Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
    ));

    let required_ready = default_pool_pump(&mut pool, |pool| {
        pool.has_eose(&required_a) && pool.has_eose(&required_b)
    })
    .await;
    assert!(
        required_ready,
        "both required-dedicated requests should fill dedicated capacity"
    );

    let preferred_a = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), preferred_pkg.clone())
    };
    let preferred_b = {
        let mut session = pool.start_session(wakeup.clone());
        session.subscribe(trivial_filter(), preferred_pkg)
    };

    assert!(
        pool.status(&preferred_a).is_empty(),
        "preferred A should wait while required routes consume dedicated capacity"
    );
    assert!(
        pool.status(&preferred_b).is_empty(),
        "preferred B should wait while required routes consume dedicated capacity"
    );

    {
        let mut session = pool.start_session(wakeup.clone());
        session.unsubscribe(required_a);
    }

    let first_preferred_ready = default_pool_pump(&mut pool, |pool| {
        pool.has_eose(&preferred_a) || pool.has_eose(&preferred_b)
    })
    .await;
    assert!(
        first_preferred_ready,
        "one preferred request should activate after first required route releases capacity"
    );

    let remaining_preferred = if pool.has_eose(&preferred_a) {
        preferred_b
    } else {
        preferred_a
    };

    {
        let mut session = pool.start_session(wakeup);
        session.unsubscribe(required_b);
    }

    let second_preferred_ready =
        default_pool_pump(&mut pool, |pool| pool.has_eose(&remaining_preferred)).await;
    assert!(
        second_preferred_ready,
        "remaining preferred request should activate after second required route releases capacity"
    );
}
