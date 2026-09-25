//! End-to-end checks of the real `agentium watch` dashboard.
//!
//! `watch --once` renders one frame from a seeded cache (relay pointed at a
//! closed port, as in `list_renders.rs`); the live test stands up an in-process
//! relay and asserts a status change published by another engine redraws the
//! running dashboard, as `log_renders.rs` does for `log --follow`.

use std::io::{BufRead, BufReader};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nostrdb::{Config, Ndb, NoteBuilder};
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

/// `[7u8; 32]` as an nsec — the key the seeded events are signed with, so the
/// engine (which reads sessions authored by its own key) finds them.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];
const KIND_SESSION_STATE: u32 = 31988;
const KIND_CONVERSATION: u32 = 1988;

/// Ingest a signed kind-31988 session-state event for session `d`.
fn seed_session(ndb: &Ndb, d: &str, title: &str, status: &str, host: &str, created_at: u64) {
    let note = NoteBuilder::new()
        .kind(KIND_SESSION_STATE)
        .content("")
        .created_at(created_at)
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
        .tag_str(host)
        .start_tag()
        .tag_str("cwd")
        .tag_str("/home/u/proj")
        .start_tag()
        .tag_str("home_dir")
        .tag_str("/home/u")
        .start_tag()
        .tag_str("backend")
        .tag_str("claude")
        .sign(&SECKEY)
        .build()
        .expect("build state note");
    ingest(ndb, &note);
}

/// Ingest a signed kind-1988 message for session `d` — only its `d` tag and
/// `created_at` matter to the dashboard's last-activity column.
fn seed_message(ndb: &Ndb, d: &str, created_at: u64) {
    let note = NoteBuilder::new()
        .kind(KIND_CONVERSATION)
        .content("streamed output")
        .created_at(created_at)
        .start_tag()
        .tag_str("d")
        .tag_str(d)
        .start_tag()
        .tag_str("role")
        .tag_str("assistant")
        .sign(&SECKEY)
        .build()
        .expect("build message note");
    ingest(ndb, &note);
}

fn ingest(ndb: &Ndb, note: &nostrdb::Note) {
    let frame = format!(r#"["EVENT",{}]"#, note.json().expect("note json"));
    ndb.process_client_event(&frame).expect("ingest");
}

/// Seed three sessions into a fresh cache at `db_path`, then drop the handle so
/// the child opens the committed cache. "Quiet" has an old state revision but a
/// fresh message, so its age must come from the message.
async fn seed_dashboard(db_path: &str, now: u64) {
    let ndb = Ndb::new(db_path, &test_config()).expect("ndb");
    let filter = nostrdb::Filter::new()
        .kinds([KIND_SESSION_STATE as u64, KIND_CONVERSATION as u64])
        .build();
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .expect("subscribe");
    seed_session(&ndb, "sess-busy", "Streaming", "working", "linux", now - 30);
    seed_session(
        &ndb,
        "sess-ask",
        "Waiting",
        "needs_input",
        "macbook",
        now - 7200,
    );
    seed_session(
        &ndb,
        "sess-quiet",
        "Quiet",
        "idle",
        "macbook",
        now - 86_400 * 3,
    );
    seed_message(&ndb, "sess-quiet", now - 120);
    ndb.wait_for_all_notes(sub, 4).await.expect("ingest seed");
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

#[tokio::test]
async fn watch_once_renders_the_dashboard() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = dir.path().to_str().expect("path").to_string();
    seed_dashboard(&db_path, now_secs()).await;

    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args([
            "--nsec",
            NSEC,
            "--db",
            &db_path,
            "--relay",
            "ws://127.0.0.1:1",
            "watch",
            "--once",
        ])
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .env_remove("COLUMNS")
        .output()
        .expect("run agentium watch");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let lines: Vec<&str> = stdout.lines().collect();
    assert!(!stdout.contains('\x1b'), "a pipe gets no ANSI:\n{stdout}");
    assert_eq!(
        lines[0], "3 sessions · 1 needs input · 1 working · 1 idle",
        "{stdout}"
    );
    // The session waiting on the user sorts first despite being older than the
    // working one, and carries the marker.
    assert!(lines[2].starts_with("» ") && lines[2].contains("Waiting"));
    assert!(lines[3].contains("Streaming") && lines[3].contains("linux"));
    // Quiet's state is days old, but its message two minutes ago is what counts.
    assert!(
        lines[4].contains("Quiet") && lines[4].contains("2m ago"),
        "{}",
        lines[4]
    );
}

/// Drain a child's stdout on a background thread into a channel, so the test can
/// read lines incrementally from a `watch` that never exits.
fn spawn_line_reader(stdout: ChildStdout) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(l) = line else { break };
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    rx
}

/// Block until a line containing every one of `needles` arrives (or `timeout`
/// elapses), panicking with everything seen so a stuck dashboard fails loudly.
fn wait_for_line(rx: &mpsc::Receiver<String>, needles: &[&str], timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                let hit = needles.iter().all(|n| line.contains(n));
                seen.push(line);
                if hit {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!(
        "no line with {needles:?} within {timeout:?}; saw:\n{}",
        seen.join("\n")
    );
}

/// A running `watch` redraws when another engine publishes a status change
/// through the relay: the session flips from Working to Needs Input and the new
/// frame shows it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watch_redraws_on_a_live_status_change() {
    use agentium_core::Engine;
    use agentium_core::session_events::build_session_state_event;

    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &test_config()).expect("relay ndb");
    let relay = nostrdb_net::relay::server::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr"))
        .expect("spawn relay");
    let url = relay.url();

    let now = now_secs();
    let child_dir = TempDir::new().expect("child tmp");
    let db_path = child_dir.path().to_str().expect("path").to_string();
    {
        let ndb = Ndb::new(&db_path, &test_config()).expect("seed ndb");
        let filter = nostrdb::Filter::new()
            .kinds([KIND_SESSION_STATE as u64])
            .build();
        let sub = ndb
            .subscribe(std::slice::from_ref(&filter))
            .expect("subscribe");
        seed_session(
            &ndb,
            "sess-live",
            "Live session",
            "working",
            "macbook",
            now - 60,
        );
        ndb.wait_for_all_notes(sub, 1).await.expect("ingest seed");
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args(["--nsec", NSEC, "--db", &db_path, "--relay", &url, "watch"])
        .env("XDG_DATA_HOME", child_dir.path())
        .env("HOME", child_dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn agentium watch");
    let lines = spawn_line_reader(child.stdout.take().expect("child stdout"));

    // The first frame is our "child is now watching" signal.
    wait_for_line(
        &lines,
        &["Live session", "Working"],
        Duration::from_secs(20),
    );

    // Another engine on the same identity flips the session to needs_input.
    let host_dir = TempDir::new().expect("host tmp");
    let mut host =
        Engine::open(host_dir.path().to_str().expect("path"), SECKEY).expect("host engine");
    host.connect(&url).expect("host connect");
    let state = build_session_state_event(
        "sess-live",
        "Live session",
        None,
        "/home/u/proj",
        "needs_input",
        None,
        "macbook",
        "/home/u",
        "claude",
        "default",
        None,
        None,
        None,
        None,
        now,
        &SECKEY,
    )
    .expect("build state");
    host.publish_event(&state).expect("publish state");
    let _ = tokio::time::timeout(Duration::from_secs(5), host.wait_for_sync()).await;

    wait_for_line(
        &lines,
        &["Live session", "Needs Input"],
        Duration::from_secs(20),
    );

    let _ = child.kill();
    let _ = child.wait();
    relay.shutdown();
}
