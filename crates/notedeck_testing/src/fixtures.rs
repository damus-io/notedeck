//! General fixture builders and NostrDB helpers for E2E tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use enostr::{FullKeypair, ProfileState, Pubkey};
use nostr::key::PublicKey;
use nostrdb::{Config, FilterBuilder, Ndb, NoteBuilder, Transaction};
use notedeck::{DataPath, DataPathType, RelayType};

use crate::cluster::AccountCluster;
use crate::device::DeviceHarness;

/// Seeds known local profile metadata on every device in the cluster.
pub fn seed_cluster_known_profiles(
    cluster: &mut AccountCluster,
    profiles: &[(FullKeypair, &'static str)],
) {
    for device in &mut cluster.devices {
        for (account, display_name) in profiles {
            seed_local_profile_metadata(device, account, display_name);
        }
    }
}

/// Seeds raw local note JSON into one device's on-disk NostrDB before app startup.
pub fn seed_local_notes_in_data_dir(data_dir: &Path, note_jsons: &[String]) {
    let db_path = ndb_path(data_dir);
    fs::create_dir_all(&db_path).expect("create db dir");

    let ndb = Ndb::new(db_path.to_str().expect("db path"), &Config::new()).expect("ndb");
    for note_json in note_jsons {
        ndb.process_client_event(note_json)
            .expect("ingest pre-seeded local note history");
    }

    let filters = [FilterBuilder::new().kinds([14]).build()];
    wait_for_import_count(
        &ndb,
        &filters,
        512,
        note_jsons.len(),
        "timed out waiting for local note preload",
    );
}

/// Seeds a local-only kind `0` profile note so tests can resolve participants.
pub fn seed_local_profile_metadata(
    device: &mut DeviceHarness,
    account: &FullKeypair,
    display_name: &str,
) {
    let mut profile = ProfileState::default();
    *profile.str_mut("name") = display_name.to_owned();
    *profile.str_mut("display_name") = display_name.to_owned();

    let note = NoteBuilder::new()
        .kind(0)
        .content(&profile.to_json())
        .sign(&account.secret_key.secret_bytes())
        .build()
        .expect("profile metadata note");

    let note_json = note.json().expect("profile metadata note json");
    let ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&ctx);
    app_ctx
        .ndb
        .process_client_event(&note_json)
        .expect("ingest local profile metadata");

    let mut publisher = app_ctx.remote.publisher(app_ctx.accounts);
    publisher.publish_note(&note, RelayType::AccountsWrite);
}

/// Returns the canonical NostrDB directory for one device data path.
pub fn ndb_path(data_dir: &Path) -> PathBuf {
    DataPath::new(data_dir).path(DataPathType::Db)
}

/// Opens a NostrDB for one prepared device data directory.
pub fn open_ndb(data_dir: &Path) -> Ndb {
    let db_path = ndb_path(data_dir);
    Ndb::new(db_path.to_str().expect("db path"), &Config::new()).expect("ndb")
}

/// Waits until at least `expected_count` events are queryable through the given filters.
pub fn wait_for_import_count(
    ndb: &Ndb,
    filters: &[nostrdb::Filter],
    limit: i32,
    expected_count: usize,
    context: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(2);

    loop {
        let txn = Transaction::new(ndb).expect("txn");
        let imported = ndb
            .query(&txn, filters, limit)
            .expect("query imported notes")
            .len();

        if imported >= expected_count {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "{context}; expected at least {expected_count}, imported {imported}"
        );

        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Converts an enostr pubkey into the nostr crate pubkey type.
pub fn nostr_pubkey(pk: &Pubkey) -> PublicKey {
    PublicKey::from_slice(pk.bytes()).expect("valid pubkey")
}

/// Adds another full account to the device and processes the resulting unknown-id action.
pub fn add_account_to_device(device: &mut DeviceHarness, account: &FullKeypair) {
    let ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&ctx);
    let Some(response) = app_ctx
        .accounts
        .add_account(enostr::Keypair::from_secret(account.secret_key.clone()))
    else {
        return;
    };

    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    response
        .unk_id_action
        .process_action(app_ctx.unknown_ids, app_ctx.ndb, &txn);
}

/// Switches the device to the provided account through the real app context.
pub fn select_account_on_device(device: &mut DeviceHarness, account: &FullKeypair) {
    let ctx = device.ctx.clone();
    {
        let app_ctx = &mut device.state_mut().notedeck.app_context(&ctx);
        app_ctx.select_account(&account.pubkey);
    }
    device.step();
}

/// Publishes one already-built note through the device's real remote publisher.
pub fn publish_note_via_device(device: &mut DeviceHarness, note: &nostrdb::Note<'_>) {
    let ctx = device.ctx.clone();
    {
        let app_ctx = &mut device.state_mut().notedeck.app_context(&ctx);
        let mut publisher = app_ctx.remote.publisher(app_ctx.accounts);
        publisher.publish_note(note, RelayType::AccountsWrite);
    }

    device.step();
    std::thread::sleep(Duration::from_millis(25));
    device.step();
}
