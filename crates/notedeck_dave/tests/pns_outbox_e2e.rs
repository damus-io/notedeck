//! End-to-end coverage for Dave PNS discovery through the shared outbox path.

use std::{
    path::Path,
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

use egui_kittest::kittest::Queryable;
use enostr::FullKeypair;
use nostr::{Event, JsonUtil, Kind};
use nostr_relay_builder::prelude::{MemoryDatabase, NostrEventsDatabase};
use nostrdb::{Filter, FilterBuilder, NoteBuilder, Transaction};
use notedeck::{App, AppContext, AppResponse, DataPathType, RelayAction};
use notedeck_dave::{session_events, session_loader, AiProvider, Dave, DaveSettings};
use notedeck_testing::{
    fixtures::{
        add_account_to_device, nostr_pubkey, process_relay_action_on_device,
        select_account_on_device,
    },
    init_tracing,
    ndb::{wait_for_two_local_query_counts, LocalQuery},
    negentropy_relay::{
        run_memory_negentropy_relay_with_mode, NegentropyRelay, NegentropyRelayMode,
    },
    stepping::{step_device_for, wait_for_device_condition},
    AppFactory, DeviceHarness,
};
use serial_test::serial;
use tempfile::TempDir;

const PNS_LIVE_LIMIT: usize = 500;
struct ControllableDave {
    dave: Dave,
    command_rx: Receiver<ControllableDaveCommand>,
}

enum ControllableDaveCommand {
    AddUserMessage {
        session_id: notedeck_dave::session::SessionId,
        text: String,
    },
}

fn agentic_dave_settings() -> DaveSettings {
    DaveSettings::with_provider(AiProvider::Codex)
}

/// Add a relay to the selected account's kind-10013 NIP-37 private relay list
/// so Dave adopts it as its PNS sync relay.
fn set_private_relay(device: &mut DeviceHarness, relay_url: &str) {
    process_relay_action_on_device(device, RelayAction::AddPrivate(relay_url.to_owned()));
}

/// Remove a relay from the private relay list, returning Dave to local-only.
fn clear_private_relay(device: &mut DeviceHarness, relay_url: &str) {
    process_relay_action_on_device(device, RelayAction::RemovePrivate(relay_url.to_owned()));
}

fn write_dave_settings(path: &notedeck::DataPath, settings: &DaveSettings) {
    let settings_dir = path.path(DataPathType::Setting);
    std::fs::create_dir_all(&settings_dir).expect("create settings dir");
    let settings_json = serde_json::to_string(settings).expect("serialize Dave settings");
    std::fs::write(settings_dir.join("dave_settings.json"), settings_json)
        .expect("write Dave settings");
}

impl ControllableDave {
    fn apply_pending_commands(&mut self, ctx: &AppContext<'_>) {
        while let Ok(command) = self.command_rx.try_recv() {
            match command {
                ControllableDaveCommand::AddUserMessage { session_id, text } => {
                    let _ =
                        self.dave
                            .add_user_message_for_session(session_id, ctx, text, Vec::new());
                }
            }
        }
    }
}

impl App for ControllableDave {
    fn update(&mut self, ctx: &mut AppContext<'_>, egui_ctx: &egui::Context) {
        self.apply_pending_commands(ctx);
        self.dave.update(ctx, egui_ctx);
    }

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.apply_pending_commands(ctx);
        self.dave.render(ctx, ui)
    }
}
fn dave_app_factory() -> AppFactory {
    Box::new(move |notedeck, egui_ctx| {
        let app_ctx = notedeck.app_context(egui_ctx);
        app_ctx.settings.complete_welcome();
        let settings = agentic_dave_settings();
        write_dave_settings(app_ctx.path, &settings);
        let ndb = app_ctx.ndb.clone();
        let dave = Dave::new(None, ndb, egui_ctx.clone(), app_ctx.path);
        drop(app_ctx);

        notedeck.set_app(dave);
    })
}
fn dave_agentic_app_factory() -> AppFactory {
    Box::new(move |notedeck, egui_ctx| {
        let app_ctx = notedeck.app_context(egui_ctx);
        app_ctx.settings.complete_welcome();
        let settings = agentic_dave_settings();
        write_dave_settings(app_ctx.path, &settings);

        let ndb = app_ctx.ndb.clone();
        let dave = Dave::new(None, ndb, egui_ctx.clone(), app_ctx.path);
        drop(app_ctx);

        notedeck.set_app(dave);
    })
}
fn controllable_dave_app_factory() -> (AppFactory, Sender<ControllableDaveCommand>) {
    let (command_tx, command_rx) = mpsc::channel();
    let app_factory = Box::new(
        move |notedeck: &mut notedeck::Notedeck, egui_ctx: &egui::Context| {
            let app_ctx = notedeck.app_context(egui_ctx);
            app_ctx.settings.complete_welcome();
            let settings = agentic_dave_settings();
            write_dave_settings(app_ctx.path, &settings);
            let ndb = app_ctx.ndb.clone();
            let dave = Dave::new(None, ndb, egui_ctx.clone(), app_ctx.path);
            drop(app_ctx);

            notedeck.set_app(ControllableDave { dave, command_rx });
        },
    );

    (app_factory, command_tx)
}
async fn seed_pns_session_states(relay_db: &MemoryDatabase, account: &FullKeypair, count: usize) {
    seed_pns_session_states_with_prefix(relay_db, account, "remote-session", count).await;
}
async fn seed_pns_session_states_with_prefix(
    relay_db: &MemoryDatabase,
    account: &FullKeypair,
    session_prefix: &str,
    count: usize,
) {
    for i in 0..count {
        let session_id = format!("{session_prefix}-{i}");
        let title = format!("Remote session {i}");
        let cli_session_id = format!("remote-cli-{i}");
        seed_pns_session_state(
            relay_db,
            account,
            &session_id,
            &title,
            "idle",
            Some(&cli_session_id),
        )
        .await;
    }
}
async fn seed_pns_session_state(
    relay_db: &MemoryDatabase,
    account: &FullKeypair,
    session_id: &str,
    title: &str,
    status: &str,
    cli_session_id: Option<&str>,
) {
    let secret_key = account.secret_key.secret_bytes();
    let pns_keys = enostr::pns::derive_pns_keys(&secret_key);
    let state_event = session_events::build_session_state_event(
        session_id,
        title,
        None,
        "/tmp/dave-e2e",
        status,
        None,
        "remote-host",
        "/tmp",
        "remote",
        "default",
        cli_session_id,
        None,
        &secret_key,
    )
    .expect("session state event");
    let pns_json =
        session_events::wrap_pns(&state_event.note_json, &pns_keys).expect("pns wrapper");
    let event = Event::from_json(pns_json).expect("pns event json");
    relay_db.save_event(&event).await.expect("save pns event");
}
async fn seed_pns_session_state_at(
    relay_db: &MemoryDatabase,
    account: &FullKeypair,
    session_id: &str,
    title: &str,
    created_at: u64,
) {
    let secret_key = account.secret_key.secret_bytes();
    let pns_keys = enostr::pns::derive_pns_keys(&secret_key);
    let state_event = session_events::build_session_state_event(
        session_id,
        title,
        None,
        "/tmp/dave-e2e",
        "idle",
        None,
        "remote-host",
        "/tmp",
        "remote",
        "default",
        Some(session_id),
        None,
        &secret_key,
    )
    .expect("session state event");
    let ciphertext = enostr::pns::encrypt(&pns_keys.conversation_key, &state_event.note_json)
        .expect("pns encrypt");
    let pns_note = NoteBuilder::new()
        .kind(enostr::pns::PNS_KIND)
        .content(&ciphertext)
        .created_at(created_at)
        .sign(&pns_keys.keypair.secret_key.secret_bytes())
        .build()
        .expect("pns note");
    let event = Event::from_json(pns_note.json().expect("pns json")).expect("pns event json");
    relay_db.save_event(&event).await.expect("save pns event");
}
async fn setup_relay() -> (MemoryDatabase, NegentropyRelay) {
    setup_relay_with_mode(NegentropyRelayMode::Normal).await
}
async fn setup_relay_with_mode(mode: NegentropyRelayMode) -> (MemoryDatabase, NegentropyRelay) {
    let remote = run_memory_negentropy_relay_with_mode(mode)
        .await
        .expect("start local relay");
    (remote.db, remote.relay)
}

async fn setup_seeded_relay(account: &FullKeypair, count: usize) -> NegentropyRelay {
    setup_seeded_relay_with_mode(account, count, NegentropyRelayMode::Normal).await
}

async fn setup_seeded_relay_with_mode(
    account: &FullKeypair,
    count: usize,
    mode: NegentropyRelayMode,
) -> NegentropyRelay {
    let (relay_db, relay) = setup_relay_with_mode(mode).await;
    seed_pns_session_states(&relay_db, account, count).await;
    relay
}
fn build_dave_device(
    relay_urls: &[&str],
    account: &FullKeypair,
    app_factory: AppFactory,
) -> DeviceHarness {
    notedeck_testing::device::build_device_in_tmpdir_with_relays(
        relay_urls,
        account,
        TempDir::new().expect("tmpdir"),
        app_factory,
    )
}
fn build_pns_device(relay: &NegentropyRelay, account: &FullKeypair) -> DeviceHarness {
    let mut device = build_dave_device(&[relay.url()], account, dave_app_factory());
    set_private_relay(&mut device, relay.url());
    device
}
fn build_agentic_pns_device(relay: &NegentropyRelay, account: &FullKeypair) -> DeviceHarness {
    let mut device = build_dave_device(&[relay.url()], account, dave_agentic_app_factory());
    set_private_relay(&mut device, relay.url());
    device
}
fn session_state_query() -> LocalQuery {
    LocalQuery::new(
        vec![session_state_filter()],
        4096,
        "query session state events",
    )
}
fn author_session_state_query(author: &FullKeypair) -> LocalQuery {
    LocalQuery::new(
        vec![author_session_state_filter(author)],
        4096,
        "query account session state events",
    )
}
fn session_state_filter() -> Filter {
    FilterBuilder::new()
        .kinds([session_events::AI_SESSION_STATE_KIND as u64])
        .build()
}
fn author_session_state_filter(author: &FullKeypair) -> Filter {
    FilterBuilder::new()
        .kinds([session_events::AI_SESSION_STATE_KIND as u64])
        .authors([author.pubkey.bytes()])
        .build()
}
fn latest_valid_session_title(device: &mut DeviceHarness, session_id: &str) -> Option<String> {
    let egui_ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&egui_ctx);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    session_loader::latest_valid_session(app_ctx.ndb, &txn, session_id).map(|state| state.title)
}
fn wait_for_latest_valid_session_title(
    device: &mut DeviceHarness,
    session_id: &str,
    expected_title: &str,
    context: &str,
) {
    wait_for_device_condition(device, Duration::from_secs(20), context, |device| {
        let title = latest_valid_session_title(device, session_id);
        if title.as_deref() == Some(expected_title) {
            Ok(())
        } else {
            Err(format!(
                "expected title {expected_title:?}, found {title:?}"
            ))
        }
    });
}
fn wait_for_visible_session_title(device: &mut DeviceHarness, title: &str, context: &str) {
    wait_for_device_condition(device, Duration::from_secs(20), context, |device| {
        if device.query_by_label(title).is_some() {
            Ok(())
        } else {
            Err(format!("expected visible Dave session title {title:?}"))
        }
    });
}
fn is_pns_filter(filter: &nostr::Filter, pns_pubkey: &nostr::PublicKey) -> bool {
    let has_kind = filter
        .kinds
        .as_ref()
        .is_some_and(|kinds| kinds.contains(&Kind::Custom(enostr::pns::PNS_KIND as u16)));
    let has_author = filter
        .authors
        .as_ref()
        .is_some_and(|authors| authors.contains(pns_pubkey));
    has_kind && has_author
}
fn pns_filter_matcher(account: &FullKeypair) -> impl FnMut(&nostr::Filter) -> bool {
    let secret_key = account.secret_key.secret_bytes();
    let pns_keys = enostr::pns::derive_pns_keys(&secret_key);
    let pns_pubkey = nostr_pubkey(&pns_keys.keypair.pubkey);
    move |filter| is_pns_filter(filter, &pns_pubkey)
}
fn pns_neg_open_session_ids(relay: &NegentropyRelay, account: &FullKeypair) -> Vec<String> {
    relay.captured_neg_open_session_ids(pns_filter_matcher(account))
}
fn wait_for_pns_neg_open(
    device: &mut DeviceHarness,
    relay: &NegentropyRelay,
    account: &FullKeypair,
    context: &str,
) {
    relay.wait_for_neg_open(
        device,
        Duration::from_secs(5),
        context,
        pns_filter_matcher(account),
    );
}

fn wait_for_pns_neg_open_session_id(
    device: &mut DeviceHarness,
    relay: &NegentropyRelay,
    account: &FullKeypair,
    context: &str,
) -> String {
    relay.wait_for_neg_open_session_id(
        device,
        Duration::from_secs(5),
        context,
        pns_filter_matcher(account),
    )
}

fn wait_for_pns_import_and_open(
    device: &mut DeviceHarness,
    relay: &NegentropyRelay,
    account: &FullKeypair,
    expected_count: usize,
    context: &str,
) {
    session_state_query().wait_for_count(
        device,
        expected_count,
        Duration::from_secs(20),
        &format!("{context} PNS session-state import"),
    );
    wait_for_pns_neg_open(device, relay, account, &format!("{context} PNS NEG-OPEN"));
}

async fn run_pns_retry_backfill_case(mode: NegentropyRelayMode, context: &str) {
    let account = FullKeypair::generate();
    let expected_count = PNS_LIVE_LIMIT + 1;
    let relay = setup_seeded_relay_with_mode(&account, expected_count, mode).await;
    let mut device = build_pns_device(&relay, &account);

    session_state_query().wait_for_count(
        &mut device,
        expected_count,
        Duration::from_secs(20),
        &format!("Dave PNS full-history import after {context} retry"),
    );
    relay.wait_for_neg_open_count(
        &mut device,
        2,
        Duration::from_secs(20),
        &format!("{context} retry PNS NEG-OPEN"),
        pns_filter_matcher(&account),
    );
}
fn captured_event_count(relay: &NegentropyRelay) -> usize {
    relay.count_captured_prefix("[\"EVENT\",")
}
fn wait_for_event_publish(device: &mut DeviceHarness, relay: &NegentropyRelay, context: &str) {
    wait_for_device_condition(device, Duration::from_secs(5), context, |_| {
        let event_count = captured_event_count(relay);
        if event_count > 0 {
            Ok(())
        } else {
            Err(format!("captured {event_count} outbound EVENT frames"))
        }
    });
}
fn captured_neg_close(relay: &NegentropyRelay, session_id: &str) -> bool {
    let expected = format!("[\"NEG-CLOSE\",\"{session_id}\"]");
    relay.has_captured_text(&expected)
}
fn wait_for_neg_close(
    device: &mut DeviceHarness,
    relay: &NegentropyRelay,
    session_id: &str,
    context: &str,
) {
    wait_for_device_condition(device, Duration::from_secs(5), context, |_| {
        if captured_neg_close(relay, session_id) {
            Ok(())
        } else {
            Err(format!("missing NEG-CLOSE for {session_id}"))
        }
    });
}

#[cfg(unix)]
fn send_spawn_agent_request(
    device: &mut DeviceHarness,
    cwd: &Path,
) -> notedeck_dave::session::SessionId {
    use std::io::{ErrorKind, Read, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(notedeck_dave::ipc::socket_path())
        .expect("connect Dave spawn-agent socket");
    stream
        .set_nonblocking(true)
        .expect("set spawn-agent socket nonblocking");
    let request = serde_json::json!({
        "type": "spawn_agent",
        "cwd": cwd,
    });
    writeln!(stream, "{request}").expect("write spawn-agent request");

    let mut response = Vec::new();
    let mut session_id = None;
    wait_for_device_condition(
        device,
        Duration::from_secs(5),
        "spawn-agent response",
        |_| {
            let mut buf = [0; 1024];
            match stream.read(&mut buf) {
                Ok(0) => {}
                Ok(read) => {
                    response.extend_from_slice(&buf[..read]);
                    if response.contains(&b'\n') {
                        let line =
                            std::str::from_utf8(&response).expect("spawn-agent UTF-8 response");
                        let value: serde_json::Value =
                            serde_json::from_str(line.trim()).expect("parse spawn-agent response");
                        assert_eq!(
                            value.get("status").and_then(|status| status.as_str()),
                            Some("ok")
                        );
                        session_id = value
                            .get("session_id")
                            .and_then(|session_id| session_id.as_u64())
                            .and_then(|session_id| session_id.try_into().ok());
                        return Ok(());
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {}
                Err(err) => panic!("read spawn-agent response: {err}"),
            }

            Err("no response yet".to_owned())
        },
    );
    session_id.expect("spawn-agent session id")
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_uses_configured_relay_outside_account_relays_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let (configured_db, configured_relay) = setup_relay().await;
    let (account_relay_db, account_relay) = setup_relay().await;
    seed_pns_session_states_with_prefix(&configured_db, &account, "configured-session", 1).await;
    seed_pns_session_states_with_prefix(&account_relay_db, &account, "account-session", 1).await;

    let mut device = build_dave_device(&[account_relay.url()], &account, dave_app_factory());
    set_private_relay(&mut device, configured_relay.url());

    wait_for_pns_import_and_open(
        &mut device,
        &configured_relay,
        &account,
        1,
        "configured relay",
    );

    assert_eq!(session_state_query().count(&mut device), 1);
    assert!(!pns_neg_open_session_ids(&configured_relay, &account).is_empty());
    assert!(pns_neg_open_session_ids(&account_relay, &account).is_empty());
    session_state_query().assert_count_stable(
        &mut device,
        1,
        8,
        "Dave PNS should not import from account relays when a configured relay is set",
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_full_history_backfills_beyond_live_limit_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let expected_count = PNS_LIVE_LIMIT + 5;
    let relay = setup_seeded_relay(&account, expected_count).await;

    let mut device = build_pns_device(&relay, &account);

    wait_for_pns_import_and_open(
        &mut device,
        &relay,
        &account,
        expected_count,
        "full-history",
    );

    assert!(session_state_query().count(&mut device) > PNS_LIVE_LIMIT);
    assert!(!pns_neg_open_session_ids(&relay, &account).is_empty());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_live_imports_stale_latest_state_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let (relay_db, relay) = setup_relay().await;
    seed_pns_session_state_at(
        &relay_db,
        &account,
        "stale-latest-session",
        "Stale latest title",
        1,
    )
    .await;

    let mut device = build_pns_device(&relay, &account);

    wait_for_latest_valid_session_title(
        &mut device,
        "stale-latest-session",
        "Stale latest title",
        "stale latest Dave PNS session state from live REQ",
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_retargets_when_selected_account_changes_e2e() {
    init_tracing();

    let first_account = FullKeypair::generate();
    let second_account = FullKeypair::generate();
    let (relay_db, relay) = setup_relay().await;
    seed_pns_session_state(
        &relay_db,
        &first_account,
        "first-account-session",
        "First account session",
        "idle",
        Some("first-account-cli"),
    )
    .await;
    seed_pns_session_state(
        &relay_db,
        &second_account,
        "second-account-session",
        "Second account session",
        "idle",
        Some("second-account-cli"),
    )
    .await;

    let mut device = build_pns_device(&relay, &first_account);

    author_session_state_query(&first_account).wait_for_count(
        &mut device,
        1,
        Duration::from_secs(20),
        "first account Dave PNS import",
    );
    assert_eq!(
        author_session_state_query(&second_account).count(&mut device),
        0
    );
    wait_for_visible_session_title(
        &mut device,
        "First account session",
        "first account visible Dave session",
    );

    add_account_to_device(&mut device, &second_account);
    select_account_on_device(&mut device, &second_account);
    // The private marker is per-account: mark it for the second account too.
    set_private_relay(&mut device, relay.url());

    author_session_state_query(&second_account).wait_for_count(
        &mut device,
        1,
        Duration::from_secs(20),
        "second account Dave PNS import after account switch",
    );
    wait_for_pns_neg_open(
        &mut device,
        &relay,
        &second_account,
        "second account PNS NEG-OPEN after account switch",
    );

    assert!(!pns_neg_open_session_ids(&relay, &first_account).is_empty());
    assert!(!pns_neg_open_session_ids(&relay, &second_account).is_empty());
    wait_for_visible_session_title(
        &mut device,
        "Second account session",
        "second account visible Dave session",
    );
    assert!(
        device.query_by_label("First account session").is_none(),
        "account switch must not leave the first account's Dave session visible"
    );

    select_account_on_device(&mut device, &first_account);
    wait_for_visible_session_title(
        &mut device,
        "First account session",
        "first account visible Dave session after switching back",
    );
    assert!(
        device.query_by_label("Second account session").is_none(),
        "switching back must hide the second account's Dave session"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_uses_latest_replaceable_session_state_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let (relay_db, relay) = setup_relay().await;
    let session_id = "replaceable-session";
    seed_pns_session_state(
        &relay_db,
        &account,
        session_id,
        "Old replaceable title",
        "idle",
        Some("old-cli-session"),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    seed_pns_session_state(
        &relay_db,
        &account,
        session_id,
        "New replaceable title",
        "working",
        Some("new-cli-session"),
    )
    .await;

    let mut device = build_pns_device(&relay, &account);

    wait_for_latest_valid_session_title(
        &mut device,
        session_id,
        "New replaceable title",
        "latest same-d Dave PNS session state",
    );
    wait_for_pns_neg_open(&mut device, &relay, &account, "replaceable PNS NEG-OPEN");

    assert_eq!(
        latest_valid_session_title(&mut device, session_id).as_deref(),
        Some("New replaceable title")
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_blocked_relay_does_not_retry_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let relay = setup_seeded_relay_with_mode(
        &account,
        PNS_LIVE_LIMIT + 1,
        NegentropyRelayMode::NegErrOnOpen("blocked: too many records".to_string()),
    )
    .await;

    let mut device = build_pns_device(&relay, &account);

    wait_for_pns_neg_open(&mut device, &relay, &account, "blocked PNS NEG-OPEN");
    let initial_open_count = pns_neg_open_session_ids(&relay, &account).len();
    assert_eq!(initial_open_count, 1);

    step_device_for(&mut device, Duration::from_secs(6));

    assert_eq!(
        pns_neg_open_session_ids(&relay, &account).len(),
        initial_open_count,
        "blocked PNS filter should not be reopened after retry backoff"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_closed_retry_backfills_beyond_live_limit_e2e() {
    init_tracing();
    run_pns_retry_backfill_case(
        NegentropyRelayMode::NegErrOnOpenOnce("closed: retry later".to_string()),
        "closed",
    )
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_disconnect_retry_backfills_beyond_live_limit_e2e() {
    init_tracing();
    run_pns_retry_backfill_case(NegentropyRelayMode::DisconnectOnOpenOnce, "disconnect").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_account_switch_cancels_old_active_session_e2e() {
    init_tracing();

    let first_account = FullKeypair::generate();
    let second_account = FullKeypair::generate();
    let (relay_db, relay) = setup_relay_with_mode(NegentropyRelayMode::SilentOnOpen).await;
    seed_pns_session_states_with_prefix(&relay_db, &first_account, "first-active-session", 1).await;
    seed_pns_session_states_with_prefix(&relay_db, &second_account, "second-active-session", 1)
        .await;

    let mut device = build_pns_device(&relay, &first_account);

    let first_session_id = wait_for_pns_neg_open_session_id(
        &mut device,
        &relay,
        &first_account,
        "first account active PNS open",
    );

    add_account_to_device(&mut device, &second_account);
    select_account_on_device(&mut device, &second_account);
    // The private marker is per-account: mark it for the second account too.
    set_private_relay(&mut device, relay.url());

    wait_for_neg_close(
        &mut device,
        &relay,
        &first_session_id,
        "first account PNS cancellation after account switch",
    );
    wait_for_pns_neg_open(
        &mut device,
        &relay,
        &second_account,
        "second account active PNS open",
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_no_private_relay_does_not_use_account_relays_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let account_relay = setup_seeded_relay(&account, 1).await;

    // No relay is marked "private", so Dave stays local-only and must not
    // subscribe to the account's regular relays for PNS state.
    let mut device = build_dave_device(&[account_relay.url()], &account, dave_app_factory());

    step_device_for(&mut device, Duration::from_millis(300));

    assert_eq!(session_state_query().count(&mut device), 0);
    assert!(pns_neg_open_session_ids(&account_relay, &account).is_empty());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_deleted_latest_same_d_suppresses_older_state_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let (relay_db, relay) = setup_relay().await;
    let session_id = "deleted-replaceable-session";
    seed_pns_session_state(
        &relay_db,
        &account,
        session_id,
        "Valid older title",
        "idle",
        Some("valid-cli-session"),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    seed_pns_session_state(
        &relay_db,
        &account,
        session_id,
        "Deleted newer title",
        "deleted",
        Some("deleted-cli-session"),
    )
    .await;

    let mut device = build_pns_device(&relay, &account);

    wait_for_pns_import_and_open(&mut device, &relay, &account, 2, "deleted latest same-d");

    assert_eq!(latest_valid_session_title(&mut device, session_id), None);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_retargets_when_configured_relay_changes_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let (_, first_relay) = setup_relay_with_mode(NegentropyRelayMode::SilentOnOpen).await;
    let (second_relay_db, second_relay) = setup_relay().await;
    seed_pns_session_states_with_prefix(&second_relay_db, &account, "retargeted-session", 1).await;

    let mut device = build_dave_device(&[first_relay.url()], &account, dave_app_factory());
    set_private_relay(&mut device, first_relay.url());

    let first_session_id = wait_for_pns_neg_open_session_id(
        &mut device,
        &first_relay,
        &account,
        "initial configured relay PNS NEG-OPEN",
    );

    // Retarget the private relay from first_relay to second_relay.
    clear_private_relay(&mut device, first_relay.url());
    set_private_relay(&mut device, second_relay.url());

    wait_for_neg_close(
        &mut device,
        &first_relay,
        &first_session_id,
        "old configured relay PNS cancellation after retarget",
    );
    wait_for_pns_import_and_open(
        &mut device,
        &second_relay,
        &account,
        1,
        "retargeted configured relay",
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_clears_configured_relay_when_private_marker_removed_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let (_, relay) = setup_relay_with_mode(NegentropyRelayMode::SilentOnOpen).await;

    let mut device = build_dave_device(&[relay.url()], &account, dave_app_factory());
    set_private_relay(&mut device, relay.url());

    let session_id = wait_for_pns_neg_open_session_id(
        &mut device,
        &relay,
        &account,
        "initial configured relay PNS NEG-OPEN before clearing private marker",
    );

    // Removing the private marker returns Dave to local-only, cancelling the
    // PNS subscription.
    clear_private_relay(&mut device, relay.url());

    wait_for_neg_close(
        &mut device,
        &relay,
        &session_id,
        "configured relay PNS cancellation after clearing private marker",
    );
    relay.assert_neg_open_count_stable(
        &mut device,
        1,
        20,
        "cleared private relay should not leave retryable scoped work",
        pns_filter_matcher(&account),
    );
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_cross_device_outbound_import_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let (_, relay) = setup_relay().await;

    let cwd = std::env::current_dir().expect("current dir");
    let mut publisher = build_agentic_pns_device(&relay, &account);

    send_spawn_agent_request(&mut publisher, &cwd);
    wait_for_event_publish(
        &mut publisher,
        &relay,
        "publisher outbound PNS session state",
    );
    notedeck_testing::shutdown_device(publisher);

    let mut receiver = build_pns_device(&relay, &account);

    wait_for_pns_import_and_open(
        &mut receiver,
        &relay,
        &account,
        1,
        "receiver publisher-state",
    );
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_two_live_devices_receive_outbound_publish_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let (_, relay) = setup_relay().await;

    let mut receiver = build_pns_device(&relay, &account);
    step_device_for(&mut receiver, Duration::from_millis(100));
    assert_eq!(session_state_query().count(&mut receiver), 0);

    // Dave IPC uses ipc::socket_path(); build publisher second so spawn-agent
    // goes to the publisher while both devices stay live.
    let cwd = std::env::current_dir().expect("current dir");
    let mut publisher = build_agentic_pns_device(&relay, &account);

    send_spawn_agent_request(&mut publisher, &cwd);
    wait_for_event_publish(
        &mut publisher,
        &relay,
        "publisher outbound PNS state while receiver is live",
    );
    let query = session_state_query();
    wait_for_two_local_query_counts(
        &mut publisher,
        (&query, 1),
        &mut receiver,
        (&query, 1),
        Duration::from_secs(20),
        "two live Dave devices after outbound PNS publish",
    );
    wait_for_pns_neg_open(
        &mut receiver,
        &relay,
        &account,
        "live receiver PNS NEG-OPEN",
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_two_live_devices_keep_account_state_isolated_e2e() {
    init_tracing();

    let first_account = FullKeypair::generate();
    let second_account = FullKeypair::generate();
    let (relay_db, relay) = setup_relay().await;
    seed_pns_session_states_with_prefix(&relay_db, &first_account, "first-device-session", 1).await;
    seed_pns_session_states_with_prefix(&relay_db, &second_account, "second-device-session", 1)
        .await;

    let mut first_device = build_pns_device(&relay, &first_account);
    let mut second_device = build_pns_device(&relay, &second_account);

    let first_query = author_session_state_query(&first_account);
    let second_query = author_session_state_query(&second_account);
    wait_for_two_local_query_counts(
        &mut first_device,
        (&first_query, 1),
        &mut second_device,
        (&second_query, 1),
        Duration::from_secs(20),
        "two live Dave devices with distinct PNS accounts",
    );

    assert_eq!(session_state_query().count(&mut first_device), 1);
    assert_eq!(session_state_query().count(&mut second_device), 1);
    assert!(!pns_neg_open_session_ids(&relay, &first_account).is_empty());
    assert!(!pns_neg_open_session_ids(&relay, &second_account).is_empty());
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_no_private_relay_does_not_publish_to_account_relays_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let (_, account_relay) = setup_relay().await;

    let cwd = std::env::current_dir().expect("current dir");
    // No private relay marked: Dave is local-only and must not publish PNS
    // events out to the account's regular relays.
    let (app_factory, command_tx) = controllable_dave_app_factory();
    let mut device = build_dave_device(&[account_relay.url()], &account, app_factory);

    let session_id = send_spawn_agent_request(&mut device, &cwd);
    command_tx
        .send(ControllableDaveCommand::AddUserMessage {
            session_id,
            text: "queued while no private relay is set".to_owned(),
        })
        .expect("queue outbound PNS event");
    step_device_for(&mut device, Duration::from_millis(200));

    assert_eq!(captured_event_count(&account_relay), 0);
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_no_private_relay_retains_outbound_events_until_marked_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let second_account = FullKeypair::generate();
    let (_, relay) = setup_relay().await;

    let cwd = std::env::current_dir().expect("current dir");
    let (app_factory, pns_relay_tx) = controllable_dave_app_factory();
    let mut device = build_dave_device(&[relay.url()], &account, app_factory);

    let session_id = send_spawn_agent_request(&mut device, &cwd);
    pns_relay_tx
        .send(ControllableDaveCommand::AddUserMessage {
            session_id,
            text: "queued while no private relay is set".to_owned(),
        })
        .expect("queue outbound PNS event");
    step_device_for(&mut device, Duration::from_millis(200));
    assert_eq!(captured_event_count(&relay), 0);

    add_account_to_device(&mut device, &second_account);
    select_account_on_device(&mut device, &second_account);
    step_device_for(&mut device, Duration::from_millis(200));
    assert_eq!(captured_event_count(&relay), 0);

    select_account_on_device(&mut device, &account);

    // Mark the relay private: Dave now syncs the retained backlog to it.
    set_private_relay(&mut device, relay.url());

    wait_for_event_publish(
        &mut device,
        &relay,
        "Dave outbound PNS event publish after marking relay private",
    );
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn dave_pns_outbound_publish_uses_configured_relay_only_e2e() {
    init_tracing();

    let account = FullKeypair::generate();
    let (_, configured_relay) = setup_relay().await;
    let (_, account_relay) = setup_relay().await;

    let cwd = std::env::current_dir().expect("current dir");
    let mut device =
        build_dave_device(&[account_relay.url()], &account, dave_agentic_app_factory());
    set_private_relay(&mut device, configured_relay.url());

    send_spawn_agent_request(&mut device, &cwd);
    wait_for_event_publish(
        &mut device,
        &configured_relay,
        "Dave outbound PNS state publish to configured relay",
    );
    step_device_for(&mut device, Duration::from_millis(200));

    // Marking the relay private publishes a kind-10002 relay list to the
    // account's write relay (account_relay), so assert specifically on the PNS
    // session-state events: they must reach the configured relay only.
    let pns_needle = format!("\"kind\":{}", enostr::pns::PNS_KIND);
    assert!(configured_relay.count_captured_events_containing(&pns_needle) > 0);
    assert_eq!(
        account_relay.count_captured_events_containing(&pns_needle),
        0
    );
}
