//! End-to-end check that the real `agentium` binary says why a read came from
//! the cache: a relay that accepts and then never answers holds the sync until
//! `SYNC_MAX`, and an unreachable one makes the backfill give up. Either way the
//! command still prints its cached result and exits cleanly, and stderr (piped,
//! so not a tty) carries one plain line, with no progress-bar bytes.

use std::net::TcpListener;
use std::process::{Command, Output};

use tempfile::TempDir;

/// Any key works: the cache is empty, so there is nothing to decrypt.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";

/// Run `agentium list` against `relay` with an empty cache in `dir`.
fn run_list(dir: &TempDir, relay: &str) -> Output {
    let db = dir.path().to_str().expect("path");
    Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args(["--nsec", NSEC, "--db", db, "--relay", relay, "list"])
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run agentium")
}

/// Assert a clean exit and plain stderr, and return stderr.
fn plain_stderr(out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        !stderr.contains('\r') && !stderr.contains('\x1b'),
        "piped stderr must carry no progress-bar bytes: {stderr:?}"
    );
    stderr
}

#[test]
fn a_stalled_relay_says_the_sync_is_still_running() {
    // Accepts the TCP connection (the listen backlog does it for us) and never
    // answers the WebSocket upgrade, so the backfill neither settles nor fails.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let relay = format!("ws://{}", listener.local_addr().expect("addr"));
    let dir = TempDir::new().expect("tmp dir");

    let out = run_list(&dir, &relay);
    let stderr = plain_stderr(&out);
    assert!(
        stderr.contains("syncing sessions: still running after 6s, showing cached sessions"),
        "{stderr}"
    );
    drop(listener);
}

#[test]
fn an_unreachable_relay_says_the_sync_failed() {
    let dir = TempDir::new().expect("tmp dir");
    let out = run_list(&dir, "ws://127.0.0.1:1");
    let stderr = plain_stderr(&out);
    assert!(
        stderr.contains("syncing sessions: failed, showing cached sessions"),
        "{stderr}"
    );
}
