//! End-to-end check that the real `agentium` binary renders synced session
//! state.
//!
//! It seeds signed kind-31988 events into a cache dir, then runs the actual
//! binary against that dir and asserts the rendered rows. This exercises the
//! binary's open → read → render path over real events; PNS decrypt and relay
//! reconcile are covered by agentium-core's own tests, and the broader
//! over-a-relay suite is a separate card. The relay points at a closed port so
//! connect fails fast and the bounded sync-settle elapses into a cache read.

use std::process::Command;

use nostrdb::{Config, Ndb, NoteBuilder};
use tempfile::TempDir;

/// `[7u8; 32]` as an nsec — the same key the seeded events are signed with, so
/// the engine (which reads sessions authored by its own key) finds them.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];
const KIND_SESSION_STATE: u32 = 31988;

/// Build and ingest a signed kind-31988 session-state event with the tags
/// `SessionState::from_note` reads.
fn seed_session(
    ndb: &Ndb,
    d: &str,
    title: &str,
    status: &str,
    host: &str,
    cwd: &str,
    backend: &str,
) {
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
        .tag_str(host)
        .start_tag()
        .tag_str("cwd")
        .tag_str(cwd)
        .start_tag()
        .tag_str("home_dir")
        .tag_str("/home/u")
        .start_tag()
        .tag_str("backend")
        .tag_str(backend)
        .start_tag()
        .tag_str("permission-mode")
        .tag_str("default")
        .sign(&SECKEY)
        .build()
        .expect("build note");
    let frame = format!(r#"["EVENT",{}]"#, note.json().expect("note json"));
    ndb.process_client_event(&frame).expect("ingest");
}

/// Run the real `agentium` binary against the seeded cache `db_path` (with `dir`
/// redirecting XDG/HOME so the run can't touch real config), passing `list_args`
/// after `list`. Asserts a clean exit and returns stdout. The relay port is
/// closed so connect fails fast and the bounded sync-settle elapses into a cache
/// read.
fn run_list(db_path: &str, dir: &TempDir, list_args: &[&str]) -> String {
    let mut args = vec![
        "--nsec",
        NSEC,
        "--db",
        db_path,
        "--relay",
        "ws://127.0.0.1:1",
        "list",
    ];
    args.extend_from_slice(list_args);
    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args(&args)
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

#[tokio::test]
async fn list_renders_seeded_sessions() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = dir.path().to_str().expect("path").to_string();

    // Seed two sessions, then drop the db handle so the subprocess opens the
    // committed cache cleanly.
    {
        let ndb = Ndb::new(&db_path, &Config::new()).expect("ndb");
        let filter = nostrdb::Filter::new()
            .kinds([KIND_SESSION_STATE as u64])
            .build();
        let sub = ndb
            .subscribe(std::slice::from_ref(&filter))
            .expect("subscribe");
        seed_session(
            &ndb,
            "sess-1",
            "Draft release notes",
            "working",
            "macbook",
            "/home/u/proj",
            "claude",
        );
        seed_session(
            &ndb,
            "sess-2",
            "Fix relay reconnect",
            "needs_input",
            "macbook",
            "/home/u/other",
            "codex",
        );
        ndb.wait_for_notes(sub, 2)
            .await
            .expect("ingest seeded events");
    }

    // Run the real binary against the seeded cache.
    let stdout = run_list(&db_path, &dir, &[]);
    // The host group header, both titles, both status labels, and the
    // needs-input summary all render.
    for needle in [
        "macbook",
        "Draft release notes",
        "Fix relay reconnect",
        "Working",
        "Needs Input",
        "session(s) need input",
    ] {
        assert!(
            stdout.contains(needle),
            "missing {needle:?} in output:\n{stdout}"
        );
    }
}

/// `list --json` emits each session's raw state *plus* the flattened
/// `agentium_uri` field, so an external agent reads the sayable ref straight from
/// machine output instead of re-encoding the word-id. The URI must equal
/// `agentium_core`'s canonical encoding of the session's stable id.
#[tokio::test]
async fn list_json_includes_agentium_uri() {
    let dir = TempDir::new().expect("tmp dir");
    let db_path = dir.path().to_str().expect("path").to_string();

    let d = "sess-json";
    {
        let ndb = Ndb::new(&db_path, &Config::new()).expect("ndb");
        let filter = nostrdb::Filter::new()
            .kinds([KIND_SESSION_STATE as u64])
            .build();
        let sub = ndb
            .subscribe(std::slice::from_ref(&filter))
            .expect("subscribe");
        seed_session(
            &ndb,
            d,
            "Draft release notes",
            "working",
            "macbook",
            "/home/u/proj",
            "claude",
        );
        ndb.wait_for_notes(sub, 1)
            .await
            .expect("ingest seeded event");
    }

    let out = Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args([
            "--nsec",
            NSEC,
            "--db",
            &db_path,
            "--relay",
            "ws://127.0.0.1:1",
            "list",
            "--json",
        ])
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run agentium");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );

    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON array");
    let row = &parsed.as_array().expect("array")[0];
    // The flattened state is still present alongside the added URI.
    assert_eq!(row["claude_session_id"], d);
    assert_eq!(
        row["agentium_uri"],
        serde_json::Value::String(agentium_core::wordid::session_ref(d)),
        "agentium_uri must be the canonical encoding of the stable id"
    );
}

/// A soft-deleted session is hidden from the default `list` but surfaced by
/// `--deleted` (deleted only) and `--all` (both) — so a tombstoned session, and
/// the durable `agentium:` ref quoting it, stay discoverable end-to-end.
#[tokio::test]
async fn list_deleted_scope_surfaces_tombstones() {
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
        seed_session(
            &ndb,
            "sess-live",
            "Live work",
            "working",
            "macbook",
            "/home/u/proj",
            "claude",
        );
        seed_session(
            &ndb,
            "sess-gone",
            "Closed spike",
            "deleted",
            "macbook",
            "/home/u/spike",
            "claude",
        );
        ndb.wait_for_notes(sub, 2)
            .await
            .expect("ingest seeded events");
    }

    // Default: the tombstone is hidden, the live session shows.
    let default = run_list(&db_path, &dir, &[]);
    assert!(
        default.contains("Live work"),
        "live shows by default:\n{default}"
    );
    assert!(
        !default.contains("Closed spike"),
        "deleted session must be hidden by default:\n{default}"
    );

    // --deleted: only the tombstone, with its Deleted label.
    let deleted = run_list(&db_path, &dir, &["--deleted"]);
    assert!(
        deleted.contains("Closed spike") && deleted.contains("Deleted"),
        "--deleted surfaces the tombstone:\n{deleted}"
    );
    assert!(
        !deleted.contains("Live work"),
        "--deleted excludes live sessions:\n{deleted}"
    );

    // --all: both.
    let all = run_list(&db_path, &dir, &["--all"]);
    assert!(
        all.contains("Live work") && all.contains("Closed spike"),
        "--all shows live and deleted together:\n{all}"
    );
}
