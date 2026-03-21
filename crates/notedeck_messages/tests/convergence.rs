//! Basic same-account device convergence end-to-end tests.

mod harness;

use std::collections::BTreeSet;
use std::time::Duration;

use enostr::FullKeypair;
use harness::fixtures::{
    build_backdated_giftwrap_note, build_local_chat_note_jsons, local_chat_messages_in_data_dir,
    seed_local_dm_relay_list, seed_local_notes_in_data_dir,
};
use harness::ui::{open_conversation_via_ui, send_message_via_ui};
use harness::{
    assert_devices_match_expected, build_messages_device,
    build_messages_device_in_path_with_relays, build_messages_device_in_tmpdir,
    build_messages_device_with_relays, init_tracing, local_chat_messages, shutdown_messages_device,
    step_device_frames, step_device_group, step_devices, wait_for_convergence,
    wait_for_device_group_messages, wait_for_device_group_messages_while_flushing,
    wait_for_device_messages, wait_for_device_messages_while_flushing, wait_for_devices_messages,
    TEST_TIMEOUT,
};
use nostr::{Event, JsonUtil};
use nostr_relay_builder::{
    builder::RateLimit,
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use notedeck::unix_time_secs;
use tempfile::TempDir;
/// Verifies that multiple devices on the same account converge on messages sent from another user.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_devices_converge_on_sent_messages_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default().rate_limit(RateLimit {
        max_reqs: 2_000,
        notes_per_minute: 10_000,
    }))
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

    let relay = LocalRelay::run(RelayBuilder::default().rate_limit(RateLimit {
        max_reqs: 2_000,
        notes_per_minute: 10_000,
    }))
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

    wait_for_device_group_messages_while_flushing(
        &mut [&mut live_device_a, &mut live_device_b],
        &expected,
        TEST_TIMEOUT,
        "live same-account devices to stay current while one device is paused",
        &mut [&mut sender_device],
    );

    assert_ne!(
        local_chat_messages(&mut lagging_device),
        expected,
        "expected the paused device to lag behind before it resumes stepping"
    );

    wait_for_device_messages_while_flushing(
        &mut lagging_device,
        &expected,
        TEST_TIMEOUT,
        "paused same-account device to catch up from relay history",
        &mut [&mut sender_device],
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

    shutdown_messages_device(
        recipient_a,
        "drop relay-a-only pre-recovery device before expanded-visibility startup",
    );
    shutdown_messages_device(
        recipient_b,
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
