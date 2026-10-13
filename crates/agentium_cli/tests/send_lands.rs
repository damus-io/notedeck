//! End-to-end check that the real `agentium send` publishes a `user` message
//! onto a session's conversation and reports the resulting event id.
//!
//! The mirror of `log_renders.rs::follow_streams_a_live_message`: there a live
//! event flows *into* the CLI's cache; here the CLI is the *publisher*. We seed a
//! signed kind-31988 session-state event into the sender's cache (so the selector
//! resolves), run the actual `agentium send <session> "…"` binary against a
//! reachable in-process relay, and assert the `user` message lands on a separate
//! verifier engine that shares the identity and relay — plus that the binary
//! prints the event id and exits 0.

use std::process::Command;
use std::time::Duration;

use nostrdb::{Config, Ndb, NoteBuilder, Transaction};
use tempfile::TempDir;

/// A [`Config`] with a small mapsize, for tests.
///
/// On Windows LMDB actually allocates the full mapsize on disk rather than only
/// mapping it virtually, so a test taking nostrdb's large default eats the CI
/// runner's disk. Mirrors `notedeck::test_util::test_config`, which this crate
/// can't reach (it doesn't depend on notedeck).
fn test_config() -> Config {
    if cfg!(target_os = "windows") {
        Config::new().set_mapsize(32 * 1024 * 1024) // 32 MiB
    } else {
        Config::new()
    }
}

/// `[7u8; 32]` as an nsec — the same key the seeded events are signed with, so
/// the engine (which reads/publishes as its own key) resolves and decrypts them.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];
const KIND_SESSION_STATE: u32 = 31988;
const KIND_CONVERSATION: u32 = 1988;

/// Seed a signed kind-31988 session-state event so a selector can resolve to a
/// live session in `status`. Minimal — just the tags `SessionState::from_note`
/// needs to place it in the list (mirrors `log_renders.rs::seed_state`).
fn seed_state(ndb: &Ndb, d: &str, title: &str, status: &str) {
    let note = NoteBuilder::new()
        .kind(KIND_SESSION_STATE)
        .content("")
        .start_tag()
        .tag_str("d")
        .tag_str(d)
        .start_tag()
        .tag_str("title")
        .tag_str(title)
        .start_tag()
        .tag_str("status")
        .tag_str(status)
        .start_tag()
        .tag_str("hostname")
        .tag_str("macbook")
        .start_tag()
        .tag_str("cwd")
        .tag_str("/home/u/proj")
        .start_tag()
        .tag_str("home_dir")
        .tag_str("/home/u")
        .sign(&SECKEY)
        .build()
        .expect("build state note");
    let frame = format!(r#"["EVENT",{}]"#, note.json().expect("note json"));
    ndb.process_client_event(&frame).expect("ingest");
}

/// Open a fresh sender cache dir and seed a single idle kind-31988 state into
/// it, so the `send` selector resolves. Returns the db path; the db handle is
/// dropped so the subprocess opens the committed cache cleanly.
async fn seed_sender_cache(dir: &TempDir, d: &str) -> String {
    seed_sender_cache_in(dir, d, "idle").await
}

/// [`seed_sender_cache`] with the seeded session in `status`.
async fn seed_sender_cache_in(dir: &TempDir, d: &str, status: &str) -> String {
    let db_path = dir.path().to_str().expect("path").to_string();
    let ndb = Ndb::new(&db_path, &test_config()).expect("ndb");
    let filter = nostrdb::Filter::new()
        .kinds([KIND_SESSION_STATE as u64])
        .build();
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .expect("subscribe");
    seed_state(&ndb, d, "Send target", status);
    ndb.wait_for_all_notes(sub, 1)
        .await
        .expect("ingest seeded state");
    db_path
}

/// The user text `send` publishes lands on a same-identity verifier engine
/// connected to the same relay, and the binary reports the event id and exits 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_publishes_user_message_to_the_relay() {
    use agentium_core::Engine;
    use agentium_core::messages::Message;

    // A real relay backed by its own ndb — the seam the published event crosses.
    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &test_config()).expect("relay ndb");
    let relay = nostrdb_net::relay::server::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr"))
        .expect("spawn relay");
    let url = relay.url();

    // Seed the SENDER's cache with just the state, so the selector resolves; the
    // send roots a fresh thread (no prior conversation).
    let sender_dir = TempDir::new().expect("sender tmp");
    let db_path = seed_sender_cache(&sender_dir, "sess-send").await;

    // A same-identity verifier engine on the same relay, with a watch installed
    // *before* the send so it catches the message however it races.
    let verifier_dir = TempDir::new().expect("verifier tmp");
    let mut verifier =
        Engine::open(verifier_dir.path().to_str().expect("path"), SECKEY).expect("verifier engine");
    verifier.connect(&url).expect("verifier connect");
    // Flush the connect through the Session's FIFO before the sender publishes —
    // same ordering requirement as interrupt_lands, see the note there.
    let _ = tokio::time::timeout(Duration::from_secs(5), verifier.wait_for_sync()).await;
    let mut watch = verifier.watch_session("sess-send").expect("watch");

    // Run the real binary: connect → settle → publish the user message → exit
    // after the bounded flush. The message is several words to prove they join.
    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args([
            "--nsec",
            NSEC,
            "--db",
            &db_path,
            "--relay",
            &url,
            "send",
            "sess-send",
            "hello",
            "from",
            "the",
            "cli",
        ])
        .env("XDG_DATA_HOME", sender_dir.path())
        .env("HOME", sender_dir.path())
        .output()
        .expect("run agentium send");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    // Reports where it went (the sayable ref) and the event id.
    assert!(
        stdout.contains("sent to") && stdout.contains("agentium:"),
        "send should report the target ref:\n{stdout}"
    );
    assert!(
        stdout.contains("event "),
        "send should report the event id:\n{stdout}"
    );

    // The joined user message lands on the verifier (same key, same relay).
    let received = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(Message::User(u)) = verifier.session_messages("sess-send").first()
                && u.text == "hello from the cli"
            {
                return true;
            }
            if !watch.changed().await {
                return false;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        received,
        "verifier engine should receive the sent user message"
    );

    relay.shutdown();
}

/// `--json` reports the event id even against an unreachable relay: the send
/// ingests locally regardless, and the durable id comes from the built event —
/// not the publish. Faster than the round-trip and pins the JSON shape.
#[tokio::test]
async fn send_json_reports_event_id_offline() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = seed_sender_cache(&dir, "sess-json").await;

    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args([
            "--nsec",
            NSEC,
            "--db",
            &db_path,
            "--relay",
            "ws://127.0.0.1:1",
            "--json",
            "send",
            "sess-json",
            "hi",
        ])
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run agentium send --json");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "nonzero exit:\n{stdout}\n{stderr}");

    // One record, one line: `| tail -1 | jq -r .session` must not yield "".
    assert_eq!(
        stdout.lines().count(),
        1,
        "send --json must be one line, or line-oriented pipelines silently \
         yield nothing: {stdout:?}"
    );

    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON object");
    assert!(
        v["session"]
            .as_str()
            .is_some_and(|s| s.starts_with("agentium:")),
        "json carries the session ref: {stdout}"
    );
    assert!(
        v.get("interrupted").is_none(),
        "a plain send says nothing about interrupting: {stdout}"
    );
    let id = v["event_id"].as_str().expect("event_id string");
    assert_eq!(id.len(), 64, "event id is 32-byte hex: {id:?}");
    assert!(
        id.chars().all(|c| c.is_ascii_hexdigit()),
        "event id is hex: {id:?}"
    );
}

/// A selector that matches no live session fails loudly (nonzero exit) rather
/// than silently starting a stray thread — the live-only resolution wiring.
#[tokio::test]
async fn send_to_unknown_session_errors() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = seed_sender_cache(&dir, "sess-known").await;

    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args([
            "--nsec",
            NSEC,
            "--db",
            &db_path,
            "--relay",
            "ws://127.0.0.1:1",
            "send",
            "agentium:no-such-session",
            "hi",
        ])
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run agentium send");
    assert!(
        !out.status.success(),
        "an unknown selector must exit nonzero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no session matching"),
        "should surface the resolver error:\n{stderr}"
    );
}

/// A kind-1988 event for one session, as the verifier decrypted it.
struct Landed {
    role: String,
    /// The `ms` tag: when the event was built, to the millisecond.
    ms: u64,
}

/// Every kind-1988 event for session `d` in `ndb`, with its role and `ms`.
fn landed_events(ndb: &Ndb, d: &str) -> Vec<Landed> {
    let txn = Transaction::new(ndb).expect("txn");
    let filter = nostrdb::Filter::new()
        .kinds([KIND_CONVERSATION as u64])
        .tags([d], 'd')
        .build();
    let Ok(results) = ndb.query(&txn, &[filter], 100) else {
        return Vec::new();
    };
    results
        .iter()
        .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
        .filter_map(|note| {
            let tag = |name| agentium_core::session_events::get_tag_value(&note, name);
            Some(Landed {
                role: tag("role")?.to_string(),
                ms: tag("ms")?.parse().ok()?,
            })
        })
        .collect()
}

/// `send --immediate` to a working session publishes the user message and
/// then the interrupt command: both land on a same-identity verifier, the
/// interrupt built no earlier than the message, and the binary reports both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_immediate_lands_the_message_then_the_interrupt() {
    use agentium_core::Engine;

    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &test_config()).expect("relay ndb");
    let relay = nostrdb_net::relay::server::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr"))
        .expect("spawn relay");
    let url = relay.url();

    // A session mid-turn, so `--immediate` has a turn to interrupt.
    let sender_dir = TempDir::new().expect("sender tmp");
    let db_path = seed_sender_cache_in(&sender_dir, "sess-imm", "working").await;

    let verifier_dir = TempDir::new().expect("verifier tmp");
    let mut verifier =
        Engine::open(verifier_dir.path().to_str().expect("path"), SECKEY).expect("verifier engine");
    verifier.connect(&url).expect("verifier connect");
    // Same ordering requirement as interrupt_lands, see the note there.
    let _ = tokio::time::timeout(Duration::from_secs(5), verifier.wait_for_sync()).await;
    let mut watch = verifier.watch_session("sess-imm").expect("watch");

    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args([
            "--nsec",
            NSEC,
            "--db",
            &db_path,
            "--relay",
            &url,
            "send",
            "--immediate",
            "sess-imm",
            "stop",
            "and",
            "read",
            "this",
        ])
        .env("XDG_DATA_HOME", sender_dir.path())
        .env("HOME", sender_dir.path())
        .output()
        .expect("run agentium send --immediate");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("sent to") && stdout.contains("interrupted agentium:"),
        "send --immediate should report the send and the interrupt:\n{stdout}"
    );

    let both = |events: &[Landed]| {
        let user = events.iter().find(|e| e.role == "user")?;
        let interrupt = events.iter().find(|e| e.role == "interrupt")?;
        Some(user.ms <= interrupt.ms)
    };
    let in_order = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(in_order) = both(&landed_events(verifier.ndb(), "sess-imm")) {
                return Some(in_order);
            }
            if !watch.changed().await {
                return None;
            }
        }
    })
    .await
    .ok()
    .flatten();
    assert_eq!(
        in_order,
        Some(true),
        "the verifier should receive the user message, then the interrupt"
    );

    relay.shutdown();
}

/// Run `agentium --json send --immediate` offline against a session seeded in
/// `status`, and return the one JSON record it prints.
async fn send_immediate_json(status: &str) -> serde_json::Value {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = seed_sender_cache_in(&dir, "sess-imm-json", status).await;

    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args([
            "--nsec",
            NSEC,
            "--db",
            &db_path,
            "--relay",
            "ws://127.0.0.1:1",
            "--json",
            "send",
            "--immediate",
            "sess-imm-json",
            "hi",
        ])
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run agentium send --immediate --json");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "nonzero exit:\n{stdout}\n{stderr}");
    assert_eq!(stdout.lines().count(), 1, "one JSON line: {stdout:?}");
    serde_json::from_str(&stdout).expect("valid JSON object")
}

/// `--json` keeps the event id and says whether the turn was interrupted: a
/// working session is, an idle one isn't, since the message alone starts its
/// turn and an interrupt would abort that.
#[tokio::test]
async fn send_immediate_json_reports_whether_it_interrupted() {
    let working = send_immediate_json("working").await;
    assert_eq!(working["interrupted"], true, "{working}");
    assert_eq!(working["event_id"].as_str().map(str::len), Some(64));

    let idle = send_immediate_json("idle").await;
    assert_eq!(idle["interrupted"], false, "{idle}");
    assert_eq!(idle["event_id"].as_str().map(str::len), Some(64));
}
