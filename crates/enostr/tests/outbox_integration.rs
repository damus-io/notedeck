//! Integration tests for the Outbox relay system
//!
//! These tests use `nostr-relay-builder::LocalRelay` to run a real relay on localhost
//! and test the full subscription lifecycle, EOSE propagation, and multi-relay coordination.

use enostr::{
    NormRelayUrl, OutboxPool, OutboxSessionHandler, OutboxSubId, RelayReqStatus, RelayStatus,
    RelayUrlPkgs, Wakeup,
};
use hashbrown::HashSet;
use nostr_relay_builder::{LocalRelay, RelayBuilder};
use nostrdb::Filter;
use std::sync::Once;
use std::time::Duration;

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
    url_pkgs.use_transparent = true;

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
    url_pkgs.use_transparent = true; // Enable transparent mode

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
    url_pkgs.use_transparent = false; // Compaction mode (default)

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

    let got_eose = pump_pool_until(&mut pool, 10, Duration::from_millis(10), |pool| {
        pool.has_eose(&id)
    })
    .await;
    assert!(
        !got_eose,
        "unreachable relay should never yield an EOSE signal"
    );

    // Should survive keepalive pings even when no websocket is available.
    pool.keepalive_ping(|| {});

    let statuses = pool.websocket_statuses();
    let status = statuses
        .into_iter()
        .find(|(relay_url, _)| *relay_url == &unreachable)
        .map(|(_, status)| status)
        .expect("missing unreachable relay status");
    assert_eq!(status, RelayStatus::Disconnected);
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
    url_pkgs.use_transparent = true;

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
