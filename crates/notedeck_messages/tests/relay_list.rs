//! DM relay-list management end-to-end tests.

mod harness;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use enostr::FullKeypair;
use harness::fixtures::{
    local_dm_relay_list_relays, local_dm_relay_list_versions,
    seed_local_dm_relay_list_ndb_only_with_relays, seed_local_dm_relay_list_with_relays,
    seed_local_profile_metadata,
};
use harness::relay::relay_dm_relay_list_relays;
use harness::ui::{open_conversation_via_ui, send_direct_message, send_message_via_ui};
use harness::{
    build_messages_device, build_messages_device_in_path_with_relays,
    build_messages_device_with_relays, init_tracing, local_chat_messages, shutdown_messages_device,
    step_device_group, wait_for_device_messages_while_flushing, TEST_TIMEOUT,
};
use nostr::{Filter as NostrFilter, Kind as NostrKind};
use nostr_relay_builder::{
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use notedeck::unix_time_secs;
use notedeck_messages::nip17::default_dm_relay_urls;
use tempfile::TempDir;
/// Verifies the latest locally-known participant kind `10050` wins over older relay-list history.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn latest_local_participant_dm_relay_list_wins_e2e() {
    init_tracing();

    let relay_a = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start relay a");
    let relay_b = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start relay b");
    let relay_a_url = relay_a.url().to_owned();
    let relay_b_url = relay_b.url().to_owned();

    let sender = FullKeypair::generate();
    let participant = FullKeypair::generate();
    let now = unix_time_secs();
    let mut sender_device =
        build_messages_device_with_relays(&[&relay_a_url, &relay_b_url], &sender);

    seed_local_dm_relay_list_with_relays(
        &mut sender_device,
        &participant,
        &[&relay_a_url],
        Some(now.saturating_sub(60)),
    );
    let expected_a = vec![relay_a_url.trim_end_matches('/').to_owned()];
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        sender_device.step();
        let actual: Vec<_> = local_dm_relay_list_relays(&mut sender_device, &participant)
            .into_iter()
            .map(|url| url.trim_end_matches('/').to_owned())
            .collect();
        if actual == expected_a {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for older participant relay list; expected {:?}, actual {:?}",
            expected_a,
            actual
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    seed_local_dm_relay_list_with_relays(
        &mut sender_device,
        &participant,
        &[&relay_b_url],
        Some(now),
    );
    let expected_b = vec![relay_b_url.trim_end_matches('/').to_owned()];
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        sender_device.step();
        let actual: Vec<_> = local_dm_relay_list_relays(&mut sender_device, &participant)
            .into_iter()
            .map(|url| url.trim_end_matches('/').to_owned())
            .collect();
        if actual == expected_b {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for newer participant relay list; expected {:?}, actual {:?}",
            expected_b,
            actual
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    relay_a.shutdown();
    relay_b.shutdown();
}

/// Verifies a fresh account publishes a default DM relay-list note after startup all-EOSE.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_publishes_default_dm_relay_list_when_missing_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let account = FullKeypair::generate();
    let mut device = build_messages_device(&relay_url, &account);
    let expected = default_dm_relay_urls()
        .iter()
        .map(|url| url.trim_end_matches('/').to_owned())
        .collect::<Vec<_>>();

    let deadline = std::time::Instant::now() + TEST_TIMEOUT;
    loop {
        device.step();
        let actual = local_dm_relay_list_relays(&mut device, &account)
            .into_iter()
            .map(|url| url.trim_end_matches('/').to_owned())
            .collect::<Vec<_>>();
        if actual == expected {
            break;
        }

        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for default dm relay list; expected {:?}, actual {:?}",
            expected,
            actual
        );

        std::thread::sleep(Duration::from_millis(20));
    }

    relay.shutdown();
}

/// Verifies sending uses the latest locally-known participant DM relay list, not a stale one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sending_uses_latest_participant_dm_relay_list_e2e() {
    init_tracing();

    let relay_a = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start relay a");
    let relay_b = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start relay b");
    let relay_a_url = relay_a.url().to_owned();
    let relay_b_url = relay_b.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");
    let now = unix_time_secs();

    let mut sender_device =
        build_messages_device_with_relays(&[&relay_a_url, &relay_b_url], &sender);

    seed_local_dm_relay_list_with_relays(
        &mut sender_device,
        &recipient,
        &[&relay_a_url],
        Some(now.saturating_sub(60)),
    );
    let expected_a = vec![relay_a_url.trim_end_matches('/').to_owned()];
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        sender_device.step();
        let actual: Vec<_> = local_dm_relay_list_relays(&mut sender_device, &recipient)
            .into_iter()
            .map(|url| url.trim_end_matches('/').to_owned())
            .collect();
        if actual == expected_a {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for relay a as latest participant dm relay list; expected {:?}, actual {:?}",
            expected_a,
            actual
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    open_conversation_via_ui(&mut sender_device, &recipient_npub);
    send_message_via_ui(&mut sender_device, "route-a");

    seed_local_dm_relay_list_with_relays(
        &mut sender_device,
        &recipient,
        &[&relay_b_url],
        Some(now),
    );
    let expected_b = vec![relay_b_url.trim_end_matches('/').to_owned()];
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        sender_device.step();
        let actual: Vec<_> = local_dm_relay_list_relays(&mut sender_device, &recipient)
            .into_iter()
            .map(|url| url.trim_end_matches('/').to_owned())
            .collect();
        if actual == expected_b {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for relay b as latest participant dm relay list; expected {:?}, actual {:?}",
            expected_b,
            actual
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    send_message_via_ui(&mut sender_device, "route-b");

    let mut recipient_a = build_messages_device(&relay_a_url, &recipient);
    let mut recipient_b = build_messages_device(&relay_b_url, &recipient);
    wait_for_device_messages_while_flushing(
        &mut recipient_a,
        &BTreeSet::from(["route-a".to_owned()]),
        TEST_TIMEOUT,
        "initial relay-list route to relay a",
        &mut [&mut sender_device],
    );
    wait_for_device_messages_while_flushing(
        &mut recipient_b,
        &BTreeSet::from(["route-b".to_owned()]),
        TEST_TIMEOUT,
        "updated relay-list route to relay b",
        &mut [&mut sender_device],
    );
    assert_eq!(
        local_chat_messages(&mut recipient_a),
        BTreeSet::from(["route-a".to_owned()]),
        "expected relay a device to retain only the first delivery after the relay-list switch"
    );

    relay_a.shutdown();
    relay_b.shutdown();
}

/// Verifies an offline same-account sender refreshes stale participant relay-list state after restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_same_account_sender_refreshes_stale_participant_relay_list_after_restart_e2e() {
    init_tracing();

    let relay_a = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start relay a");
    let relay_b = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start relay b");
    let relay_a_url = relay_a.url().to_owned();
    let relay_b_url = relay_b.url().to_owned();

    let sender = FullKeypair::generate();
    let recipient = FullKeypair::generate();
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");
    let now = unix_time_secs();

    let mut sender_current =
        build_messages_device_with_relays(&[&relay_a_url, &relay_b_url], &sender);
    let offline_dir = TempDir::new().expect("offline sender dir");
    let offline_path = offline_dir.path().to_path_buf();
    let mut sender_offline = build_messages_device_in_path_with_relays(
        &[&relay_a_url, &relay_b_url],
        &sender,
        &offline_path,
    );
    let mut recipient_a = build_messages_device(&relay_a_url, &recipient);
    let mut recipient_b = build_messages_device(&relay_b_url, &recipient);

    for sender_device in [&mut sender_current, &mut sender_offline] {
        seed_local_dm_relay_list_with_relays(
            sender_device,
            &recipient,
            &[&relay_a_url],
            Some(now.saturating_sub(60)),
        );
        seed_local_profile_metadata(sender_device, &recipient, "relay-list-recipient");
    }
    seed_local_dm_relay_list_with_relays(
        &mut recipient_a,
        &recipient,
        &[&relay_a_url],
        Some(now.saturating_sub(60)),
    );
    seed_local_dm_relay_list_with_relays(
        &mut recipient_b,
        &recipient,
        &[&relay_a_url],
        Some(now.saturating_sub(60)),
    );

    step_device_group(&mut [
        &mut sender_current,
        &mut sender_offline,
        &mut recipient_a,
        &mut recipient_b,
    ]);
    std::thread::sleep(Duration::from_millis(100));
    step_device_group(&mut [
        &mut sender_current,
        &mut sender_offline,
        &mut recipient_a,
        &mut recipient_b,
    ]);

    send_direct_message(
        &mut sender_current,
        &recipient_npub,
        "route-a-before-offline",
    );
    wait_for_device_messages_while_flushing(
        &mut recipient_a,
        &BTreeSet::from(["route-a-before-offline".to_owned()]),
        TEST_TIMEOUT,
        "initial stale relay-list route before sender goes offline",
        &mut [&mut sender_current],
    );

    shutdown_messages_device(
        sender_offline,
        "offline same-account sender shutdown before restart",
    );

    seed_local_dm_relay_list_with_relays(
        &mut recipient_b,
        &recipient,
        &[&relay_b_url],
        Some(now.saturating_add(1)),
    );
    step_device_group(&mut [&mut sender_current, &mut recipient_b]);
    std::thread::sleep(Duration::from_millis(100));
    step_device_group(&mut [&mut sender_current, &mut recipient_b]);

    let mut restarted_sender = build_messages_device_in_path_with_relays(
        &[&relay_a_url, &relay_b_url],
        &sender,
        &offline_path,
    );
    let expected_new_version = (
        now.saturating_add(1),
        vec![relay_b_url.trim_end_matches('/').to_owned()],
    );
    let local_version_deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        restarted_sender.step();
        let actual_versions = local_dm_relay_list_versions(&mut restarted_sender, &recipient)
            .into_iter()
            .map(|(created_at, relays)| {
                (
                    created_at,
                    relays
                        .into_iter()
                        .map(|url| url.trim_end_matches('/').to_owned())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        if actual_versions
            .iter()
            .any(|version| version == &expected_new_version)
        {
            break;
        }

        assert!(
            Instant::now() < local_version_deadline,
            "timed out waiting for restarted sender to ingest newer participant relay-list note; expected {:?}, actual {:?}",
            expected_new_version,
            actual_versions
        );

        std::thread::sleep(Duration::from_millis(20));
    }

    let expected_relays = vec![relay_b_url.trim_end_matches('/').to_owned()];
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        restarted_sender.step();
        let actual = local_dm_relay_list_relays(&mut restarted_sender, &recipient)
            .into_iter()
            .map(|url| url.trim_end_matches('/').to_owned())
            .collect::<Vec<_>>();
        if actual == expected_relays {
            break;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for restarted sender to refresh participant relay list; expected {:?}, actual {:?}",
            expected_relays,
            actual
        );

        std::thread::sleep(Duration::from_millis(20));
    }

    send_direct_message(
        &mut restarted_sender,
        &recipient_npub,
        "route-b-after-restart",
    );
    wait_for_device_messages_while_flushing(
        &mut recipient_b,
        &BTreeSet::from(["route-b-after-restart".to_owned()]),
        TEST_TIMEOUT,
        "same-account sender after refreshing stale relay-list state on restart",
        &mut [&mut restarted_sender],
    );
    assert_eq!(
        local_chat_messages(&mut recipient_a),
        BTreeSet::from(["route-a-before-offline".to_owned()]),
        "expected relay a recipient to retain only the pre-offline delivery"
    );

    relay_a.shutdown();
    relay_b.shutdown();
}

/// Verifies restart alone does not refresh a known participant's newer DM relay-list note.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_should_prefetch_newer_known_participant_relay_list_e2e() {
    init_tracing();

    let relay_a = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start relay a");
    let relay_b = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start relay b");
    let relay_a_url = relay_a.url().to_owned();
    let relay_b_url = relay_b.url().to_owned();

    let sender = FullKeypair::generate();
    let recipient = FullKeypair::generate();
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");
    let now = unix_time_secs();

    let mut sender_current =
        build_messages_device_with_relays(&[&relay_a_url, &relay_b_url], &sender);
    let offline_dir = TempDir::new().expect("participant relay prefetch dir");
    let offline_path = offline_dir.path().to_path_buf();
    let mut sender_offline = build_messages_device_in_path_with_relays(
        &[&relay_a_url, &relay_b_url],
        &sender,
        &offline_path,
    );
    let mut recipient_a = build_messages_device(&relay_a_url, &recipient);
    let mut recipient_b = build_messages_device(&relay_b_url, &recipient);

    for sender_device in [&mut sender_current, &mut sender_offline] {
        seed_local_dm_relay_list_with_relays(
            sender_device,
            &recipient,
            &[&relay_a_url],
            Some(now.saturating_sub(60)),
        );
        seed_local_profile_metadata(sender_device, &recipient, "relay-prefetch-recipient");
    }
    seed_local_dm_relay_list_with_relays(
        &mut recipient_a,
        &recipient,
        &[&relay_a_url],
        Some(now.saturating_sub(60)),
    );
    seed_local_dm_relay_list_with_relays(
        &mut recipient_b,
        &recipient,
        &[&relay_a_url],
        Some(now.saturating_sub(60)),
    );

    step_device_group(&mut [
        &mut sender_current,
        &mut sender_offline,
        &mut recipient_a,
        &mut recipient_b,
    ]);
    std::thread::sleep(Duration::from_millis(100));
    step_device_group(&mut [
        &mut sender_current,
        &mut sender_offline,
        &mut recipient_a,
        &mut recipient_b,
    ]);

    let expected_a = vec![relay_a_url.trim_end_matches('/').to_owned()];
    let initial_deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        sender_current.step();
        let actual: Vec<_> = local_dm_relay_list_relays(&mut sender_current, &recipient)
            .into_iter()
            .map(|url| url.trim_end_matches('/').to_owned())
            .collect();
        if actual == expected_a {
            break;
        }
        assert!(
            Instant::now() < initial_deadline,
            "timed out waiting for initial participant relay-list prefetch; expected {:?}, actual {:?}",
            expected_a,
            actual
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    send_direct_message(
        &mut sender_current,
        &recipient_npub,
        "prefetch-before-offline",
    );
    wait_for_device_messages_while_flushing(
        &mut recipient_a,
        &BTreeSet::from(["prefetch-before-offline".to_owned()]),
        TEST_TIMEOUT,
        "initial direct-message route before participant relay-list update",
        &mut [&mut sender_current],
    );

    shutdown_messages_device(
        sender_offline,
        "participant relay prefetch sender shutdown before restart",
    );

    seed_local_dm_relay_list_with_relays(
        &mut recipient_b,
        &recipient,
        &[&relay_b_url],
        Some(now.saturating_add(1)),
    );
    step_device_group(&mut [&mut sender_current, &mut recipient_b]);
    std::thread::sleep(Duration::from_millis(100));
    step_device_group(&mut [&mut sender_current, &mut recipient_b]);

    let mut restarted_sender = build_messages_device_in_path_with_relays(
        &[&relay_a_url, &relay_b_url],
        &sender,
        &offline_path,
    );
    let expected_new_version = (
        now.saturating_add(1),
        vec![relay_b_url.trim_end_matches('/').to_owned()],
    );
    let local_version_deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        restarted_sender.step();
        let actual_versions = local_dm_relay_list_versions(&mut restarted_sender, &recipient)
            .into_iter()
            .map(|(created_at, relays)| {
                (
                    created_at,
                    relays
                        .into_iter()
                        .map(|url| url.trim_end_matches('/').to_owned())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        if actual_versions
            .iter()
            .any(|version| version == &expected_new_version)
        {
            break;
        }

        assert!(
            Instant::now() < local_version_deadline,
            "timed out waiting for restarted sender to ingest newer known participant relay-list note; expected {:?}, actual {:?}",
            expected_new_version,
            actual_versions
        );

        std::thread::sleep(Duration::from_millis(20));
    }

    relay_a.shutdown();
    relay_b.shutdown();
}

/// Verifies a fresh account publishes a default DM relay list even when one relay never sends EOSE.
///
/// When a device connects to multiple relays and one is unreachable, the relay-list ensure
/// state machine should not stall forever waiting for all-EOSE. After a timeout it should
/// publish a backdated default list so the account can send and receive immediately.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_dm_relay_list_publishes_default_despite_partial_eose_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    // Second relay URL that will never respond — a bogus address on a high port
    let unreachable_url = "ws://127.0.0.1:1";

    let account = FullKeypair::generate();
    let mut device = build_messages_device_with_relays(&[&relay_url, unreachable_url], &account);

    let expected = default_dm_relay_urls()
        .iter()
        .map(|url| url.trim_end_matches('/').to_owned())
        .collect::<Vec<_>>();

    // Give the device 20 seconds to publish a default relay list.
    // With one unreachable relay, all-EOSE never arrives, so without the timeout fix
    // the ensure state machine stalls and no list is ever published.
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        device.step();
        let actual = local_dm_relay_list_relays(&mut device, &account)
            .into_iter()
            .map(|url| url.trim_end_matches('/').to_owned())
            .collect::<Vec<_>>();
        if actual == expected {
            break;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for default dm relay list with partial EOSE; expected {:?}, actual {:?}",
            expected,
            actual
        );

        std::thread::sleep(Duration::from_millis(50));
    }

    relay.shutdown();
}

/// Verifies that after the timeout fallback publishes a backdated default relay list,
/// a later real selected-account relay list arriving locally is republished to relays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_dm_relay_list_republishes_late_real_list_after_timeout_fallback_e2e() {
    init_tracing();

    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });
    let relay = LocalRelay::run(RelayBuilder::default().database(relay_db.clone()))
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    // Second relay URL that will never respond, forcing the ensure timeout path.
    let unreachable_url = "ws://127.0.0.1:1";

    let account = FullKeypair::generate();
    let mut device = build_messages_device_with_relays(&[&relay_url, unreachable_url], &account);

    let expected_fallback = default_dm_relay_urls()
        .iter()
        .map(|url| url.trim_end_matches('/').to_owned())
        .collect::<Vec<_>>();
    let expected_real = vec![relay_url.trim_end_matches('/').to_owned()];
    let relay_list_filter = NostrFilter::new().kind(NostrKind::Custom(10050));

    // Wait for the timeout fallback to publish the backdated default list locally and remotely.
    let fallback_deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        device.step();

        let actual = local_dm_relay_list_relays(&mut device, &account)
            .into_iter()
            .map(|url| url.trim_end_matches('/').to_owned())
            .collect::<Vec<_>>();
        let relay_count = relay_db
            .count(vec![relay_list_filter.clone()])
            .await
            .expect("query relay-list count after timeout fallback");
        if actual == expected_fallback && relay_count >= 1 {
            break;
        }

        assert!(
            Instant::now() < fallback_deadline,
            "timed out waiting for timeout fallback relay list; expected local {:?} and relay count >= 1, actual local {:?}, relay count {}",
            expected_fallback,
            actual,
            relay_count
        );

        std::thread::sleep(Duration::from_millis(50));
    }

    // Inject the real selected-account relay list into local NDB only. The device must
    // notice it in FallbackPublished state and republish it to AccountsWrite.
    seed_local_dm_relay_list_ndb_only_with_relays(
        &mut device,
        &account,
        &[&relay_url],
        Some(unix_time_secs()),
    );

    let real_list_deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        device.step();

        let actual = local_dm_relay_list_relays(&mut device, &account)
            .into_iter()
            .map(|url| url.trim_end_matches('/').to_owned())
            .collect::<Vec<_>>();
        let relay_actual = relay_dm_relay_list_relays(&relay_db, &account).await;
        if actual == expected_real && relay_actual == expected_real {
            break;
        }

        assert!(
            Instant::now() < real_list_deadline,
            "timed out waiting for late real relay list republish; expected local {:?} and relay {:?}, actual local {:?} and relay {:?}",
            expected_real,
            expected_real,
            actual,
            relay_actual
        );

        std::thread::sleep(Duration::from_millis(50));
    }

    relay.shutdown();
}
