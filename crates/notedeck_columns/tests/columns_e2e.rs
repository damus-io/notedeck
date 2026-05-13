//! Full-app end-to-end scenarios for Columns using a real local relay and full Notedeck hosts.

use std::time::Duration;

use egui_kittest::kittest::Queryable;
use enostr::FullKeypair;
use nostr::{Alphabet, Event, JsonUtil, Kind, SingleLetterTag};
use nostr_relay_builder::prelude::{MemoryDatabase, NostrEventsDatabase};
use nostrdb::{Filter, FilterBuilder, Ndb, NoteBuilder, Transaction};
use notedeck::{construct_people_list_note, filter, App, AppContext, AppResponse, RootNoteIdBuf};
use notedeck_columns::{
    timeline::{thread::Threads, ThreadSelection, TimelineCache, TimelineKind},
    Damus,
};
use notedeck_testing::{
    device::{build_device_in_tmpdir_with_relays, DeviceHarness},
    fixtures::{
        add_account_to_device, ndb_path, nostr_pubkey, select_account_on_device, test_config,
        wait_for_import_count,
    },
    init_tracing,
    ndb::LocalQuery,
    negentropy_relay::{run_memory_negentropy_relay, NegentropyRelay},
    stepping::wait_for_device_condition,
};
use serial_test::serial;
use tempfile::TempDir;
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

struct ThreadLoadApp {
    threads: Threads,
    selection: ThreadSelection,
    col: usize,
    opened: bool,
}

impl ThreadLoadApp {
    fn new(selection: ThreadSelection, col: usize) -> Self {
        Self {
            threads: Threads::default(),
            selection,
            col,
            opened: false,
        }
    }
}

impl App for ThreadLoadApp {
    fn update(&mut self, ctx: &mut AppContext<'_>, _egui_ctx: &egui::Context) {
        if self.opened {
            return;
        }

        let txn = Transaction::new(ctx.ndb).expect("txn");
        let mut scoped_subs = ctx.remote.scoped_subs(ctx.accounts);
        let _ = self.threads.open(
            ctx.ndb,
            &txn,
            &mut scoped_subs,
            &self.selection,
            true,
            self.col,
            0.0,
        );
        self.opened = true;
    }

    fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
        AppResponse::none()
    }
}

struct TimelineAndThreadLoadApp {
    timelines: TimelineCache,
    threads: Threads,
    kind: TimelineKind,
    selection: ThreadSelection,
    thread_col: usize,
    timeline_opened: bool,
    thread_opened: bool,
}

impl TimelineAndThreadLoadApp {
    fn new(kind: TimelineKind, selection: ThreadSelection, thread_col: usize) -> Self {
        Self {
            timelines: TimelineCache::default(),
            threads: Threads::default(),
            kind,
            selection,
            thread_col,
            timeline_opened: false,
            thread_opened: false,
        }
    }
}

struct SingleTimelineApp {
    timelines: TimelineCache,
    kind: TimelineKind,
    opened: bool,
}

impl SingleTimelineApp {
    fn new(kind: TimelineKind) -> Self {
        Self {
            timelines: TimelineCache::default(),
            kind,
            opened: false,
        }
    }
}

impl App for SingleTimelineApp {
    fn update(&mut self, ctx: &mut AppContext<'_>, _egui_ctx: &egui::Context) {
        if self.opened {
            return;
        }

        let txn = Transaction::new(ctx.ndb).expect("txn");
        let mut scoped_subs = ctx.remote.scoped_subs(ctx.accounts);
        let account_pk = scoped_subs.selected_account_pubkey();
        let _ = self.timelines.open(
            ctx.ndb,
            ctx.note_cache,
            &txn,
            &mut scoped_subs,
            &self.kind,
            account_pk,
            true,
        );
        self.opened = true;
    }

    fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
        AppResponse::none()
    }
}

impl App for TimelineAndThreadLoadApp {
    fn update(&mut self, ctx: &mut AppContext<'_>, _egui_ctx: &egui::Context) {
        let txn = Transaction::new(ctx.ndb).expect("txn");
        let mut scoped_subs = ctx.remote.scoped_subs(ctx.accounts);
        let account_pk = scoped_subs.selected_account_pubkey();

        if !self.timeline_opened {
            let _ = self.timelines.open(
                ctx.ndb,
                ctx.note_cache,
                &txn,
                &mut scoped_subs,
                &self.kind,
                account_pk,
                true,
            );
            self.timeline_opened = true;
        }

        if !self.thread_opened {
            let _ = self.threads.open(
                ctx.ndb,
                &txn,
                &mut scoped_subs,
                &self.selection,
                true,
                self.thread_col,
                0.0,
            );
            self.thread_opened = true;
        }
    }

    fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
        AppResponse::none()
    }
}

fn thread_app_factory(selection: ThreadSelection, col: usize) -> notedeck_testing::AppFactory {
    Box::new(move |notedeck, _ctx| {
        notedeck.set_app(ThreadLoadApp::new(selection, col));
    })
}

fn timeline_and_thread_app_factory(
    kind: TimelineKind,
    selection: ThreadSelection,
    col: usize,
) -> notedeck_testing::AppFactory {
    Box::new(move |notedeck, _ctx| {
        notedeck.set_app(TimelineAndThreadLoadApp::new(kind.clone(), selection, col));
    })
}

fn single_timeline_app_factory(kind: TimelineKind) -> notedeck_testing::AppFactory {
    Box::new(move |notedeck, _ctx| {
        notedeck.set_app(SingleTimelineApp::new(kind));
    })
}
fn build_columns_device(
    relay_url: &str,
    account: &FullKeypair,
    tmpdir: TempDir,
    app_factory: notedeck_testing::AppFactory,
) -> DeviceHarness {
    build_device_in_tmpdir_with_relays(&[relay_url], account, tmpdir, app_factory)
}
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
fn build_pubkey_tagged_text_note(
    account: &FullKeypair,
    tagged_pubkey: &enostr::Pubkey,
    content: &str,
    created_at: u64,
) -> nostrdb::Note<'static> {
    NoteBuilder::new()
        .kind(1)
        .content(content)
        .created_at(created_at)
        .start_tag()
        .tag_str("p")
        .tag_str(&tagged_pubkey.hex())
        .sign(&account.secret_key.secret_bytes())
        .build()
        .expect("pubkey-tagged text note")
}
fn build_giftwrap_note(
    account: &FullKeypair,
    recipient: &enostr::Pubkey,
    content: &str,
    created_at: u64,
) -> nostrdb::Note<'static> {
    NoteBuilder::new()
        .kind(1059)
        .content(content)
        .created_at(created_at)
        .start_tag()
        .tag_str("p")
        .tag_str(&recipient.hex())
        .sign(&account.secret_key.secret_bytes())
        .build()
        .expect("giftwrap note")
}
fn build_reply_note(
    account: &FullKeypair,
    replying_to: &nostrdb::Note<'_>,
    content: &str,
    created_at: u64,
) -> nostrdb::Note<'static> {
    NoteBuilder::new()
        .kind(1)
        .content(content)
        .created_at(created_at)
        .start_tag()
        .tag_str("e")
        .tag_str(&hex::encode(replying_to.id()))
        .tag_str("")
        .tag_str("root")
        .sign(&account.secret_key.secret_bytes())
        .build()
        .expect("reply note")
}
fn rendered_note_count(device: &DeviceHarness, substring: &str) -> usize {
    device.query_all_by_label_contains(substring).count()
}
fn seed_notes_in_tmpdir(tmpdir: &TempDir, note_jsons: &[String], kind: u64) {
    let db_path = ndb_path(tmpdir.path());
    std::fs::create_dir_all(&db_path).expect("create db dir");

    let ndb = Ndb::new(db_path.to_str().expect("db path"), &test_config()).expect("ndb");
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
struct HomeTimelineFixture {
    alice: FullKeypair,
    relay_url: String,
    tmpdir: TempDir,
    // Keep the relay alive for the duration of the test
    _relay: NegentropyRelay,
}
struct ThreadFixture {
    alice: FullKeypair,
    carol: FullKeypair,
    root: nostrdb::Note<'static>,
    root_id: [u8; 32],
    relay_db: MemoryDatabase,
    relay_url: String,
    tmpdir: TempDir,
    // Keep the relay alive for the duration of the test and expose captured
    // negentropy traffic for failure-path assertions.
    relay: NegentropyRelay,
}
struct TimelineAndThreadFixture {
    alice: FullKeypair,
    bob: FullKeypair,
    carol: FullKeypair,
    root_id: [u8; 32],
    relay_url: String,
    tmpdir: TempDir,
    // Keep the relay alive for the duration of the test
    _relay: NegentropyRelay,
}
struct LiveOnlyTimelineFixture {
    alice: FullKeypair,
    bob: FullKeypair,
    relay_url: String,
    tmpdir: TempDir,
    // Keep the relay alive for the duration of the test and expose captured
    // negentropy traffic for wire-level assertions.
    relay: NegentropyRelay,
}
#[derive(Clone, Copy)]
enum LiveOnlySeed<'a> {
    Home,
    Profile,
    Notifications,
    Search { query: &'a str },
    PeopleList { identifier: &'a str },
}
struct GiftwrapFixture {
    alice: FullKeypair,
    relay_url: String,
    tmpdir: TempDir,
    // Keep the relay alive for the duration of the test and expose captured
    // negentropy traffic for wire-level assertions.
    relay: NegentropyRelay,
}
async fn setup_home_timeline_fixture() -> HomeTimelineFixture {
    let (relay_db, relay_url, relay) = setup_relay().await;
    let alice = FullKeypair::generate();
    let bob = FullKeypair::generate();
    let carol = FullKeypair::generate();

    // Seed kind-1 notes from bob and carol
    for i in 0u64..5 {
        let bob_note = build_text_note(&bob, &format!("bob note {i}"), 1_700_000_000 + i);
        let carol_note = build_text_note(&carol, &format!("carol note {i}"), 1_700_000_000 + i);
        for note in [&bob_note, &carol_note] {
            save_note(&relay_db, note).await;
        }
    }

    // Seed alice's contact list
    let contact_list = construct_contact_list_note(vec![bob.pubkey, carol.pubkey])
        .sign(&alice.secret_key.secret_bytes())
        .build()
        .expect("contact list note");
    save_note(&relay_db, &contact_list).await;

    for (account, name) in [(&bob, "bob"), (&carol, "carol")] {
        let profile = NoteBuilder::new()
            .kind(0)
            .content(&format!(r#"{{"name":"{name}","display_name":"{name}"}}"#))
            .sign(&account.secret_key.secret_bytes())
            .build()
            .expect("profile note");
        save_note(&relay_db, &profile).await;
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
async fn setup_relay() -> (MemoryDatabase, String, NegentropyRelay) {
    let remote = run_memory_negentropy_relay()
        .await
        .expect("start local relay");
    let relay_url = remote.relay.url().to_owned();
    (remote.db, relay_url, remote.relay)
}
fn construct_contact_list_note<'a>(pks: Vec<enostr::Pubkey>) -> NoteBuilder<'a> {
    let mut builder = NoteBuilder::new()
        .content("")
        .kind(3)
        .options(nostrdb::NoteBuildOptions::default());

    for pk in pks {
        builder = builder.start_tag().tag_str("p").tag_str(&pk.hex());
    }

    builder
}
async fn save_note(relay_db: &MemoryDatabase, note: &nostrdb::Note<'_>) {
    let event = Event::from_json(note.json().expect("json")).expect("parse event");
    relay_db.save_event(&event).await.expect("save event");
}
async fn setup_thread_fixture(reply_count: usize) -> ThreadFixture {
    let (relay_db, relay_url, relay) = setup_relay().await;
    let alice = FullKeypair::generate();
    let carol = FullKeypair::generate();
    let root = build_text_note(&carol, "thread root", 1_700_100_000);
    save_note(&relay_db, &root).await;

    for i in 0..reply_count as u64 {
        let reply = build_reply_note(
            &carol,
            &root,
            &format!("thread reply {i}"),
            1_700_100_100 + i,
        );
        save_note(&relay_db, &reply).await;
    }

    ThreadFixture {
        alice,
        carol,
        root: root.clone(),
        root_id: *root.id(),
        relay_db,
        relay_url,
        tmpdir: TempDir::new().expect("tmpdir"),
        relay,
    }
}
async fn setup_timeline_and_thread_fixture(
    bob_note_count: usize,
    carol_reply_count: usize,
) -> TimelineAndThreadFixture {
    let (relay_db, relay_url, relay) = setup_relay().await;
    let alice = FullKeypair::generate();
    let bob = FullKeypair::generate();
    let carol = FullKeypair::generate();

    for i in 0..bob_note_count as u64 {
        let note = build_text_note(&bob, &format!("bob home {i}"), 1_700_200_000 + i);
        save_note(&relay_db, &note).await;
    }

    let root = build_text_note(&carol, "carol thread root", 1_700_300_000);
    save_note(&relay_db, &root).await;
    for i in 0..carol_reply_count as u64 {
        let reply = build_reply_note(
            &carol,
            &root,
            &format!("carol thread reply {i}"),
            1_700_300_100 + i,
        );
        save_note(&relay_db, &reply).await;
    }

    let contact_list = construct_contact_list_note(vec![bob.pubkey])
        .sign(&alice.secret_key.secret_bytes())
        .build()
        .expect("contact list note");
    let tmpdir = TempDir::new().expect("tmpdir");
    let contact_json = contact_list.json().expect("contact list json");
    seed_notes_in_tmpdir(&tmpdir, &[contact_json], 3);

    TimelineAndThreadFixture {
        alice,
        bob,
        carol,
        root_id: *root.id(),
        relay_url,
        tmpdir,
        _relay: relay,
    }
}

async fn setup_live_only_fixture(
    note_count: usize,
    seed: LiveOnlySeed<'_>,
) -> LiveOnlyTimelineFixture {
    let (relay_db, relay_url, relay) = setup_relay().await;
    let alice = FullKeypair::generate();
    let bob = FullKeypair::generate();

    for i in 0..note_count as u64 {
        let note = match seed {
            LiveOnlySeed::Home => {
                build_text_note(&bob, &format!("bob history {i}"), 1_700_000_000 + i)
            }
            LiveOnlySeed::Profile => {
                build_text_note(&bob, &format!("profile history {i}"), 1_700_400_000 + i)
            }
            LiveOnlySeed::Notifications => build_pubkey_tagged_text_note(
                &bob,
                &alice.pubkey,
                &format!("notification history {i}"),
                1_700_500_000 + i,
            ),
            LiveOnlySeed::Search { query } => build_text_note(
                &bob,
                &format!("{query} search history {i}"),
                1_700_600_000 + i,
            ),
            LiveOnlySeed::PeopleList { .. } => {
                build_text_note(&bob, &format!("people list history {i}"), 1_700_700_000 + i)
            }
        };
        save_note(&relay_db, &note).await;
    }

    let tmpdir = TempDir::new().expect("tmpdir");
    if matches!(seed, LiveOnlySeed::Home) {
        let contact_list = construct_contact_list_note(vec![bob.pubkey])
            .sign(&alice.secret_key.secret_bytes())
            .build()
            .expect("contact list note");
        save_note(&relay_db, &contact_list).await;

        let profile = NoteBuilder::new()
            .kind(0)
            .content(r#"{"name":"bob","display_name":"bob"}"#)
            .sign(&bob.secret_key.secret_bytes())
            .build()
            .expect("profile note");
        save_note(&relay_db, &profile).await;

        let contact_json = contact_list.json().expect("contact list json");
        seed_notes_in_tmpdir(&tmpdir, &[contact_json], 3);
    }

    if let LiveOnlySeed::PeopleList { identifier } = seed {
        let people_list = construct_people_list_note(identifier, &[bob.pubkey])
            .sign(&alice.secret_key.secret_bytes())
            .build()
            .expect("people list note");
        save_note(&relay_db, &people_list).await;

        let list_json = people_list.json().expect("people list json");
        seed_notes_in_tmpdir(&tmpdir, &[list_json], 30000);
    }

    LiveOnlyTimelineFixture {
        alice,
        bob,
        relay_url,
        tmpdir,
        relay,
    }
}

async fn run_live_only_timeline_case(
    seed: LiveOnlySeed<'_>,
    expected_limit: usize,
    app_factory: impl FnOnce(&LiveOnlyTimelineFixture) -> notedeck_testing::AppFactory,
    context: &str,
) {
    let total_notes = expected_limit + 40;
    let fixture = setup_live_only_fixture(total_notes, seed).await;
    let app_factory = app_factory(&fixture);
    let mut device = build_columns_device(
        &fixture.relay_url,
        &fixture.alice,
        fixture.tmpdir,
        app_factory,
    );

    let imported = match seed {
        LiveOnlySeed::Home | LiveOnlySeed::Profile => author_note_query(&fixture.bob)
            .wait_for_count_plateau(
                &mut device,
                expected_limit.saturating_sub(1),
                8,
                Duration::from_secs(20),
                context,
            ),
        LiveOnlySeed::Notifications
        | LiveOnlySeed::Search { .. }
        | LiveOnlySeed::PeopleList { .. } => {
            let query = author_note_query(&fixture.bob);
            let import_context = format!("{context} live-only import");
            query.wait_for_count(
                &mut device,
                expected_limit,
                Duration::from_secs(20),
                &import_context,
            );
            let stable_context = format!("{context} should not backfill beyond live limit");
            query.assert_count_stable(&mut device, expected_limit, 8, &stable_context);
            query.count(&mut device)
        }
    };

    assert!(
        imported <= expected_limit,
        "{context} should stay within live limit {expected_limit}, got {imported}"
    );
    assert!(
        imported < total_notes,
        "{context} should not complete full-history backfill, got {imported}/{total_notes}"
    );

    if matches!(seed, LiveOnlySeed::Profile) {
        assert!(
            fixture
                .relay
                .neg_open_count(profile_filter_matcher(&fixture.bob))
                == 0,
            "profile timeline should not open negentropy"
        );
    }
}

async fn setup_giftwrap_fixture(note_count: usize) -> GiftwrapFixture {
    let (relay_db, relay_url, relay) = setup_relay().await;
    let alice = FullKeypair::generate();
    let sender = FullKeypair::generate();

    for i in 0..note_count as u64 {
        let note = build_giftwrap_note(
            &sender,
            &alice.pubkey,
            &format!("giftwrap history {i}"),
            1_700_800_000 + i,
        );
        save_note(&relay_db, &note).await;
    }

    GiftwrapFixture {
        alice,
        relay_url,
        tmpdir: TempDir::new().expect("tmpdir"),
        relay,
    }
}
fn wait_for_contact_notes(device: &mut DeviceHarness) {
    wait_for_device_condition(
        device,
        Duration::from_secs(15),
        "home timeline to render notes",
        |device| {
            let bob_count = rendered_note_count(device, "bob note");
            let carol_count = rendered_note_count(device, "carol note");
            if bob_count > 0 && carol_count > 0 {
                Ok(())
            } else {
                Err(format!(
                    "bob notes visible: {bob_count}, carol notes visible: {carol_count}"
                ))
            }
        },
    );
}
fn author_note_query(author: &FullKeypair) -> LocalQuery {
    LocalQuery::new(
        vec![author_note_filter(author)],
        2048,
        "query local author notes",
    )
}
fn giftwrap_query(recipient: &FullKeypair) -> LocalQuery {
    LocalQuery::new(
        vec![giftwrap_filter(recipient)],
        2048,
        "query local giftwrap notes",
    )
}
fn author_note_filter(author: &FullKeypair) -> Filter {
    FilterBuilder::new()
        .authors([author.pubkey.bytes()])
        .kinds([1])
        .build()
}
fn giftwrap_filter(recipient: &FullKeypair) -> Filter {
    FilterBuilder::new()
        .kinds([1059])
        .pubkeys([recipient.pubkey.bytes()])
        .build()
}
fn profile_filter_matcher(author: &FullKeypair) -> impl FnMut(&nostr::Filter) -> bool {
    let author_pubkey = nostr_pubkey(&author.pubkey);
    move |filter| is_profile_filter(filter, &author_pubkey)
}
fn giftwrap_filter_matcher(recipient: &FullKeypair) -> impl FnMut(&nostr::Filter) -> bool {
    let recipient_pubkey = nostr_pubkey(&recipient.pubkey);
    move |filter| is_giftwrap_filter(filter, &recipient_pubkey)
}
fn thread_filter_matcher(root_id: &[u8; 32]) -> impl FnMut(&nostr::Filter) -> bool + '_ {
    move |filter| is_thread_replies_filter(filter, root_id)
}
fn is_profile_filter(filter: &nostr::Filter, author: &nostr::PublicKey) -> bool {
    let Some(authors) = &filter.authors else {
        return false;
    };
    if authors.len() != 1 || !authors.contains(author) {
        return false;
    }

    let Some(kinds) = &filter.kinds else {
        return false;
    };
    kinds.contains(&Kind::TextNote)
}
fn is_giftwrap_filter(filter: &nostr::Filter, recipient: &nostr::PublicKey) -> bool {
    let Some(kinds) = &filter.kinds else {
        return false;
    };
    if !kinds.contains(&Kind::GiftWrap) {
        return false;
    }

    let p_tag = SingleLetterTag::lowercase(Alphabet::P);
    let Some(pubkeys) = filter.generic_tags.get(&p_tag) else {
        return false;
    };

    pubkeys.len() == 1 && pubkeys.contains(&recipient.to_hex())
}
fn is_thread_replies_filter(filter: &nostr::Filter, root_id: &[u8; 32]) -> bool {
    let Some(kinds) = &filter.kinds else {
        return false;
    };
    if !kinds.contains(&Kind::TextNote) {
        return false;
    }

    let e_tag = SingleLetterTag::lowercase(Alphabet::E);
    let Some(events) = filter.generic_tags.get(&e_tag) else {
        return false;
    };

    let root_id = hex::encode(root_id);
    events.len() == 1 && events.contains(&root_id)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn home_timeline_fills_from_contacts_e2e() {
    init_tracing();

    let fixture = setup_home_timeline_fixture().await;
    let mut device = build_columns_device(
        &fixture.relay_url,
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
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn home_timeline_stays_live_only_without_full_history_e2e() {
    init_tracing();

    run_live_only_timeline_case(
        LiveOnlySeed::Home,
        filter::default_remote_limit() as usize,
        |_| columns_app_factory(),
        "home timeline should stay live-only",
    )
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn profile_timeline_stays_live_only_without_full_history_e2e() {
    init_tracing();

    run_live_only_timeline_case(
        LiveOnlySeed::Profile,
        filter::default_remote_limit() as usize,
        |fixture| single_timeline_app_factory(TimelineKind::profile(fixture.bob.pubkey)),
        "profile timeline should stay live-only",
    )
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn notifications_timeline_stays_live_only_without_full_history_e2e() {
    init_tracing();

    run_live_only_timeline_case(
        LiveOnlySeed::Notifications,
        filter::default_limit() as usize,
        |fixture| single_timeline_app_factory(TimelineKind::notifications(fixture.alice.pubkey)),
        "notifications timeline",
    )
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn search_timeline_stays_live_only_without_full_history_e2e() {
    init_tracing();

    let query = "history-query";
    run_live_only_timeline_case(
        LiveOnlySeed::Search { query },
        filter::default_limit() as usize,
        |_| single_timeline_app_factory(TimelineKind::search(query.to_owned())),
        "search timeline",
    )
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn people_list_timeline_stays_live_only_without_full_history_e2e() {
    init_tracing();

    run_live_only_timeline_case(
        LiveOnlySeed::PeopleList {
            identifier: "friends",
        },
        filter::default_remote_limit() as usize,
        |fixture| {
            single_timeline_app_factory(TimelineKind::people_list(
                fixture.alice.pubkey,
                "friends".to_owned(),
            ))
        },
        "people-list timeline",
    )
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn giftwrap_sub_backfills_beyond_live_limit_e2e() {
    init_tracing();

    let live_limit = 500usize;
    let total_giftwraps = live_limit + 20;
    let fixture = setup_giftwrap_fixture(total_giftwraps).await;
    let mut device = build_columns_device(
        &fixture.relay_url,
        &fixture.alice,
        fixture.tmpdir,
        columns_app_factory(),
    );

    let giftwrap_query = giftwrap_query(&fixture.alice);
    giftwrap_query.wait_for_count(
        &mut device,
        total_giftwraps,
        Duration::from_secs(20),
        "giftwrap full-history import",
    );

    assert_eq!(giftwrap_query.count(&mut device), total_giftwraps);
    assert!(
        fixture
            .relay
            .neg_open_count(giftwrap_filter_matcher(&fixture.alice))
            > 0,
        "giftwrap import should open negentropy"
    );
    assert!(
        fixture.relay.count_captured_prefix("[\"NEG-CLOSE\",") > 0,
        "giftwrap import should close a completed negentropy session"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn thread_loads_full_reply_set_e2e() {
    init_tracing();

    let remote_limit = filter::default_remote_limit() as usize;
    let reply_count = remote_limit + 40;
    let fixture = setup_thread_fixture(reply_count).await;
    let selection = ThreadSelection::from_root_id(RootNoteIdBuf::new_unsafe(fixture.root_id));
    let mut device = build_columns_device(
        &fixture.relay_url,
        &fixture.alice,
        fixture.tmpdir,
        thread_app_factory(selection, 7),
    );

    let carol_notes = author_note_query(&fixture.carol);
    carol_notes.wait_for_count(
        &mut device,
        reply_count + 1,
        Duration::from_secs(20),
        "thread reply import",
    );

    let imported = carol_notes.count(&mut device);
    assert!(
        imported > remote_limit,
        "expected thread import to exceed live remote limit {remote_limit}, got {imported}"
    );
    assert_eq!(
        imported,
        reply_count + 1,
        "expected thread root plus all replies to be imported"
    );
    assert!(
        fixture
            .relay
            .neg_open_count(thread_filter_matcher(&fixture.root_id))
            > 0,
        "thread import should open negentropy"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn account_switch_restores_thread_subscription_e2e() {
    init_tracing();

    let remote_limit = filter::default_remote_limit() as usize;
    let initial_replies = remote_limit + 30;
    let older_replies = 25usize;
    let fixture = setup_thread_fixture(initial_replies).await;
    let selection = ThreadSelection::from_root_id(RootNoteIdBuf::new_unsafe(fixture.root_id));
    let mut device = build_columns_device(
        &fixture.relay_url,
        &fixture.alice,
        fixture.tmpdir,
        thread_app_factory(selection, 7),
    );

    let carol_notes = author_note_query(&fixture.carol);
    carol_notes.wait_for_count(
        &mut device,
        initial_replies + 1,
        Duration::from_secs(20),
        "initial thread import before account switch",
    );

    let dave = FullKeypair::generate();
    add_account_to_device(&mut device, &dave);
    select_account_on_device(&mut device, &dave);

    for i in 0..older_replies as u64 {
        let reply = build_reply_note(
            &fixture.carol,
            &fixture.root,
            &format!("thread older reply {i}"),
            1_700_099_000 + i,
        );
        save_note(&fixture.relay_db, &reply).await;
    }

    carol_notes.assert_count_stable(
        &mut device,
        initial_replies + 1,
        8,
        "thread should not keep backfilling while its account is deselected",
    );

    select_account_on_device(&mut device, &fixture.alice);

    carol_notes.wait_for_count(
        &mut device,
        initial_replies + older_replies + 1,
        Duration::from_secs(20),
        "restored thread import after account switch",
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn profile_live_loading_and_thread_loading_progress_concurrently_e2e() {
    init_tracing();

    let remote_limit = filter::default_remote_limit() as usize;
    let bob_note_count = remote_limit + 30;
    let carol_reply_count = remote_limit + 20;
    let fixture = setup_timeline_and_thread_fixture(bob_note_count, carol_reply_count).await;
    let selection = ThreadSelection::from_root_id(RootNoteIdBuf::new_unsafe(fixture.root_id));
    let mut device = build_columns_device(
        &fixture.relay_url,
        &fixture.alice,
        fixture.tmpdir,
        timeline_and_thread_app_factory(TimelineKind::profile(fixture.bob.pubkey), selection, 11),
    );

    wait_for_device_condition(
        &mut device,
        Duration::from_secs(20),
        "concurrent profile live load + thread load",
        |device| {
            let bob_imported = author_note_query(&fixture.bob).count(device);
            let carol_imported = author_note_query(&fixture.carol).count(device);
            if bob_imported >= remote_limit && carol_imported > carol_reply_count {
                Ok(())
            } else {
                Err(format!(
                    "bob imported {bob_imported}/{bob_note_count}, carol imported {carol_imported}/{}",
                    carol_reply_count + 1
                ))
            }
        },
    );

    assert_eq!(
        author_note_query(&fixture.bob).count(&mut device),
        remote_limit
    );
    assert_eq!(
        author_note_query(&fixture.carol).count(&mut device),
        carol_reply_count + 1
    );
}
