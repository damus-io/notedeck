//! End-to-end checks for `agentium resume` over the real binary.
//!
//! Like `list_renders.rs`, it seeds signed kind-31988 events into a cache dir
//! and runs the actual binary against a closed relay port (connect fails fast,
//! the bounded sync-settle elapses, and the read falls through to the cache).
//! These cover the binary's resolve + error surface; the host-side reopen +
//! revive lives in notedeck_dave and the engine publish path in agentium-core.

use std::process::Command;

use nostrdb::{Config, Ndb, NoteBuilder};
use tempfile::TempDir;

/// `[7u8; 32]` as an nsec — the key the seeded events are signed with, so the
/// engine (which reads sessions authored by its own key) finds them.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];
const KIND_SESSION_STATE: u32 = 31988;

/// Seed a signed kind-31988 session-state event. `cli` sets the `cli_session`
/// tag (the id `claude --resume` needs); pass `None` to omit it, modelling a
/// session whose backend never started.
fn seed_session(ndb: &Ndb, d: &str, status: &str, cli: Option<&str>) {
    let mut builder = NoteBuilder::new()
        .kind(KIND_SESSION_STATE)
        .content("")
        .start_tag()
        .tag_str("d")
        .tag_str(d)
        .start_tag()
        .tag_str("title")
        .tag_str("Dead Session")
        .start_tag()
        .tag_str("status")
        .tag_str(status)
        .start_tag()
        .tag_str("hostname")
        .tag_str("build-server")
        .start_tag()
        .tag_str("cwd")
        .tag_str("/home/u/proj")
        .start_tag()
        .tag_str("home_dir")
        .tag_str("/home/u")
        .start_tag()
        .tag_str("backend")
        .tag_str("claude")
        .start_tag()
        .tag_str("permission-mode")
        .tag_str("default");
    if let Some(cli) = cli {
        builder = builder.start_tag().tag_str("cli_session").tag_str(cli);
    }
    let note = builder.sign(&SECKEY).build().expect("build note");
    let frame = format!(r#"["EVENT",{}]"#, note.json().expect("note json"));
    ndb.process_client_event(&frame).expect("ingest");
}

/// Run the real `agentium` binary against the seeded cache `db_path`, redirecting
/// XDG/HOME so the run can't touch real config. Returns `(success, stdout,
/// stderr)`. The relay port is closed so connect fails fast.
fn run_agentium(db_path: &str, dir: &TempDir, args: &[&str]) -> (bool, String, String) {
    let mut full = vec![
        "--nsec",
        NSEC,
        "--db",
        db_path,
        "--relay",
        "ws://127.0.0.1:1",
    ];
    full.extend_from_slice(args);
    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args(&full)
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run agentium");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test]
async fn resume_reports_command_for_deleted_session() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = dir.path().to_str().expect("path").to_string();
    {
        let ndb = Ndb::new(&db_path, &Config::new()).expect("ndb");
        let filter = nostrdb::Filter::new()
            .kinds([KIND_SESSION_STATE as u64])
            .build();
        let sub = ndb
            .subscribe(std::slice::from_ref(&filter))
            .expect("subscribe");
        // A soft-deleted session whose backend did run (has a cli_session).
        seed_session(&ndb, "dead-1", "deleted", Some("cli-abc"));
        let _ = ndb.wait_for_notes(sub, 1).await.expect("indexed");
    }

    // Resolve the tombstoned session by its d-tag and report the resume command.
    let (ok, stdout, stderr) = run_agentium(&db_path, &dir, &["resume", "dead-1"]);
    assert!(
        ok,
        "resume should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("resume command sent") && stdout.contains("build-server"),
        "expected a resume confirmation naming the host, got:\n{stdout}"
    );
}

#[tokio::test]
async fn resume_errors_on_unknown_selector() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = dir.path().to_str().expect("path").to_string();
    {
        let ndb = Ndb::new(&db_path, &Config::new()).expect("ndb");
        seed_session(&ndb, "dead-1", "deleted", Some("cli-abc"));
    }

    let (ok, _stdout, stderr) = run_agentium(&db_path, &dir, &["resume", "no-such-session"]);
    assert!(!ok, "resume of an unknown selector must fail");
    assert!(
        stderr.contains("no session"),
        "expected a no-match error, got stderr:\n{stderr}"
    );
}

#[tokio::test]
async fn resume_errors_when_backend_never_started() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = dir.path().to_str().expect("path").to_string();
    {
        let ndb = Ndb::new(&db_path, &Config::new()).expect("ndb");
        let filter = nostrdb::Filter::new()
            .kinds([KIND_SESSION_STATE as u64])
            .build();
        let sub = ndb
            .subscribe(std::slice::from_ref(&filter))
            .expect("subscribe");
        // No cli_session tag: the backend never started, so there's nothing to
        // --resume.
        seed_session(&ndb, "never-ran", "deleted", None);
        let _ = ndb.wait_for_notes(sub, 1).await.expect("indexed");
    }

    let (ok, _stdout, stderr) = run_agentium(&db_path, &dir, &["resume", "never-ran"]);
    assert!(!ok, "resume of a never-started session must fail");
    assert!(
        stderr.contains("no CLI session to resume"),
        "expected a never-started error, got stderr:\n{stderr}"
    );
}
