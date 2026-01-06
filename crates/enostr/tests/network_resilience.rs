//! Network Resilience Integration Tests
//!
//! This module provides integration tests for verifying relay pool behavior
//! under degraded network conditions. Tests are designed to run with the
//! `network-resilience-test.sh` script which applies system-level network
//! throttling via the `throttle` tool.
//!
//! # Test Scenarios
//!
//! - `baseline`: No throttling, establishes baseline behavior
//! - `3g`: Standard 3G network (1600/768 kbit/s, 150ms RTT)
//! - `3gslow`: Slow 3G network (400/400 kbit/s, 200ms RTT)
//! - `packetloss`: 3G with 10% packet loss
//! - `disconnect`: Intermittent connection drops
//!
//! # Environment Variables
//!
//! - `TEST_RELAY_URL`: WebSocket URL of the test relay (required for integration tests)
//! - `TEST_SCENARIO`: Current test scenario name (default: unknown)
//! - `RUST_LOG`: Log level (default: info)
//!
//! Integration tests are skipped when `TEST_RELAY_URL` is not set, allowing
//! `cargo test` to pass without a running relay.
//!
//! # Architecture
//!
//! Tests use a `TestContext` struct to manage state, avoiding global variables.
//! All functions follow the nevernesting pattern with early returns.
//! Performance-critical sections are marked with `#[profiling::function]`.

use enostr::{RelayPool, RelayStatus};
use nostrdb::Filter;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ============================================================================
// Configuration and State Management
// ============================================================================

/// Test scenario configuration with expected timing constraints.
///
/// Each scenario has different expected latencies and error tolerances
/// based on the simulated network conditions.
#[derive(Debug, Clone)]
pub struct ScenarioConfig {
    /// Name of the test scenario
    pub name: String,
    /// Maximum time allowed for initial connection
    pub connection_timeout: Duration,
    /// Maximum time allowed for subscription responses
    pub subscription_timeout: Duration,
    /// Maximum acceptable error rate (0.0 - 1.0)
    pub max_error_rate: f64,
    /// Minimum expected EOSE responses for multi-subscription tests
    pub min_eose_expected: usize,
}

impl ScenarioConfig {
    /// Creates a configuration for the given scenario name.
    ///
    /// # Arguments
    ///
    /// * `scenario` - The scenario name from TEST_SCENARIO env var
    ///
    /// # Returns
    ///
    /// A `ScenarioConfig` with appropriate timeouts and thresholds.
    pub fn from_scenario(scenario: &str) -> Self {
        match scenario {
            "baseline" => Self {
                name: scenario.to_string(),
                connection_timeout: Duration::from_secs(15),
                subscription_timeout: Duration::from_secs(20),
                max_error_rate: 0.1,
                min_eose_expected: 5,
            },
            "3g" => Self {
                name: scenario.to_string(),
                connection_timeout: Duration::from_secs(20),
                subscription_timeout: Duration::from_secs(30),
                max_error_rate: 0.15,
                min_eose_expected: 5,
            },
            "3gslow" => Self {
                name: scenario.to_string(),
                connection_timeout: Duration::from_secs(30),
                subscription_timeout: Duration::from_secs(45),
                max_error_rate: 0.2,
                min_eose_expected: 4,
            },
            "packetloss" => Self {
                name: scenario.to_string(),
                connection_timeout: Duration::from_secs(30),
                subscription_timeout: Duration::from_secs(45),
                max_error_rate: 0.3,
                min_eose_expected: 1,
            },
            "disconnect" => Self {
                name: scenario.to_string(),
                connection_timeout: Duration::from_secs(45),
                subscription_timeout: Duration::from_secs(60),
                max_error_rate: 0.5,
                min_eose_expected: 1,
            },
            _ => Self {
                name: scenario.to_string(),
                connection_timeout: Duration::from_secs(30),
                subscription_timeout: Duration::from_secs(30),
                max_error_rate: 0.2,
                min_eose_expected: 3,
            },
        }
    }
}

/// Test context containing all state needed for network resilience tests.
///
/// This struct encapsulates the relay pool and configuration, ensuring
/// no global state is used. All test functions receive this context
/// as a mutable reference.
pub struct TestContext {
    /// The relay pool under test
    pub pool: RelayPool,
    /// Scenario-specific configuration
    pub config: ScenarioConfig,
    /// URL of the test relay
    pub relay_url: String,
}

impl TestContext {
    /// Creates a new test context for the current environment.
    ///
    /// Reads configuration from environment variables and initializes
    /// the relay pool with the test relay URL.
    ///
    /// # Returns
    ///
    /// A `Result` containing the initialized `TestContext` or an error.
    ///
    /// # Errors
    ///
    /// Returns an error if the relay URL is invalid.
    pub fn new() -> enostr::Result<Self> {
        let relay_url =
            env::var("TEST_RELAY_URL").unwrap_or_else(|_| "ws://127.0.0.1:7777".to_string());
        let scenario = env::var("TEST_SCENARIO").unwrap_or_else(|_| "unknown".to_string());

        let config = ScenarioConfig::from_scenario(&scenario);
        let mut pool = RelayPool::new();
        let wakeup = create_wakeup_callback();

        pool.add_url(relay_url.clone(), wakeup)?;

        Ok(Self {
            pool,
            config,
            relay_url,
        })
    }

    /// Returns the scenario name for logging.
    pub fn scenario_name(&self) -> &str {
        &self.config.name
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Checks if integration tests should run based on environment.
///
/// Integration tests require a live relay and are skipped when TEST_RELAY_URL
/// is not set. This allows `cargo test` to pass without a running relay.
///
/// # Returns
///
/// `true` if TEST_RELAY_URL is set and integration tests should run.
fn should_run_integration_tests() -> bool {
    env::var("TEST_RELAY_URL").is_ok()
}

/// Creates a simple wakeup callback for the relay pool.
///
/// The callback uses an atomic boolean to track wakeup state,
/// which is useful for debugging but not blocking.
fn create_wakeup_callback() -> impl Fn() + Send + Sync + Clone + 'static {
    let woken = Arc::new(AtomicBool::new(false));
    move || {
        woken.store(true, Ordering::SeqCst);
    }
}

/// Waits for the relay pool to establish a connection.
///
/// Polls the relay pool for connection events until either a connection
/// is established or the timeout expires. Uses early returns for clarity.
///
/// # Arguments
///
/// * `ctx` - The test context containing the pool and configuration
///
/// # Returns
///
/// `true` if connected within the timeout, `false` otherwise.
#[profiling::function]
fn wait_for_connection(ctx: &mut TestContext) -> bool {
    let start = Instant::now();
    let timeout = ctx.config.connection_timeout;
    let wakeup = create_wakeup_callback();

    loop {
        // Check timeout first (early return)
        if start.elapsed() >= timeout {
            return false;
        }

        ctx.pool.keepalive_ping(wakeup.clone());

        // Process pending events
        while let Some(event) = ctx.pool.try_recv() {
            if let ewebsock::WsEvent::Opened = event.event {
                return true;
            }
        }

        // Check relay status
        for relay in &ctx.pool.relays {
            if matches!(relay.status(), RelayStatus::Connected) {
                return true;
            }
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Processes relay pool events for a specified duration.
///
/// This function polls the relay pool and tracks various event types,
/// returning statistics about what was observed.
///
/// # Arguments
///
/// * `ctx` - The test context
/// * `duration` - How long to run the event loop
///
/// # Returns
///
/// An `EventStats` struct with counts of different event types.
#[profiling::function]
fn process_events_for_duration(ctx: &mut TestContext, duration: Duration) -> EventStats {
    let start = Instant::now();
    let wakeup = create_wakeup_callback();
    let mut stats = EventStats::default();

    while start.elapsed() < duration {
        ctx.pool.keepalive_ping(wakeup.clone());

        while let Some(event) = ctx.pool.try_recv() {
            match &event.event {
                ewebsock::WsEvent::Opened => stats.connections += 1,
                ewebsock::WsEvent::Closed => stats.disconnections += 1,
                ewebsock::WsEvent::Error(_) => stats.errors += 1,
                ewebsock::WsEvent::Message(msg) => {
                    stats.messages += 1;
                    if let ewebsock::WsMessage::Text(text) = msg {
                        if text.contains("EOSE") {
                            stats.eose_count += 1;
                        }
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    stats
}

/// Statistics collected from processing relay events.
#[derive(Debug, Default)]
pub struct EventStats {
    /// Number of successful connections observed
    pub connections: usize,
    /// Number of disconnections observed
    pub disconnections: usize,
    /// Number of errors observed
    pub errors: usize,
    /// Total messages received
    pub messages: usize,
    /// Number of EOSE (End of Stored Events) messages
    pub eose_count: usize,
}

impl EventStats {
    /// Calculates the error rate based on total events.
    ///
    /// # Returns
    ///
    /// The error rate as a value between 0.0 and 1.0.
    pub fn error_rate(&self) -> f64 {
        let total = self.connections + self.disconnections + self.errors + self.messages;
        if total == 0 {
            return 0.0;
        }
        self.errors as f64 / total as f64
    }
}

// ============================================================================
// Test Functions
// ============================================================================

/// Tests basic relay connection establishment.
///
/// Verifies that the relay pool can connect to a relay under the current
/// network conditions within the scenario-specific timeout.
#[test]
fn test_relay_connection() {
    if !should_run_integration_tests() {
        println!("Skipping integration test (TEST_RELAY_URL not set)");
        return;
    }

    let mut ctx = match TestContext::new() {
        Ok(ctx) => ctx,
        Err(e) => {
            panic!("Failed to create test context: {}", e);
        }
    };

    println!(
        "[{}] Testing relay connection to {}",
        ctx.scenario_name(),
        ctx.relay_url
    );

    let connected = wait_for_connection(&mut ctx);

    assert!(
        connected,
        "[{}] Failed to connect within {:?}",
        ctx.scenario_name(),
        ctx.config.connection_timeout
    );

    println!("[{}] Connection test passed", ctx.scenario_name());
}

/// Tests relay reconnection after disconnection.
///
/// First establishes a connection, then monitors for disconnect/reconnect
/// cycles, which are expected in the disconnect scenario.
#[test]
fn test_relay_reconnection() {
    if !should_run_integration_tests() {
        println!("Skipping integration test (TEST_RELAY_URL not set)");
        return;
    }

    let mut ctx = match TestContext::new() {
        Ok(ctx) => ctx,
        Err(e) => {
            panic!("Failed to create test context: {}", e);
        }
    };

    println!("[{}] Testing relay reconnection", ctx.scenario_name());

    // Establish initial connection (early return on failure)
    if !wait_for_connection(&mut ctx) {
        panic!("[{}] Initial connection failed", ctx.scenario_name());
    }

    println!("[{}] Initial connection established", ctx.scenario_name());

    // For disconnect scenario, monitor reconnection behavior
    if ctx.config.name != "disconnect" {
        println!(
            "[{}] Reconnection test passed (non-disconnect scenario)",
            ctx.scenario_name()
        );
        return;
    }

    // Monitor disconnect/reconnect cycles
    let test_duration = Duration::from_secs(30);
    let stats = process_events_for_duration(&mut ctx, test_duration);

    println!(
        "[{}] Observed {} disconnects, {} reconnects, {} errors",
        ctx.scenario_name(),
        stats.disconnections,
        stats.connections,
        stats.errors
    );

    // Verify we can handle disconnects gracefully
    assert!(
        stats.connections > 0 || stats.disconnections == 0,
        "[{}] Expected reconnection after disconnects",
        ctx.scenario_name()
    );

    println!("[{}] Reconnection test passed", ctx.scenario_name());
}

/// Tests subscription functionality under degraded network.
///
/// Creates a subscription and waits for EOSE (End of Stored Events),
/// verifying that the relay can process queries under load.
#[test]
fn test_subscription_under_load() {
    if !should_run_integration_tests() {
        println!("Skipping integration test (TEST_RELAY_URL not set)");
        return;
    }

    let mut ctx = match TestContext::new() {
        Ok(ctx) => ctx,
        Err(e) => {
            panic!("Failed to create test context: {}", e);
        }
    };

    println!("[{}] Testing subscriptions", ctx.scenario_name());

    // Connect first (early return on failure)
    if !wait_for_connection(&mut ctx) {
        panic!("[{}] Connection failed", ctx.scenario_name());
    }

    // Create subscription for recent notes
    let filter = Filter::new().kinds(vec![1]).limit(10).build();
    let sub_id = "test-sub-1".to_string();

    ctx.pool.subscribe(sub_id.clone(), vec![filter]);
    println!("[{}] Subscription sent", ctx.scenario_name());

    // Wait for EOSE
    let timeout = ctx.config.subscription_timeout;
    let stats = process_events_for_duration(&mut ctx, timeout);

    // Clean up
    ctx.pool.unsubscribe(sub_id);

    // Verify EOSE received (skip strict check for disconnect scenario)
    if ctx.config.name == "disconnect" {
        println!(
            "[{}] Subscription test completed (disconnect scenario, {} messages)",
            ctx.scenario_name(),
            stats.messages
        );
        return;
    }

    assert!(
        stats.eose_count > 0,
        "[{}] Did not receive EOSE (got {} messages)",
        ctx.scenario_name(),
        stats.messages
    );

    println!("[{}] Subscription test passed", ctx.scenario_name());
}

/// Measures connection latency under different network conditions.
///
/// Records the time taken to establish a connection and compares
/// against scenario-specific expectations.
#[test]
#[profiling::function]
fn test_connection_latency() {
    if !should_run_integration_tests() {
        println!("Skipping integration test (TEST_RELAY_URL not set)");
        return;
    }

    let scenario = env::var("TEST_SCENARIO").unwrap_or_else(|_| "unknown".to_string());
    let relay_url =
        env::var("TEST_RELAY_URL").unwrap_or_else(|_| "ws://127.0.0.1:7777".to_string());

    println!("[{}] Measuring connection latency", scenario);

    let start = Instant::now();

    let mut ctx = match TestContext::new() {
        Ok(ctx) => ctx,
        Err(e) => {
            panic!("Failed to create test context: {}", e);
        }
    };

    let connected = wait_for_connection(&mut ctx);
    let latency = start.elapsed();

    // Early return for disconnect scenario (connection may fail initially)
    if !connected && ctx.config.name == "disconnect" {
        println!(
            "[{}] Connection not established (expected for disconnect scenario)",
            ctx.scenario_name()
        );
        return;
    }

    if !connected {
        panic!(
            "[{}] Failed to connect to {} within timeout",
            scenario, relay_url
        );
    }

    println!("[{}] Connection established in {:?}", scenario, latency);

    // Verify latency is within expectations
    assert!(
        latency < ctx.config.connection_timeout,
        "[{}] Connection latency {:?} exceeded expected {:?}",
        scenario,
        latency,
        ctx.config.connection_timeout
    );

    println!("[{}] Latency test passed", scenario);
}

/// Tests multiple concurrent subscriptions.
///
/// Creates several subscriptions simultaneously and verifies that
/// all receive responses under degraded network conditions.
#[test]
fn test_multiple_subscriptions() {
    if !should_run_integration_tests() {
        println!("Skipping integration test (TEST_RELAY_URL not set)");
        return;
    }

    let mut ctx = match TestContext::new() {
        Ok(ctx) => ctx,
        Err(e) => {
            panic!("Failed to create test context: {}", e);
        }
    };

    println!(
        "[{}] Testing multiple concurrent subscriptions",
        ctx.scenario_name()
    );

    // Connect first (early return on failure)
    if !wait_for_connection(&mut ctx) {
        panic!("[{}] Connection failed", ctx.scenario_name());
    }

    // Create multiple subscriptions
    let sub_count = 5usize;
    for i in 0..sub_count {
        let filter = Filter::new().kinds(vec![1]).limit(5).build();
        let sub_id = format!("multi-sub-{}", i);
        ctx.pool.subscribe(sub_id, vec![filter]);
    }

    println!("[{}] Sent {} subscriptions", ctx.scenario_name(), sub_count);

    // Wait for responses
    let timeout = ctx.config.subscription_timeout;
    let stats = process_events_for_duration(&mut ctx, timeout);

    // Clean up subscriptions
    for i in 0..sub_count {
        ctx.pool.unsubscribe(format!("multi-sub-{}", i));
    }

    println!(
        "[{}] Received {}/{} EOSE responses",
        ctx.scenario_name(),
        stats.eose_count,
        sub_count
    );

    // Verify minimum expected responses
    assert!(
        stats.eose_count >= ctx.config.min_eose_expected,
        "[{}] Expected at least {} EOSE, got {}",
        ctx.scenario_name(),
        ctx.config.min_eose_expected,
        stats.eose_count
    );

    println!(
        "[{}] Multiple subscriptions test passed",
        ctx.scenario_name()
    );
}

/// Tests sustained connection stability.
///
/// Maintains a connection for an extended period while monitoring
/// for errors and disconnections.
#[test]
fn test_sustained_connection() {
    if !should_run_integration_tests() {
        println!("Skipping integration test (TEST_RELAY_URL not set)");
        return;
    }

    let mut ctx = match TestContext::new() {
        Ok(ctx) => ctx,
        Err(e) => {
            panic!("Failed to create test context: {}", e);
        }
    };

    println!("[{}] Testing sustained connection", ctx.scenario_name());

    // Connect first (early return on failure)
    if !wait_for_connection(&mut ctx) {
        panic!("[{}] Initial connection failed", ctx.scenario_name());
    }

    // Determine test duration based on scenario
    let test_duration = match ctx.config.name.as_str() {
        "disconnect" => Duration::from_secs(45),
        _ => Duration::from_secs(20),
    };

    println!(
        "[{}] Running sustained test for {:?}",
        ctx.scenario_name(),
        test_duration
    );

    let stats = process_events_for_duration(&mut ctx, test_duration);
    let error_rate = stats.error_rate();

    println!(
        "[{}] Sustained test: {} events, {:.1}% error rate",
        ctx.scenario_name(),
        stats.messages + stats.connections + stats.disconnections + stats.errors,
        error_rate * 100.0
    );

    // Verify error rate is acceptable
    assert!(
        error_rate <= ctx.config.max_error_rate,
        "[{}] Error rate {:.1}% exceeded max {:.1}%",
        ctx.scenario_name(),
        error_rate * 100.0,
        ctx.config.max_error_rate * 100.0
    );

    println!("[{}] Sustained connection test passed", ctx.scenario_name());
}

// ============================================================================
// Module Tests
// ============================================================================

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// Tests ScenarioConfig creation for known scenarios.
    #[test]
    fn test_scenario_config_baseline() {
        let config = ScenarioConfig::from_scenario("baseline");
        assert_eq!(config.name, "baseline");
        assert_eq!(config.connection_timeout, Duration::from_secs(15));
    }

    /// Tests ScenarioConfig creation for unknown scenarios.
    #[test]
    fn test_scenario_config_unknown() {
        let config = ScenarioConfig::from_scenario("unknown_scenario");
        assert_eq!(config.name, "unknown_scenario");
        // Should use default values
        assert_eq!(config.connection_timeout, Duration::from_secs(30));
    }

    /// Tests EventStats error rate calculation.
    #[test]
    fn test_event_stats_error_rate() {
        let stats = EventStats {
            connections: 10,
            disconnections: 0,
            errors: 2,
            messages: 88,
            eose_count: 5,
        };

        let rate = stats.error_rate();
        assert!((rate - 0.02).abs() < 0.001, "Expected ~2% error rate");
    }

    /// Tests EventStats with zero events.
    #[test]
    fn test_event_stats_empty() {
        let stats = EventStats::default();
        assert_eq!(stats.error_rate(), 0.0);
    }
}
