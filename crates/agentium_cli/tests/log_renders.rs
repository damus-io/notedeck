//! End-to-end check that the real `agentium log` renders a session's
//! kind-1988 conversation transcript.
//!
//! It seeds a signed kind-31988 session-state event (so the selector resolves)
//! plus signed kind-1988 conversation notes, then runs the actual binary against
//! that cache dir and asserts the rendered transcript. The relay points at a
//! closed port so connect fails fast and the bounded sync-settle elapses into a
//! cache read — mirroring `list_renders.rs`.
//!
//! The load-bearing assertion is ordering: the notes are seeded so their `seq`
//! tag order *disagrees* with their `ms` (wall-clock) order, and the transcript
//! must follow `ms`. That guards the card's one review note — the display axis is
//! `EventOrder` (millisecond wall-clock), never `seq`.

use std::io::{BufRead, BufReader};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nostrdb::{Config, Ndb, NoteBuilder};
use tempfile::TempDir;

/// `[7u8; 32]` as an nsec — the same key the seeded events are signed with, so
/// the engine (which reads sessions authored by its own key) finds them.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];
const KIND_SESSION_STATE: u32 = 31988;
const KIND_CONVERSATION: u32 = 1988;
const KIND_SOURCE_DATA: u32 = 1989;

/// Seed a signed kind-31988 session-state event so a selector can resolve to a
/// session whose conversation we then seed. Minimal — just the tags
/// `SessionState::from_note` needs to place it in the list.
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
    ingest(ndb, &note);
}

/// Seed a signed kind-1988 conversation note with the `d`/`role`/`seq`/`ms` tags
/// the loader reads. `ms` is the authoritative ordering axis; `seq` is passed
/// deliberately out of step with it so the ordering test is meaningful. A
/// `tool_result` note also carries a `tool-name` tag, like the real event.
fn seed_message(ndb: &Ndb, d: &str, role: &str, content: &str, seq: u32, ms: u64) {
    let mut builder = NoteBuilder::new()
        .kind(KIND_CONVERSATION)
        .content(content)
        .start_tag()
        .tag_str("d")
        .tag_str(d)
        .start_tag()
        .tag_str("role")
        .tag_str(role)
        .start_tag()
        .tag_str("seq")
        .tag_str(&seq.to_string())
        .start_tag()
        .tag_str("ms")
        .tag_str(&ms.to_string());
    if role == "tool_result" {
        builder = builder.start_tag().tag_str("tool-name").tag_str("Bash");
    }
    let note = builder.sign(&SECKEY).build().expect("build conv note");
    ingest(ndb, &note);
}

/// Seed a signed kind-1989 source-data note carrying one raw JSONL line, as the
/// `--jsonl` reconstructor reads (`d` + `seq` + `source-data`).
fn seed_source_line(ndb: &Ndb, d: &str, seq: u32, line: &str) {
    let note = NoteBuilder::new()
        .kind(KIND_SOURCE_DATA)
        .content("")
        .start_tag()
        .tag_str("d")
        .tag_str(d)
        .start_tag()
        .tag_str("seq")
        .tag_str(&seq.to_string())
        .start_tag()
        .tag_str("source-data")
        .tag_str(line)
        .sign(&SECKEY)
        .build()
        .expect("build source note");
    ingest(ndb, &note);
}

fn ingest(ndb: &Ndb, note: &nostrdb::Note) {
    let frame = format!(r#"["EVENT",{}]"#, note.json().expect("note json"));
    ndb.process_client_event(&frame).expect("ingest");
}

/// Run the real `agentium` binary against the seeded cache `db_path` (with `dir`
/// redirecting XDG/HOME so the run can't touch real config), passing `args` after
/// `log`. Asserts a clean exit and returns stdout. The relay port is closed so
/// connect fails fast and the bounded sync-settle elapses into a cache read.
fn run_log(db_path: &str, dir: &TempDir, args: &[&str]) -> String {
    let mut argv = vec![
        "--nsec",
        NSEC,
        "--db",
        db_path,
        "--relay",
        "ws://127.0.0.1:1",
        "log",
    ];
    argv.extend_from_slice(args);
    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args(&argv)
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run agentium");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    stdout
}

/// Seed a state plus a three-message conversation whose `seq` order (assistant,
/// user "again", user "first") disagrees with the `ms` order (user "first",
/// assistant, user "again"). Returns the db path; the db handle is dropped so the
/// subprocess opens the committed cache cleanly.
async fn seed_conversation(dir: &TempDir) -> String {
    let db_path = dir.path().to_str().expect("path").to_string();
    let ndb = Ndb::new(&db_path, &Config::new()).expect("ndb");
    let filter = nostrdb::Filter::new()
        .kinds([KIND_SESSION_STATE as u64, KIND_CONVERSATION as u64])
        .build();
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .expect("subscribe");
    seed_state(&ndb, "sess-conv", "Chat session");
    // ms-order: first(1000) < answer(2000) < again(3000).
    // seq-order: answer(1) < again(3) < first(5) — deliberately different.
    seed_message(&ndb, "sess-conv", "user", "first question", 5, 1000);
    seed_message(&ndb, "sess-conv", "assistant", "the answer", 1, 2000);
    seed_message(&ndb, "sess-conv", "user", "again please", 3, 3000);
    // Wait until all four events are ingested before the subprocess opens the db.
    // `wait_for_all_notes` accumulates to the full count — `wait_for_notes`
    // returns after *any* batch, which can race the multi-event seed.
    ndb.wait_for_all_notes(sub, 4)
        .await
        .expect("ingest seeded events");
    db_path
}

/// Assert `first` appears before `second` in `haystack`.
fn assert_before(haystack: &str, first: &str, second: &str) {
    let a = haystack
        .find(first)
        .unwrap_or_else(|| panic!("missing {first:?} in:\n{haystack}"));
    let b = haystack
        .find(second)
        .unwrap_or_else(|| panic!("missing {second:?} in:\n{haystack}"));
    assert!(
        a < b,
        "{first:?} must render before {second:?} (event order, not seq):\n{haystack}"
    );
}

#[tokio::test]
async fn log_renders_in_event_order_not_seq() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = seed_conversation(&dir).await;

    let stdout = run_log(&db_path, &dir, &["sess-conv"]);

    // All three bodies render, each under its role header.
    for needle in [
        "user",
        "assistant",
        "first question",
        "the answer",
        "again please",
    ] {
        assert!(stdout.contains(needle), "missing {needle:?} in:\n{stdout}");
    }
    // Ordering is by `ms` (wall-clock), not `seq`: first → answer → again.
    assert_before(&stdout, "first question", "the answer");
    assert_before(&stdout, "the answer", "again please");
}

#[tokio::test]
async fn log_role_and_last_filters() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = seed_conversation(&dir).await;

    // --role user keeps only the two user messages, dropping the assistant one.
    let only_users = run_log(&db_path, &dir, &["sess-conv", "--role", "user"]);
    assert!(only_users.contains("first question") && only_users.contains("again please"));
    assert!(
        !only_users.contains("the answer"),
        "--role user must drop the assistant message:\n{only_users}"
    );

    // Comma-separated roles select the union: user OR assistant is all three.
    let both = run_log(&db_path, &dir, &["sess-conv", "--role", "user,assistant"]);
    assert!(
        both.contains("first question")
            && both.contains("the answer")
            && both.contains("again please"),
        "--role user,assistant keeps every message here:\n{both}"
    );

    // -n 1 (short for --last) keeps only the final (ms-newest) message.
    let last_one = run_log(&db_path, &dir, &["sess-conv", "-n", "1"]);
    assert!(last_one.contains("again please"));
    assert!(
        !last_one.contains("first question"),
        "-n 1 keeps only the trailing message:\n{last_one}"
    );
}

#[tokio::test]
async fn log_color_flag_overrides_tty_detection() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = seed_conversation(&dir).await;

    // Piped (non-tty) stdout: default `auto` stays plain, so the transcript is
    // safe to grep or redirect.
    let plain = run_log(&db_path, &dir, &["sess-conv"]);
    assert!(
        !plain.contains('\x1b'),
        "auto color must stay off when piped:\n{plain:?}"
    );

    // `--color always` keeps ANSI even when piped — the "pipe into my own less"
    // case. `--no-pager` is a harmless explicit here (already unpaged when piped).
    let colored = run_log(
        &db_path,
        &dir,
        &["sess-conv", "--color", "always", "--no-pager"],
    );
    assert!(
        colored.contains('\x1b'),
        "--color always must emit ANSI even when piped:\n{colored:?}"
    );
    // Still the same transcript, just painted.
    assert!(colored.contains("first question"));
}

#[tokio::test]
async fn log_pager_routes_output_through_the_pager_command() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = seed_conversation(&dir).await;

    // Force paging with a transparent pager (`cat`), exercising the real
    // spawn → write → wait path even though stdout is a pipe. The transcript
    // must reach us through the pager unchanged.
    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args([
            "--nsec",
            NSEC,
            "--db",
            &db_path,
            "--relay",
            "ws://127.0.0.1:1",
            "log",
            "sess-conv",
            "--pager",
        ])
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .env("AGENTIUM_PAGER", "cat")
        .output()
        .expect("run agentium");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "nonzero exit:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("first question") && stdout.contains("again please"),
        "paged output must carry the transcript:\n{stdout}"
    );
}

#[tokio::test]
async fn log_json_emits_role_tagged_objects() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = seed_conversation(&dir).await;

    let stdout = run_log(&db_path, &dir, &["sess-conv", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON array");
    let rows = parsed.as_array().expect("array");
    assert_eq!(rows.len(), 3);
    // Emitted in event order, each tagged by role.
    assert_eq!(rows[0]["role"], "user");
    assert_eq!(rows[0]["text"], "first question");
    assert_eq!(rows[1]["role"], "assistant");
    assert_eq!(rows[1]["text"], "the answer");
    assert_eq!(rows[2]["role"], "user");
    assert_eq!(rows[2]["text"], "again please");
}

/// Drain a child's stdout on a background thread into a channel, so the test can
/// read lines incrementally (the `--follow` child never exits, so `.output()`
/// would block forever).
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

/// Block until a line containing `needle` arrives (or `timeout` elapses),
/// returning every line seen so far. Panics on timeout with the transcript, so a
/// stuck follower fails loudly instead of hanging the suite.
fn wait_for_line(rx: &mpsc::Receiver<String>, needle: &str, timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                let hit = line.contains(needle);
                seen.push(line);
                if hit {
                    return seen;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!(
        "did not see {needle:?} within {timeout:?}; saw:\n{}",
        seen.join("\n")
    );
}

/// `agentium log --follow` streams a message that lands *after* the initial
/// tail, without a restart. Unlike the other tests (which point the relay at a
/// closed port and read a static cache), this stands up a real in-process relay
/// so a live event can flow into the child's cache and fire its watch: a
/// same-identity publisher engine `send_message`s through the relay, and the new
/// line must appear on the running child's stdout. Then we kill the child rather
/// than wait on Ctrl-C.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn follow_streams_a_live_message() {
    use agentium_core::Engine;

    // A real relay backed by its own ndb — the seam the live event crosses (a
    // cross-process cache write can't fire the child's in-process subscription).
    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &Config::new()).expect("relay ndb");
    let relay =
        nostrdb_relay::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr")).expect("spawn relay");
    let url = relay.url();

    // Seed the child's cache with the session state + one message, so the
    // selector resolves and the follow has a tail to print — its arrival on
    // stdout is our "child is now in the follow loop" signal.
    let child_dir = TempDir::new().expect("child tmp");
    let db_path = child_dir.path().to_str().expect("path").to_string();
    {
        let ndb = Ndb::new(&db_path, &Config::new()).expect("seed ndb");
        let filter = nostrdb::Filter::new()
            .kinds([KIND_SESSION_STATE as u64, KIND_CONVERSATION as u64])
            .build();
        let sub = ndb
            .subscribe(std::slice::from_ref(&filter))
            .expect("subscribe");
        seed_state(&ndb, "sess-follow", "Followed session");
        seed_message(&ndb, "sess-follow", "user", "initial tail msg", 1, 1000);
        ndb.wait_for_all_notes(sub, 2).await.expect("ingest seed");
    }

    // Spawn the real binary in --follow against the reachable relay, so live
    // envelopes keep flowing into its cache and firing the watch.
    let mut child = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args([
            "--nsec",
            NSEC,
            "--db",
            &db_path,
            "--relay",
            &url,
            "log",
            "sess-follow",
            "--follow",
        ])
        .env("XDG_DATA_HOME", child_dir.path())
        .env("HOME", child_dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn agentium --follow");
    let lines = spawn_line_reader(child.stdout.take().expect("child stdout"));

    // Initial tail printed → the child has synced and entered the follow loop.
    wait_for_line(&lines, "initial tail msg", Duration::from_secs(20));

    // A same-identity publisher engine sends a new message through the relay.
    let pub_dir = TempDir::new().expect("pub tmp");
    let mut publisher =
        Engine::open(pub_dir.path().to_str().expect("path"), SECKEY).expect("publisher engine");
    let mut pub_tx = publisher.transport_handle().expect("pub transport");
    publisher.connect(&mut pub_tx, &url).expect("pub connect");
    publisher
        .send_message(&mut pub_tx, "sess-follow", "live streamed msg")
        .expect("send live message");
    // Flush the publish through the loop's FIFO so it actually reaches the relay.
    let _ = tokio::time::timeout(Duration::from_secs(5), publisher.wait_for_sync()).await;

    // The live message streams onto the still-running child's stdout.
    wait_for_line(&lines, "live streamed msg", Duration::from_secs(20));

    let _ = child.kill();
    let _ = child.wait();
    relay.shutdown();
}

#[tokio::test]
async fn log_jsonl_reconstructs_source_archive_in_seq_order() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = dir.path().to_str().expect("path").to_string();

    {
        let ndb = Ndb::new(&db_path, &Config::new()).expect("ndb");
        let filter = nostrdb::Filter::new()
            .kinds([KIND_SESSION_STATE as u64, KIND_SOURCE_DATA as u64])
            .build();
        let sub = ndb
            .subscribe(std::slice::from_ref(&filter))
            .expect("subscribe");
        seed_state(&ndb, "sess-src", "Archived session");
        // Seeded out of seq order; the reconstructor sorts back to 1,2.
        seed_source_line(&ndb, "sess-src", 2, r#"{"type":"assistant","n":2}"#);
        seed_source_line(&ndb, "sess-src", 1, r#"{"type":"user","n":1}"#);
        ndb.wait_for_all_notes(sub, 3)
            .await
            .expect("ingest seeded events");
    }

    let stdout = run_log(&db_path, &dir, &["sess-src", "--jsonl"]);
    // Raw source lines, emitted verbatim in original (seq) order.
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![r#"{"type":"user","n":1}"#, r#"{"type":"assistant","n":2}"#],
        "jsonl must replay raw source lines in seq order:\n{stdout}"
    );
}
