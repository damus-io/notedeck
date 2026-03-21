//! Full-app harness utilities for Messages end-to-end tests.
//!
//! Delegates generic device management, stepping, and UI helpers to
//! `notedeck_testing` and layers messages-specific functionality on top.

pub mod fixtures;
pub mod ui;

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use enostr::FullKeypair;
use nostrdb::Transaction;
use notedeck_messages::{
    nip17::{conversation_filter, parse_chat_message},
    MessagesApp,
};
use tempfile::TempDir;

// Re-export everything from the shared harness that tests use directly.
pub use notedeck_testing::cluster::AccountCluster;
pub use notedeck_testing::device::DeviceHarness;
pub use notedeck_testing::fixtures::{
    add_account_to_device, publish_note_via_device, select_account_on_device,
};
pub use notedeck_testing::stepping::{
    step_clusters, step_device_frames, step_device_group, step_devices, warm_up_clusters,
};

pub use notedeck_testing::init_tracing;

/// Maximum time any polling loop will wait before failing.
///
/// Generous enough for slow CI runners. Tests exit as soon as the
/// condition is met, so fast machines aren't penalized.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(120);

/// App factory that installs the Messages app on a device.
fn messages_app_factory() -> notedeck_testing::AppFactory {
    Box::new(|notedeck, _ctx| {
        notedeck.set_app(MessagesApp::new());
    })
}

/// Creates one full Messages device against a fresh temporary data directory.
pub fn build_messages_device(relay: &str, account: &FullKeypair) -> DeviceHarness {
    build_messages_device_with_relays(&[relay], account)
}

/// Creates one full Messages device against a fresh tempdir with multiple relays.
pub fn build_messages_device_with_relays(relays: &[&str], account: &FullKeypair) -> DeviceHarness {
    let tmpdir = TempDir::new().expect("tmpdir");
    build_messages_device_in_tmpdir_with_relays(relays, account, tmpdir)
}

/// Creates one full Messages device backed by an already-prepared tempdir.
pub fn build_messages_device_in_tmpdir(
    relay: &str,
    account: &FullKeypair,
    tmpdir: TempDir,
) -> DeviceHarness {
    build_messages_device_in_tmpdir_with_relays(&[relay], account, tmpdir)
}

/// Creates one full Messages device backed by an already-prepared tempdir with multiple relays.
pub fn build_messages_device_in_tmpdir_with_relays(
    relays: &[&str],
    account: &FullKeypair,
    tmpdir: TempDir,
) -> DeviceHarness {
    notedeck_testing::device::build_device_in_tmpdir_with_relays(
        relays,
        account,
        tmpdir,
        messages_app_factory(),
    )
}

/// Creates one full Messages device backed by an externally owned data dir.
pub fn build_messages_device_in_path_with_relays(
    relays: &[&str],
    account: &FullKeypair,
    data_dir: &Path,
) -> DeviceHarness {
    notedeck_testing::device::build_device_in_path_with_relays(
        relays,
        account,
        data_dir,
        messages_app_factory(),
    )
}

/// Shuts down a Messages device deterministically before dropping it.
pub fn shutdown_messages_device(mut device: DeviceHarness, _context: &str) {
    device.state_mut().notedeck.shutdown_app();
    drop(device);
}

/// Builds an account cluster with Messages app devices.
pub fn build_messages_cluster(
    name: &'static str,
    relay: &str,
    device_count: usize,
) -> AccountCluster {
    AccountCluster::new(name, relay, device_count, messages_app_factory)
}

// ---------------------------------------------------------------------------
// Messages-specific helpers
// ---------------------------------------------------------------------------

/// Returns the visible local chat messages for the selected account on one device.
pub fn local_chat_messages(device: &mut DeviceHarness) -> BTreeSet<String> {
    let ctx = device.ctx.clone();
    let app_ctx = device.state_mut().notedeck.app_context(&ctx);
    let me = *app_ctx.accounts.selected_account_pubkey();
    let filters = conversation_filter(&me);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    let results = app_ctx
        .ndb
        .query(&txn, &filters, 1024)
        .expect("query local chat messages");

    results
        .into_iter()
        .filter_map(|result| parse_chat_message(&result.note).map(|msg| msg.message.to_owned()))
        .collect()
}

/// Returns the number of local chat messages visible for the selected account.
pub fn local_chat_message_count(device: &mut DeviceHarness) -> usize {
    let ctx = device.ctx.clone();
    let app_ctx = device.state_mut().notedeck.app_context(&ctx);
    let me = *app_ctx.accounts.selected_account_pubkey();
    let filters = conversation_filter(&me);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    let results = app_ctx
        .ndb
        .query(&txn, &filters, 1024)
        .expect("query local chat messages");
    results.len()
}

/// Waits until one device can observe the expected local message set from durable state.
///
/// Use this only when all relevant relay writes are already durable and no sender
/// device needs to keep stepping to flush its outbox.
pub fn wait_for_device_messages(
    device: &mut DeviceHarness,
    expected: &BTreeSet<String>,
    timeout: Duration,
    context: &str,
) {
    wait_for_device_messages_impl(device, expected, timeout, context, &mut []);
}

/// Waits until a single device matches the expected local message set.
///
/// Any `senders` are stepped each iteration so their outbox continues
/// flushing while the primary device polls for incoming messages.
pub fn wait_for_device_messages_while_flushing(
    device: &mut DeviceHarness,
    expected: &BTreeSet<String>,
    timeout: Duration,
    context: &str,
    senders: &mut [&mut DeviceHarness],
) {
    wait_for_device_messages_impl(device, expected, timeout, context, senders);
}

/// Implements the shared polling loop behind the single-device wait helpers.
fn wait_for_device_messages_impl(
    device: &mut DeviceHarness,
    expected: &BTreeSet<String>,
    timeout: Duration,
    context: &str,
    senders: &mut [&mut DeviceHarness],
) {
    let deadline = Instant::now() + timeout;

    loop {
        device.step();
        for sender in senders.iter_mut() {
            sender.step();
        }

        let actual = local_chat_messages(device);
        if actual == *expected {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; expected {:?}, actual {:?}",
            expected,
            actual
        );

        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until every device matches the expected local message set.
pub fn wait_for_devices_messages(
    devices: &mut [DeviceHarness],
    expected: &BTreeSet<String>,
    timeout: Duration,
    context: &str,
) {
    let deadline = Instant::now() + timeout;

    loop {
        step_devices(devices);

        if devices
            .iter_mut()
            .all(|device| local_chat_messages(device) == *expected)
        {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; expected {:?}, actual {:?}",
            expected,
            devices
                .iter_mut()
                .map(local_chat_messages)
                .collect::<Vec<BTreeSet<String>>>()
        );

        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until every borrowed device matches the expected local message set.
pub fn wait_for_device_group_messages(
    devices: &mut [&mut DeviceHarness],
    expected: &BTreeSet<String>,
    timeout: Duration,
    context: &str,
) {
    wait_for_device_group_messages_impl(devices, expected, timeout, context, &mut []);
}

/// Waits until every borrowed device matches while also stepping any active senders.
pub fn wait_for_device_group_messages_while_flushing(
    devices: &mut [&mut DeviceHarness],
    expected: &BTreeSet<String>,
    timeout: Duration,
    context: &str,
    senders: &mut [&mut DeviceHarness],
) {
    wait_for_device_group_messages_impl(devices, expected, timeout, context, senders);
}

/// Implements the shared polling loop behind the borrowed-group wait helpers.
fn wait_for_device_group_messages_impl(
    devices: &mut [&mut DeviceHarness],
    expected: &BTreeSet<String>,
    timeout: Duration,
    context: &str,
    senders: &mut [&mut DeviceHarness],
) {
    let deadline = Instant::now() + timeout;

    loop {
        step_device_group(devices);
        for sender in senders.iter_mut() {
            sender.step();
        }

        if devices
            .iter_mut()
            .all(|device| local_chat_messages(device) == *expected)
        {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; expected {:?}, actual {:?}",
            expected,
            devices
                .iter_mut()
                .map(|device| local_chat_messages(device))
                .collect::<Vec<BTreeSet<String>>>()
        );

        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until every receiver device converges while also stepping the sender.
pub fn wait_for_convergence(
    sender: &mut DeviceHarness,
    devices: &mut [DeviceHarness],
    expected: &BTreeSet<String>,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;

    loop {
        sender.step();
        step_devices(devices);

        if devices
            .iter_mut()
            .all(|device| local_chat_messages(device) == *expected)
        {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for message convergence; expected {:?}, actual {:?}",
            expected,
            devices
                .iter_mut()
                .map(local_chat_messages)
                .collect::<Vec<BTreeSet<String>>>()
        );

        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Asserts that every device matches the expected message set.
pub fn assert_devices_match_expected(
    devices: &mut [DeviceHarness],
    expected: &BTreeSet<String>,
    context: &str,
) {
    let actual_sets: Vec<BTreeSet<String>> = devices.iter_mut().map(local_chat_messages).collect();

    assert!(
        actual_sets.iter().all(|set| set == expected),
        "{context}; expected {:?}, actual {:?}",
        expected,
        actual_sets
    );
}

/// Returns `true` when every device in a cluster matches the expected set.
pub fn cluster_converged_on(cluster: &mut AccountCluster, expected: &BTreeSet<String>) -> bool {
    cluster
        .devices
        .iter_mut()
        .all(|device| local_chat_messages(device) == *expected)
}

/// Returns each device's local chat set for diagnostics.
pub fn cluster_actual_sets(cluster: &mut AccountCluster) -> Vec<BTreeSet<String>> {
    cluster
        .devices
        .iter_mut()
        .map(local_chat_messages)
        .collect()
}

/// Waits until every cluster converges on its own expected local message set.
pub fn wait_for_cluster_convergence(
    alice: &mut AccountCluster,
    expected_alice: &BTreeSet<String>,
    bob: &mut AccountCluster,
    expected_bob: &BTreeSet<String>,
    carol: &mut AccountCluster,
    expected_carol: &BTreeSet<String>,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;

    loop {
        step_clusters(&mut [alice, bob, carol]);

        if cluster_converged_on(alice, expected_alice)
            && cluster_converged_on(bob, expected_bob)
            && cluster_converged_on(carol, expected_carol)
        {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for cluster convergence; {}; {}; {}",
            cluster_delivery_report(alice, expected_alice),
            cluster_delivery_report(bob, expected_bob),
            cluster_delivery_report(carol, expected_carol),
        );

        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Asserts that every device in one cluster matches the expected set.
pub fn assert_cluster_matches_expected(cluster: &mut AccountCluster, expected: &BTreeSet<String>) {
    let actual_sets = cluster_actual_sets(cluster);

    assert!(
        actual_sets.iter().all(|set| set == expected),
        "{} devices did not match expected delivery; {}",
        cluster.name,
        actual_sets
            .iter()
            .enumerate()
            .map(|(idx, actual)| format!(
                "{}[{idx}] {}",
                cluster.name,
                message_gap_summary(expected, actual)
            ))
            .collect::<Vec<_>>()
            .join("; "),
    );
}

/// Summarizes the delivery gap between one expected and actual message set.
fn message_gap_summary(expected: &BTreeSet<String>, actual: &BTreeSet<String>) -> String {
    let missing = expected
        .difference(actual)
        .take(6)
        .cloned()
        .collect::<Vec<_>>();
    let extras = actual
        .difference(expected)
        .take(6)
        .cloned()
        .collect::<Vec<_>>();
    let received = expected.intersection(actual).count();
    let missing_count = expected.len().saturating_sub(received);
    let extra_count = actual.len().saturating_sub(received);

    format!(
        "received {received}/{expected_total}, missing {missing_count} {missing:?}, extras {extra_count} {extras:?}",
        expected_total = expected.len(),
    )
}

/// Builds a compact per-device delivery report for one account cluster.
fn cluster_delivery_report(cluster: &mut AccountCluster, expected: &BTreeSet<String>) -> String {
    cluster
        .devices
        .iter_mut()
        .enumerate()
        .map(|(idx, device)| {
            let actual = local_chat_messages(device);
            format!(
                "{}[{idx}] {}",
                cluster.name,
                message_gap_summary(expected, &actual)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}
