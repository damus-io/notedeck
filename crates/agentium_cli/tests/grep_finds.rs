//! End-to-end check that the real `agentium grep` searches message text across
//! *several* sessions in one run.
//!
//! Like `log_renders.rs`, it seeds signed kind-31988 state events plus their
//! kind-1988 conversation notes into a temp cache, then runs the actual binary
//! against that cache dir with the relay pointed at a closed port, so connect
//! fails fast and the read falls through to the cache.
//!
//! The load-bearing assertions are the ones the shell loop this command replaces
//! can't make cheaply: one invocation spans every session, the `list` filters
//! pick which sessions are searched, and the `log` filters pick which messages
//! are searched.

use std::process::Command;

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

/// `[7u8; 32]` as an nsec — the same key the seeded events are signed with, so
/// the engine (which reads sessions authored by its own key) finds them.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];
const KIND_SESSION_STATE: u32 = 31988;
const KIND_CONVERSATION: u32 = 1988;

/// Seed a signed kind-31988 session-state event. `cwd` varies across the seeded
/// sessions so the `--cwd` filter has something to select on.
fn seed_state(ndb: &Ndb, d: &str, title: &str, cwd: &str) {
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
        .tag_str(cwd)
        .start_tag()
        .tag_str("home_dir")
        .tag_str("/home/u")
        .sign(&SECKEY)
        .build()
        .expect("build state note");
    ingest(ndb, &note);
}

/// Seed a signed kind-1988 conversation note with the `d`/`role`/`seq`/`ms` tags
/// the loader reads.
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

fn ingest(ndb: &Ndb, note: &nostrdb::Note) {
    let frame = format!(r#"["EVENT",{}]"#, note.json().expect("note json"));
    ndb.process_client_event(&frame).expect("ingest");
}

/// Run the real `agentium` binary against the seeded cache `db_path` (with `dir`
/// redirecting XDG/HOME so the run can't touch real config), passing `args` after
/// `grep`. Asserts a clean exit and returns stdout. The relay port is closed so
/// connect fails fast and the bounded sync-settle elapses into a cache read.
fn run_grep(db_path: &str, dir: &TempDir, args: &[&str]) -> String {
    let mut argv = vec![
        "--nsec",
        NSEC,
        "--db",
        db_path,
        "--relay",
        "ws://127.0.0.1:1",
        "--no-pager",
        "--color",
        "never",
        "grep",
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

/// Seed two sessions in different working directories, each mentioning the
/// search term in a different role, plus one session that never mentions it.
/// Returns the db path; the db handle is dropped so the subprocess opens the
/// committed cache cleanly.
async fn seed_sessions(dir: &TempDir) -> String {
    let db_path = dir.path().to_str().expect("path").to_string();
    let ndb = Ndb::new(&db_path, &test_config()).expect("ndb");
    let filter = nostrdb::Filter::new()
        .kinds([KIND_SESSION_STATE as u64, KIND_CONVERSATION as u64])
        .build();
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .expect("subscribe");

    seed_state(&ndb, "sess-soh", "Ship of Harkinian", "/home/u/soh");
    seed_message(&ndb, "sess-soh", "user", "why does it crash?", 1, 1000);
    seed_message(
        &ndb,
        "sess-soh",
        "assistant",
        "the terminal resize hook is missing\nsecond line, no match",
        2,
        2000,
    );

    seed_state(&ndb, "sess-proj", "Other project", "/home/u/proj");
    seed_message(
        &ndb,
        "sess-proj",
        "user",
        "mind the TERMINAL width",
        1,
        1000,
    );
    seed_message(&ndb, "sess-proj", "tool_result", "terminal: ok", 2, 2000);

    seed_state(&ndb, "sess-quiet", "Quiet session", "/home/u/quiet");
    seed_message(&ndb, "sess-quiet", "user", "nothing to find here", 1, 1000);

    ndb.wait_for_all_notes(sub, 8)
        .await
        .expect("ingest seeded events");
    db_path
}

#[tokio::test]
async fn grep_spans_every_session_in_one_run() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = seed_sessions(&dir).await;

    // One invocation, every session: the pattern hits the assistant line in one
    // session and the tool_result in another, and never the session that
    // doesn't mention it.
    let out = run_grep(&db_path, &dir, &["terminal"]);
    assert!(
        out.contains("Ship of Harkinian") && out.contains("Other project"),
        "both matching sessions are headed by name: {out}"
    );
    assert!(
        !out.contains("Quiet session"),
        "a session with no match gets no header: {out}"
    );
    assert!(
        out.contains("agentium:"),
        "each header leads with the pasteable ref: {out}"
    );
    // Matching is per line, like grep: the assistant message's second line
    // doesn't match, so it isn't printed.
    assert!(out.contains("the terminal resize hook is missing"));
    assert!(
        !out.contains("second line, no match"),
        "non-matching lines are dropped: {out}"
    );
    // Smart-case: an all-lowercase pattern carries no case demand, so the
    // upper-case mention comes along without `-i`.
    assert!(
        out.contains("mind the TERMINAL width"),
        "a lowercase pattern folds case: {out}"
    );
    // `-s` restores grep(1)'s own default and drops it again...
    let out = run_grep(&db_path, &dir, &["-s", "terminal"]);
    assert!(
        !out.contains("mind the TERMINAL width"),
        "-s pins the case: {out}"
    );
    assert!(
        out.contains("the terminal resize hook is missing"),
        "-s still matches the exact spelling: {out}"
    );
    // ...as does spelling the case into the pattern, which is the same ask.
    let out = run_grep(&db_path, &dir, &["Terminal"]);
    assert!(
        !out.contains("the terminal resize hook is missing"),
        "an uppercase pattern matches exactly: {out}"
    );
    // And `-i` overrides that spelling.
    let out = run_grep(&db_path, &dir, &["-i", "Terminal"]);
    assert!(
        out.contains("the terminal resize hook is missing"),
        "-i folds case: {out}"
    );
}

#[tokio::test]
async fn grep_honors_the_session_and_message_filters() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = seed_sessions(&dir).await;

    // `--cwd` picks which sessions are searched at all.
    let out = run_grep(&db_path, &dir, &["--cwd", "soh", "-i", "terminal"]);
    assert!(out.contains("Ship of Harkinian"));
    assert!(
        !out.contains("Other project"),
        "--cwd narrows the search: {out}"
    );

    // `--role` picks which messages are searched, so the user mention of the
    // term is invisible to an assistant-only search.
    let out = run_grep(&db_path, &dir, &["--role", "assistant", "-i", "terminal"]);
    assert!(out.contains("the terminal resize hook is missing"));
    assert!(
        !out.contains("mind the TERMINAL width"),
        "--role narrows: {out}"
    );

    // `--no-tools` folds the tool_result away, leaving that session's only hit
    // the user line.
    let out = run_grep(&db_path, &dir, &["--no-tools", "-i", "terminal"]);
    assert!(
        !out.contains("terminal: ok"),
        "--no-tools folds tool results: {out}"
    );

    // A pattern nothing matches says so rather than printing nothing.
    let out = run_grep(&db_path, &dir, &["zzz-no-such-text"]);
    assert_eq!(out.trim(), "no matches");
}

#[tokio::test]
async fn grep_json_groups_matches_under_each_session() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = seed_sessions(&dir).await;

    let out = run_grep(&db_path, &dir, &["--json", "-i", "terminal"]);
    let rows: serde_json::Value = serde_json::from_str(&out).expect("json array");
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 2, "one row per matching session: {out}");

    let soh = rows
        .iter()
        .find(|r| r["title"] == "Ship of Harkinian")
        .expect("the soh session row");
    // The session identity is flattened in, so `agentium_uri` feeds straight
    // back into `log`/`send`.
    assert!(
        soh["agentium_uri"]
            .as_str()
            .is_some_and(|u| u.starts_with("agentium:")),
        "row carries the ref: {out}"
    );
    assert_eq!(soh["cwd"], "/home/u/soh");
    let matches = soh["matches"].as_array().expect("matches array");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["role"], "assistant");
    assert_eq!(matches[0]["text"], "the terminal resize hook is missing");
}
