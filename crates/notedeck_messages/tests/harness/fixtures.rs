//! Fixture builders and raw NostrDB preload helpers for Messages end-to-end tests.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use enostr::FullKeypair;
use nostr::{
    event::{EventBuilder, Kind, Tag},
    nips::nip44,
    util::JsonUtil,
};
use nostrdb::{FilterBuilder, Ndb, Note, NoteBuilder, Transaction};
use notedeck::{unix_time_secs, RelayType};
use notedeck_messages::nip17::{
    conversation_filter, parse_chat_message, parse_dm_relay_list_relays,
    participant_dm_relay_list_filter, OsRng,
};

// Re-export general fixtures from the shared harness.
pub use notedeck_testing::fixtures::{
    ndb_path, nostr_pubkey, open_ndb, seed_cluster_known_profiles, seed_local_notes_in_data_dir,
    seed_local_profile_metadata, test_config, wait_for_import_count,
};

use super::{AccountCluster, DeviceHarness, TEST_TIMEOUT};

/// Seeds one account's DM relay-list note on every device in the cluster.
pub fn seed_cluster_dm_relay_list(cluster: &mut AccountCluster, relay: &str) {
    for device in &mut cluster.devices {
        seed_local_dm_relay_list(device, &cluster.account, relay);
    }
}

/// Seeds raw local giftwrap JSON into one device NostrDB using the account key.
pub fn seed_local_giftwraps_in_data_dir(
    data_dir: &Path,
    account: &FullKeypair,
    note_jsons: &[String],
) {
    let db_path = ndb_path(data_dir);
    std::fs::create_dir_all(&db_path).expect("create messages db dir");

    let ndb = Ndb::new(db_path.to_str().expect("db path"), &test_config()).expect("ndb");
    ndb.add_key(&account.secret_key.secret_bytes());

    for note_json in note_jsons {
        ndb.process_client_event(note_json)
            .expect("ingest pre-seeded local giftwrap");
    }

    let filters = [FilterBuilder::new()
        .kinds([1059])
        .pubkeys([account.pubkey.bytes()])
        .build()];
    wait_for_import_count(
        &ndb,
        &filters,
        32,
        note_jsons.len(),
        "timed out waiting for local giftwrap preload",
    );
}

/// Returns the local chat-message set stored in one device data dir before startup.
pub fn local_chat_messages_in_data_dir(data_dir: &Path, account: &FullKeypair) -> BTreeSet<String> {
    let ndb = open_ndb(data_dir);
    let txn = Transaction::new(&ndb).expect("txn");
    let filters = conversation_filter(&account.pubkey);
    let results = ndb
        .query(&txn, &filters, 1024)
        .expect("query local chat messages from disk");

    results
        .into_iter()
        .filter_map(|result| parse_chat_message(&result.note).map(|msg| msg.message.to_owned()))
        .collect()
}

/// Returns the raw local giftwrap wrapper timestamps stored before app startup.
pub fn local_giftwrap_created_ats_in_data_dir(data_dir: &Path, account: &FullKeypair) -> Vec<u64> {
    let ndb = open_ndb(data_dir);
    let txn = Transaction::new(&ndb).expect("txn");
    let filters = [FilterBuilder::new()
        .kinds([1059])
        .pubkeys([account.pubkey.bytes()])
        .build()];
    let results = ndb
        .query(&txn, &filters, 32)
        .expect("query local giftwrap wrappers from disk");

    results
        .into_iter()
        .map(|result| result.note.created_at())
        .collect()
}

/// Seeds a local-only kind `10050` DM relay-list note through the app's real path.
///
/// Uses a future timestamp so the seeded list always supersedes any default
/// relay list that `relay_ensure` might publish during device initialization.
pub fn seed_local_dm_relay_list(device: &mut DeviceHarness, account: &FullKeypair, relay: &str) {
    let future_ts = Some(notedeck::unix_time_secs() as u64 + 600);
    seed_local_dm_relay_list_with_relays(device, account, &[relay], future_ts);
}

/// Seeds a local-only kind `10050` DM relay-list note with explicit relay URLs and timestamp.
///
/// The note is both stored locally in NDB and published to the device's relays.
pub fn seed_local_dm_relay_list_with_relays(
    device: &mut DeviceHarness,
    account: &FullKeypair,
    relays: &[&str],
    created_at: Option<u64>,
) {
    seed_local_dm_relay_list_ndb_only_with_relays(device, account, relays, created_at);

    let note = build_dm_relay_list_note(account, relays, created_at);
    let ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&ctx);
    let mut publisher = app_ctx.remote.publisher(app_ctx.accounts);
    publisher.publish_note(&note, RelayType::AccountsWrite);
}

/// Seeds a kind `10050` DM relay-list note into NDB only (no relay publish).
///
/// Useful when a test needs to control relay routing without the relay list
/// propagating to other participants via the network.
///
/// Polls NDB until the note is indexed and queryable. This prevents a race
/// where `relay_ensure` runs on the next `step()` before NDB's async writer
/// has committed the seeded note, causing it to publish a default relay list
/// that overwrites the test seed.
pub fn seed_local_dm_relay_list_ndb_only_with_relays(
    device: &mut DeviceHarness,
    account: &FullKeypair,
    relays: &[&str],
    created_at: Option<u64>,
) {
    let note = build_dm_relay_list_note(account, relays, created_at);
    let expected_created_at = note.created_at();
    let note_json = note.json().expect("dm relay list note json");
    let ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&ctx);
    app_ctx
        .ndb
        .process_client_event(&note_json)
        .expect("ingest local dm relay list");

    // Poll until the note is visible in NDB. process_client_event is async
    // (writes to a pipeline processed by a background thread), so the note
    // may not be queryable immediately.
    let filter = participant_dm_relay_list_filter(&account.pubkey);
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        let txn = Transaction::new(app_ctx.ndb).expect("txn");
        if let Ok(results) = app_ctx.ndb.query(&txn, std::slice::from_ref(&filter), 1) {
            if results
                .first()
                .is_some_and(|r| r.note.created_at() >= expected_created_at)
            {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "seeded dm relay list for {} not visible in NDB after {:?}",
            account.pubkey,
            TEST_TIMEOUT
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Returns the latest DM relay-list relay URLs stored locally for one account.
pub fn local_dm_relay_list_relays(
    device: &mut DeviceHarness,
    account: &FullKeypair,
) -> Vec<String> {
    let ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&ctx);
    let filter = participant_dm_relay_list_filter(&account.pubkey);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    let results = app_ctx
        .ndb
        .query(&txn, std::slice::from_ref(&filter), 1)
        .expect("query local dm relay list");
    let Some(result) = results.first() else {
        return Vec::new();
    };

    parse_dm_relay_list_relays(&result.note)
        .into_iter()
        .map(|relay| relay.to_string())
        .collect()
}

/// Returns locally stored DM relay-list note versions for one account, newest first.
pub fn local_dm_relay_list_versions(
    device: &mut DeviceHarness,
    account: &FullKeypair,
) -> Vec<(u64, Vec<String>)> {
    let ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&ctx);
    let filter = FilterBuilder::new()
        .kinds([10050])
        .authors([account.pubkey.bytes()])
        .build();
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    let results = app_ctx
        .ndb
        .query(&txn, std::slice::from_ref(&filter), 8)
        .expect("query local dm relay list versions");

    results
        .into_iter()
        .map(|result| {
            (
                result.note.created_at(),
                parse_dm_relay_list_relays(&result.note)
                    .into_iter()
                    .map(|relay| relay.to_string())
                    .collect(),
            )
        })
        .collect()
}

/// Builds signed local kind `14` notes for one sender-to-recipient conversation subset.
pub fn build_local_chat_note_jsons(
    sender: &FullKeypair,
    recipient: &FullKeypair,
    messages: &[&str],
) -> Vec<String> {
    messages
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            NoteBuilder::new()
                .kind(14)
                .content(message)
                .created_at(1_700_000_000 + idx as u64)
                .start_tag()
                .tag_str("p")
                .tag_str(&recipient.pubkey.hex())
                .sign(&sender.secret_key.secret_bytes())
                .build()
                .expect("signed local chat note")
                .json()
                .expect("signed local chat note json")
        })
        .collect()
}

/// Builds a valid kind `1059` giftwrap with an explicit wrapper timestamp.
pub fn build_backdated_giftwrap_note(
    sender: &FullKeypair,
    recipient: &FullKeypair,
    message: &str,
    wrap_created_at: u64,
) -> Note<'static> {
    let rumor_json = EventBuilder::new(Kind::PrivateDirectMessage, message)
        .tags([Tag::public_key(nostr_pubkey(&recipient.pubkey))])
        .build(nostr_pubkey(&sender.pubkey))
        .as_json();

    let recipient_pk = nostr_pubkey(&recipient.pubkey);
    let mut rng = OsRng;
    let encrypted_rumor = nip44::encrypt_with_rng(
        &mut rng,
        &sender.secret_key,
        &recipient_pk,
        &rumor_json,
        nip44::Version::V2,
    )
    .expect("encrypt rumor");

    let seal_json = NoteBuilder::new()
        .kind(13)
        .content(&encrypted_rumor)
        .created_at(unix_time_secs())
        .sign(&sender.secret_key.secret_bytes())
        .build()
        .expect("seal note")
        .json()
        .expect("seal note json");

    let wrap_keys = FullKeypair::generate();
    let encrypted_seal = nip44::encrypt_with_rng(
        &mut rng,
        &wrap_keys.secret_key,
        &recipient_pk,
        &seal_json,
        nip44::Version::V2,
    )
    .expect("encrypt seal");

    NoteBuilder::new()
        .kind(1059)
        .content(&encrypted_seal)
        .created_at(wrap_created_at)
        .start_tag()
        .tag_str("p")
        .tag_str(&recipient.pubkey.hex())
        .sign(&wrap_keys.secret_key.secret_bytes())
        .build()
        .expect("giftwrap note")
}

/// Builds a valid kind `1059` note that cannot be decrypted by the intended recipient.
pub fn build_invalid_giftwrap_note(recipient: &FullKeypair, wrap_created_at: u64) -> Note<'static> {
    let wrap_keys = FullKeypair::generate();
    NoteBuilder::new()
        .kind(1059)
        .content("not-a-valid-nip44-payload")
        .created_at(wrap_created_at)
        .start_tag()
        .tag_str("p")
        .tag_str(&recipient.pubkey.hex())
        .sign(&wrap_keys.secret_key.secret_bytes())
        .build()
        .expect("invalid giftwrap note")
}

/// Builds a valid-looking kind `1059` note addressed to one recipient but encrypted for another.
pub fn build_misdirected_giftwrap_note(
    sender: &FullKeypair,
    tagged_recipient: &FullKeypair,
    encryption_recipient: &FullKeypair,
    message: &str,
    wrap_created_at: u64,
) -> Note<'static> {
    let rumor_json = EventBuilder::new(Kind::PrivateDirectMessage, message)
        .tags([Tag::public_key(nostr_pubkey(&tagged_recipient.pubkey))])
        .build(nostr_pubkey(&sender.pubkey))
        .as_json();

    let mut rng = OsRng;
    let encrypted_rumor = nip44::encrypt_with_rng(
        &mut rng,
        &sender.secret_key,
        &nostr_pubkey(&tagged_recipient.pubkey),
        &rumor_json,
        nip44::Version::V2,
    )
    .expect("encrypt rumor");

    let seal_json = NoteBuilder::new()
        .kind(13)
        .content(&encrypted_rumor)
        .created_at(unix_time_secs())
        .sign(&sender.secret_key.secret_bytes())
        .build()
        .expect("seal note")
        .json()
        .expect("seal note json");

    let wrap_keys = FullKeypair::generate();
    let encrypted_seal = nip44::encrypt_with_rng(
        &mut rng,
        &wrap_keys.secret_key,
        &nostr_pubkey(&encryption_recipient.pubkey),
        &seal_json,
        nip44::Version::V2,
    )
    .expect("encrypt misdirected seal");

    NoteBuilder::new()
        .kind(1059)
        .content(&encrypted_seal)
        .created_at(wrap_created_at)
        .start_tag()
        .tag_str("p")
        .tag_str(&tagged_recipient.pubkey.hex())
        .sign(&wrap_keys.secret_key.secret_bytes())
        .build()
        .expect("misdirected giftwrap note")
}

/// Builds a signed kind `10050` DM relay-list note with explicit relay URLs and timestamp.
fn build_dm_relay_list_note(
    account: &FullKeypair,
    relays: &[&str],
    created_at: Option<u64>,
) -> Note<'static> {
    let mut builder = NoteBuilder::new().kind(10050).content("");
    if let Some(created_at) = created_at {
        builder = builder.created_at(created_at);
    }
    for relay in relays {
        builder = builder.start_tag().tag_str("relay").tag_str(relay);
    }

    builder
        .sign(&account.secret_key.secret_bytes())
        .build()
        .expect("dm relay list note")
}
