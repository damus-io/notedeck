//! Full-app end-to-end scenarios for Columns using a real local relay and full Notedeck hosts.

use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use enostr::FullKeypair;
use nostr::{Event, JsonUtil};
use nostr_relay_builder::{
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use nostrdb::{Config, FilterBuilder, Ndb, NoteBuilder};
use notedeck_columns::Damus;
use notedeck_testing::{
    device::{build_device_in_tmpdir_with_relays, DeviceHarness},
    fixtures::{ndb_path, wait_for_import_count},
    init_tracing,
};
use serial_test::serial;
use tempfile::TempDir;

/// App factory that installs the Columns app on a device with a Home timeline.
fn columns_app_factory() -> notedeck_testing::AppFactory {
    Box::new(|notedeck, ctx| {
        let args = vec!["--column".to_string(), "contacts".to_string()];
        let mut app_ctx = notedeck.app_context(ctx);
        // Skip onboarding/welcome screen for tests
        app_ctx.settings.complete_welcome();
        let damus = Damus::new(&mut app_ctx, &args);
        drop(app_ctx);
        notedeck.set_app(damus);
    })
}

/// Builds a signed kind-3 contact list note with the given contacts.
fn build_contact_list_note(
    account: &FullKeypair,
    contacts: &[&FullKeypair],
) -> nostrdb::Note<'static> {
    let mut builder = NoteBuilder::new().kind(3).content("");
    for contact in contacts {
        builder = builder
            .start_tag()
            .tag_str("p")
            .tag_str(&contact.pubkey.hex());
    }
    builder
        .sign(&account.secret_key.secret_bytes())
        .build()
        .expect("contact list note")
}

/// Builds a signed kind-1 text note.
fn build_text_note(
    account: &FullKeypair,
    content: &str,
    created_at: u64,
) -> nostrdb::Note<'static> {
    NoteBuilder::new()
        .kind(1)
        .content(content)
        .created_at(created_at)
        .sign(&account.secret_key.secret_bytes())
        .build()
        .expect("text note")
}

/// Returns the count of note labels containing the given substring visible in the UI.
fn rendered_note_count(device: &DeviceHarness, substring: &str) -> usize {
    device.query_all_by_label_contains(substring).count()
}

/// Pre-seeds notes into a tmpdir's ndb so they're available at device boot time.
fn seed_notes_in_tmpdir(tmpdir: &TempDir, note_jsons: &[String], kind: u64) {
    let db_path = ndb_path(tmpdir.path());
    std::fs::create_dir_all(&db_path).expect("create db dir");

    let ndb = Ndb::new(db_path.to_str().expect("db path"), &Config::new()).expect("ndb");
    for note_json in note_jsons {
        ndb.process_client_event(note_json)
            .expect("ingest pre-seeded note");
    }

    let filters = [FilterBuilder::new().kinds([kind]).build()];
    wait_for_import_count(
        &ndb,
        &filters,
        512,
        note_jsons.len(),
        &format!("timed out waiting for kind-{kind} preload"),
    );
}

/// Shared test fixture: relay with pre-seeded notes and a prepared tmpdir.
struct HomeTimelineFixture {
    alice: FullKeypair,
    relay_url: String,
    tmpdir: TempDir,
    // Keep the relay alive for the duration of the test
    _relay: LocalRelay,
}

/// Creates the shared fixture for home timeline tests:
/// - alice (user), bob and carol (contacts)
/// - relay pre-seeded with bob+carol's kind-1 notes and profiles
/// - alice's contact list pre-seeded in local ndb
async fn setup_home_timeline_fixture() -> HomeTimelineFixture {
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
    let carol = FullKeypair::generate();

    // Seed kind-1 notes from bob and carol
    for i in 0u64..5 {
        let bob_note = build_text_note(&bob, &format!("bob note {i}"), 1_700_000_000 + i);
        let carol_note = build_text_note(&carol, &format!("carol note {i}"), 1_700_000_000 + i);
        for note in [&bob_note, &carol_note] {
            let event = Event::from_json(&note.json().expect("json")).expect("parse event");
            relay_db.save_event(&event).await.expect("save event");
        }
    }

    // Seed alice's contact list
    let contact_list = build_contact_list_note(&alice, &[&bob, &carol]);
    let contact_event =
        Event::from_json(&contact_list.json().expect("json")).expect("parse contact list");
    relay_db
        .save_event(&contact_event)
        .await
        .expect("save contact list");

    // Seed profile metadata
    for (account, name) in [(&bob, "bob"), (&carol, "carol")] {
        let profile = NoteBuilder::new()
            .kind(0)
            .content(&format!(r#"{{"name":"{name}","display_name":"{name}"}}"#))
            .sign(&account.secret_key.secret_bytes())
            .build()
            .expect("profile note");
        let event = Event::from_json(&profile.json().expect("json")).expect("parse profile");
        relay_db.save_event(&event).await.expect("save profile");
    }

    // Pre-seed alice's contact list into local ndb
    let tmpdir = TempDir::new().expect("tmpdir");
    let contact_json = contact_list.json().expect("contact list json");
    seed_notes_in_tmpdir(&tmpdir, &[contact_json], 3);

    HomeTimelineFixture {
        alice,
        relay_url,
        tmpdir,
        _relay: relay,
    }
}

/// Waits for notes from both contacts to appear in the rendered timeline.
fn wait_for_contact_notes(device: &mut DeviceHarness) {
    let deadline = Instant::now() + Duration::from_secs(15);

    loop {
        device.step();

        let bob_count = rendered_note_count(device, "bob note");
        let carol_count = rendered_note_count(device, "carol note");
        if bob_count > 0 && carol_count > 0 {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for home timeline to render notes; \
             bob notes visible: {bob_count}, carol notes visible: {carol_count}"
        );

        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Home timeline fills with notes from contacts fetched from a local relay.
///
/// Setup:
/// 1. Create alice (the user), bob and carol (contacts)
/// 2. Pre-seed the relay with bob+carol's kind-1 notes and profiles
/// 3. Pre-seed alice's contact list into the local ndb before device boot
/// 4. Boot a Columns device for alice — the Home timeline should fill
///
/// Assertion: notes from bob+carol are rendered in the timeline UI.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn home_timeline_fills_from_contacts_e2e() {
    init_tracing();

    let fixture = setup_home_timeline_fixture().await;
    let mut device = build_device_in_tmpdir_with_relays(
        &[&fixture.relay_url],
        &fixture.alice,
        fixture.tmpdir,
        columns_app_factory(),
    );

    wait_for_contact_notes(&mut device);

    let bob_count = rendered_note_count(&device, "bob note");
    let carol_count = rendered_note_count(&device, "carol note");
    assert!(
        bob_count > 0 && carol_count > 0,
        "expected notes from both contacts to be rendered; \
         bob notes visible: {bob_count}, carol notes visible: {carol_count}"
    );
}
