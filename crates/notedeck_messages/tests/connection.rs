//! Connection recovery and reliability end-to-end tests.

mod harness;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use enostr::FullKeypair;
use harness::fixtures::{build_backdated_giftwrap_note, seed_local_dm_relay_list};
use harness::ui::{open_conversation_via_ui, send_message_via_ui};
use harness::{
    build_messages_device, init_tracing, local_chat_message_count, local_chat_messages,
    wait_for_device_messages, TEST_TIMEOUT,
};
use nostr::{Event, JsonUtil};
use nostr_relay_builder::{
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use notedeck::unix_time_secs;
/// Extracts the port number from a relay URL like `ws://127.0.0.1:12345/`.
fn extract_port(relay_url: &str) -> u16 {
    url::Url::parse(relay_url)
        .expect("parse relay URL")
        .port()
        .expect("relay URL must have a port")
}

/// Restarts a local relay on a fixed port, retrying until the OS releases the socket.
async fn restart_relay_on_port_with_retry(
    relay_db: &MemoryDatabase,
    relay_port: u16,
    timeout: Duration,
) -> LocalRelay {
    let deadline = Instant::now() + timeout;

    loop {
        match LocalRelay::run(
            RelayBuilder::default()
                .port(relay_port)
                .database(relay_db.clone()),
        )
        .await
        {
            Ok(relay) => return relay,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "timed out restarting relay on port {relay_port}; last error: {error:?}"
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    }
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
    let relay2 =
        restart_relay_on_port_with_retry(&relay_db, relay_port, Duration::from_secs(5)).await;

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

        // After 5 seconds, restore the proxy so reconnection CAN work.
        // This isolates the bug to detection, not recovery.
        if !restored_proxy && Instant::now() > restore_at {
            proxy.restore();
            restored_proxy = true;
        }

        let current = local_chat_messages(&mut bob_device);
        if current == all_expected {
            break;
        }

        std::thread::sleep(Duration::from_millis(50));
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
