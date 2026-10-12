//! End-to-end check that the real `agentium link` / `agentium unlink` publish a
//! kind-1988 set-issue command and report success only once the session's host
//! has republished its kind-31988 state with the new link.
//!
//! The host here is a same-key helper engine on an in-process relay, standing in
//! for Dave: it publishes the session's state, waits for the CLI's set-issue
//! command, decodes it, and republishes the state carrying the decoded link —
//! what Dave's `handle_remote_set_issue` + dirty-state publish do (that half is
//! covered by notedeck_dave's own test). The value here is the CLI's half: the
//! command it publishes, and that its confirm wait resolves on the host's answer
//! and fails loudly without one.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use agentium_core::Engine;
use agentium_core::session_events::{
    self, AI_CONVERSATION_KIND, SET_ISSUE_ROLE, build_session_state_event, decode_set_issue,
};
use nostrdb::{Config, Ndb};
use tempfile::TempDir;

/// A [`Config`] with a small mapsize, for tests (see `spawn_lands.rs`).
fn test_config() -> Config {
    if cfg!(target_os = "windows") {
        Config::new().set_mapsize(32 * 1024 * 1024) // 32 MiB
    } else {
        Config::new()
    }
}

/// *This* worktree's freshly-built `agentium` (see [`bin_testing::worktree_bin`]).
fn agentium_bin() -> PathBuf {
    bin_testing::worktree_bin(
        Path::new(env!("CARGO_BIN_EXE_agentium")),
        "agentium_cli",
        is_current_build,
    )
}

/// A current build lists the `link` command in its no-arg usage; a stale
/// sibling that predates it doesn't. (No-arg usage goes to stderr.)
fn is_current_build(bin: &Path) -> bool {
    let Ok(out) = Command::new(bin).output() else {
        return false;
    };
    let mut usage = String::from_utf8_lossy(&out.stdout).into_owned();
    usage.push_str(&String::from_utf8_lossy(&out.stderr));
    usage.contains("unlink <session>")
}

/// `[7u8; 32]` as an nsec — shared by the CLI and the helper host so their PNS
/// envelopes round-trip.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];

/// The session the host runs and the CLI links.
const SID: &str = "link-target";
const ISSUE: &str = "headway:gamedev/wall-harsh-hip";

/// A real relay plus a same-identity "host" engine connected to it.
struct World {
    relay: nostrdb_net::relay::server::RelayHandle,
    url: String,
    host: Engine,
    _relay_dir: TempDir,
    _host_dir: TempDir,
}

impl World {
    async fn new() -> Self {
        let relay_dir = TempDir::new().expect("relay tmp");
        let relay_ndb =
            Ndb::new(relay_dir.path().to_str().expect("path"), &test_config()).expect("relay ndb");
        let relay =
            nostrdb_net::relay::server::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr"))
                .expect("spawn relay");
        let url = relay.url();

        let host_dir = TempDir::new().expect("host tmp");
        let mut host =
            Engine::open(host_dir.path().to_str().expect("path"), SECKEY).expect("host engine");
        host.connect(&url).expect("host connect");
        let _ = tokio::time::timeout(Duration::from_secs(5), host.wait_for_sync()).await;

        World {
            relay,
            url,
            host,
            _relay_dir: relay_dir,
            _host_dir: host_dir,
        }
    }

    /// Publish the session's state as its host would, linked to `issue_url`.
    async fn publish_state(&self, issue_url: Option<&str>, created_at: u64) {
        let state = build_session_state_event(
            &agentium_core::session_loader::SessionState {
                claude_session_id: SID.to_string(),
                title: "Link target".into(),
                cwd: "/home/u/proj".into(),
                status: "idle".into(),
                hostname: "test-host".into(),
                home_dir: "/home/u".into(),
                backend: Some("claude".into()),
                permission_mode: Some("default".into()),
                cli_session_id: Some("".into()),
                issue_url: issue_url.map(Into::into),
                created_at: created_at,
                ..Default::default()
            },
            &SECKEY,
        )
        .expect("build state");
        self.host.publish_event(&state).expect("publish state");
        let _ = tokio::time::timeout(Duration::from_secs(5), self.host.wait_for_sync()).await;
    }

    /// Wait (bounded) for a set-issue command asking for `want` to reach the
    /// host. `check, then wait`: scan the session's conversation events, else
    /// block on the host's watch for the next one.
    async fn await_set_issue(&self, want: Option<&str>) -> bool {
        let mut watch = self.host.watch_session(SID).expect("watch");
        let filter = nostrdb::Filter::new()
            .kinds([AI_CONVERSATION_KIND as u64])
            .build();
        tokio::time::timeout(Duration::from_secs(25), async {
            loop {
                {
                    let txn = nostrdb::Transaction::new(self.host.ndb()).expect("txn");
                    let results = self
                        .host
                        .ndb()
                        .query(&txn, std::slice::from_ref(&filter), 64);
                    let seen = results.unwrap_or_default().iter().any(|r| {
                        session_events::get_tag_value(&r.note, "role") == Some(SET_ISSUE_ROLE)
                            && decode_set_issue(r.note.content()) == Some(want.map(String::from))
                    });
                    if seen {
                        return true;
                    }
                }
                if !watch.changed().await {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false)
    }
}

/// Run the real binary with `args` against `url`, in its own cache dir.
fn run_cli(url: &str, dir: &Path, args: &[&str]) -> Output {
    let db = dir.to_str().expect("path");
    let mut full = vec!["--nsec", NSEC, "--db", db, "--relay", url];
    full.extend_from_slice(args);
    Command::new(agentium_bin())
        .args(&full)
        .env("XDG_DATA_HOME", dir)
        .env("HOME", dir)
        .output()
        .expect("run agentium")
}

/// `link` sends the issue to the host, waits for the host's republished state,
/// and reports the link; `show` then names the card. `unlink` clears it the
/// same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn link_and_unlink_confirm_on_the_hosts_republish() {
    let world = World::new().await;
    world.publish_state(None, 1_770_000_000).await;

    let cli_dir = TempDir::new().expect("cli tmp");
    let home = cli_dir.path().to_path_buf();
    let url = world.url.clone();
    let cli = tokio::task::spawn_blocking(move || run_cli(&url, &home, &["link", SID, ISSUE]));

    // Host side: apply the command by republishing the session with its link.
    assert!(
        world.await_set_issue(Some(ISSUE)).await,
        "host sees the link"
    );
    world.publish_state(Some(ISSUE), 1_770_000_001).await;

    let out = cli.await.expect("join cli");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains(&format!("to {ISSUE}")),
        "link should confirm:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let shown = run_cli(&world.url, cli_dir.path(), &["show", SID]);
    let shown_out = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.status.success() && shown_out.contains(ISSUE),
        "show should print the linked issue:\n{shown_out}"
    );

    // Linking again to the same issue is a no-op that still succeeds.
    let again = run_cli(&world.url, cli_dir.path(), &["link", SID, ISSUE]);
    assert!(
        again.status.success() && String::from_utf8_lossy(&again.stdout).contains("(already)"),
        "re-linking should report it is already linked:\n{}",
        String::from_utf8_lossy(&again.stdout)
    );

    // Unlink clears it the same way.
    let home = cli_dir.path().to_path_buf();
    let url = world.url.clone();
    let cli = tokio::task::spawn_blocking(move || run_cli(&url, &home, &["unlink", SID]));
    assert!(world.await_set_issue(None).await, "host sees the unlink");
    world.publish_state(None, 1_770_000_002).await;

    let out = cli.await.expect("join cli");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("unlinked agentium:"),
        "unlink should confirm:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    world.relay.shutdown();
}

/// A host that never applies the command (offline, or a Dave build from
/// before `link`) makes `link` fail with that explanation instead of
/// reporting a link that didn't happen.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn link_fails_when_the_host_never_republishes() {
    let world = World::new().await;
    world.publish_state(None, 1_770_000_000).await;

    let cli_dir = TempDir::new().expect("cli tmp");
    let home = cli_dir.path().to_path_buf();
    let url = world.url.clone();
    let out = tokio::task::spawn_blocking(move || run_cli(&url, &home, &["link", SID, ISSUE]))
        .await
        .expect("join cli");

    assert!(
        !out.status.success(),
        "an unanswered link must exit nonzero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("hasn't republished"),
        "should say the host didn't apply it:\n{stderr}"
    );
    world.relay.shutdown();
}
