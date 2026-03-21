//! Giftwrap handling edge-case end-to-end tests.

mod harness;

use std::collections::BTreeSet;
use std::time::Duration;

use enostr::FullKeypair;
use harness::fixtures::{
    build_backdated_giftwrap_note, build_invalid_giftwrap_note, build_misdirected_giftwrap_note,
    local_giftwrap_created_ats_in_data_dir, seed_local_giftwraps_in_data_dir,
};
use harness::{
    build_messages_device, build_messages_device_in_path_with_relays,
    build_messages_device_in_tmpdir, build_messages_device_with_relays, init_tracing,
    local_chat_message_count, local_chat_messages, publish_note_via_device, step_device_frames,
    wait_for_device_messages, wait_for_device_messages_while_flushing, TEST_TIMEOUT,
};
use nostr::{Event, JsonUtil};
use nostr_relay_builder::{
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use notedeck::unix_time_secs;
use tempfile::TempDir;
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
