//! Multi-conversation fan-in, interleave, and restart end-to-end tests.

mod harness;

use std::collections::BTreeSet;
use std::time::Duration;

use enostr::FullKeypair;
use harness::fixtures::{
    local_dm_relay_list_relays, seed_local_dm_relay_list, seed_local_dm_relay_list_with_relays,
    seed_local_profile_metadata,
};
use harness::relay::{wait_for_relay_count_at_least, wait_for_relay_giftwrap_count};
use harness::ui::{
    build_direct_message_batch, open_conversation_via_ui, send_direct_message, send_message_via_ui,
};
use harness::{
    assert_devices_match_expected, build_messages_device,
    build_messages_device_in_path_with_relays, build_messages_device_with_relays, init_tracing,
    local_chat_messages, shutdown_messages_device, step_device_frames, step_device_group,
    step_devices, wait_for_convergence, wait_for_device_group_messages,
    wait_for_device_group_messages_while_flushing, wait_for_device_messages,
    wait_for_devices_messages, DeviceHarness, TEST_TIMEOUT,
};
use nostr::{Filter as NostrFilter, Kind as NostrKind};
use nostr_relay_builder::{
    builder::RateLimit,
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use tempfile::TempDir;
/// Verifies same-account devices converge when many different direct-message conversations update at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_converge_across_many_direct_conversations_e2e() {
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
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");
    let sender_accounts = vec![
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
    ];
    let mut sender_devices: Vec<_> = sender_accounts
        .iter()
        .map(|sender| build_messages_device(&relay_url, sender))
        .collect();
    let mut recipient_devices = vec![
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
    ];

    for (sender_device, sender_account) in sender_devices.iter_mut().zip(&sender_accounts) {
        seed_local_dm_relay_list(sender_device, sender_account, &relay_url);
        seed_local_dm_relay_list(sender_device, &recipient, &relay_url);
    }
    for recipient_device in &mut recipient_devices {
        seed_local_dm_relay_list(recipient_device, &recipient, &relay_url);
    }

    step_devices(&mut sender_devices);
    step_devices(&mut recipient_devices);
    std::thread::sleep(Duration::from_millis(100));
    step_devices(&mut sender_devices);
    step_devices(&mut recipient_devices);

    for sender_device in &mut sender_devices {
        open_conversation_via_ui(sender_device, &recipient_npub);
    }

    let mut expected = BTreeSet::new();
    for (idx, sender_device) in sender_devices.iter_mut().enumerate() {
        let sender_name = format!("sender-{}", idx + 1);
        let batch = build_direct_message_batch(&sender_name, "recipient", 4);

        for message in batch {
            expected.insert(message.clone());
            send_message_via_ui(sender_device, &message);
            sender_device.step();
            wait_for_relay_giftwrap_count(
                &relay_db,
                expected.len() * 2,
                TEST_TIMEOUT,
                "giftwraps to land on relay before recipient step",
                &mut [],
            )
            .await;
            step_devices(&mut recipient_devices);
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    wait_for_devices_messages(
        &mut recipient_devices,
        &expected,
        TEST_TIMEOUT,
        "same-account devices to converge across many direct-message conversations",
    );
    assert_devices_match_expected(
        &mut recipient_devices,
        &expected,
        "expected same-account devices to converge across many conversations",
    );

    relay.shutdown();
}

/// Verifies cold-start same-account devices backfill many pre-existing direct-message conversations at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_backfill_many_preexisting_conversations_e2e() {
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
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");
    let sender_accounts = vec![
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
    ];
    let mut sender_devices: Vec<_> = sender_accounts
        .iter()
        .map(|sender| build_messages_device(&relay_url, sender))
        .collect();

    for (sender_device, sender_account) in sender_devices.iter_mut().zip(&sender_accounts) {
        seed_local_dm_relay_list(sender_device, sender_account, &relay_url);
        seed_local_dm_relay_list(sender_device, &recipient, &relay_url);
    }

    step_devices(&mut sender_devices);
    std::thread::sleep(Duration::from_millis(100));
    step_devices(&mut sender_devices);

    for sender_device in &mut sender_devices {
        open_conversation_via_ui(sender_device, &recipient_npub);
    }

    let mut expected = BTreeSet::new();
    for (idx, sender_device) in sender_devices.iter_mut().enumerate() {
        let sender_name = format!("history-sender-{}", idx + 1);
        let batch = build_direct_message_batch(&sender_name, "recipient", 3);

        for message in batch {
            expected.insert(message.clone());
            send_message_via_ui(sender_device, &message);
            sender_device.step();
            wait_for_relay_giftwrap_count(
                &relay_db,
                expected.len() * 2,
                TEST_TIMEOUT,
                "giftwraps to land on relay before creating cold-start recipients",
                &mut [],
            )
            .await;
        }
    }

    for sender_device in &mut sender_devices {
        step_device_frames(sender_device, 3);
    }

    let mut recipient_devices = vec![
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
    ];

    wait_for_devices_messages(
        &mut recipient_devices,
        &expected,
        TEST_TIMEOUT,
        "same-account cold-start backfill across many direct-message conversations",
    );
    assert_devices_match_expected(
        &mut recipient_devices,
        &expected,
        "expected same-account devices to backfill many pre-existing conversations",
    );

    relay.shutdown();
}

/// Verifies a restarted same-account device catches up across many different direct-message conversations.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_device_restart_catches_up_across_many_direct_conversations_e2e() {
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
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");
    let sender_accounts = vec![
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
    ];
    let mut sender_devices: Vec<_> = sender_accounts
        .iter()
        .map(|sender| build_messages_device(&relay_url, sender))
        .collect();
    let mut stable_device = build_messages_device(&relay_url, &recipient);
    let restart_dir = TempDir::new().expect("restart dir");
    let restart_path = restart_dir.path().to_path_buf();
    let mut restarting_device =
        build_messages_device_in_path_with_relays(&[&relay_url], &recipient, &restart_path);

    for (sender_device, sender_account) in sender_devices.iter_mut().zip(&sender_accounts) {
        seed_local_dm_relay_list(sender_device, sender_account, &relay_url);
        seed_local_dm_relay_list(sender_device, &recipient, &relay_url);
    }
    seed_local_dm_relay_list(&mut stable_device, &recipient, &relay_url);
    seed_local_dm_relay_list(&mut restarting_device, &recipient, &relay_url);

    step_devices(&mut sender_devices);
    step_device_group(&mut [&mut stable_device, &mut restarting_device]);
    std::thread::sleep(Duration::from_millis(100));
    step_devices(&mut sender_devices);
    step_device_group(&mut [&mut stable_device, &mut restarting_device]);

    for sender_device in &mut sender_devices {
        open_conversation_via_ui(sender_device, &recipient_npub);
    }

    let mut initial_expected = BTreeSet::new();
    let mut sender_expected = [BTreeSet::new(), BTreeSet::new(), BTreeSet::new()];
    for (idx, sender_device) in sender_devices.iter_mut().enumerate() {
        let sender_name = format!("restart-sender-{}", idx + 1);
        let batch = build_direct_message_batch(&sender_name, "recipient", 2);

        for message in batch {
            initial_expected.insert(message.clone());
            sender_expected[idx].insert(message.clone());
            let relay_giftwrap_count_before = relay_db
                .count(vec![NostrFilter::new().kind(NostrKind::GiftWrap)])
                .await
                .expect("query relay giftwrap count before many-conversation restart send");
            send_message_via_ui(sender_device, &message);
            wait_for_device_messages(
                sender_device,
                &sender_expected[idx],
                TEST_TIMEOUT,
                "sender self-copy before many-conversation restart",
            );
            wait_for_relay_count_at_least(
                &relay_db,
                NostrFilter::new().kind(NostrKind::GiftWrap),
                relay_giftwrap_count_before + 2,
                TEST_TIMEOUT,
                "many-conversation pre-restart giftwraps to land on the relay",
                &mut [],
            )
            .await;
            step_device_group(&mut [&mut stable_device, &mut restarting_device]);
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    wait_for_device_group_messages(
        &mut [&mut stable_device, &mut restarting_device],
        &initial_expected,
        TEST_TIMEOUT,
        "same-account devices to converge on many direct conversations before restart",
    );

    shutdown_messages_device(
        restarting_device,
        "many-conversation restart device shutdown before reopen",
    );

    let mut expected = initial_expected.clone();
    for (idx, sender_device) in sender_devices.iter_mut().enumerate() {
        let sender_name = format!("restart-sender-{}", idx + 1);
        let batch = build_direct_message_batch(&sender_name, "recipient-later", 2);

        for message in batch {
            expected.insert(message.clone());
            sender_expected[idx].insert(message.clone());
            let relay_giftwrap_count_before = relay_db
                .count(vec![NostrFilter::new().kind(NostrKind::GiftWrap)])
                .await
                .expect("query relay giftwrap count during many-conversation restart gap");
            send_message_via_ui(sender_device, &message);
            wait_for_device_messages(
                sender_device,
                &sender_expected[idx],
                TEST_TIMEOUT,
                "sender self-copy during many-conversation restart gap",
            );
            wait_for_relay_count_at_least(
                &relay_db,
                NostrFilter::new().kind(NostrKind::GiftWrap),
                relay_giftwrap_count_before + 2,
                TEST_TIMEOUT,
                "many-conversation restart gap giftwraps to land on the relay",
                &mut [],
            )
            .await;
            stable_device.step();
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    wait_for_device_messages(
        &mut stable_device,
        &expected,
        TEST_TIMEOUT,
        "stable same-account device after restart gap across many conversations",
    );

    let mut restarted_device =
        build_messages_device_in_path_with_relays(&[&relay_url], &recipient, &restart_path);
    wait_for_device_messages(
        &mut restarted_device,
        &expected,
        TEST_TIMEOUT,
        "restarted same-account device to recover many missed conversations",
    );

    assert_eq!(local_chat_messages(&mut stable_device), expected);
    assert_eq!(local_chat_messages(&mut restarted_device), expected);

    relay.shutdown();
}

/// Verifies interleaved sends across many conversations still converge on every same-account device.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_converge_during_interleaved_multi_conversation_sends_e2e() {
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
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");
    let sender_accounts = vec![
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
    ];
    let mut sender_devices: Vec<_> = sender_accounts
        .iter()
        .map(|sender| build_messages_device(&relay_url, sender))
        .collect();
    let mut recipient_devices = vec![
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
    ];

    for (sender_device, sender_account) in sender_devices.iter_mut().zip(&sender_accounts) {
        seed_local_dm_relay_list(sender_device, sender_account, &relay_url);
        seed_local_dm_relay_list(sender_device, &recipient, &relay_url);
    }
    for recipient_device in &mut recipient_devices {
        seed_local_dm_relay_list(recipient_device, &recipient, &relay_url);
    }

    step_devices(&mut sender_devices);
    step_devices(&mut recipient_devices);
    std::thread::sleep(Duration::from_millis(100));
    step_devices(&mut sender_devices);
    step_devices(&mut recipient_devices);

    for sender_device in &mut sender_devices {
        open_conversation_via_ui(sender_device, &recipient_npub);
    }

    let mut expected = BTreeSet::new();
    for round in 1..=4 {
        for (idx, sender_device) in sender_devices.iter_mut().enumerate() {
            let message = format!("interleave-s{}:{round:02}", idx + 1);
            expected.insert(message.clone());
            send_message_via_ui(sender_device, &message);
            step_device_frames(sender_device, 3);
            wait_for_relay_giftwrap_count(
                &relay_db,
                expected.len() * 2,
                TEST_TIMEOUT,
                "interleaved giftwraps to land on relay",
                &mut [],
            )
            .await;
            step_devices(&mut recipient_devices);
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    wait_for_devices_messages(
        &mut recipient_devices,
        &expected,
        TEST_TIMEOUT,
        "same-account devices to converge during interleaved multi-conversation sends",
    );
    assert_devices_match_expected(
        &mut recipient_devices,
        &expected,
        "expected same-account devices to converge during interleaved conversation updates",
    );

    relay.shutdown();
}

/// Verifies startup can merge backfill from old conversations with live delivery from new conversations.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_merge_startup_backfill_with_live_multi_conversation_delivery_e2e() {
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
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");
    let history_senders = vec![FullKeypair::generate(), FullKeypair::generate()];
    let live_senders = vec![FullKeypair::generate(), FullKeypair::generate()];
    let mut history_sender_devices: Vec<_> = history_senders
        .iter()
        .map(|sender| build_messages_device(&relay_url, sender))
        .collect();

    for (sender_device, sender_account) in history_sender_devices.iter_mut().zip(&history_senders) {
        seed_local_dm_relay_list(sender_device, sender_account, &relay_url);
        seed_local_dm_relay_list(sender_device, &recipient, &relay_url);
    }

    step_devices(&mut history_sender_devices);
    std::thread::sleep(Duration::from_millis(100));
    step_devices(&mut history_sender_devices);

    for sender_device in &mut history_sender_devices {
        open_conversation_via_ui(sender_device, &recipient_npub);
    }

    let mut expected = BTreeSet::new();
    for (idx, sender_device) in history_sender_devices.iter_mut().enumerate() {
        for message in build_direct_message_batch(&format!("startup-history-{}", idx + 1), "dm", 2)
        {
            expected.insert(message.clone());
            send_message_via_ui(sender_device, &message);
            sender_device.step();
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    for sender_device in &mut history_sender_devices {
        step_device_frames(sender_device, 3);
    }

    {
        let mut history_sender_refs: Vec<&mut DeviceHarness> =
            history_sender_devices.iter_mut().collect();
        wait_for_relay_count_at_least(
            &relay_db,
            NostrFilter::new().kind(NostrKind::GiftWrap),
            expected.len(),
            TEST_TIMEOUT,
            "startup-history giftwraps to land on relay",
            history_sender_refs.as_mut_slice(),
        )
        .await;
    }

    std::thread::sleep(Duration::from_millis(150));

    let mut live_sender_devices: Vec<_> = live_senders
        .iter()
        .map(|sender| build_messages_device(&relay_url, sender))
        .collect();
    for (sender_device, sender_account) in live_sender_devices.iter_mut().zip(&live_senders) {
        seed_local_dm_relay_list(sender_device, sender_account, &relay_url);
        seed_local_dm_relay_list(sender_device, &recipient, &relay_url);
    }

    step_devices(&mut live_sender_devices);
    std::thread::sleep(Duration::from_millis(100));
    step_devices(&mut live_sender_devices);

    for sender_device in &mut live_sender_devices {
        open_conversation_via_ui(sender_device, &recipient_npub);
        step_device_frames(sender_device, 3);
    }

    let mut recipient_devices = vec![
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
    ];
    for recipient_device in &mut recipient_devices {
        seed_local_dm_relay_list(recipient_device, &recipient, &relay_url);
    }

    step_devices(&mut recipient_devices);
    std::thread::sleep(Duration::from_millis(100));
    step_devices(&mut recipient_devices);

    for (idx, sender_device) in live_sender_devices.iter_mut().enumerate() {
        for message in build_direct_message_batch(&format!("startup-live-{}", idx + 1), "dm", 2) {
            expected.insert(message.clone());
            send_message_via_ui(sender_device, &message);
            step_device_frames(sender_device, 2);
            step_devices(&mut recipient_devices);
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    {
        let mut live_sender_refs: Vec<&mut DeviceHarness> =
            live_sender_devices.iter_mut().collect();
        wait_for_relay_count_at_least(
            &relay_db,
            NostrFilter::new().kind(NostrKind::GiftWrap),
            expected.len(),
            TEST_TIMEOUT,
            "startup-live giftwraps to land on relay",
            live_sender_refs.as_mut_slice(),
        )
        .await;
    }

    {
        let mut recipient_refs: Vec<&mut DeviceHarness> = recipient_devices.iter_mut().collect();
        let mut live_sender_refs: Vec<&mut DeviceHarness> =
            live_sender_devices.iter_mut().collect();
        wait_for_device_group_messages_while_flushing(
            recipient_refs.as_mut_slice(),
            &expected,
            TEST_TIMEOUT,
            "same-account devices to merge startup backfill with live multi-conversation delivery",
            live_sender_refs.as_mut_slice(),
        );
    }

    assert_devices_match_expected(
        &mut recipient_devices,
        &expected,
        "expected startup sync to combine historical and live multi-conversation delivery",
    );

    relay.shutdown();
}

/// Verifies high-volume fan-in across many conversations still converges on every same-account device.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_handle_high_volume_multi_conversation_fan_in_e2e() {
    init_tracing();

    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });
    let relay = LocalRelay::run(
        RelayBuilder::default()
            .database(relay_db.clone())
            .rate_limit(RateLimit {
                max_reqs: 20,
                notes_per_minute: 4_000,
            }),
    )
    .await
    .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let recipient = FullKeypair::generate();
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");
    let sender_accounts = vec![
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
    ];
    let mut sender_devices: Vec<_> = sender_accounts
        .iter()
        .map(|sender| build_messages_device(&relay_url, sender))
        .collect();
    let mut recipient_devices = vec![
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
    ];

    for (sender_device, sender_account) in sender_devices.iter_mut().zip(&sender_accounts) {
        seed_local_dm_relay_list(sender_device, sender_account, &relay_url);
        seed_local_dm_relay_list(sender_device, &recipient, &relay_url);
    }
    for recipient_device in &mut recipient_devices {
        seed_local_dm_relay_list(recipient_device, &recipient, &relay_url);
    }

    step_devices(&mut sender_devices);
    step_devices(&mut recipient_devices);
    std::thread::sleep(Duration::from_millis(100));
    step_devices(&mut sender_devices);
    step_devices(&mut recipient_devices);

    for sender_device in &mut sender_devices {
        open_conversation_via_ui(sender_device, &recipient_npub);
    }

    const MESSAGES_PER_SENDER: usize = 8;
    let batches: Vec<Vec<String>> = (0..sender_devices.len())
        .map(|idx| {
            build_direct_message_batch(
                &format!("fanin-{}", idx + 1),
                "recipient",
                MESSAGES_PER_SENDER,
            )
        })
        .collect();

    let mut expected = BTreeSet::new();
    for round in 0..MESSAGES_PER_SENDER {
        for (sender_device, batch) in sender_devices.iter_mut().zip(&batches) {
            let message = batch[round].clone();
            expected.insert(message.clone());
            send_message_via_ui(sender_device, &message);
            step_device_frames(sender_device, 3);
            wait_for_relay_giftwrap_count(
                &relay_db,
                expected.len() * 2,
                TEST_TIMEOUT,
                "high-volume giftwraps to land on relay",
                &mut [],
            )
            .await;
            step_devices(&mut recipient_devices);
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    wait_for_devices_messages(
        &mut recipient_devices,
        &expected,
        TEST_TIMEOUT,
        "same-account devices to converge under high-volume multi-conversation fan-in",
    );
    assert_devices_match_expected(
        &mut recipient_devices,
        &expected,
        "expected same-account devices to converge under multi-conversation fan-in load",
    );

    relay.shutdown();
}

/// Verifies mixed explicit and fallback relay routing still delivers across many conversations.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_handle_partial_participant_relay_knowledge_across_conversations_e2e()
{
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
    let recipient_a = FullKeypair::generate();
    let recipient_b = FullKeypair::generate();
    let recipient_fallback = FullKeypair::generate();

    let mut sender_primary =
        build_messages_device_with_relays(&[&relay_a_url, &relay_b_url], &sender);
    let mut sender_peer = build_messages_device_with_relays(&[&relay_a_url, &relay_b_url], &sender);
    let mut recipient_device_a = build_messages_device(&relay_a_url, &recipient_a);
    let mut recipient_device_b = build_messages_device(&relay_b_url, &recipient_b);
    let mut recipient_device_fallback =
        build_messages_device_with_relays(&[&relay_a_url, &relay_b_url], &recipient_fallback);

    for sender_device in [&mut sender_primary, &mut sender_peer] {
        seed_local_dm_relay_list_with_relays(
            sender_device,
            &sender,
            &[&relay_a_url, &relay_b_url],
            None,
        );
        seed_local_dm_relay_list_with_relays(sender_device, &recipient_a, &[&relay_a_url], None);
        seed_local_dm_relay_list_with_relays(sender_device, &recipient_b, &[&relay_b_url], None);
        seed_local_profile_metadata(sender_device, &recipient_a, "recipient-a");
        seed_local_profile_metadata(sender_device, &recipient_b, "recipient-b");
        seed_local_profile_metadata(sender_device, &recipient_fallback, "recipient-fallback");
    }
    seed_local_dm_relay_list(&mut recipient_device_a, &recipient_a, &relay_a_url);
    seed_local_dm_relay_list(&mut recipient_device_b, &recipient_b, &relay_b_url);
    seed_local_dm_relay_list_with_relays(
        &mut recipient_device_fallback,
        &recipient_fallback,
        &[&relay_a_url, &relay_b_url],
        None,
    );

    step_device_group(&mut [
        &mut sender_primary,
        &mut sender_peer,
        &mut recipient_device_a,
        &mut recipient_device_b,
        &mut recipient_device_fallback,
    ]);
    std::thread::sleep(Duration::from_millis(100));
    step_device_group(&mut [
        &mut sender_primary,
        &mut sender_peer,
        &mut recipient_device_a,
        &mut recipient_device_b,
        &mut recipient_device_fallback,
    ]);

    send_direct_message(
        &mut sender_primary,
        &recipient_a.pubkey.npub().expect("recipient a npub"),
        "explicit-relay-a",
    );
    wait_for_convergence(
        &mut sender_primary,
        std::slice::from_mut(&mut recipient_device_a),
        &BTreeSet::from(["explicit-relay-a".to_owned()]),
        TEST_TIMEOUT,
    );

    send_direct_message(
        &mut sender_peer,
        &recipient_b.pubkey.npub().expect("recipient b npub"),
        "explicit-relay-b",
    );
    wait_for_convergence(
        &mut sender_peer,
        std::slice::from_mut(&mut recipient_device_b),
        &BTreeSet::from(["explicit-relay-b".to_owned()]),
        TEST_TIMEOUT,
    );

    send_direct_message(
        &mut sender_primary,
        &recipient_fallback
            .pubkey
            .npub()
            .expect("recipient fallback npub"),
        "fallback-accounts-write",
    );
    wait_for_convergence(
        &mut sender_primary,
        std::slice::from_mut(&mut recipient_device_fallback),
        &BTreeSet::from(["fallback-accounts-write".to_owned()]),
        TEST_TIMEOUT,
    );

    let expected_sender_view = BTreeSet::from([
        "explicit-relay-a".to_owned(),
        "explicit-relay-b".to_owned(),
        "fallback-accounts-write".to_owned(),
    ]);
    wait_for_device_group_messages(
        &mut [&mut sender_primary, &mut sender_peer],
        &expected_sender_view,
        TEST_TIMEOUT,
        "same-account sender devices to converge across mixed routing conversations",
    );

    relay_a.shutdown();
    relay_b.shutdown();
}

/// Verifies a restarted same-account device can recover while new conversations continue to update.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_device_restart_recovers_during_active_multi_conversation_delivery_e2e() {
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
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");
    let sender_accounts = vec![
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
    ];
    let mut sender_devices: Vec<_> = sender_accounts
        .iter()
        .map(|sender| build_messages_device(&relay_url, sender))
        .collect();
    let mut stable_device = build_messages_device(&relay_url, &recipient);
    let restart_dir = TempDir::new().expect("restart dir");
    let restart_path = restart_dir.path().to_path_buf();
    let mut restarting_device =
        build_messages_device_in_path_with_relays(&[&relay_url], &recipient, &restart_path);

    for (sender_device, sender_account) in sender_devices.iter_mut().zip(&sender_accounts) {
        seed_local_dm_relay_list(sender_device, sender_account, &relay_url);
        seed_local_dm_relay_list(sender_device, &recipient, &relay_url);
    }
    seed_local_dm_relay_list(&mut stable_device, &recipient, &relay_url);
    seed_local_dm_relay_list(&mut restarting_device, &recipient, &relay_url);

    step_devices(&mut sender_devices);
    step_device_group(&mut [&mut stable_device, &mut restarting_device]);
    std::thread::sleep(Duration::from_millis(100));
    step_devices(&mut sender_devices);
    step_device_group(&mut [&mut stable_device, &mut restarting_device]);

    for sender_device in &mut sender_devices {
        open_conversation_via_ui(sender_device, &recipient_npub);
    }

    assert_eq!(
        local_dm_relay_list_relays(&mut sender_devices[2], &recipient),
        vec![format!("{relay_url}/")],
        "expected third sender to target the recipient through the active relay before the pre-restart send",
    );

    let mut expected = BTreeSet::new();
    for (idx, sender_device) in sender_devices.iter_mut().enumerate() {
        let message = format!("restart-active-s{}:pre", idx + 1);
        expected.insert(message.clone());
        let relay_giftwrap_count_before = relay_db
            .count(vec![NostrFilter::new().kind(NostrKind::GiftWrap)])
            .await
            .expect("query relay giftwrap count before pre-restart send");
        send_message_via_ui(sender_device, &message);
        sender_device.step();
        if idx == 2 {
            wait_for_device_messages(
                sender_device,
                &BTreeSet::from([message.clone()]),
                TEST_TIMEOUT,
                "third sender to persist its own pre-restart self-copy",
            );
        }
        wait_for_relay_count_at_least(
            &relay_db,
            NostrFilter::new().kind(NostrKind::GiftWrap),
            relay_giftwrap_count_before + 2,
            TEST_TIMEOUT,
            "pre-restart giftwraps to land on the relay",
            &mut [],
        )
        .await;
        step_device_group(&mut [&mut stable_device, &mut restarting_device]);
        std::thread::sleep(Duration::from_millis(25));
    }

    wait_for_device_group_messages(
        &mut [&mut stable_device, &mut restarting_device],
        &expected,
        TEST_TIMEOUT,
        "same-account devices to converge before restarting during active delivery",
    );

    shutdown_messages_device(
        restarting_device,
        "active-delivery restart device shutdown before reopen",
    );

    for (idx, sender_device) in sender_devices.iter_mut().enumerate() {
        let message = format!("restart-active-s{}:mid", idx + 1);
        expected.insert(message.clone());
        send_message_via_ui(sender_device, &message);
        sender_device.step();
        stable_device.step();
        std::thread::sleep(Duration::from_millis(25));
    }

    let mut restarted_device =
        build_messages_device_in_path_with_relays(&[&relay_url], &recipient, &restart_path);

    for (idx, sender_device) in sender_devices.iter_mut().enumerate() {
        let message = format!("restart-active-s{}:post", idx + 1);
        expected.insert(message.clone());
        send_message_via_ui(sender_device, &message);
        sender_device.step();
        step_device_group(&mut [&mut stable_device, &mut restarted_device]);
        std::thread::sleep(Duration::from_millis(25));
    }

    wait_for_device_group_messages(
        &mut [&mut stable_device, &mut restarted_device],
        &expected,
        TEST_TIMEOUT,
        "same-account restarted device to recover while multi-conversation delivery continues",
    );

    assert_eq!(local_chat_messages(&mut stable_device), expected);
    assert_eq!(local_chat_messages(&mut restarted_device), expected);

    relay.shutdown();
}
