//! End-to-end checks for `agentium approve`/`deny`/`mode`: the real binary
//! publishes a `permission_response` answering the session's pending request
//! (or a `set_permission_mode` command), and it reaches a same-identity
//! verifier engine over a real relay.
//!
//! The sibling of `interrupt_lands.rs`, plus a seeded `permission_request` note
//! so there's something pending to answer. The offline cases (nothing pending,
//! a question approve) run against an unreachable relay, since they have to
//! fail before anything is published.

use std::process::{Command, Output};
use std::time::Duration;

use agentium_core::session_events::{
    ThreadingState, build_permission_request_event, decode_permission_response, get_tag_value,
};
use nostrdb::{Config, Ndb, NoteBuilder, Transaction};
use tempfile::TempDir;

/// A [`Config`] with a small mapsize, for tests (see `interrupt_lands.rs`).
fn test_config() -> Config {
    if cfg!(target_os = "windows") {
        Config::new().set_mapsize(32 * 1024 * 1024) // 32 MiB
    } else {
        Config::new()
    }
}

/// `[7u8; 32]` as an nsec — the key the seeded events are signed with.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];
const KIND_SESSION_STATE: u32 = 31988;
const KIND_CONVERSATION: u32 = 1988;
const SESSION: &str = "sess-perm";

/// Ingest a signed note as a client would, un-wrapped: the loader reads plain
/// kind-1988/31988 notes by this identity the same as decrypted PNS ones.
fn ingest_json(ndb: &Ndb, note_json: &str) {
    ndb.process_client_event(&format!(r#"["EVENT",{note_json}]"#))
        .expect("ingest");
}

/// Seed a live kind-31988 state for [`SESSION`] so the selector resolves.
fn seed_state(ndb: &Ndb) {
    let note = NoteBuilder::new()
        .kind(KIND_SESSION_STATE)
        .content("")
        .start_tag()
        .tag_str("d")
        .tag_str(SESSION)
        .start_tag()
        .tag_str("title")
        .tag_str("Permission target")
        .start_tag()
        .tag_str("status")
        .tag_str("needs_input")
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
    ingest_json(ndb, &note.json().expect("note json"));
}

/// Open a fresh sender cache in `dir` holding the session state plus one
/// pending `permission_request` per `(tool_name, tool_input)` in `requests`, and
/// wait until all of it is queryable. Returns the db path and the perm-ids, in
/// order; the db handle is dropped so the subprocess opens the cache cleanly.
async fn seed_sender_cache(
    dir: &TempDir,
    requests: &[(&str, serde_json::Value)],
) -> (String, Vec<uuid::Uuid>) {
    let db_path = dir.path().to_str().expect("path").to_string();
    let ndb = Ndb::new(&db_path, &test_config()).expect("ndb");
    let filter = nostrdb::Filter::new()
        .kinds([KIND_SESSION_STATE as u64, KIND_CONVERSATION as u64])
        .build();
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .expect("subscribe");

    seed_state(&ndb);
    let mut threading = ThreadingState::new();
    let mut perm_ids = Vec::new();
    for (tool, input) in requests {
        let perm_id = uuid::Uuid::new_v4();
        let req =
            build_permission_request_event(&perm_id, tool, input, SESSION, &mut threading, &SECKEY)
                .expect("request event");
        ingest_json(&ndb, &req.note_json);
        perm_ids.push(perm_id);
    }

    ndb.wait_for_all_notes(sub, 1 + requests.len() as u32)
        .await
        .expect("ingest seeded notes");
    (db_path, perm_ids)
}

/// Run the real `agentium` binary against `db_path`/`relay`, with `dir`
/// redirecting XDG/HOME so the run can't touch real config.
fn run_agentium(dir: &TempDir, db_path: &str, relay: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args(["--nsec", NSEC, "--db", db_path, "--relay", relay])
        .args(args)
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run agentium")
}

/// The content of every kind-1988 note on [`SESSION`] in `ndb` whose `role` tag
/// is `role`, paired with its `perm-id` tag (if any).
fn notes_with_role(ndb: &Ndb, role: &str) -> Vec<(Option<String>, String)> {
    let txn = Transaction::new(ndb).expect("txn");
    let filter = nostrdb::Filter::new()
        .kinds([KIND_CONVERSATION as u64])
        .tags([SESSION], 'd')
        .build();
    let Ok(results) = ndb.query(&txn, &[filter], 100) else {
        return vec![];
    };
    results
        .iter()
        .filter(|qr| get_tag_value(&qr.note, "role") == Some(role))
        .map(|qr| {
            (
                get_tag_value(&qr.note, "perm-id").map(str::to_string),
                qr.note.content().to_string(),
            )
        })
        .collect()
}

/// A connected verifier engine sharing the sender's identity and relay, with
/// its REQ flushed to the relay before the sender publishes (see
/// `interrupt_lands.rs` for why that ordering has to be forced).
async fn verifier(dir: &TempDir, url: &str) -> agentium_core::Engine {
    let mut verifier = agentium_core::Engine::open(dir.path().to_str().expect("path"), SECKEY)
        .expect("verifier engine");
    verifier.connect(url).expect("verifier connect");
    let _ = tokio::time::timeout(Duration::from_secs(5), verifier.wait_for_sync()).await;
    verifier
}

/// Wait (bounded) until `landed` holds on the verifier, waking on each new
/// event for [`SESSION`].
async fn wait_until(verifier: &agentium_core::Engine, landed: impl Fn(&Ndb) -> bool) -> bool {
    let mut watch = verifier.watch_session(SESSION).expect("watch");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if landed(verifier.ndb()) {
                return true;
            }
            if !watch.changed().await {
                return false;
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// `approve` answers the *newest* pending request by default, carries the
/// `--message`, and the response lands on the verifier.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approve_answers_the_newest_pending_request() {
    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &test_config()).expect("relay ndb");
    let relay = nostrdb_net::relay::server::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr"))
        .expect("spawn relay");
    let url = relay.url();

    let sender_dir = TempDir::new().expect("sender tmp");
    let (db_path, perm_ids) = seed_sender_cache(
        &sender_dir,
        &[
            ("Read", serde_json::json!({ "file_path": "/etc/hosts" })),
            ("Bash", serde_json::json!({ "command": "ls" })),
        ],
    )
    .await;
    let newest = perm_ids[1].to_string();

    let verifier_dir = TempDir::new().expect("verifier tmp");
    let verifier = verifier(&verifier_dir, &url).await;

    let out = run_agentium(
        &sender_dir,
        &db_path,
        &url,
        &["--message", "go ahead", "approve", SESSION],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains(&format!("approved Bash request {}", &newest[..8])),
        "approve should report the request it answered:\n{stdout}"
    );

    let landed = wait_until(&verifier, |ndb| {
        notes_with_role(ndb, "permission_response")
            .iter()
            .any(|(perm, content)| {
                let decoded = decode_permission_response(content);
                perm.as_deref() == Some(newest.as_str())
                    && decoded.response_type
                        == agentium_core::messages::PermissionResponseType::Allowed
                    && decoded.message.as_deref() == Some("go ahead")
            })
    })
    .await;
    assert!(
        landed,
        "an allow for the newest request should reach the verifier"
    );
    // The older request was left alone.
    let older = perm_ids[0].to_string();
    assert!(
        !notes_with_role(verifier.ndb(), "permission_response")
            .iter()
            .any(|(perm, _)| perm.as_deref() == Some(older.as_str())),
        "only the newest request is answered"
    );

    relay.shutdown();
}

/// `mode <s> acceptEdits` publishes the canonical `accept_edits`, which is the
/// only spelling the host's mode parser recognizes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mode_publishes_the_canonical_mode() {
    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &test_config()).expect("relay ndb");
    let relay = nostrdb_net::relay::server::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr"))
        .expect("spawn relay");
    let url = relay.url();

    let sender_dir = TempDir::new().expect("sender tmp");
    let (db_path, _) = seed_sender_cache(&sender_dir, &[]).await;

    let verifier_dir = TempDir::new().expect("verifier tmp");
    let verifier = verifier(&verifier_dir, &url).await;

    let out = run_agentium(
        &sender_dir,
        &db_path,
        &url,
        &["mode", SESSION, "acceptEdits"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(stdout.contains("accept_edits mode"), "{stdout}");

    let landed = wait_until(&verifier, |ndb| {
        notes_with_role(ndb, "set_permission_mode")
            .iter()
            .any(|(_, content)| {
                serde_json::from_str::<serde_json::Value>(content)
                    .is_ok_and(|v| v == serde_json::json!({ "mode": "accept_edits" }))
            })
    })
    .await;
    assert!(landed, "the mode command should reach the verifier");

    relay.shutdown();
}

/// With nothing pending, `approve` fails loudly instead of publishing a stray
/// response.
#[tokio::test]
async fn approve_with_nothing_pending_errors() {
    let dir = TempDir::new().expect("tmp dir");
    let (db_path, _) = seed_sender_cache(&dir, &[]).await;

    let out = run_agentium(&dir, &db_path, "ws://127.0.0.1:1", &["approve", SESSION]);
    assert!(!out.status.success(), "nothing pending must exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no pending permission request"),
        "should say nothing is pending:\n{stderr}"
    );
}

/// An `AskUserQuestion` request can't be approved from the CLI (approving one
/// is sending its answers), but the refusal names the way out.
#[tokio::test]
async fn approve_refuses_a_question() {
    let dir = TempDir::new().expect("tmp dir");
    let question = serde_json::json!({
        "questions": [{
            "question": "Which?",
            "header": "Pick",
            "options": [{ "label": "A", "description": "a" }],
            "multiSelect": false,
        }]
    });
    let (db_path, _) = seed_sender_cache(&dir, &[("AskUserQuestion", question)]).await;

    let out = run_agentium(&dir, &db_path, "ws://127.0.0.1:1", &["approve", SESSION]);
    assert!(
        !out.status.success(),
        "approving a question must exit nonzero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("AskUserQuestion") && stderr.contains("agentium deny"),
        "should explain the refusal:\n{stderr}"
    );
}
