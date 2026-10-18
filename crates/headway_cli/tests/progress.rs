//! The sync's progress line must stay out of captured stderr: when stderr is not
//! a terminal (agents, tests, pipes) no `\r` or escape byte may reach it, so
//! anything matching on stderr reads exactly the messages it always did.
//!
//! Drives the real `headway` binary through every relay leg — the plaintext
//! reconcile, the gift-wrap and PNS key-share pulls, the sealed-board envelope
//! sync, the self-share flush — against an embedded relay, and the offline
//! fallback against a closed port.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use nostrdb::{Config, Ndb};
use serde_json::Value;

/// Test signing key (the same all-`0x42` secret `e2e.rs` uses).
const SECRET: [u8; 32] = [0x42; 32];

fn nsec() -> String {
    let hrp = bech32::Hrp::parse("nsec").expect("hrp");
    bech32::encode::<bech32::Bech32>(hrp, &SECRET).expect("encode nsec")
}

/// A [`Config`] with a small mapsize on Windows, where LMDB allocates it on disk.
fn test_config() -> Config {
    if cfg!(target_os = "windows") {
        Config::new().set_mapsize(32 * 1024 * 1024)
    } else {
        Config::new()
    }
}

/// The `headway` binary this worktree built (see [`bin_testing::worktree_bin`]).
fn headway_bin() -> PathBuf {
    bin_testing::worktree_bin(
        Path::new(env!("CARGO_BIN_EXE_headway")),
        "headway_cli",
        |_| true,
    )
}

/// Run `headway` against `url`/`db` with `extra`, stderr piped (so not a tty).
fn headway(bin: &Path, url: &str, db: &str, extra: &[&str]) -> Output {
    Command::new(bin)
        .args(["--nsec", &nsec(), "--relay", url, "--db", db])
        .args(extra)
        .env("HEADWAY_BOARD", "headway")
        .env_remove("HEADWAY_COMMENT_NSEC")
        .env_remove("HEADWAY_COMMENT_NSEC_FILE")
        .env_remove("HEADWAY_PAGER")
        .env_remove("PAGER")
        .output()
        .expect("run headway")
}

/// Assert `out` succeeded and its stderr holds no terminal control bytes.
fn assert_plain_stderr(out: &Output, what: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{what} failed: {stderr}");
    assert!(
        !stderr.contains('\r') && !stderr.contains('\x1b'),
        "{what} wrote terminal control bytes to piped stderr: {stderr:?}"
    );
}

/// How many cards the `--json` board printout holds across its columns.
fn total_cards(board: &Value) -> usize {
    board["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| c["cards"].as_array().map_or(0, Vec::len))
        .sum()
}

/// Seeding a sealed board, editing it, and joining it from a fresh cache runs
/// every relay leg; none of them may draw on piped stderr.
#[test]
fn syncing_legs_write_no_control_bytes_to_piped_stderr() {
    let bin = headway_bin();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();
    assert_plain_stderr(
        &headway(&bin, &url, db, &["--board", "headway", "seed"]),
        "seed",
    );
    assert_plain_stderr(
        &headway(&bin, &url, db, &["add", "Progress card", "--col", "Todo"]),
        "add",
    );

    // A fresh cache pulls the key-share, peels it, then syncs and peels the
    // board's envelopes. The relay ingests asynchronously, so retry until the
    // card has arrived; every attempt must be clean.
    let fresh_dir = tempfile::tempdir().expect("fresh dir");
    let fresh = fresh_dir.path().to_str().unwrap();
    for _ in 0..50 {
        let out = headway(&bin, &url, fresh, &["show", "--json"]);
        assert_plain_stderr(&out, "show from a fresh cache");
        let board: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        if total_cards(&board) == 1 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("the sealed card never reached a fresh cache");
}

/// With no relay listening, the offline fallback's warnings come through
/// verbatim and unspliced.
#[test]
fn offline_fallback_warning_stays_plain() {
    let bin = headway_bin();
    // Bind and drop, so the port is (almost certainly) closed.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    let url = format!("ws://127.0.0.1:{port}");
    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    let out = headway(&bin, &url, db, &["show"]);
    assert_plain_stderr(&out, "offline show");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr
            .lines()
            .any(|l| l == "working offline against the local cache (--relay to point elsewhere)"),
        "offline fallback line missing or spliced: {stderr:?}"
    );
}
