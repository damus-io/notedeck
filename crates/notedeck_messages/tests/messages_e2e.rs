//! Full-app end-to-end scenarios for Messages using a real local relay and full Notedeck hosts.

mod harness;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use enostr::FullKeypair;
use harness::fixtures::{
    build_backdated_giftwrap_note, build_invalid_giftwrap_note, build_local_chat_note_jsons,
    build_misdirected_giftwrap_note, local_chat_messages_in_data_dir, local_dm_relay_list_relays,
    local_dm_relay_list_versions, local_giftwrap_created_ats_in_data_dir, nostr_pubkey,
    seed_cluster_dm_relay_list, seed_cluster_known_profiles, seed_local_dm_relay_list,
    seed_local_dm_relay_list_ndb_only_with_relays, seed_local_dm_relay_list_with_relays,
    seed_local_giftwraps_in_data_dir, seed_local_notes_in_data_dir, seed_local_profile_metadata,
};
use harness::ui::{
    build_direct_message_batch, open_conversation_via_ui, send_direct_message, send_message_via_ui,
};
use harness::{
    add_account_to_device, assert_cluster_matches_expected, assert_devices_match_expected,
    build_messages_cluster, build_messages_device, build_messages_device_in_path_with_relays,
    build_messages_device_in_tmpdir, build_messages_device_with_relays, cluster_actual_sets,
    cluster_converged_on, init_tracing, local_chat_message_count, local_chat_messages,
    publish_note_via_device, select_account_on_device, step_clusters, step_device_frames,
    step_device_group, step_devices, wait_for_cluster_convergence, wait_for_convergence,
    wait_for_device_group_messages, wait_for_device_group_messages_while_flushing,
    wait_for_device_messages, wait_for_device_messages_while_flushing, wait_for_devices_messages,
    wait_for_messages_device_shutdown, warm_up_clusters, DeviceHarness, TEST_TIMEOUT,
};
use nostr::{nips::nip17, Event, Filter as NostrFilter, JsonUtil, Kind as NostrKind};
use nostr_relay_builder::{
    builder::RateLimit,
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use notedeck::unix_time_secs;
use notedeck_messages::nip17::default_dm_relay_urls;
use tempfile::TempDir;

/// Waits until the relay database stores the expected number of giftwrap events.
///
/// Any `senders` are stepped each iteration so their outbox continues
/// flushing events to the relay while we poll.
async fn wait_for_relay_giftwrap_count(
    relay_db: &MemoryDatabase,
    expected_count: usize,
    timeout: Duration,
    context: &str,
    senders: &mut [&mut DeviceHarness],
) {
    let deadline = Instant::now() + timeout;
    let filter = NostrFilter::new().kind(NostrKind::GiftWrap);

    loop {
        for sender in senders.iter_mut() {
            sender.step();
        }

        let actual = relay_db
            .count(vec![filter.clone()])
            .await
            .expect("query relay giftwrap count");
        if actual == expected_count {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; expected relay giftwrap count {}, actual {}",
            expected_count,
            actual
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until the relay database stores at least the expected number of events matching `filter`.
///
/// Any `senders` are stepped each iteration so their outbox continues
/// flushing events to the relay while we poll.
async fn wait_for_relay_count_at_least(
    relay_db: &MemoryDatabase,
    filter: NostrFilter,
    expected_min_count: usize,
    timeout: Duration,
    context: &str,
    senders: &mut [&mut DeviceHarness],
) {
    let deadline = Instant::now() + timeout;

    loop {
        for sender in senders.iter_mut() {
            sender.step();
        }

        let actual = relay_db
            .count(vec![filter.clone()])
            .await
            .expect("query relay giftwrap count");
        if actual >= expected_min_count {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; expected relay giftwrap count at least {}, actual {}",
            expected_min_count,
            actual
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Returns the latest remote DM relay-list relay URLs stored for one account.
async fn relay_dm_relay_list_relays(
    relay_db: &MemoryDatabase,
    account: &FullKeypair,
) -> Vec<String> {
    let filter = NostrFilter::new()
        .authors([nostr_pubkey(&account.pubkey)])
        .kind(NostrKind::Custom(10050))
        .limit(1);
    let events = relay_db
        .query(vec![filter])
        .await
        .expect("query relay dm relay list");

    let Some(event) = events.first() else {
        return Vec::new();
    };

    nip17::extract_relay_list(event)
        .map(|relay| relay.to_string().trim_end_matches('/').to_owned())
        .collect()
}

/// Verifies that multiple devices on the same account converge on messages sent from another user.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_converge_on_sent_messages_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let mut sender_device = build_messages_device(&relay_url, &sender);
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");

    let mut devices = vec![
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
    ];

    seed_local_dm_relay_list(&mut sender_device, &sender, &relay_url);
    for device in &mut devices {
        seed_local_dm_relay_list(device, &recipient, &relay_url);
    }

    sender_device.step();
    step_devices(&mut devices);
    std::thread::sleep(Duration::from_millis(100));
    sender_device.step();
    step_devices(&mut devices);

    open_conversation_via_ui(&mut sender_device, &recipient_npub);

    let expected = BTreeSet::from([
        "msg-01".to_owned(),
        "msg-02".to_owned(),
        "msg-03".to_owned(),
        "msg-04".to_owned(),
        "msg-05".to_owned(),
        "msg-06".to_owned(),
    ]);
    for message in &expected {
        send_message_via_ui(&mut sender_device, message);
        sender_device.step();
        std::thread::sleep(Duration::from_millis(25));
    }

    wait_for_convergence(&mut sender_device, &mut devices, &expected, TEST_TIMEOUT);

    assert_devices_match_expected(&mut devices, &expected, "expected all devices to converge");

    relay.shutdown();
}

/// Verifies that a cold-start account backfills giftwrapped DMs already present on the relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_backfill_preexisting_giftwraps_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");

    let mut sender_device = build_messages_device(&relay_url, &sender);
    seed_local_dm_relay_list(&mut sender_device, &sender, &relay_url);
    seed_local_dm_relay_list(&mut sender_device, &recipient, &relay_url);

    sender_device.step();
    std::thread::sleep(Duration::from_millis(100));
    sender_device.step();

    open_conversation_via_ui(&mut sender_device, &recipient_npub);

    let expected = BTreeSet::from([
        "history-01".to_owned(),
        "history-02".to_owned(),
        "history-03".to_owned(),
        "history-04".to_owned(),
        "history-05".to_owned(),
        "history-06".to_owned(),
    ]);
    for message in &expected {
        send_message_via_ui(&mut sender_device, message);
        sender_device.step();
        std::thread::sleep(Duration::from_millis(25));
    }

    step_device_frames(&mut sender_device, 4);
    std::thread::sleep(Duration::from_millis(150));

    let mut devices = vec![
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
    ];

    wait_for_devices_messages(&mut devices, &expected, TEST_TIMEOUT, "cold-start backfill");
    assert_devices_match_expected(&mut devices, &expected, "expected all devices to backfill");

    relay.shutdown();
}

/// Verifies a paused same-account device can catch up from relay history after other devices stay current.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_catch_up_after_one_device_falls_behind_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");

    let mut sender_device = build_messages_device(&relay_url, &sender);
    let mut live_device_a = build_messages_device(&relay_url, &recipient);
    let mut live_device_b = build_messages_device(&relay_url, &recipient);
    let mut lagging_device = build_messages_device(&relay_url, &recipient);

    seed_local_dm_relay_list(&mut sender_device, &sender, &relay_url);
    seed_local_dm_relay_list(&mut sender_device, &recipient, &relay_url);
    seed_local_dm_relay_list(&mut live_device_a, &recipient, &relay_url);
    seed_local_dm_relay_list(&mut live_device_b, &recipient, &relay_url);
    seed_local_dm_relay_list(&mut lagging_device, &recipient, &relay_url);

    sender_device.step();
    step_device_group(&mut [&mut live_device_a, &mut live_device_b, &mut lagging_device]);
    std::thread::sleep(Duration::from_millis(100));
    sender_device.step();
    step_device_group(&mut [&mut live_device_a, &mut live_device_b, &mut lagging_device]);

    open_conversation_via_ui(&mut sender_device, &recipient_npub);

    let expected = BTreeSet::from([
        "lag-01".to_owned(),
        "lag-02".to_owned(),
        "lag-03".to_owned(),
        "lag-04".to_owned(),
        "lag-05".to_owned(),
        "lag-06".to_owned(),
    ]);
    for message in &expected {
        send_message_via_ui(&mut sender_device, message);
        sender_device.step();
        step_device_group(&mut [&mut live_device_a, &mut live_device_b]);
        std::thread::sleep(Duration::from_millis(25));
    }

    wait_for_device_group_messages(
        &mut [&mut live_device_a, &mut live_device_b],
        &expected,
        TEST_TIMEOUT,
        "live same-account devices to stay current while one device is paused",
    );

    assert_ne!(
        local_chat_messages(&mut lagging_device),
        expected,
        "expected the paused device to lag behind before it resumes stepping"
    );

    wait_for_device_messages(
        &mut lagging_device,
        &expected,
        TEST_TIMEOUT,
        "paused same-account device to catch up from relay history",
    );

    assert_eq!(local_chat_messages(&mut live_device_a), expected);
    assert_eq!(local_chat_messages(&mut live_device_b), expected);
    assert_eq!(local_chat_messages(&mut lagging_device), expected);

    relay.shutdown();
}

/// Verifies that devices with divergent local NostrDB history reconcile to the same relay-backed set on startup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_reconcile_divergent_local_history_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");

    let mut sender_device = build_messages_device(&relay_url, &sender);
    seed_local_dm_relay_list(&mut sender_device, &sender, &relay_url);
    seed_local_dm_relay_list(&mut sender_device, &recipient, &relay_url);

    sender_device.step();
    std::thread::sleep(Duration::from_millis(100));
    sender_device.step();

    open_conversation_via_ui(&mut sender_device, &recipient_npub);

    let expected = BTreeSet::from([
        "divergent-01".to_owned(),
        "divergent-02".to_owned(),
        "divergent-03".to_owned(),
        "divergent-04".to_owned(),
        "divergent-05".to_owned(),
        "divergent-06".to_owned(),
    ]);
    for message in &expected {
        send_message_via_ui(&mut sender_device, message);
        sender_device.step();
        std::thread::sleep(Duration::from_millis(25));
    }

    wait_for_device_messages(
        &mut sender_device,
        &expected,
        TEST_TIMEOUT,
        "sender local history",
    );

    let subset_a = build_local_chat_note_jsons(
        &sender,
        &recipient,
        &["divergent-01", "divergent-03", "divergent-05"],
    );
    let subset_b =
        build_local_chat_note_jsons(&sender, &recipient, &["divergent-02", "divergent-04"]);
    let subset_c = build_local_chat_note_jsons(
        &sender,
        &recipient,
        &["divergent-01", "divergent-02", "divergent-06"],
    );

    let tmpdir_a = TempDir::new().expect("tmpdir a");
    let tmpdir_b = TempDir::new().expect("tmpdir b");
    let tmpdir_c = TempDir::new().expect("tmpdir c");

    seed_local_notes_in_data_dir(tmpdir_a.path(), &subset_a, &[14]);
    seed_local_notes_in_data_dir(tmpdir_b.path(), &subset_b, &[14]);
    seed_local_notes_in_data_dir(tmpdir_c.path(), &subset_c, &[14]);

    let before_sets = vec![
        local_chat_messages_in_data_dir(tmpdir_a.path(), &recipient),
        local_chat_messages_in_data_dir(tmpdir_b.path(), &recipient),
        local_chat_messages_in_data_dir(tmpdir_c.path(), &recipient),
    ];
    assert_eq!(
        before_sets,
        vec![
            BTreeSet::from([
                "divergent-01".to_owned(),
                "divergent-03".to_owned(),
                "divergent-05".to_owned(),
            ]),
            BTreeSet::from(["divergent-02".to_owned(), "divergent-04".to_owned()]),
            BTreeSet::from([
                "divergent-01".to_owned(),
                "divergent-02".to_owned(),
                "divergent-06".to_owned(),
            ]),
        ],
        "expected distinct local histories before startup sync"
    );

    let mut devices = vec![
        build_messages_device_in_tmpdir(&relay_url, &recipient, tmpdir_a),
        build_messages_device_in_tmpdir(&relay_url, &recipient, tmpdir_b),
        build_messages_device_in_tmpdir(&relay_url, &recipient, tmpdir_c),
    ];

    wait_for_devices_messages(
        &mut devices,
        &expected,
        TEST_TIMEOUT,
        "divergent local histories to reconcile",
    );
    assert_devices_match_expected(&mut devices, &expected, "expected all devices to reconcile");

    relay.shutdown();
}

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

    wait_for_messages_device_shutdown(
        restarting_device,
        &restart_path,
        TEST_TIMEOUT,
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

    wait_for_messages_device_shutdown(
        restarting_device,
        &restart_path,
        TEST_TIMEOUT,
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

    let relay = LocalRelay::run(RelayBuilder::default())
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

    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        step_devices(&mut live_sender_devices);
        step_devices(&mut recipient_devices);

        if recipient_devices
            .iter_mut()
            .all(|device| local_chat_messages(device) == expected)
        {
            break;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for same-account devices to merge startup backfill with live multi-conversation delivery; expected {:?}, actual {:?}",
            expected,
            recipient_devices
                .iter_mut()
                .map(local_chat_messages)
                .collect::<Vec<BTreeSet<String>>>()
        );

        std::thread::sleep(Duration::from_millis(20));
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

    wait_for_messages_device_shutdown(
        restarting_device,
        &restart_path,
        TEST_TIMEOUT,
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

/// Verifies startup backfills a relay giftwrap whose wrapper timestamp is older
/// than a wrapper already present in the local NostrDB.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_backfills_relay_giftwrap_older_than_local_wrapper_created_at_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let now = unix_time_secs();
    let newer_wrap_created_at = now.saturating_sub(60);
    let older_wrap_created_at = now.saturating_sub(2 * 24 * 60 * 60);

    let local_newer = build_backdated_giftwrap_note(
        &sender,
        &recipient,
        "local-newer-wrap",
        newer_wrap_created_at,
    );
    let relay_older = build_backdated_giftwrap_note(
        &sender,
        &recipient,
        "relay-older-wrap",
        older_wrap_created_at,
    );

    assert!(
        older_wrap_created_at < newer_wrap_created_at,
        "expected relay wrapper {} to be older than local wrapper {}",
        older_wrap_created_at,
        newer_wrap_created_at
    );

    let tmpdir = TempDir::new().expect("tmpdir");
    seed_local_giftwraps_in_data_dir(
        tmpdir.path(),
        &recipient,
        &[local_newer.json().expect("local newer giftwrap json")],
    );
    assert_eq!(
        local_giftwrap_created_ats_in_data_dir(tmpdir.path(), &recipient),
        vec![newer_wrap_created_at],
        "expected one newer local wrapper before startup"
    );

    let mut sender_device = build_messages_device(&relay_url, &sender);
    publish_note_via_device(&mut sender_device, &local_newer);
    publish_note_via_device(&mut sender_device, &relay_older);

    step_device_frames(&mut sender_device, 3);
    std::thread::sleep(Duration::from_millis(150));

    let mut recipient_device = build_messages_device_in_tmpdir(&relay_url, &recipient, tmpdir);
    let expected = BTreeSet::from(["local-newer-wrap".to_owned(), "relay-older-wrap".to_owned()]);

    wait_for_device_messages(
        &mut recipient_device,
        &expected,
        TEST_TIMEOUT,
        "backdated giftwrap backfill",
    );

    assert_eq!(
        local_chat_messages(&mut recipient_device),
        expected,
        "expected startup sync to keep the newer local giftwrap and backfill the older relay wrapper"
    );

    relay.shutdown();
}

/// Verifies cold-start history sync unions pre-existing giftwraps spread across multiple relays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_backfill_split_history_across_relays_e2e() {
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

    let mut sender_a = build_messages_device(&relay_a_url, &sender);
    let mut sender_b = build_messages_device(&relay_b_url, &sender);
    seed_local_dm_relay_list(&mut sender_a, &sender, &relay_a_url);
    seed_local_dm_relay_list(&mut sender_b, &sender, &relay_b_url);
    seed_local_dm_relay_list(&mut sender_a, &recipient, &relay_a_url);
    seed_local_dm_relay_list(&mut sender_b, &recipient, &relay_b_url);

    step_device_frames(&mut sender_a, 2);
    step_device_frames(&mut sender_b, 2);

    open_conversation_via_ui(&mut sender_a, &recipient_npub);
    open_conversation_via_ui(&mut sender_b, &recipient_npub);

    for message in ["relay-a-01", "relay-a-02", "relay-a-03"] {
        send_message_via_ui(&mut sender_a, message);
    }
    for message in ["relay-b-01", "relay-b-02", "relay-b-03"] {
        send_message_via_ui(&mut sender_b, message);
    }

    step_device_frames(&mut sender_a, 3);
    step_device_frames(&mut sender_b, 3);
    std::thread::sleep(Duration::from_millis(150));

    let mut recipient_device =
        build_messages_device_with_relays(&[&relay_a_url, &relay_b_url], &recipient);
    let expected = BTreeSet::from([
        "relay-a-01".to_owned(),
        "relay-a-02".to_owned(),
        "relay-a-03".to_owned(),
        "relay-b-01".to_owned(),
        "relay-b-02".to_owned(),
        "relay-b-03".to_owned(),
    ]);

    wait_for_device_messages(
        &mut recipient_device,
        &expected,
        TEST_TIMEOUT,
        "multi-relay cold-start backfill",
    );
    assert_eq!(local_chat_messages(&mut recipient_device), expected);

    relay_a.shutdown();
    relay_b.shutdown();
}

/// Verifies same-account devices recover to the union of history after startup with expanded relay visibility.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_converge_after_expanded_relay_visibility_startup_e2e() {
    init_tracing();

    let relay_a_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });
    let relay_b_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });
    let relay_a = LocalRelay::run(RelayBuilder::default().database(relay_a_db.clone()))
        .await
        .expect("start relay a");
    let relay_b = LocalRelay::run(RelayBuilder::default().database(relay_b_db.clone()))
        .await
        .expect("start relay b");
    let relay_a_url = relay_a.url().to_owned();
    let relay_b_url = relay_b.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let expected_a = BTreeSet::from(["relay-a-only-01".to_owned(), "relay-a-only-02".to_owned()]);
    let expected_b = BTreeSet::from(["relay-b-only-01".to_owned(), "relay-b-only-02".to_owned()]);
    let expected_union = BTreeSet::from([
        "relay-a-only-01".to_owned(),
        "relay-a-only-02".to_owned(),
        "relay-b-only-01".to_owned(),
        "relay-b-only-02".to_owned(),
    ]);

    for (idx, message) in expected_a.iter().enumerate() {
        let wrap = build_backdated_giftwrap_note(
            &sender,
            &recipient,
            message,
            unix_time_secs() + idx as u64,
        );
        let event =
            Event::from_json(wrap.json().expect("relay a wrap json")).expect("relay a wrap event");
        relay_a_db
            .save_event(&event)
            .await
            .expect("seed relay a visibility history");
    }
    for (idx, message) in expected_b.iter().enumerate() {
        let wrap = build_backdated_giftwrap_note(
            &sender,
            &recipient,
            message,
            unix_time_secs() + expected_a.len() as u64 + idx as u64,
        );
        let event =
            Event::from_json(wrap.json().expect("relay b wrap json")).expect("relay b wrap event");
        relay_b_db
            .save_event(&event)
            .await
            .expect("seed relay b visibility history");
    }

    let pre_recovery_a_dir = TempDir::new().expect("pre-recovery a dir");
    let pre_recovery_b_dir = TempDir::new().expect("pre-recovery b dir");
    let recovered_a_dir = TempDir::new().expect("recovered a dir");
    let recovered_b_dir = TempDir::new().expect("recovered b dir");

    let local_a =
        build_local_chat_note_jsons(&sender, &recipient, &["relay-a-only-01", "relay-a-only-02"]);
    let local_b =
        build_local_chat_note_jsons(&sender, &recipient, &["relay-b-only-01", "relay-b-only-02"]);
    for dir in [pre_recovery_a_dir.path(), recovered_a_dir.path()] {
        seed_local_notes_in_data_dir(dir, &local_a, &[14]);
    }
    for dir in [pre_recovery_b_dir.path(), recovered_b_dir.path()] {
        seed_local_notes_in_data_dir(dir, &local_b, &[14]);
    }

    let mut recipient_a = build_messages_device_in_path_with_relays(
        &[&relay_a_url],
        &recipient,
        pre_recovery_a_dir.path(),
    );
    let mut recipient_b = build_messages_device_in_path_with_relays(
        &[&relay_b_url],
        &recipient,
        pre_recovery_b_dir.path(),
    );
    wait_for_device_messages(
        &mut recipient_a,
        &expected_a,
        TEST_TIMEOUT,
        "recipient device with relay a visibility only",
    );
    wait_for_device_messages(
        &mut recipient_b,
        &expected_b,
        TEST_TIMEOUT,
        "recipient device with relay b visibility only",
    );

    wait_for_messages_device_shutdown(
        recipient_a,
        pre_recovery_a_dir.path(),
        TEST_TIMEOUT,
        "drop relay-a-only pre-recovery device before expanded-visibility startup",
    );
    wait_for_messages_device_shutdown(
        recipient_b,
        pre_recovery_b_dir.path(),
        TEST_TIMEOUT,
        "drop relay-b-only pre-recovery device before expanded-visibility startup",
    );

    let mut recovered_a = build_messages_device_in_path_with_relays(
        &[&relay_a_url, &relay_b_url],
        &recipient,
        recovered_a_dir.path(),
    );
    let mut recovered_b = build_messages_device_in_path_with_relays(
        &[&relay_a_url, &relay_b_url],
        &recipient,
        recovered_b_dir.path(),
    );

    wait_for_device_group_messages(
        &mut [&mut recovered_a, &mut recovered_b],
        &expected_union,
        TEST_TIMEOUT,
        "same-account devices to converge after relay visibility recovers",
    );

    assert_eq!(local_chat_messages(&mut recovered_a), expected_union);
    assert_eq!(local_chat_messages(&mut recovered_b), expected_union);

    relay_a.shutdown();
    relay_b.shutdown();
}

/// Verifies the same giftwrap seen on multiple relays only produces one local chat note.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_giftwrap_across_relays_is_deduped_e2e() {
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
    let wrap = build_backdated_giftwrap_note(&sender, &recipient, "dedupe-me", unix_time_secs());

    let mut publisher = build_messages_device_with_relays(&[&relay_a_url, &relay_b_url], &sender);
    publish_note_via_device(&mut publisher, &wrap);

    let mut recipient_device =
        build_messages_device_with_relays(&[&relay_a_url, &relay_b_url], &recipient);
    let expected = BTreeSet::from(["dedupe-me".to_owned()]);
    wait_for_device_messages_while_flushing(
        &mut recipient_device,
        &expected,
        TEST_TIMEOUT,
        "duplicate giftwrap dedupe",
        &mut [&mut publisher],
    );
    assert_eq!(
        local_chat_message_count(&mut recipient_device),
        1,
        "expected duplicate relay delivery to produce one local kind 14 note"
    );

    relay_a.shutdown();
    relay_b.shutdown();
}

/// Verifies one malformed giftwrap does not block neighboring valid history from being processed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_giftwrap_does_not_block_valid_history_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let now = unix_time_secs();
    let valid = build_backdated_giftwrap_note(&sender, &recipient, "valid-neighbor", now);
    let invalid = build_invalid_giftwrap_note(&recipient, now.saturating_sub(1));

    let mut publisher = build_messages_device(&relay_url, &sender);
    publish_note_via_device(&mut publisher, &invalid);
    publish_note_via_device(&mut publisher, &valid);

    let mut recipient_device = build_messages_device(&relay_url, &recipient);
    let expected = BTreeSet::from(["valid-neighbor".to_owned()]);
    wait_for_device_messages_while_flushing(
        &mut recipient_device,
        &expected,
        TEST_TIMEOUT,
        "valid history after malformed giftwrap",
        &mut [&mut publisher],
    );
    assert_eq!(
        local_chat_message_count(&mut recipient_device),
        1,
        "expected malformed giftwrap to be ignored without duplicating or blocking valid notes"
    );

    relay.shutdown();
}

/// Verifies multiple giftwrap failure modes do not block neighboring valid history from processing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_giftwrap_failures_do_not_block_valid_history_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let bystander = FullKeypair::generate();
    let now = unix_time_secs();
    let malformed = build_invalid_giftwrap_note(&recipient, now.saturating_sub(3));
    let misdirected = build_misdirected_giftwrap_note(
        &sender,
        &recipient,
        &bystander,
        "bad-misdirected-wrap",
        now.saturating_sub(2),
    );
    let valid_a =
        build_backdated_giftwrap_note(&sender, &recipient, "valid-batch-01", now.saturating_sub(1));
    let valid_b = build_backdated_giftwrap_note(&sender, &recipient, "valid-batch-02", now);

    let mut publisher = build_messages_device(&relay_url, &sender);
    publish_note_via_device(&mut publisher, &malformed);
    publish_note_via_device(&mut publisher, &valid_a);
    publish_note_via_device(&mut publisher, &misdirected);
    publish_note_via_device(&mut publisher, &valid_b);

    let mut recipient_device = build_messages_device(&relay_url, &recipient);
    let expected = BTreeSet::from(["valid-batch-01".to_owned(), "valid-batch-02".to_owned()]);
    wait_for_device_messages_while_flushing(
        &mut recipient_device,
        &expected,
        TEST_TIMEOUT,
        "mixed malformed and misdirected giftwrap history",
        &mut [&mut publisher],
    );
    assert_eq!(
        local_chat_messages(&mut recipient_device),
        expected,
        "expected malformed and misdirected giftwraps to be ignored without blocking valid history"
    );
    assert_eq!(
        local_chat_message_count(&mut recipient_device),
        2,
        "expected only the valid neighbor messages to be decrypted into local chat notes"
    );

    relay.shutdown();
}

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

/// Verifies account switching isolates local Messages state and backfills the newly selected account.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switching_accounts_isolates_messages_state_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let account_a = FullKeypair::generate();
    let account_b = FullKeypair::generate();
    let sender = FullKeypair::generate();

    let mut switching_device = build_messages_device(&relay_url, &account_a);
    add_account_to_device(&mut switching_device, &account_b);
    select_account_on_device(&mut switching_device, &account_b);
    step_device_frames(&mut switching_device, 20);
    std::thread::sleep(Duration::from_millis(200));

    let mut sender_to_a = build_messages_device(&relay_url, &sender);
    let mut sender_to_b = build_messages_device(&relay_url, &sender);
    seed_local_dm_relay_list(&mut sender_to_a, &sender, &relay_url);
    seed_local_dm_relay_list(&mut sender_to_b, &sender, &relay_url);
    seed_local_dm_relay_list(&mut sender_to_a, &account_a, &relay_url);
    seed_local_dm_relay_list(&mut sender_to_b, &account_b, &relay_url);

    open_conversation_via_ui(
        &mut sender_to_b,
        &account_b.pubkey.npub().expect("account b npub"),
    );
    send_message_via_ui(&mut sender_to_b, "for-account-b");

    wait_for_device_messages_while_flushing(
        &mut switching_device,
        &BTreeSet::from(["for-account-b".to_owned()]),
        TEST_TIMEOUT,
        "account b message visibility before switching away",
        &mut [&mut sender_to_b],
    );

    select_account_on_device(&mut switching_device, &account_b);
    select_account_on_device(&mut switching_device, &account_a);
    open_conversation_via_ui(
        &mut sender_to_a,
        &account_a.pubkey.npub().expect("account a npub"),
    );
    send_message_via_ui(&mut sender_to_a, "for-account-a");
    wait_for_device_messages_while_flushing(
        &mut switching_device,
        &BTreeSet::from(["for-account-a".to_owned()]),
        TEST_TIMEOUT,
        "account a message visibility after switching back",
        &mut [&mut sender_to_a],
    );

    select_account_on_device(&mut switching_device, &account_b);
    assert_eq!(
        local_chat_messages(&mut switching_device),
        BTreeSet::from(["for-account-b".to_owned()]),
        "expected switching back to account b to restore only account b messages"
    );

    relay.shutdown();
}

/// Verifies concurrent sends from two same-account devices converge inside one DM thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_sender_devices_converge_after_concurrent_same_thread_sends_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let sender = FullKeypair::generate();
    let recipient = FullKeypair::generate();
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");

    let mut sender_device_a = build_messages_device(&relay_url, &sender);
    let mut sender_device_b = build_messages_device(&relay_url, &sender);
    let mut recipient_devices = vec![
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
    ];

    for sender_device in [&mut sender_device_a, &mut sender_device_b] {
        seed_local_dm_relay_list(sender_device, &sender, &relay_url);
        seed_local_dm_relay_list(sender_device, &recipient, &relay_url);
    }
    for recipient_device in &mut recipient_devices {
        seed_local_dm_relay_list(recipient_device, &recipient, &relay_url);
    }

    step_device_group(&mut [&mut sender_device_a, &mut sender_device_b]);
    step_devices(&mut recipient_devices);
    std::thread::sleep(Duration::from_millis(100));
    step_device_group(&mut [&mut sender_device_a, &mut sender_device_b]);
    step_devices(&mut recipient_devices);

    open_conversation_via_ui(&mut sender_device_a, &recipient_npub);
    open_conversation_via_ui(&mut sender_device_b, &recipient_npub);

    let batch_a = build_direct_message_batch("same-account-a", "thread", 4);
    let batch_b = build_direct_message_batch("same-account-b", "thread", 4);
    let mut expected = BTreeSet::new();

    for round in 0..4 {
        let message_a = batch_a[round].clone();
        let message_b = batch_b[round].clone();
        expected.insert(message_a.clone());
        expected.insert(message_b.clone());

        send_message_via_ui(&mut sender_device_a, &message_a);
        send_message_via_ui(&mut sender_device_b, &message_b);
        step_device_group(&mut [&mut sender_device_a, &mut sender_device_b]);
        step_devices(&mut recipient_devices);
        std::thread::sleep(Duration::from_millis(20));
    }

    wait_for_device_group_messages(
        &mut [&mut sender_device_a, &mut sender_device_b],
        &expected,
        TEST_TIMEOUT,
        "same-account sender devices to converge after concurrent same-thread sends",
    );
    wait_for_devices_messages(
        &mut recipient_devices,
        &expected,
        TEST_TIMEOUT,
        "recipient devices to converge after same-account concurrent sends",
    );

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
    wait_for_messages_device_shutdown(
        recipient_device,
        &churn_path,
        TEST_TIMEOUT,
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

    wait_for_messages_device_shutdown(
        sender_offline,
        &offline_path,
        TEST_TIMEOUT,
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

    wait_for_messages_device_shutdown(
        sender_offline,
        &offline_path,
        TEST_TIMEOUT,
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

/// Verifies startup relay replay does not duplicate already-ingested local giftwrap history.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_relay_replay_is_deduped_against_local_giftwrap_history_e2e() {
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
    let now = unix_time_secs();
    let initial_wrap = build_backdated_giftwrap_note(
        &sender,
        &recipient,
        "restart-dedupe-01",
        now.saturating_sub(1),
    );
    let live_wrap = build_backdated_giftwrap_note(&sender, &recipient, "restart-dedupe-02", now);
    let initial_event = Event::from_json(initial_wrap.json().expect("initial wrap json"))
        .expect("initial wrap event");
    relay_db
        .save_event(&initial_event)
        .await
        .expect("seed initial relay history");

    let restart_dir = TempDir::new().expect("restart dedupe dir");
    let restart_path = restart_dir.path().to_path_buf();
    let local_initial = vec![initial_wrap.json().expect("initial local giftwrap json")];
    seed_local_giftwraps_in_data_dir(&restart_path, &recipient, &local_initial);

    let mut publisher = build_messages_device(&relay_url, &sender);
    let mut recipient_device =
        build_messages_device_in_path_with_relays(&[&relay_url], &recipient, &restart_path);
    let expected_initial = BTreeSet::from(["restart-dedupe-01".to_owned()]);
    wait_for_device_messages(
        &mut recipient_device,
        &expected_initial,
        TEST_TIMEOUT,
        "startup replay should stay deduped against local history",
    );
    assert_eq!(local_chat_message_count(&mut recipient_device), 1);

    publish_note_via_device(&mut publisher, &live_wrap);
    let expected_final = BTreeSet::from([
        "restart-dedupe-01".to_owned(),
        "restart-dedupe-02".to_owned(),
    ]);
    wait_for_device_messages_while_flushing(
        &mut recipient_device,
        &expected_final,
        TEST_TIMEOUT,
        "live delivery after startup dedupe replay",
        &mut [&mut publisher],
    );
    assert_eq!(local_chat_message_count(&mut recipient_device), 2);

    relay.shutdown();
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
            wait_for_messages_device_shutdown(
                flapping_device,
                &restart_path,
                TEST_TIMEOUT,
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

/// Verifies both sides of a DM thread see the full interleaved history when both parties reply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bidirectional_dm_thread_converges_across_devices_e2e() {
    init_tracing();

    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });
    let relay = LocalRelay::run(RelayBuilder::default().database(relay_db.clone()))
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let mut alice = build_messages_cluster("alice", &relay_url, 2);
    let mut bob = build_messages_cluster("bob", &relay_url, 2);

    seed_cluster_dm_relay_list(&mut alice, &relay_url);
    seed_cluster_dm_relay_list(&mut bob, &relay_url);

    // Cross-seed: alice's devices need bob's relay list for sending, and vice versa
    for device in &mut alice.devices {
        seed_local_dm_relay_list(device, &bob.account, &relay_url);
    }
    for device in &mut bob.devices {
        seed_local_dm_relay_list(device, &alice.account, &relay_url);
    }

    let profiles = vec![
        (alice.account.clone(), alice.name),
        (bob.account.clone(), bob.name),
    ];
    seed_cluster_known_profiles(&mut alice, &profiles);
    seed_cluster_known_profiles(&mut bob, &profiles);

    warm_up_clusters(&mut [&mut alice, &mut bob]);

    let mut expected = BTreeSet::new();

    for round in 1..=4 {
        let alice_msg = format!("alice->bob:{round:02}");
        expected.insert(alice_msg.clone());
        if round == 1 {
            // First round: open conversation via profile search + send
            send_direct_message(alice.device(0), &bob.npub, &alice_msg);
        } else {
            // Subsequent rounds: conversation already open, just send
            send_message_via_ui(alice.device(0), &alice_msg);
            alice.device(0).step();
            std::thread::sleep(Duration::from_millis(25));
        }
        wait_for_relay_count_at_least(
            &relay_db,
            NostrFilter::new().kind(NostrKind::GiftWrap),
            expected.len() * 2,
            TEST_TIMEOUT,
            "alice giftwraps to land on relay",
            &mut [],
        )
        .await;
        step_clusters(&mut [&mut alice, &mut bob]);

        let bob_msg = format!("bob->alice:{round:02}");
        expected.insert(bob_msg.clone());
        if round == 1 {
            send_direct_message(bob.device(0), &alice.npub, &bob_msg);
        } else {
            send_message_via_ui(bob.device(0), &bob_msg);
            bob.device(0).step();
            std::thread::sleep(Duration::from_millis(25));
        }
        wait_for_relay_count_at_least(
            &relay_db,
            NostrFilter::new().kind(NostrKind::GiftWrap),
            expected.len() * 2,
            TEST_TIMEOUT,
            "bob giftwraps to land on relay",
            &mut [],
        )
        .await;
        step_clusters(&mut [&mut alice, &mut bob]);

        std::thread::sleep(Duration::from_millis(25));
    }

    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        step_clusters(&mut [&mut alice, &mut bob]);

        if cluster_converged_on(&mut alice, &expected) && cluster_converged_on(&mut bob, &expected)
        {
            break;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for bidirectional DM convergence; alice {:?}, bob {:?}",
            cluster_actual_sets(&mut alice),
            cluster_actual_sets(&mut bob)
        );

        std::thread::sleep(Duration::from_millis(20));
    }

    assert_cluster_matches_expected(&mut alice, &expected);
    assert_cluster_matches_expected(&mut bob, &expected);

    relay.shutdown();
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

/// Verifies that three accounts can exchange direct messages pairwise and still converge per account.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_accounts_pairwise_mesh_converges_across_devices_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let mut alice = build_messages_cluster("alice", &relay_url, 2);
    let mut bob = build_messages_cluster("bob", &relay_url, 2);
    let mut carol = build_messages_cluster("carol", &relay_url, 2);

    seed_cluster_dm_relay_list(&mut alice, &relay_url);
    seed_cluster_dm_relay_list(&mut bob, &relay_url);
    seed_cluster_dm_relay_list(&mut carol, &relay_url);

    let profiles = vec![
        (alice.account.clone(), alice.name),
        (bob.account.clone(), bob.name),
        (carol.account.clone(), carol.name),
    ];
    seed_cluster_known_profiles(&mut alice, &profiles);
    seed_cluster_known_profiles(&mut bob, &profiles);
    seed_cluster_known_profiles(&mut carol, &profiles);

    warm_up_clusters(&mut [&mut alice, &mut bob, &mut carol]);

    let alice_to_bob = "alice->bob:01".to_owned();
    let bob_to_alice = "bob->alice:01".to_owned();
    let alice_to_carol = "alice->carol:01".to_owned();
    let carol_to_alice = "carol->alice:01".to_owned();
    let bob_to_carol = "bob->carol:01".to_owned();
    let carol_to_bob = "carol->bob:01".to_owned();

    send_direct_message(alice.device(0), &bob.npub, &alice_to_bob);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);
    send_direct_message(alice.device(1), &carol.npub, &alice_to_carol);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);
    send_direct_message(bob.device(0), &alice.npub, &bob_to_alice);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);
    send_direct_message(bob.device(1), &carol.npub, &bob_to_carol);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);
    send_direct_message(carol.device(0), &alice.npub, &carol_to_alice);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);
    send_direct_message(carol.device(1), &bob.npub, &carol_to_bob);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);

    let expected_alice = BTreeSet::from([
        alice_to_bob.clone(),
        alice_to_carol.clone(),
        bob_to_alice.clone(),
        carol_to_alice.clone(),
    ]);
    let expected_bob = BTreeSet::from([
        alice_to_bob.clone(),
        bob_to_alice.clone(),
        bob_to_carol.clone(),
        carol_to_bob.clone(),
    ]);
    let expected_carol = BTreeSet::from([
        alice_to_carol.clone(),
        bob_to_carol.clone(),
        carol_to_alice.clone(),
        carol_to_bob.clone(),
    ]);

    wait_for_cluster_convergence(
        &mut alice,
        &expected_alice,
        &mut bob,
        &expected_bob,
        &mut carol,
        &expected_carol,
        TEST_TIMEOUT,
    );

    assert_cluster_matches_expected(&mut alice, &expected_alice);
    assert_cluster_matches_expected(&mut bob, &expected_bob);
    assert_cluster_matches_expected(&mut carol, &expected_carol);

    relay.shutdown();
}

/// Verifies sustained high-volume pairwise traffic delivers every expected message to every device.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_accounts_high_volume_pairwise_mesh_delivers_every_message_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default().rate_limit(RateLimit {
        max_reqs: 20,
        notes_per_minute: 2_000,
    }))
    .await
    .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let mut alice = build_messages_cluster("alice", &relay_url, 2);
    let mut bob = build_messages_cluster("bob", &relay_url, 2);
    let mut carol = build_messages_cluster("carol", &relay_url, 2);

    seed_cluster_dm_relay_list(&mut alice, &relay_url);
    seed_cluster_dm_relay_list(&mut bob, &relay_url);
    seed_cluster_dm_relay_list(&mut carol, &relay_url);

    let profiles = vec![
        (alice.account.clone(), alice.name),
        (bob.account.clone(), bob.name),
        (carol.account.clone(), carol.name),
    ];
    seed_cluster_known_profiles(&mut alice, &profiles);
    seed_cluster_known_profiles(&mut bob, &profiles);
    seed_cluster_known_profiles(&mut carol, &profiles);

    warm_up_clusters(&mut [&mut alice, &mut bob, &mut carol]);

    open_conversation_via_ui(alice.device(0), &bob.npub);
    open_conversation_via_ui(alice.device(1), &carol.npub);
    open_conversation_via_ui(bob.device(0), &alice.npub);
    open_conversation_via_ui(bob.device(1), &carol.npub);
    open_conversation_via_ui(carol.device(0), &alice.npub);
    open_conversation_via_ui(carol.device(1), &bob.npub);

    const MESSAGES_PER_DIRECTION: usize = 40;
    const CONVERGENCE_BATCH: usize = 5;

    let alice_to_bob = build_direct_message_batch("alice", "bob", MESSAGES_PER_DIRECTION);
    let alice_to_carol = build_direct_message_batch("alice", "carol", MESSAGES_PER_DIRECTION);
    let bob_to_alice = build_direct_message_batch("bob", "alice", MESSAGES_PER_DIRECTION);
    let bob_to_carol = build_direct_message_batch("bob", "carol", MESSAGES_PER_DIRECTION);
    let carol_to_alice = build_direct_message_batch("carol", "alice", MESSAGES_PER_DIRECTION);
    let carol_to_bob = build_direct_message_batch("carol", "bob", MESSAGES_PER_DIRECTION);

    for idx in 0..MESSAGES_PER_DIRECTION {
        send_message_via_ui(alice.device(0), &alice_to_bob[idx]);
        send_message_via_ui(alice.device(1), &alice_to_carol[idx]);
        send_message_via_ui(bob.device(0), &bob_to_alice[idx]);
        send_message_via_ui(bob.device(1), &bob_to_carol[idx]);
        send_message_via_ui(carol.device(0), &carol_to_alice[idx]);
        send_message_via_ui(carol.device(1), &carol_to_bob[idx]);

        step_clusters(&mut [&mut alice, &mut bob, &mut carol]);

        if (idx + 1) % CONVERGENCE_BATCH == 0 {
            let expected_alice = alice_to_bob[..=idx]
                .iter()
                .chain(alice_to_carol[..=idx].iter())
                .chain(bob_to_alice[..=idx].iter())
                .chain(carol_to_alice[..=idx].iter())
                .cloned()
                .collect::<BTreeSet<_>>();
            let expected_bob = alice_to_bob[..=idx]
                .iter()
                .chain(bob_to_alice[..=idx].iter())
                .chain(bob_to_carol[..=idx].iter())
                .chain(carol_to_bob[..=idx].iter())
                .cloned()
                .collect::<BTreeSet<_>>();
            let expected_carol = alice_to_carol[..=idx]
                .iter()
                .chain(bob_to_carol[..=idx].iter())
                .chain(carol_to_alice[..=idx].iter())
                .chain(carol_to_bob[..=idx].iter())
                .cloned()
                .collect::<BTreeSet<_>>();

            wait_for_cluster_convergence(
                &mut alice,
                &expected_alice,
                &mut bob,
                &expected_bob,
                &mut carol,
                &expected_carol,
                TEST_TIMEOUT,
            );
        }
    }

    let expected_alice = alice_to_bob
        .iter()
        .chain(alice_to_carol.iter())
        .chain(bob_to_alice.iter())
        .chain(carol_to_alice.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected_bob = alice_to_bob
        .iter()
        .chain(bob_to_alice.iter())
        .chain(bob_to_carol.iter())
        .chain(carol_to_bob.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected_carol = alice_to_carol
        .iter()
        .chain(bob_to_carol.iter())
        .chain(carol_to_alice.iter())
        .chain(carol_to_bob.iter())
        .cloned()
        .collect::<BTreeSet<_>>();

    wait_for_cluster_convergence(
        &mut alice,
        &expected_alice,
        &mut bob,
        &expected_bob,
        &mut carol,
        &expected_carol,
        TEST_TIMEOUT,
    );

    assert_cluster_matches_expected(&mut alice, &expected_alice);
    assert_cluster_matches_expected(&mut bob, &expected_bob);
    assert_cluster_matches_expected(&mut carol, &expected_carol);

    relay.shutdown();
}

/// Verifies that a cold-start device successfully backfills more than 500 messages from a relay
/// by injecting data directly into the relay's memory database to confirm reliability.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires negentropy to sync beyond the giftwrap limit:500 filter"]
async fn messages_backfill_reliability_limit_e2e() {
    init_tracing();

    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        max_events: Some(75_000),
    });
    let relay = LocalRelay::run(RelayBuilder::default().database(relay_db.clone()))
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let recipient = FullKeypair::generate();
    let sender = FullKeypair::generate();
    let mut expected = BTreeSet::new();
    let now = unix_time_secs();

    // Inject 600 unique giftwraps directly into the relay database
    // Each message consists of a rumor wrapped in a seal wrapped in a giftwrap.
    for i in 1..=600 {
        let msg_content = format!("reliability-limit-msg-{:03}", i);
        expected.insert(msg_content.clone());
        let wrap =
            build_backdated_giftwrap_note(&sender, &recipient, &msg_content, now - 1000 + i as u64);

        let event = Event::from_json(wrap.json().expect("wrap json")).expect("invalid event json");
        // Using direct injection to avoid publishing bottlenecks
        relay_db
            .save_event(&event)
            .await
            .expect("failed to save event in relay memory db");
    }

    // Initialize the recipient device (cold boot)
    let mut recipient_device = build_messages_device(&relay_url, &recipient);
    seed_local_dm_relay_list(&mut recipient_device, &recipient, &relay_url);

    // Wait for the recipient to fetch all messages.
    // This will time out at 500 if the pagination bug exists.
    wait_for_device_messages(
        &mut recipient_device,
        &expected,
        TEST_TIMEOUT,
        "cold-start backfill of 600 injected messages",
    );

    assert_eq!(
        local_chat_message_count(&mut recipient_device),
        600,
        "expected exactly 600 messages ingested in local NostrDB"
    );

    relay.shutdown();
}

/// Extracts the port number from a relay URL like `ws://127.0.0.1:12345/`.
fn extract_port(relay_url: &str) -> u16 {
    url::Url::parse(relay_url)
        .expect("parse relay URL")
        .port()
        .expect("relay URL must have a port")
}

/// Forwards bytes between two TCP halves while `active` is true.
/// When `active` becomes false, waits on `resume` — holding sockets alive
/// but stopping all forwarding (simulating a black-holed network path).
/// When `resume` is notified, forwarding resumes on the existing connection.
async fn relay_bytes(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    active: Arc<AtomicBool>,
    resume: Arc<tokio::sync::Notify>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = vec![0u8; 8192];
    loop {
        if !active.load(Ordering::Relaxed) {
            // Black hole mode: hold sockets alive but stop forwarding.
            // The TCP connection stays open (no RST, no FIN) — silent stall.
            // Wait for resume notification to start forwarding again.
            resume.notified().await;
        }
        match tokio::time::timeout(Duration::from_millis(50), from.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                if to.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue, // timeout — recheck active flag
        }
    }
}

/// A TCP proxy that forwards WebSocket connections to a target relay.
///
/// Supports two failure modes:
/// - `black_hole()`: stops forwarding but keeps TCP connections alive (no RST,
///   no FIN, no Close frame). This perfectly simulates Will's bug: the connection
///   is silently dead after laptop sleep / NAT timeout, but the app still thinks
///   it's connected because no disconnect event is ever received.
/// - `kill()`: drops all sockets abruptly (TCP RST, no WebSocket Close frame).
struct TcpProxy {
    addr: std::net::SocketAddr,
    /// When false, data forwarding stops but TCP stays alive.
    active: Arc<AtomicBool>,
    /// Wakes parked relay_bytes tasks when transitioning out of black-hole mode.
    resume: Arc<tokio::sync::Notify>,
    shutdown: tokio::sync::broadcast::Sender<()>,
    /// Handles to spawned relay_bytes tasks so kill() can abort them.
    tasks: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

#[allow(dead_code)]
impl TcpProxy {
    async fn start(target_url: &str) -> Self {
        use tokio::net::TcpListener;

        let parsed = url::Url::parse(target_url).expect("parse target URL");
        let target_addr = format!(
            "{}:{}",
            parsed.host_str().expect("host"),
            parsed.port().expect("port")
        );

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind proxy listener");
        let addr = listener.local_addr().expect("proxy local addr");

        let active = Arc::new(AtomicBool::new(true));
        let resume = Arc::new(tokio::sync::Notify::new());
        let tasks: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
        let mut accept_shutdown = shutdown_tx.subscribe();

        let proxy_active = active.clone();
        let proxy_resume = resume.clone();
        let proxy_tasks = tasks.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        let (client, _) = match result {
                            Ok(r) => r,
                            Err(_) => continue,
                        };
                        let target = match tokio::net::TcpStream::connect(&target_addr).await {
                            Ok(t) => t,
                            Err(_) => continue,
                        };
                        let (cr, cw) = client.into_split();
                        let (tr, tw) = target.into_split();
                        let c2r = tokio::spawn(relay_bytes(
                            cr, tw,
                            proxy_active.clone(),
                            proxy_resume.clone(),
                        ));
                        let r2c = tokio::spawn(relay_bytes(
                            tr, cw,
                            proxy_active.clone(),
                            proxy_resume.clone(),
                        ));
                        proxy_tasks.lock().unwrap().push(c2r);
                        proxy_tasks.lock().unwrap().push(r2c);
                    }
                    _ = accept_shutdown.recv() => break,
                }
            }
        });

        TcpProxy {
            addr,
            active,
            resume,
            shutdown: shutdown_tx,
            tasks,
        }
    }

    fn url(&self) -> String {
        format!("ws://{}/", self.addr)
    }

    /// Black-hole the proxy: stop forwarding data but keep all TCP connections
    /// alive. No RST, no FIN, no WebSocket Close — just silence.
    /// This is exactly what happens during laptop sleep or NAT timeout.
    fn black_hole(&self) {
        self.active.store(false, Ordering::Relaxed);
    }

    /// Resume forwarding on all connections (including those parked by black_hole).
    fn restore(&self) {
        self.active.store(true, Ordering::Relaxed);
        self.resume.notify_waiters();
    }

    /// Kill the proxy: abort all relay tasks and stop accepting connections.
    fn kill(&self) {
        let _ = self.shutdown.send(());
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

/// Verifies that a device recovers messages after its relay goes down and comes back up.
///
/// This simulates the core problem described in DECK-918: after a device loses
/// its relay connection (e.g., from sleep/background), it must detect the stale
/// connection, reconnect, re-establish subscriptions, and receive any messages
/// that arrived while it was disconnected.
///
/// Setup:
/// 1. Alice sends DMs to Bob via a local relay, Bob's device converges
/// 2. Shut down the relay (simulating connection loss from sleep)
/// 3. Seed new giftwrap messages directly into the relay's MemoryDatabase
/// 4. Restart the relay on the same port with the same database
/// 5. Bob's device should detect the disconnect, reconnect, and receive new messages
///
/// Assertion: Bob's device ends up with both the original and new messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_recover_after_relay_restart_e2e() {
    init_tracing();

    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });

    let relay = LocalRelay::run(RelayBuilder::default().database(relay_db.clone()))
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();
    let relay_port = extract_port(&relay_url);

    let alice = FullKeypair::generate();
    let bob = FullKeypair::generate();
    let bob_npub = bob.pubkey.npub().expect("bob npub");

    let mut alice_device = build_messages_device(&relay_url, &alice);
    let mut bob_device = build_messages_device(&relay_url, &bob);

    seed_local_dm_relay_list(&mut alice_device, &alice, &relay_url);
    seed_local_dm_relay_list(&mut alice_device, &bob, &relay_url);
    seed_local_dm_relay_list(&mut bob_device, &bob, &relay_url);

    // Warm up connections
    alice_device.step();
    bob_device.step();
    std::thread::sleep(Duration::from_millis(100));
    alice_device.step();
    bob_device.step();

    // Phase 1: Alice sends initial messages, Bob converges
    open_conversation_via_ui(&mut alice_device, &bob_npub);

    let initial_messages: BTreeSet<String> =
        (1..=4).map(|i| format!("before-restart-{i:02}")).collect();
    for message in &initial_messages {
        send_message_via_ui(&mut alice_device, message);
        alice_device.step();
        bob_device.step();
        std::thread::sleep(Duration::from_millis(25));
    }

    wait_for_device_messages(
        &mut bob_device,
        &initial_messages,
        TEST_TIMEOUT,
        "bob to receive initial messages before relay restart",
    );

    // Phase 2: Kill the relay — Bob's connection goes stale
    relay.shutdown();

    // Give the shutdown a moment to propagate
    std::thread::sleep(Duration::from_millis(200));
    bob_device.step();

    // Phase 3: Seed new giftwrap messages into the relay database while it's "down"
    let new_messages: Vec<String> = (1..=3).map(|i| format!("after-restart-{i:02}")).collect();
    for (i, message) in new_messages.iter().enumerate() {
        let wrap =
            build_backdated_giftwrap_note(&alice, &bob, message, unix_time_secs() + i as u64);
        let event =
            Event::from_json(wrap.json().expect("wrap json")).expect("parse giftwrap event");
        relay_db.save_event(&event).await.expect("seed giftwrap");
    }

    // Phase 4: Restart the relay on the same port with the same database
    let relay2 = LocalRelay::run(
        RelayBuilder::default()
            .port(relay_port)
            .database(relay_db.clone()),
    )
    .await
    .expect("restart relay on same port");

    // Sanity: the new relay has the same URL
    assert_eq!(
        relay2.url(),
        relay_url,
        "restarted relay must bind to the same URL"
    );

    // Phase 5: Bob's device should reconnect and receive the new messages
    let all_expected: BTreeSet<String> = initial_messages
        .iter()
        .chain(new_messages.iter())
        .cloned()
        .collect();

    wait_for_device_messages(
        &mut bob_device,
        &all_expected,
        TEST_TIMEOUT,
        "bob to recover messages after relay restart",
    );

    assert_eq!(
        local_chat_messages(&mut bob_device),
        all_expected,
        "bob should have both pre-restart and post-restart messages"
    );

    relay2.shutdown();
}

/// Reproduces the stale-connection bug from DECK-918 / Will's comment:
/// "we don't have the re-ping when coming from background to foreground
/// or from laptop sleep. this causes stale connections/subs on resume."
///
/// This test uses a TCP proxy between Bob's device and the relay. After
/// initial messages flow, the proxy is killed (simulating an abrupt network
/// failure like laptop sleep — TCP dies without a WebSocket Close frame).
/// New messages are then seeded into the relay. The test checks whether
/// Bob's device detects the stale connection and recovers the new messages.
///
/// Currently expected to FAIL: the app has no pong-timeout detection, so
/// the stale connection is never detected and the device never reconnects.
/// When the fix is implemented, this test should pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_connection_detected_after_silent_stall_e2e() {
    init_tracing();

    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });

    let relay = LocalRelay::run(RelayBuilder::default().database(relay_db.clone()))
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    // TCP proxy sits between Bob's device and the real relay
    let proxy = TcpProxy::start(&relay_url).await;
    let proxy_url = proxy.url();

    let alice = FullKeypair::generate();
    let bob = FullKeypair::generate();
    let bob_npub = bob.pubkey.npub().expect("bob npub");

    // Alice connects directly to the relay; Bob connects through the proxy.
    // Bob gets a short pong timeout (3s) so the test doesn't need to wait 90s.
    let mut alice_device = build_messages_device(&relay_url, &alice);
    let mut bob_device = notedeck_testing::device::build_device_with_relays(
        &[&proxy_url],
        &bob,
        Box::new(|notedeck, _ctx| {
            notedeck.set_pong_timeout(Duration::from_secs(3));
            notedeck.set_app(notedeck_messages::MessagesApp::new());
        }),
    );

    seed_local_dm_relay_list(&mut alice_device, &alice, &relay_url);
    seed_local_dm_relay_list(&mut alice_device, &bob, &relay_url);
    // Bob's DM relay list points at the proxy URL (his "relay")
    seed_local_dm_relay_list(&mut bob_device, &bob, &proxy_url);

    // Warm up connections
    alice_device.step();
    bob_device.step();
    std::thread::sleep(Duration::from_millis(100));
    alice_device.step();
    bob_device.step();

    // Phase 1: Alice sends initial messages through the proxy, Bob converges
    open_conversation_via_ui(&mut alice_device, &bob_npub);

    let initial_messages: BTreeSet<String> = (1..=3).map(|i| format!("pre-sleep-{i:02}")).collect();
    for message in &initial_messages {
        send_message_via_ui(&mut alice_device, message);
        alice_device.step();
        bob_device.step();
        std::thread::sleep(Duration::from_millis(25));
    }

    wait_for_device_messages(
        &mut bob_device,
        &initial_messages,
        TEST_TIMEOUT,
        "bob to receive initial messages through proxy",
    );

    // Phase 2: Black-hole the proxy — Bob's TCP connection stays alive but
    // no data flows in either direction. No RST, no FIN, no Close frame.
    // This is exactly what happens during laptop sleep: the OS doesn't send
    // any TCP control frames, the connection just goes silent. The app still
    // thinks it's connected because no disconnect event is ever received.
    proxy.black_hole();

    // Give the black hole a moment to take effect (relay_bytes polls at 50ms)
    std::thread::sleep(Duration::from_millis(200));

    // Phase 3: Seed new messages into the relay while Bob is "asleep"
    let new_messages: Vec<String> = (1..=3).map(|i| format!("during-sleep-{i:02}")).collect();
    for (i, message) in new_messages.iter().enumerate() {
        let wrap =
            build_backdated_giftwrap_note(&alice, &bob, message, unix_time_secs() + i as u64);
        let event =
            Event::from_json(wrap.json().expect("wrap json")).expect("parse giftwrap event");
        relay_db.save_event(&event).await.expect("seed giftwrap");
    }

    // Phase 4: Step Bob's device for 15 seconds. The proxy is black-holed:
    // TCP connections are alive but silent. If the app had pong-timeout detection,
    // it would notice the silence, transition to Disconnected, and attempt to
    // reconnect. But currently keepalive_ping is fire-and-forget — no pong timeout.
    //
    // We also un-black-hole the proxy after a few seconds so that IF the app
    // reconnects, the new connection would actually work and deliver messages.
    let all_expected: BTreeSet<String> = initial_messages
        .iter()
        .chain(new_messages.iter())
        .cloned()
        .collect();

    let deadline = Instant::now() + TEST_TIMEOUT;
    let restore_at = Instant::now() + Duration::from_secs(5);
    let mut restored_proxy = false;
    while Instant::now() < deadline {
        bob_device.step();
        std::thread::sleep(Duration::from_millis(50));

        // After 5 seconds, restore the proxy so reconnection CAN work.
        // This isolates the bug to detection, not recovery.
        if !restored_proxy && Instant::now() > restore_at {
            proxy.restore();
            restored_proxy = true;
        }

        let current = local_chat_messages(&mut bob_device);
        if current.len() > initial_messages.len() {
            break;
        }
    }

    let final_messages = local_chat_messages(&mut bob_device);

    // THE BUG: Bob never detects the stale connection, so he never reconnects,
    // so he never receives the new messages. This assertion demonstrates the bug.
    assert_eq!(
        final_messages,
        all_expected,
        "STALE CONNECTION BUG: Bob's device failed to detect the silent TCP stall \
         and recover messages sent while sleeping. \
         The proxy was black-holed (no RST, no FIN — just silence) to simulate \
         laptop sleep / NAT timeout. The app has no pong-timeout detection, so it \
         never realizes the connection is dead. \
         Bob has {} messages but should have {}. \
         Missing: {:?}",
        final_messages.len(),
        all_expected.len(),
        all_expected.difference(&final_messages).collect::<Vec<_>>()
    );

    proxy.kill();
    relay.shutdown();
}
