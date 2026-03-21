//! Device restart and recovery end-to-end tests.

mod harness;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use enostr::FullKeypair;
use harness::fixtures::{
    local_dm_relay_list_relays, seed_local_dm_relay_list, seed_local_dm_relay_list_with_relays,
};
use harness::relay::wait_for_relay_count_at_least;
use harness::ui::{open_conversation_via_ui, send_message_via_ui};
use harness::{
    build_messages_device, build_messages_device_in_path_with_relays, init_tracing,
    local_chat_messages, shutdown_messages_device, step_device_group,
    wait_for_device_group_messages, wait_for_device_group_messages_while_flushing,
    wait_for_device_messages, wait_for_device_messages_while_flushing, TEST_TIMEOUT,
};
use nostr::{Filter as NostrFilter, Kind as NostrKind};
use nostr_relay_builder::{
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use tempfile::TempDir;
/// Verifies a restarted same-account device recovers missed relay history from its existing on-disk DB.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_device_restart_catches_up_from_existing_db_e2e() {
    init_tracing();

    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });
    let relay = LocalRelay::run(RelayBuilder::default().database(relay_db.clone()))
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");

    let mut sender_device = build_messages_device(&relay_url, &sender);
    let mut stable_device = build_messages_device(&relay_url, &recipient);
    let restart_dir = TempDir::new().expect("restart dir");
    let restart_path = restart_dir.path().to_path_buf();
    let mut restarting_device =
        build_messages_device_in_path_with_relays(&[&relay_url], &recipient, &restart_path);

    seed_local_dm_relay_list(&mut sender_device, &sender, &relay_url);
    seed_local_dm_relay_list(&mut sender_device, &recipient, &relay_url);
    seed_local_dm_relay_list(&mut stable_device, &recipient, &relay_url);
    seed_local_dm_relay_list(&mut restarting_device, &recipient, &relay_url);

    sender_device.step();
    step_device_group(&mut [&mut stable_device, &mut restarting_device]);
    std::thread::sleep(Duration::from_millis(100));
    sender_device.step();
    step_device_group(&mut [&mut stable_device, &mut restarting_device]);

    open_conversation_via_ui(&mut sender_device, &recipient_npub);

    let initial = BTreeSet::from(["restart-01".to_owned(), "restart-02".to_owned()]);
    let mut sender_expected = BTreeSet::new();
    for message in &initial {
        sender_expected.insert(message.clone());
        let relay_giftwrap_count_before = relay_db
            .count(vec![NostrFilter::new().kind(NostrKind::GiftWrap)])
            .await
            .expect("query relay giftwrap count before initial restart send");
        send_message_via_ui(&mut sender_device, message);
        wait_for_device_messages(
            &mut sender_device,
            &sender_expected,
            TEST_TIMEOUT,
            "sender self-copy before restart",
        );
        wait_for_relay_count_at_least(
            &relay_db,
            NostrFilter::new().kind(NostrKind::GiftWrap),
            relay_giftwrap_count_before + 2,
            TEST_TIMEOUT,
            "initial restart giftwraps to land on the relay",
            &mut [&mut sender_device],
        )
        .await;
        step_device_group(&mut [&mut stable_device, &mut restarting_device]);
        std::thread::sleep(Duration::from_millis(25));
    }

    wait_for_device_group_messages(
        &mut [&mut stable_device, &mut restarting_device],
        &initial,
        TEST_TIMEOUT,
        "initial same-account device convergence before restart",
    );

    shutdown_messages_device(
        restarting_device,
        "same-account restart device shutdown before reopen",
    );

    let expected = BTreeSet::from([
        "restart-01".to_owned(),
        "restart-02".to_owned(),
        "restart-03".to_owned(),
        "restart-04".to_owned(),
        "restart-05".to_owned(),
    ]);
    sender_expected = initial.clone();
    for message in ["restart-03", "restart-04", "restart-05"] {
        sender_expected.insert(message.to_owned());
        let relay_giftwrap_count_before = relay_db
            .count(vec![NostrFilter::new().kind(NostrKind::GiftWrap)])
            .await
            .expect("query relay giftwrap count during restart gap");
        send_message_via_ui(&mut sender_device, message);
        wait_for_device_messages(
            &mut sender_device,
            &sender_expected,
            TEST_TIMEOUT,
            "sender self-copy during restart gap",
        );
        wait_for_relay_count_at_least(
            &relay_db,
            NostrFilter::new().kind(NostrKind::GiftWrap),
            relay_giftwrap_count_before + 2,
            TEST_TIMEOUT,
            "restart gap giftwraps to land on the relay",
            &mut [&mut sender_device],
        )
        .await;
        stable_device.step();
        std::thread::sleep(Duration::from_millis(25));
    }

    wait_for_device_messages(
        &mut stable_device,
        &expected,
        TEST_TIMEOUT,
        "stable same-account device after peer restart",
    );

    let mut restarted_device =
        build_messages_device_in_path_with_relays(&[&relay_url], &recipient, &restart_path);
    wait_for_device_messages(
        &mut restarted_device,
        &expected,
        TEST_TIMEOUT,
        "restarted same-account device to recover missed relay history",
    );

    assert_eq!(local_chat_messages(&mut stable_device), expected);
    assert_eq!(local_chat_messages(&mut restarted_device), expected);

    relay.shutdown();
}

/// Verifies a device recovers messages sent while it was offline by restarting
/// with the relay that has the missed messages.
///
/// 1. Recipient sees initial messages from both senders on both relays.
/// 2. Recipient goes offline (dropped).  Messages are sent while it's down.
/// 3. Recipient restarts with both relays and catches up on everything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_device_recovers_missed_messages_after_offline_restart_e2e() {
    init_tracing();

    let relay_a_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        max_events: Some(75_000),
    });
    let relay_a = LocalRelay::run(RelayBuilder::default().database(relay_a_db.clone()))
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

    let mut sender_a = build_messages_device(&relay_a_url, &sender);
    let mut sender_b = build_messages_device(&relay_b_url, &sender);
    let churn_dir = TempDir::new().expect("relay visibility churn dir");
    let churn_path = churn_dir.path().to_path_buf();
    let mut recipient_device = build_messages_device_in_path_with_relays(
        &[&relay_a_url, &relay_b_url],
        &recipient,
        &churn_path,
    );

    seed_local_dm_relay_list(&mut sender_a, &sender, &relay_a_url);
    seed_local_dm_relay_list(&mut sender_b, &sender, &relay_b_url);
    seed_local_dm_relay_list(&mut sender_a, &recipient, &relay_a_url);
    seed_local_dm_relay_list(&mut sender_b, &recipient, &relay_b_url);
    seed_local_dm_relay_list_with_relays(
        &mut recipient_device,
        &recipient,
        &[&relay_a_url, &relay_b_url],
        None,
    );

    // Warm up: poll-step until the recipient's relay list is visible locally,
    // meaning relay_ensure has run and the device is ready to receive giftwraps.
    let warmup_deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        step_device_group(&mut [&mut sender_a, &mut sender_b, &mut recipient_device]);
        let relays = local_dm_relay_list_relays(&mut recipient_device, &recipient);
        if !relays.is_empty() {
            break;
        }
        assert!(
            Instant::now() < warmup_deadline,
            "timed out waiting for recipient relay list to become visible during warmup"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    open_conversation_via_ui(&mut sender_a, &recipient_npub);
    open_conversation_via_ui(&mut sender_b, &recipient_npub);

    send_message_via_ui(&mut sender_a, "before-outage-a");
    send_message_via_ui(&mut sender_b, "before-outage-b");

    let expected_initial =
        BTreeSet::from(["before-outage-a".to_owned(), "before-outage-b".to_owned()]);
    wait_for_device_messages_while_flushing(
        &mut recipient_device,
        &expected_initial,
        TEST_TIMEOUT,
        "recipient device before going offline",
        &mut [&mut sender_a, &mut sender_b],
    );

    // Phase 2: recipient goes fully offline.  Messages arrive on the relays
    // while the device is down.
    shutdown_messages_device(
        recipient_device,
        "recipient device shutdown before offline restart",
    );

    send_message_via_ui(&mut sender_b, "during-outage-b");

    let relay_a_count_before = relay_a_db
        .count(vec![NostrFilter::new().kind(NostrKind::GiftWrap)])
        .await
        .expect("query relay a giftwrap count before outage send");
    send_message_via_ui(&mut sender_a, "during-outage-a");

    // Wait for at least 2 new giftwraps on relay-a (one per participant)
    // before restarting the recipient.
    wait_for_relay_count_at_least(
        &relay_a_db,
        NostrFilter::new().kind(NostrKind::GiftWrap),
        relay_a_count_before + 2,
        TEST_TIMEOUT,
        "relay a to store the outage-window giftwraps",
        &mut [&mut sender_a, &mut sender_b],
    )
    .await;

    // Phase 3: recipient restarts with both relays and catches up.
    let expected_all = BTreeSet::from([
        "before-outage-a".to_owned(),
        "before-outage-b".to_owned(),
        "during-outage-b".to_owned(),
        "during-outage-a".to_owned(),
    ]);
    let mut recovered_recipient = build_messages_device_in_path_with_relays(
        &[&relay_a_url, &relay_b_url],
        &recipient,
        &churn_path,
    );
    wait_for_device_messages(
        &mut recovered_recipient,
        &expected_all,
        TEST_TIMEOUT,
        "recipient device after restart recovers missed messages",
    );

    relay_a.shutdown();
    relay_b.shutdown();
}

/// Verifies repeated same-account stop-start cycles preserve history without gaps or duplicates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_same_account_restart_cycles_preserve_message_history_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");

    let mut sender_device = build_messages_device(&relay_url, &sender);
    let mut stable_device = build_messages_device(&relay_url, &recipient);
    let restart_dir = TempDir::new().expect("restart cycles dir");
    let restart_path = restart_dir.path().to_path_buf();
    let mut flapping_device = Some(build_messages_device_in_path_with_relays(
        &[&relay_url],
        &recipient,
        &restart_path,
    ));

    seed_local_dm_relay_list(&mut sender_device, &sender, &relay_url);
    seed_local_dm_relay_list(&mut sender_device, &recipient, &relay_url);
    seed_local_dm_relay_list(&mut stable_device, &recipient, &relay_url);
    seed_local_dm_relay_list(
        flapping_device.as_mut().expect("initial flapping device"),
        &recipient,
        &relay_url,
    );

    step_device_group(&mut [
        &mut sender_device,
        &mut stable_device,
        flapping_device
            .as_mut()
            .expect("flapping device for warmup"),
    ]);
    std::thread::sleep(Duration::from_millis(100));
    step_device_group(&mut [
        &mut sender_device,
        &mut stable_device,
        flapping_device
            .as_mut()
            .expect("flapping device for warmup"),
    ]);

    open_conversation_via_ui(&mut sender_device, &recipient_npub);

    let mut expected = BTreeSet::from(["restart-cycle-baseline".to_owned()]);
    send_message_via_ui(&mut sender_device, "restart-cycle-baseline");
    wait_for_device_group_messages_while_flushing(
        &mut [
            &mut stable_device,
            flapping_device
                .as_mut()
                .expect("flapping device after baseline"),
        ],
        &expected,
        TEST_TIMEOUT,
        "baseline same-account convergence before repeated restart cycles",
        &mut [&mut sender_device],
    );

    for cycle in 1..=3 {
        if let Some(flapping_device) = flapping_device.take() {
            shutdown_messages_device(
                flapping_device,
                "flapping same-account device shutdown before restart cycle reopen",
            );
        }

        for suffix in ["offline-a", "offline-b"] {
            let message = format!("restart-cycle-{cycle}:{suffix}");
            expected.insert(message.clone());
            send_message_via_ui(&mut sender_device, &message);
            sender_device.step();
            stable_device.step();
            std::thread::sleep(Duration::from_millis(20));
        }

        wait_for_device_messages(
            &mut stable_device,
            &expected,
            TEST_TIMEOUT,
            "stable same-account device while peer is offline during restart cycles",
        );

        flapping_device = Some(build_messages_device_in_path_with_relays(
            &[&relay_url],
            &recipient,
            &restart_path,
        ));

        let post_restart = format!("restart-cycle-{cycle}:post-restart");
        expected.insert(post_restart.clone());
        send_message_via_ui(&mut sender_device, &post_restart);
        step_device_group(&mut [
            &mut sender_device,
            &mut stable_device,
            flapping_device
                .as_mut()
                .expect("flapping device after restart"),
        ]);
        std::thread::sleep(Duration::from_millis(20));

        wait_for_device_group_messages_while_flushing(
            &mut [
                &mut stable_device,
                flapping_device
                    .as_mut()
                    .expect("flapping device during convergence"),
            ],
            &expected,
            TEST_TIMEOUT,
            "same-account devices across repeated restart cycles",
            &mut [&mut sender_device],
        );
    }

    assert_eq!(local_chat_messages(&mut stable_device), expected);
    assert_eq!(
        local_chat_messages(flapping_device.as_mut().expect("final flapping device")),
        expected
    );

    relay.shutdown();
}
