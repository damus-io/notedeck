//! End-to-end coverage for Notedeck's double-ratchet Messages path.

mod harness;

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use enostr::FullKeypair;
use harness::fixtures::{nostr_pubkey, seed_local_dm_relay_list};
use harness::ui::{open_conversation_via_ui, send_message_via_ui, wait_for_label};
use harness::{init_tracing, local_chat_messages, DeviceHarness, LocalRelayExt, TEST_TIMEOUT};
use nostr::util::JsonUtil;
use nostr::{Event, Filter as NostrFilter, Kind as NostrKind};
use nostr_relay_builder::{
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use nostrdb::{FilterBuilder, Transaction};
use notedeck::DOUBLE_RATCHET_SIG_PREFIX;
use notedeck_messages::{
    nip17::{conversation_filter, parse_chat_message},
    MessagesApp,
};
use tempfile::TempDir;

fn build_ratchet_messages_device(
    relay: &str,
    account: &FullKeypair,
    tmpdir: TempDir,
) -> DeviceHarness {
    notedeck_testing::device::build_device_in_tmpdir_with_relays(
        &[relay],
        account,
        tmpdir,
        Box::new(|notedeck, _ctx| {
            notedeck.set_app(MessagesApp::new());
        }),
    )
}

fn build_ratchet_messages_device_in_path(
    relay: &str,
    account: &FullKeypair,
    data_dir: &Path,
) -> DeviceHarness {
    notedeck_testing::device::build_device_in_path_with_relays(
        &[relay],
        account,
        data_dir,
        Box::new(|notedeck, _ctx| {
            notedeck.set_app(MessagesApp::new());
        }),
    )
}

fn build_nip17_messages_device_in_path(
    relay: &str,
    account: &FullKeypair,
    data_dir: &Path,
) -> DeviceHarness {
    notedeck_testing::device::build_device_in_path_with_relays(
        &[relay],
        account,
        data_dir,
        Box::new(|notedeck, _ctx| {
            notedeck.set_app(MessagesApp::new_nip17_only_for_tests());
        }),
    )
}

fn tag_value<'a>(event: &'a Event, name: &str) -> Option<&'a str> {
    event.tags.iter().find_map(|tag| {
        let vals = tag.as_slice();
        if vals.first().map(|s| s.as_str()) == Some(name) {
            return vals.get(1).map(|s| s.as_str());
        }

        None
    })
}

fn has_double_ratchet_app_keys<'a>(
    events: impl Iterator<Item = &'a Event>,
    account: &FullKeypair,
) -> bool {
    let author = nostr_pubkey(&account.pubkey);
    events.into_iter().any(|event| {
        event.pubkey == author
            && tag_value(event, "d") == Some("double-ratchet/app-keys")
            && event
                .tags
                .iter()
                .any(|tag| tag.as_slice().first().map(|s| s.as_str()) == Some("device"))
    })
}

fn has_double_ratchet_invite<'a>(
    events: impl Iterator<Item = &'a Event>,
    account: &FullKeypair,
) -> bool {
    let author = nostr_pubkey(&account.pubkey);
    events.into_iter().any(|event| {
        event.pubkey == author
            && tag_value(event, "d").is_some_and(|d| d.starts_with("double-ratchet/invites/"))
            && event
                .tags
                .iter()
                .any(|tag| tag.as_slice().first().map(|s| s.as_str()) == Some("ephemeralKey"))
    })
}

async fn wait_for_ratchet_bootstrap_material(
    relay_db: &MemoryDatabase,
    alice: &FullKeypair,
    bob: &FullKeypair,
    devices: &mut [&mut DeviceHarness],
) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    let filter = NostrFilter::new().kind(NostrKind::Custom(
        nostr_double_ratchet::APP_KEYS_EVENT_KIND as u16,
    ));

    loop {
        for device in devices.iter_mut() {
            device.step();
        }

        let events = relay_db
            .query(vec![filter.clone()])
            .await
            .expect("query ratchet bootstrap events");

        if has_double_ratchet_app_keys(events.iter(), alice)
            && has_double_ratchet_app_keys(events.iter(), bob)
            && has_double_ratchet_invite(events.iter(), alice)
            && has_double_ratchet_invite(events.iter(), bob)
        {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for both Notedeck devices to publish ratchet AppKeys and invites; found {} kind-{} event(s)",
            events.len(),
            nostr_double_ratchet::APP_KEYS_EVENT_KIND,
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn relay_kind_count(relay_db: &MemoryDatabase, kind: u32) -> usize {
    relay_db
        .query(vec![NostrFilter::new().kind(NostrKind::Custom(kind as u16))])
        .await
        .expect("query relay kind")
        .len()
}

async fn relay_nostr_kind_count(relay_db: &MemoryDatabase, kind: NostrKind) -> usize {
    relay_db
        .query(vec![NostrFilter::new().kind(kind)])
        .await
        .expect("query relay kind")
        .len()
}

fn local_kind_count(device: &mut DeviceHarness, kind: u32) -> usize {
    let ctx = device.ctx.clone();
    let app_ctx = device.state_mut().notedeck.app_context(&ctx);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    let filters = [FilterBuilder::new().kinds([kind.into()]).build()];

    let results = app_ctx
        .ndb
        .query(&txn, &filters, 1024)
        .expect("query local kind");
    results.len()
}

fn wait_for_more_local_events(
    device: &mut DeviceHarness,
    kind: u32,
    previous_count: usize,
    steppers: &mut [&mut DeviceHarness],
    context: &str,
) {
    let deadline = Instant::now() + TEST_TIMEOUT;

    loop {
        device.step();
        for stepper in steppers.iter_mut() {
            stepper.step();
        }

        let current_count = local_kind_count(device, kind);
        if current_count > previous_count {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; local kind-{kind} count stayed at {previous_count}"
        );

        std::thread::sleep(Duration::from_millis(20));
    }
}

fn local_has_double_ratchet_app_keys(device: &mut DeviceHarness, account: &FullKeypair) -> bool {
    let ctx = device.ctx.clone();
    let app_ctx = device.state_mut().notedeck.app_context(&ctx);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    let filter = FilterBuilder::new()
        .kinds([nostr_double_ratchet::APP_KEYS_EVENT_KIND as u64])
        .authors([account.pubkey.bytes()])
        .limit(4)
        .build();
    let results = app_ctx
        .ndb
        .query(&txn, std::slice::from_ref(&filter), 4)
        .expect("query local AppKeys");

    for result in results {
        let Ok(json) = result.note.json() else {
            continue;
        };
        let Ok(event) = Event::from_json(json) else {
            continue;
        };
        if has_double_ratchet_app_keys(std::iter::once(&event), account) {
            return true;
        }
    }

    false
}

fn wait_for_local_double_ratchet_app_keys(
    device: &mut DeviceHarness,
    account: &FullKeypair,
    steppers: &mut [&mut DeviceHarness],
    context: &str,
) {
    let deadline = Instant::now() + TEST_TIMEOUT;

    loop {
        device.step();
        for stepper in steppers.iter_mut() {
            stepper.step();
        }

        if local_has_double_ratchet_app_keys(device, account) {
            return;
        }

        assert!(Instant::now() < deadline, "timed out waiting for {context}");

        std::thread::sleep(Duration::from_millis(20));
    }
}

fn ratchet_user_record_path(data_dir: &Path, owner: &FullKeypair, peer: &FullKeypair) -> PathBuf {
    data_dir
        .join("storage")
        .join("double-ratchet")
        .join(hex::encode(owner.pubkey.bytes()))
        .join(format!("user_{}.json", hex::encode(peer.pubkey.bytes())))
}

fn local_has_send_ready_ratchet_session(
    data_dir: &Path,
    owner: &FullKeypair,
    peer: &FullKeypair,
) -> bool {
    let path = ratchet_user_record_path(data_dir, owner, peer);
    let Ok(data) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(record) = serde_json::from_str::<nostr_double_ratchet::StoredUserRecord>(&data) else {
        return false;
    };

    record.devices.into_iter().any(|device| {
        if device.is_stale {
            return false;
        }

        device.active_session.is_some_and(|state| {
            state.their_next_nostr_public_key.is_some() && state.our_current_nostr_key.is_some()
        })
    })
}

fn wait_for_local_send_ready_ratchet_session(
    data_dir: &Path,
    owner: &FullKeypair,
    peer: &FullKeypair,
    devices: &mut [&mut DeviceHarness],
    context: &str,
) {
    let deadline = Instant::now() + TEST_TIMEOUT;

    loop {
        for device in devices.iter_mut() {
            device.step();
        }

        if local_has_send_ready_ratchet_session(data_dir, owner, peer) {
            return;
        }

        assert!(Instant::now() < deadline, "timed out waiting for {context}");

        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn wait_for_more_relay_events(
    relay_db: &MemoryDatabase,
    kind: u32,
    previous_count: usize,
    devices: &mut [&mut DeviceHarness],
    context: &str,
) {
    let deadline = Instant::now() + TEST_TIMEOUT;

    loop {
        for device in devices.iter_mut() {
            device.step();
        }

        let current_count = relay_kind_count(relay_db, kind).await;
        if current_count > previous_count {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; kind-{kind} count stayed at {previous_count}"
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_more_relay_nostr_kind_events(
    relay_db: &MemoryDatabase,
    kind: NostrKind,
    previous_count: usize,
    devices: &mut [&mut DeviceHarness],
    context: &str,
) {
    let deadline = Instant::now() + TEST_TIMEOUT;

    loop {
        for device in devices.iter_mut() {
            device.step();
        }

        let current_count = relay_nostr_kind_count(relay_db, kind).await;
        if current_count > previous_count {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; {kind:?} count stayed at {previous_count}"
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn ingest_relay_kind_into_device(
    relay_db: &MemoryDatabase,
    device: &mut DeviceHarness,
    kind: u32,
) {
    let events = relay_db
        .query(vec![NostrFilter::new().kind(NostrKind::Custom(kind as u16))])
        .await
        .expect("query relay events");
    let ctx = device.ctx.clone();
    let app_ctx = device.state_mut().notedeck.app_context(&ctx);

    for event in events {
        let json = event.as_json();
        app_ctx
            .ndb
            .process_client_event(&json)
            .expect("ingest relay event into device");
    }
}

fn step_devices_for(devices: &mut [&mut DeviceHarness], frames: usize) {
    for _ in 0..frames {
        for device in devices.iter_mut() {
            device.step();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn local_double_ratchet_chat_messages(device: &mut DeviceHarness) -> BTreeSet<String> {
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
        .filter(|result| result.note.sig().starts_with(&DOUBLE_RATCHET_SIG_PREFIX))
        .filter_map(|result| parse_chat_message(&result.note).map(|msg| msg.message.to_owned()))
        .collect()
}

fn wait_for_ratchet_messages(
    device: &mut DeviceHarness,
    expected: &BTreeSet<String>,
    senders: &mut [&mut DeviceHarness],
    context: &str,
) {
    let deadline = Instant::now() + TEST_TIMEOUT;

    loop {
        device.step();
        for sender in senders.iter_mut() {
            sender.step();
        }

        let actual = local_double_ratchet_chat_messages(device);
        if actual == *expected {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; expected ratchet messages {:?}, actual {:?}, all local chat messages {:?}",
            expected,
            actual,
            local_chat_messages(device)
        );

        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn wait_for_ratchet_messages_from_relay(
    relay_db: &MemoryDatabase,
    device: &mut DeviceHarness,
    expected: &BTreeSet<String>,
    senders: &mut [&mut DeviceHarness],
    context: &str,
) {
    let deadline = Instant::now() + TEST_TIMEOUT;

    loop {
        ingest_relay_kind_into_device(relay_db, device, nostr_double_ratchet::MESSAGE_EVENT_KIND)
            .await;
        device.step();
        for sender in senders.iter_mut() {
            sender.step();
        }

        let actual = local_double_ratchet_chat_messages(device);
        if actual == *expected {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; expected ratchet messages {:?}, actual {:?}, all local chat messages {:?}",
            expected,
            actual,
            local_chat_messages(device)
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Verifies two full Notedeck Messages instances in distinct data dirs can exchange
/// double-ratchet DMs through a real local relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn notedeck_double_ratchet_two_datadirs_round_trip_e2e() {
    init_tracing();

    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });
    let relay = LocalRelay::run(RelayBuilder::default().database(relay_db.clone()))
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let alice = FullKeypair::generate();
    let bob = FullKeypair::generate();
    let alice_npub = alice.pubkey.npub().expect("alice npub");
    let bob_npub = bob.pubkey.npub().expect("bob npub");
    let alice_tmpdir = TempDir::new().expect("alice data dir");
    let bob_tmpdir = TempDir::new().expect("bob data dir");
    assert_ne!(
        alice_tmpdir.path(),
        bob_tmpdir.path(),
        "the e2e must use separate Notedeck data dirs"
    );
    let alice_data_dir = alice_tmpdir.path().to_path_buf();
    let bob_data_dir = bob_tmpdir.path().to_path_buf();

    let mut alice_device = build_ratchet_messages_device(&relay_url, &alice, alice_tmpdir);
    let mut bob_device = build_ratchet_messages_device(&relay_url, &bob, bob_tmpdir);

    seed_local_dm_relay_list(&mut alice_device, &alice, &relay_url);
    seed_local_dm_relay_list(&mut alice_device, &bob, &relay_url);
    seed_local_dm_relay_list(&mut bob_device, &bob, &relay_url);
    seed_local_dm_relay_list(&mut bob_device, &alice, &relay_url);

    wait_for_ratchet_bootstrap_material(
        &relay_db,
        &alice,
        &bob,
        &mut [&mut alice_device, &mut bob_device],
    )
    .await;

    open_conversation_via_ui(&mut alice_device, &bob_npub);
    wait_for_local_double_ratchet_app_keys(
        &mut alice_device,
        &bob,
        &mut [&mut bob_device],
        "Alice to discover Bob's double-ratchet AppKeys",
    );

    let alice_message = "notedeck-ratchet-alice-to-bob";
    let relay_invite_responses_before =
        relay_kind_count(&relay_db, nostr_double_ratchet::INVITE_RESPONSE_KIND).await;
    let alice_invite_responses_before = local_kind_count(
        &mut alice_device,
        nostr_double_ratchet::INVITE_RESPONSE_KIND,
    );
    let before_alice_send =
        relay_kind_count(&relay_db, nostr_double_ratchet::MESSAGE_EVENT_KIND).await;
    send_message_via_ui(&mut alice_device, alice_message);
    wait_for_more_relay_events(
        &relay_db,
        nostr_double_ratchet::MESSAGE_EVENT_KIND,
        before_alice_send,
        &mut [&mut alice_device, &mut bob_device],
        "Alice's double-ratchet outer message to reach the relay",
    )
    .await;

    let mut expected = BTreeSet::from([alice_message.to_owned()]);
    wait_for_ratchet_messages(
        &mut bob_device,
        &expected,
        &mut [&mut alice_device],
        "Bob to decrypt Alice's Notedeck double-ratchet message",
    );
    wait_for_local_send_ready_ratchet_session(
        &bob_data_dir,
        &bob,
        &alice,
        &mut [&mut bob_device, &mut alice_device],
        "Bob to install a send-ready double-ratchet session for Alice",
    );
    wait_for_more_relay_events(
        &relay_db,
        nostr_double_ratchet::INVITE_RESPONSE_KIND,
        relay_invite_responses_before,
        &mut [&mut bob_device, &mut alice_device],
        "Bob's double-ratchet invite response to reach the relay",
    )
    .await;
    let before_alice_processes_invite_response =
        relay_kind_count(&relay_db, nostr_double_ratchet::MESSAGE_EVENT_KIND).await;
    let bob_before_alice_flush =
        local_kind_count(&mut bob_device, nostr_double_ratchet::MESSAGE_EVENT_KIND);
    wait_for_more_local_events(
        &mut alice_device,
        nostr_double_ratchet::INVITE_RESPONSE_KIND,
        alice_invite_responses_before,
        &mut [&mut bob_device],
        "Alice to ingest Bob's double-ratchet invite response",
    );
    wait_for_more_relay_events(
        &relay_db,
        nostr_double_ratchet::MESSAGE_EVENT_KIND,
        before_alice_processes_invite_response,
        &mut [&mut alice_device, &mut bob_device],
        "Alice to install Bob's invite response and flush ratchet history",
    )
    .await;
    wait_for_more_local_events(
        &mut bob_device,
        nostr_double_ratchet::MESSAGE_EVENT_KIND,
        bob_before_alice_flush,
        &mut [&mut alice_device],
        "Bob to ingest Alice's double-ratchet history flush",
    );
    wait_for_local_send_ready_ratchet_session(
        &alice_data_dir,
        &alice,
        &bob,
        &mut [&mut alice_device, &mut bob_device],
        "Alice to install a send-ready double-ratchet session for Bob",
    );
    step_devices_for(&mut [&mut alice_device, &mut bob_device], 8);

    open_conversation_via_ui(&mut bob_device, &alice_npub);
    wait_for_label(&mut bob_device, alice_message, TEST_TIMEOUT);

    let bob_message = "notedeck-ratchet-bob-to-alice";
    let before_bob_send =
        relay_kind_count(&relay_db, nostr_double_ratchet::MESSAGE_EVENT_KIND).await;
    send_message_via_ui(&mut bob_device, bob_message);
    let mut expected_with_bob_reply = expected.clone();
    expected_with_bob_reply.insert(bob_message.to_owned());
    wait_for_ratchet_messages(
        &mut bob_device,
        &expected_with_bob_reply,
        &mut [&mut alice_device],
        "Bob to store his reply as a local double-ratchet message",
    );
    wait_for_more_relay_events(
        &relay_db,
        nostr_double_ratchet::MESSAGE_EVENT_KIND,
        before_bob_send,
        &mut [&mut bob_device, &mut alice_device],
        "Bob's double-ratchet outer message to reach the relay",
    )
    .await;
    ingest_relay_kind_into_device(
        &relay_db,
        &mut alice_device,
        nostr_double_ratchet::MESSAGE_EVENT_KIND,
    )
    .await;

    expected.insert(bob_message.to_owned());
    wait_for_ratchet_messages(
        &mut alice_device,
        &expected,
        &mut [&mut bob_device],
        "Alice to decrypt Bob's Notedeck double-ratchet reply",
    );
    wait_for_label(&mut alice_device, alice_message, TEST_TIMEOUT);
    wait_for_label(&mut alice_device, bob_message, TEST_TIMEOUT);
    wait_for_ratchet_messages(
        &mut bob_device,
        &expected,
        &mut [&mut alice_device],
        "Bob to retain both double-ratchet messages locally",
    );
    wait_for_label(&mut bob_device, alice_message, TEST_TIMEOUT);
    wait_for_label(&mut bob_device, bob_message, TEST_TIMEOUT);

    let relay_message_count =
        relay_kind_count(&relay_db, nostr_double_ratchet::MESSAGE_EVENT_KIND).await;
    assert!(
        relay_message_count > before_alice_send,
        "expected kind-{} ratchet outer events on relay, found {relay_message_count}",
        nostr_double_ratchet::MESSAGE_EVENT_KIND
    );
    assert_eq!(
        local_double_ratchet_chat_messages(&mut alice_device),
        expected,
        "Alice's visible chat history should be backed by marker-signed ratchet inner rumors"
    );
    assert_eq!(
        local_double_ratchet_chat_messages(&mut bob_device),
        expected,
        "Bob's visible chat history should be backed by marker-signed ratchet inner rumors"
    );

    relay.shutdown_and_wait().await;
}

/// Verifies an existing NIP-17 chat upgrades to double ratchet after the peer later enables DR.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn existing_chat_upgrades_when_peer_enables_double_ratchet_later_e2e() {
    init_tracing();

    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });
    let relay = LocalRelay::run(RelayBuilder::default().database(relay_db.clone()))
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let alice = FullKeypair::generate();
    let bob = FullKeypair::generate();
    let alice_npub = alice.pubkey.npub().expect("alice npub");
    let bob_npub = bob.pubkey.npub().expect("bob npub");
    let alice_tmpdir = TempDir::new().expect("alice data dir");
    let bob_tmpdir = TempDir::new().expect("bob data dir");
    let alice_data_dir = alice_tmpdir.path().to_path_buf();
    let bob_data_dir = bob_tmpdir.path().to_path_buf();

    let mut alice_device =
        build_ratchet_messages_device_in_path(&relay_url, &alice, &alice_data_dir);
    let mut bob_device = build_nip17_messages_device_in_path(&relay_url, &bob, &bob_data_dir);

    seed_local_dm_relay_list(&mut alice_device, &alice, &relay_url);
    seed_local_dm_relay_list(&mut alice_device, &bob, &relay_url);
    seed_local_dm_relay_list(&mut bob_device, &bob, &relay_url);
    seed_local_dm_relay_list(&mut bob_device, &alice, &relay_url);

    open_conversation_via_ui(&mut alice_device, &bob_npub);

    let legacy_message = "notedeck-upgrade-before-bob-dr";
    let giftwraps_before_legacy = relay_nostr_kind_count(&relay_db, NostrKind::GiftWrap).await;
    let ratchet_messages_before_legacy =
        relay_kind_count(&relay_db, nostr_double_ratchet::MESSAGE_EVENT_KIND).await;
    send_message_via_ui(&mut alice_device, legacy_message);
    wait_for_more_relay_nostr_kind_events(
        &relay_db,
        NostrKind::GiftWrap,
        giftwraps_before_legacy,
        &mut [&mut alice_device, &mut bob_device],
        "Alice's pre-upgrade NIP-17 giftwrap to reach the relay",
    )
    .await;
    wait_for_label(&mut bob_device, legacy_message, TEST_TIMEOUT);
    assert_eq!(
        relay_kind_count(&relay_db, nostr_double_ratchet::MESSAGE_EVENT_KIND).await,
        ratchet_messages_before_legacy,
        "pre-upgrade send should not publish a double-ratchet outer message"
    );

    notedeck_testing::shutdown_device(bob_device);
    let mut bob_device = build_ratchet_messages_device_in_path(&relay_url, &bob, &bob_data_dir);
    seed_local_dm_relay_list(&mut bob_device, &bob, &relay_url);
    seed_local_dm_relay_list(&mut bob_device, &alice, &relay_url);
    let invite_responses_before_upgrade =
        relay_kind_count(&relay_db, nostr_double_ratchet::INVITE_RESPONSE_KIND).await;

    wait_for_ratchet_bootstrap_material(
        &relay_db,
        &alice,
        &bob,
        &mut [&mut alice_device, &mut bob_device],
    )
    .await;
    wait_for_local_double_ratchet_app_keys(
        &mut alice_device,
        &bob,
        &mut [&mut bob_device],
        "Alice's existing chat to discover Bob's later double-ratchet AppKeys",
    );
    open_conversation_via_ui(&mut bob_device, &alice_npub);
    wait_for_local_double_ratchet_app_keys(
        &mut bob_device,
        &alice,
        &mut [&mut alice_device],
        "Bob's reopened existing chat to discover Alice's double-ratchet AppKeys",
    );
    wait_for_more_relay_events(
        &relay_db,
        nostr_double_ratchet::INVITE_RESPONSE_KIND,
        invite_responses_before_upgrade,
        &mut [&mut alice_device, &mut bob_device],
        "Alice's double-ratchet invite response to reach Bob after Bob enables DR",
    )
    .await;
    ingest_relay_kind_into_device(
        &relay_db,
        &mut bob_device,
        nostr_double_ratchet::INVITE_RESPONSE_KIND,
    )
    .await;
    wait_for_local_send_ready_ratchet_session(
        &alice_data_dir,
        &alice,
        &bob,
        &mut [&mut alice_device, &mut bob_device],
        "Alice's existing chat to install a send-ready double-ratchet session after Bob enables DR",
    );
    wait_for_local_send_ready_ratchet_session(
        &bob_data_dir,
        &bob,
        &alice,
        &mut [&mut bob_device, &mut alice_device],
        "Bob's reopened existing chat to install a send-ready double-ratchet session after enabling DR",
    );

    let upgraded_message = "notedeck-upgrade-after-bob-dr";
    let ratchet_messages_before_upgrade =
        relay_kind_count(&relay_db, nostr_double_ratchet::MESSAGE_EVENT_KIND).await;
    send_message_via_ui(&mut alice_device, upgraded_message);
    wait_for_more_relay_events(
        &relay_db,
        nostr_double_ratchet::MESSAGE_EVENT_KIND,
        ratchet_messages_before_upgrade,
        &mut [&mut alice_device, &mut bob_device],
        "Alice's upgraded double-ratchet outer message to reach the relay",
    )
    .await;

    let upgraded_expected = BTreeSet::from([upgraded_message.to_owned()]);
    wait_for_ratchet_messages_from_relay(
        &relay_db,
        &mut bob_device,
        &upgraded_expected,
        &mut [&mut alice_device],
        "Bob to decrypt Alice's post-upgrade double-ratchet message",
    )
    .await;
    wait_for_label(&mut bob_device, legacy_message, TEST_TIMEOUT);
    wait_for_label(&mut bob_device, upgraded_message, TEST_TIMEOUT);
    assert!(
        !local_double_ratchet_chat_messages(&mut alice_device).contains(legacy_message),
        "the pre-upgrade message should remain NIP-17 locally"
    );
    assert!(
        local_double_ratchet_chat_messages(&mut alice_device).contains(upgraded_message),
        "the post-upgrade message should be stored as a double-ratchet inner rumor"
    );

    relay.shutdown_and_wait().await;
}
