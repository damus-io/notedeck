//! End-to-end check that the real `agentium interrupt` publishes a kind-1988
//! interrupt command (`role = "interrupt"`) onto a session's conversation.
//!
//! The sibling of `send_lands.rs`: there the CLI publishes a `user` message;
//! here it publishes the fire-and-forget interrupt a host applies to abort the
//! in-flight turn. We seed a signed kind-31988 state event into the sender's
//! cache (so the selector resolves), run the actual `agentium interrupt
//! <session>` binary against a reachable in-process relay, and assert the
//! interrupt command lands on a separate verifier engine that shares the identity
//! and relay — plus that the binary reports it and exits 0.

use std::process::Command;
use std::time::Duration;

use nostrdb::{Config, Ndb, NoteBuilder, Transaction};
use tempfile::TempDir;

/// `[7u8; 32]` as an nsec — the same key the seeded events are signed with, so
/// the engine (which reads/publishes as its own key) resolves and decrypts them.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];
const KIND_SESSION_STATE: u32 = 31988;
const KIND_CONVERSATION: u32 = 1988;

/// Seed a signed kind-31988 session-state event so a selector can resolve to a
/// live session (mirrors `send_lands.rs::seed_state`).
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
        .tag_str("working")
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
/// the `interrupt` selector resolves. Returns the db path; the db handle is
/// dropped so the subprocess opens the committed cache cleanly.
async fn seed_sender_cache(dir: &TempDir, d: &str) -> String {
    let db_path = dir.path().to_str().expect("path").to_string();
    let ndb = Ndb::new(&db_path, &Config::new()).expect("ndb");
    let filter = nostrdb::Filter::new()
        .kinds([KIND_SESSION_STATE as u64])
        .build();
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .expect("subscribe");
    seed_state(&ndb, d, "Interrupt target");
    ndb.wait_for_all_notes(sub, 1)
        .await
        .expect("ingest seeded state");
    db_path
}

/// Whether a kind-1988 interrupt command (`role = "interrupt"`) for session `d`
/// is present in `ndb` (the verifier's decrypted cache).
fn interrupt_landed(ndb: &Ndb, d: &str) -> bool {
    let txn = Transaction::new(ndb).expect("txn");
    let filter = nostrdb::Filter::new()
        .kinds([KIND_CONVERSATION as u64])
        .tags([d], 'd')
        .build();
    let Ok(results) = ndb.query(&txn, &[filter], 100) else {
        return false;
    };
    results.iter().any(|qr| {
        ndb.get_note_by_key(&txn, qr.note_key)
            .ok()
            .and_then(|note| {
                agentium_core::session_events::get_tag_value(&note, "role")
                    .map(|r| r == "interrupt")
            })
            .unwrap_or(false)
    })
}

/// The interrupt `interrupt` publishes lands on a same-identity verifier engine
/// connected to the same relay, and the binary reports it and exits 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_reaches_the_relay() {
    use agentium_core::Engine;

    // A real relay backed by its own ndb — the seam the published event crosses.
    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &Config::new()).expect("relay ndb");
    let relay =
        nostrdb_relay::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr")).expect("spawn relay");
    let url = relay.url();

    // Seed the SENDER's cache with just the state, so the selector resolves.
    let sender_dir = TempDir::new().expect("sender tmp");
    let db_path = seed_sender_cache(&sender_dir, "sess-int").await;

    // A same-identity verifier engine on the same relay, with a watch installed
    // *before* the interrupt so it catches the command however it races.
    let verifier_dir = TempDir::new().expect("verifier tmp");
    let mut verifier =
        Engine::open(verifier_dir.path().to_str().expect("path"), SECKEY).expect("verifier engine");
    let mut ver_tx = verifier.transport_handle().expect("verifier transport");
    verifier
        .connect(&mut ver_tx, &url)
        .expect("verifier connect");
    let mut watch = verifier.watch_session("sess-int").expect("watch");

    // Run the real binary: connect → settle → publish the interrupt → exit after
    // the bounded flush.
    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args([
            "--nsec",
            NSEC,
            "--db",
            &db_path,
            "--relay",
            &url,
            "interrupt",
            "sess-int",
        ])
        .env("XDG_DATA_HOME", sender_dir.path())
        .env("HOME", sender_dir.path())
        .output()
        .expect("run agentium interrupt");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("interrupt sent to") && stdout.contains("agentium:"),
        "interrupt should report the target ref:\n{stdout}"
    );

    // The interrupt command lands on the verifier (same key, same relay).
    let received = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if interrupt_landed(verifier.ndb(), "sess-int") {
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
        "verifier engine should receive the interrupt command"
    );

    relay.shutdown();
}

/// A selector that matches no live session fails loudly (nonzero exit) rather
/// than silently publishing a stray command — the live-only resolution wiring.
#[tokio::test]
async fn interrupt_unknown_session_errors() {
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
            "interrupt",
            "agentium:no-such-session",
        ])
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run agentium interrupt");
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
