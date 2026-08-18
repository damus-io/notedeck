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

use nostrdb::{Config, Ndb, NoteBuilder};
use tempfile::TempDir;

/// `[7u8; 32]` as an nsec — the same key the seeded events are signed with, so
/// the engine (which reads/publishes as its own key) resolves and decrypts them.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];
const KIND_SESSION_STATE: u32 = 31988;

/// Seed a signed kind-31988 session-state event so a selector can resolve to a
/// live session. Minimal — just the tags `SessionState::from_note` needs to
/// place it in the list (mirrors `log_renders.rs::seed_state`).
fn seed_state(ndb: &Ndb, d: &str, title: &str) {
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
        .tag_str("idle")
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

/// Open a fresh sender cache dir and seed a single kind-31988 state into it, so
/// the `send` selector resolves. Returns the db path; the db handle is dropped so
/// the subprocess opens the committed cache cleanly.
async fn seed_sender_cache(dir: &TempDir, d: &str) -> String {
    let db_path = dir.path().to_str().expect("path").to_string();
    let ndb = Ndb::new(&db_path, &Config::new()).expect("ndb");
    let filter = nostrdb::Filter::new()
        .kinds([KIND_SESSION_STATE as u64])
        .build();
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .expect("subscribe");
    seed_state(&ndb, d, "Send target");
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
        Ndb::new(relay_dir.path().to_str().expect("path"), &Config::new()).expect("relay ndb");
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

    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON object");
    assert!(
        v["session"]
            .as_str()
            .is_some_and(|s| s.starts_with("agentium:")),
        "json carries the session ref: {stdout}"
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
