//! Integration tests for the Outbox relay system
//!
//! These tests use `nostr-relay-builder::LocalRelay` to run a real relay on localhost
//! and test the full subscription lifecycle, EOSE propagation, and multi-relay coordination.

use enostr::{
    FullKeypair, NormRelayUrl, OutboxPool, OutboxSessionHandler, OutboxSubId, RelayId,
    RelayReqStatus, RelayStatus, RelayUrlPkgs, Wakeup,
};
use hashbrown::HashSet;
use nostr_relay_builder::{LocalRelay, RelayBuilder};
use nostrdb::{Config, Filter, NoteBuilder};
use std::sync::Once;
use std::time::Duration;

/// Returns a [`Config`] with a small mapsize suitable for tests on Windows.
fn test_config() -> Config {
    if cfg!(target_os = "windows") {
        Config::new().set_mapsize(32 * 1024 * 1024)
    } else {
        Config::new()
    }
}

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
    let mut url_pkgs = RelayUrlPkgs::new(urls);
    url_pkgs.routing_preference = enostr::RelayRoutingPreference::RequireDedicated;

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

// ==================== Transparent vs Compaction Mode ====================

#[tokio::test]
async fn transparent_mode_subscription() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let mut url_pkgs = RelayUrlPkgs::new(urls);
    url_pkgs.routing_preference = enostr::RelayRoutingPreference::RequireDedicated; // Enable transparent mode

    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), url_pkgs)
    };

    let got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&id)).await;
    assert!(got_eose, "transparent mode should receive EOSE");
}

#[tokio::test]
async fn compaction_mode_subscription() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();

    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let mut url_pkgs = RelayUrlPkgs::new(urls);
    url_pkgs.routing_preference = enostr::RelayRoutingPreference::NoPreference; // Compaction mode

    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), url_pkgs)
    };

    let got_eose = default_pool_pump(&mut pool, |pool| pool.has_eose(&id)).await;
    assert!(got_eose, "compaction mode should receive EOSE");
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

    // Wait for EOSE
    let got_eose = pump_pool_until(&mut pool, 50, Duration::from_millis(5), |pool| {
        pool.has_eose(&id)
    })
    .await;
    assert!(got_eose, "should receive EOSE for oneshot subscription");

    // Trigger EOSE processing by starting an empty session
    {
        let _ = pool.start_session(MockWakeup::default());
    }

    // Verify subscription was removed
    let filters_after = pool.filters(&id);
    assert!(
        filters_after.is_none(),
        "oneshot subscription should be removed after EOSE"
    );
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

    let got_all_eose = pump_pool_until(&mut pool, 100, Duration::from_millis(10), |pool| {
        pool.all_have_eose(&id)
    })
    .await;
    assert!(got_all_eose, "oneshot should receive EOSE from all relays");

    {
        let _ = pool.start_session(MockWakeup::default());
    }

    assert!(
        pool.filters(&id).is_none(),
        "oneshot metadata should be removed after EOSE processing"
    );
    assert!(
        pool.status(&id).is_empty(),
        "oneshot should be fully unsubscribed on all relays after EOSE processing"
    );
}

// ==================== Since Optimization After EOSE ====================

fn filter_has_since(filter: &Filter) -> bool {
    filter.since().is_some()
}

/// After EOSE is received, filters should have `since` applied for future re-subscriptions.
#[tokio::test]
async fn eose_applies_since_to_filters() {
    let (_relay, url) = create_test_relay().await;

    let mut pool = OutboxPool::default();

    // Subscribe with transparent mode (faster EOSE)
    let mut urls = HashSet::new();
    urls.insert(url.clone());
    let mut url_pkgs = RelayUrlPkgs::new(urls);
    url_pkgs.routing_preference = enostr::RelayRoutingPreference::RequireDedicated;

    let id = {
        let mut session = pool.start_session(MockWakeup::default());
        session.subscribe(
            vec![Filter::new().kinds(vec![1]).limit(10).build()],
            url_pkgs,
        )
    };

    // Verify filters don't have since initially
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

    // After EOSE processing, filters should have since applied
    let optimized_filters = pool.filters(&id).expect("subscription still exists");

    assert!(
        filter_has_since(&optimized_filters[0]),
        "filters should have since after EOSE"
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
    let mut url_pkgs = RelayUrlPkgs::new(urls);
    url_pkgs.routing_preference = enostr::RelayRoutingPreference::RequireDedicated;

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

// ==================== Publish-Receive Tests ====================

/// Publish a signed kind-1 note with the given content to the relay via the pool.
fn publish_note(pool: &mut OutboxPool, keypair: &FullKeypair, content: &str, url: &NormRelayUrl) {
    let note = NoteBuilder::new()
        .kind(1)
        .content(content)
        .sign(&keypair.secret_key.secret_bytes())
        .build()
        .expect("build signed note");
    pool.broadcast_note(
        &note,
        vec![RelayId::Websocket(url.clone())],
        &MockWakeup::default(),
    );
}

/// Build a signed kind-1 note JSON for raw websocket tests.
fn build_event_json(keypair: &FullKeypair, content: &str) -> String {
    let note = NoteBuilder::new()
        .kind(1)
        .content(content)
        .sign(&keypair.secret_key.secret_bytes())
        .build()
        .expect("build signed note");
    let note_json = note.json().expect("note json");
    format!(r#"["EVENT",{}]"#, note_json)
}

/// Pumps the pool, collecting received event JSON strings until `expected_count`
/// events are gathered or the attempt limit is reached.
async fn pump_pool_collecting(
    pool: &mut OutboxPool,
    received: &mut Vec<String>,
    max_attempts: usize,
    sleep_duration: Duration,
    expected_count: usize,
) -> bool {
    for _ in 0..max_attempts {
        pool.try_recv(100, |raw| {
            received.push(raw.event_json.to_string());
        });
        if received.len() >= expected_count {
            return true;
        }
        tokio::time::sleep(sleep_duration).await;
    }
    // Final drain
    pool.try_recv(100, |raw| {
        received.push(raw.event_json.to_string());
    });
    received.len() >= expected_count
}

/// Pumps the pool until the relay reports Connected status.
async fn wait_for_pool_connected(pool: &mut OutboxPool, url: &NormRelayUrl) -> bool {
    let target = url.clone();
    pump_pool_until(pool, 100, Duration::from_millis(15), move |pool| {
        pool.websocket_statuses()
            .iter()
            .any(|(u, s)| **u == target && *s == RelayStatus::Connected)
    })
    .await
}

fn relay_url_set(url: &NormRelayUrl) -> HashSet<NormRelayUrl> {
    let mut urls = HashSet::new();
    urls.insert(url.clone());
    urls
}

/// Pool A subscribes, Pool B publishes, verify Pool A receives the event.
#[tokio::test]
async fn basic_publish_receive() {
    init_tracing();

    let (_relay, url) = create_test_relay().await;

    // Subscriber pool
    let mut sub_pool = OutboxPool::default();
    let sub_id = {
        let mut session = sub_pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), RelayUrlPkgs::new(relay_url_set(&url)))
    };
    let got_eose = default_pool_pump(&mut sub_pool, |p| p.has_eose(&sub_id)).await;
    assert!(got_eose, "subscriber should get EOSE");

    // Publisher pool
    let mut pub_pool = OutboxPool::default();
    let keypair = FullKeypair::generate();
    publish_note(&mut pub_pool, &keypair, "hello-from-publisher", &url);
    let connected = wait_for_pool_connected(&mut pub_pool, &url).await;
    assert!(connected, "publisher should connect to relay");

    // Collect received events on subscriber
    let mut received = Vec::new();
    let got_event = pump_pool_collecting(
        &mut sub_pool,
        &mut received,
        200,
        Duration::from_millis(15),
        1,
    )
    .await;

    assert!(got_event, "subscriber should receive the published event");
    assert_eq!(received.len(), 1);
    assert!(
        received[0].contains("hello-from-publisher"),
        "received event should contain the published content"
    );
}

/// Multiple subscriber pools all receive the same published event.
#[tokio::test]
async fn publish_receive_multiple_subscribers() {
    init_tracing();

    let (_relay, url) = create_test_relay().await;

    // Create 3 subscriber pools
    let mut sub_pools: Vec<OutboxPool> = Vec::new();
    let mut sub_ids: Vec<OutboxSubId> = Vec::new();
    for _ in 0..3 {
        let mut pool = OutboxPool::default();
        let id = {
            let mut session = pool.start_session(MockWakeup::default());
            session.subscribe(trivial_filter(), RelayUrlPkgs::new(relay_url_set(&url)))
        };
        let got_eose = default_pool_pump(&mut pool, |p| p.has_eose(&id)).await;
        assert!(got_eose, "subscriber should get EOSE");
        sub_pools.push(pool);
        sub_ids.push(id);
    }

    // Publisher
    let mut pub_pool = OutboxPool::default();
    let keypair = FullKeypair::generate();
    publish_note(&mut pub_pool, &keypair, "fan-out-test", &url);
    let connected = wait_for_pool_connected(&mut pub_pool, &url).await;
    assert!(connected, "publisher should connect");

    // Verify all 3 subscribers receive the event
    for (i, pool) in sub_pools.iter_mut().enumerate() {
        let mut received = Vec::new();
        let got =
            pump_pool_collecting(pool, &mut received, 200, Duration::from_millis(15), 1).await;
        assert!(
            got,
            "subscriber {i} should receive the event, got {} events",
            received.len()
        );
        assert!(received[0].contains("fan-out-test"));
    }
}

/// Burst-publish N events, verify subscriber receives all N.
async fn burst_publish_receive(n: usize) {
    init_tracing();

    let (_relay, url) = create_test_relay().await;

    // Subscriber
    let mut sub_pool = OutboxPool::default();
    let sub_id = {
        let mut session = sub_pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), RelayUrlPkgs::new(relay_url_set(&url)))
    };
    let got_eose = default_pool_pump(&mut sub_pool, |p| p.has_eose(&sub_id)).await;
    assert!(got_eose, "subscriber should get EOSE");

    // Publisher — connect first, then burst-send
    let mut pub_pool = OutboxPool::default();
    let keypair = FullKeypair::generate();

    // Establish connection first via dummy subscribe
    {
        let mut session = pub_pool.start_session(MockWakeup::default());
        session.subscribe(
            vec![Filter::new().kinds(vec![99999]).build()],
            RelayUrlPkgs::new(relay_url_set(&url)),
        );
    }
    let connected = wait_for_pool_connected(&mut pub_pool, &url).await;
    assert!(connected, "publisher should connect");

    // Burst-send all notes with no delay
    for i in 0..n {
        publish_note(&mut pub_pool, &keypair, &format!("burst-msg-{i}"), &url);
    }
    // Pump publisher to flush
    for _ in 0..10 {
        pub_pool.try_recv(100, |_| {});
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Collect on subscriber
    let mut received = Vec::new();
    let got_all = pump_pool_collecting(
        &mut sub_pool,
        &mut received,
        500,
        Duration::from_millis(20),
        n,
    )
    .await;

    assert!(
        got_all,
        "subscriber should receive all {n} events, got {}",
        received.len()
    );

    // Verify all unique messages present
    for i in 0..n {
        let expected = format!("burst-msg-{i}");
        assert!(
            received.iter().any(|r| r.contains(&expected)),
            "missing event with content '{expected}'"
        );
    }
}

#[tokio::test]
async fn burst_publish_receive_6() {
    burst_publish_receive(6).await;
}

#[tokio::test]
async fn burst_publish_receive_12() {
    burst_publish_receive(12).await;
}

#[tokio::test]
async fn burst_publish_receive_20() {
    burst_publish_receive(20).await;
}

/// Burst-publish with multiple subscribers — all must receive all events.
#[tokio::test]
async fn burst_publish_receive_multiple_subscribers() {
    init_tracing();

    let (_relay, url) = create_test_relay().await;
    let n = 12;

    // 3 subscribers
    let mut sub_pools: Vec<OutboxPool> = Vec::new();
    for _ in 0..3 {
        let mut pool = OutboxPool::default();
        let id = {
            let mut session = pool.start_session(MockWakeup::default());
            session.subscribe(trivial_filter(), RelayUrlPkgs::new(relay_url_set(&url)))
        };
        let got_eose = default_pool_pump(&mut pool, |p| p.has_eose(&id)).await;
        assert!(got_eose);
        sub_pools.push(pool);
    }

    // Publisher — connect then burst
    let mut pub_pool = OutboxPool::default();
    let keypair = FullKeypair::generate();
    {
        let mut session = pub_pool.start_session(MockWakeup::default());
        session.subscribe(
            vec![Filter::new().kinds(vec![99999]).build()],
            RelayUrlPkgs::new(relay_url_set(&url)),
        );
    }
    let connected = wait_for_pool_connected(&mut pub_pool, &url).await;
    assert!(connected);

    for i in 0..n {
        publish_note(
            &mut pub_pool,
            &keypair,
            &format!("multi-sub-burst-{i}"),
            &url,
        );
    }
    for _ in 0..10 {
        pub_pool.try_recv(100, |_| {});
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Verify each subscriber got all 12
    for (i, pool) in sub_pools.iter_mut().enumerate() {
        let mut received = Vec::new();
        let got_all =
            pump_pool_collecting(pool, &mut received, 500, Duration::from_millis(20), n).await;
        assert!(
            got_all,
            "subscriber {i} should receive all {n} events, got {}",
            received.len()
        );
    }
}

/// Bypass OutboxPool — use raw ewebsock to verify the websocket layer directly.
#[tokio::test]
async fn raw_websocket_burst_publish_receive() {
    init_tracing();

    let (_relay, url) = create_test_relay().await;
    let n = 12;

    // Connect subscriber
    let url_str = url.to_string();
    let (mut sub_sender, sub_receiver) =
        enostr::ewebsock::connect(&url_str, enostr::ewebsock::Options::default())
            .expect("connect subscriber");

    // Connect publisher
    let (mut pub_sender, pub_receiver) =
        enostr::ewebsock::connect(&url_str, enostr::ewebsock::Options::default())
            .expect("connect publisher");

    // Wait for both to open
    for (name, receiver) in [("sub", &sub_receiver), ("pub", &pub_receiver)] {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(enostr::ewebsock::WsEvent::Opened) = receiver.try_recv() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{name} should connect within 5s"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    // Subscribe on subscriber connection
    let req = r#"["REQ","sub1",{"kinds":[1]}]"#;
    sub_sender.send(enostr::ewebsock::WsMessage::Text(req.to_string()));

    // Wait for EOSE
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(enostr::ewebsock::WsEvent::Message(enostr::ewebsock::WsMessage::Text(text))) =
            sub_receiver.try_recv()
        {
            if text.contains("EOSE") {
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "should receive EOSE within 5s"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Build and send N events via publisher
    let keypair = FullKeypair::generate();
    for i in 0..n {
        let event_msg = build_event_json(&keypair, &format!("raw-ws-msg-{i}"));
        pub_sender.send(enostr::ewebsock::WsMessage::Text(event_msg));
    }

    // Collect events on subscriber
    let mut received = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(enostr::ewebsock::WsEvent::Message(enostr::ewebsock::WsMessage::Text(text))) =
            sub_receiver.try_recv()
        {
            if text.starts_with("[\"EVENT\"") {
                received.push(text);
            }
        }
        if received.len() >= n {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "should receive all {n} events within 10s, got {}",
            received.len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(received.len(), n);
    for i in 0..n {
        let expected = format!("raw-ws-msg-{i}");
        assert!(
            received.iter().any(|r| r.contains(&expected)),
            "missing raw ws event with content '{expected}'"
        );
    }

    drop(sub_sender);
    drop(pub_sender);
}

// ==================== Silent Message Loss on Dead Connection ====================

/// Proves that messages published after a relay connection dies but before
/// try_recv detects the disconnect are silently lost.
///
/// This is a real production issue: WsSender::send() calls tx.send(msg).ok(),
/// discarding the SendError when the background thread has exited.
#[tokio::test]
async fn publish_to_dead_connection_loses_message() {
    init_tracing();

    let (relay, url) = create_test_relay().await;

    // Publisher pool — connect and verify
    let mut pub_pool = OutboxPool::default();
    let keypair = FullKeypair::generate();
    {
        let mut session = pub_pool.start_session(MockWakeup::default());
        session.subscribe(
            vec![Filter::new().kinds(vec![99999]).build()],
            RelayUrlPkgs::new(relay_url_set(&url)),
        );
    }
    let connected = wait_for_pool_connected(&mut pub_pool, &url).await;
    assert!(connected, "publisher should connect");

    // Publish msg-1 while alive — verify it reaches the relay
    publish_note(&mut pub_pool, &keypair, "msg-alive", &url);
    for _ in 0..5 {
        pub_pool.try_recv(100, |_| {});
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Kill the relay
    drop(relay);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Status is STALE — still says Connected because try_recv hasn't run
    let target = url.clone();
    let stale_connected = pub_pool
        .websocket_statuses()
        .iter()
        .any(|(u, s)| **u == target && *s == RelayStatus::Connected);
    assert!(
        stale_connected,
        "status should still say Connected (stale) before pumping"
    );

    // Publish msg-2 while status is stale Connected but connection is dead.
    // broadcast() checks is_connected() -> true, calls conn.send() which
    // writes to a dead mpsc channel. WsSender::send() discards the error.
    publish_note(&mut pub_pool, &keypair, "msg-lost", &url);

    // Now pump — this processes the Closed/Error event, updates to Disconnected
    for _ in 0..10 {
        pub_pool.try_recv(100, |_| {});
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let is_disconnected = pub_pool
        .websocket_statuses()
        .iter()
        .any(|(u, s)| **u == target && *s == RelayStatus::Disconnected);
    assert!(
        is_disconnected,
        "publisher should detect the dead connection after pumping"
    );

    // msg-lost was silently dropped — it went to conn.send() which wrote to
    // a dead mpsc channel. WsSender::send() calls tx.send(msg).ok() which
    // discards the SendError. The message never reached BroadcastCache
    // (because is_connected() was true), and never reached the wire
    // (because the background thread was dead).
}

// ==================== Publish-Receive + NDB Ingestion ====================

/// Verify events received via OutboxPool can be ingested into NDB and queried.
#[tokio::test]
async fn publish_receive_ndb_ingest() {
    init_tracing();

    let (_relay, url) = create_test_relay().await;

    // Set up NDB
    let tmpdir = tempfile::TempDir::new().expect("tmpdir");
    let db_path = tmpdir.path().join("db");
    std::fs::create_dir_all(&db_path).expect("create db dir");
    let ndb = nostrdb::Ndb::new(db_path.to_str().unwrap(), &test_config()).expect("ndb");

    // Subscriber pool
    let mut sub_pool = OutboxPool::default();
    let sub_id = {
        let mut session = sub_pool.start_session(MockWakeup::default());
        session.subscribe(trivial_filter(), RelayUrlPkgs::new(relay_url_set(&url)))
    };
    let got_eose = default_pool_pump(&mut sub_pool, |p| p.has_eose(&sub_id)).await;
    assert!(got_eose, "subscriber should get EOSE");

    // Publisher: connect then burst-send 6 notes
    let mut pub_pool = OutboxPool::default();
    let keypair = FullKeypair::generate();
    {
        let mut session = pub_pool.start_session(MockWakeup::default());
        session.subscribe(
            vec![Filter::new().kinds(vec![99999]).build()],
            RelayUrlPkgs::new(relay_url_set(&url)),
        );
    }
    let connected = wait_for_pool_connected(&mut pub_pool, &url).await;
    assert!(connected);

    let n = 6;
    for i in 0..n {
        publish_note(&mut pub_pool, &keypair, &format!("ndb-ingest-{i}"), &url);
    }
    for _ in 0..10 {
        pub_pool.try_recv(100, |_| {});
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Receive events and ingest into NDB (same path as the real app)
    let mut ingested = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        sub_pool.try_recv(100, |raw| {
            if let Err(e) = ndb.process_event_with(
                raw.event_json,
                nostrdb::IngestMetadata::new().relay(raw.url),
            ) {
                tracing::error!("ndb ingest error: {e}");
            } else {
                ingested += 1;
            }
        });
        if ingested >= n {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "should ingest {n} events, got {ingested}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }

    // Query NDB for the notes — NDB ingestion is async, so poll
    let filter = nostrdb::Filter::new()
        .kinds([1])
        .authors([keypair.pubkey.bytes()])
        .build();
    let query_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let txn = nostrdb::Transaction::new(&ndb).expect("txn");
        let results = ndb.query(&txn, &[filter.clone()], 100).expect("query");
        if results.len() >= n {
            // Verify contents
            let contents: std::collections::BTreeSet<String> = results
                .iter()
                .filter_map(|r| {
                    let c = r.note.content();
                    if c.starts_with("ndb-ingest-") {
                        Some(c.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(
                contents.len(),
                n,
                "all {n} unique messages should be in NDB"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < query_deadline,
            "NDB should have {n} notes, found {}",
            results.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
