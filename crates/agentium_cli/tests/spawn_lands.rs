//! End-to-end check that the real `agentium spawn --wait` publishes a kind-31989
//! spawn command, waits for a host to answer with the new session's kind-31988
//! state, and prints its durable `agentium:` ref — plus that `--prompt` delivers
//! a first `user` message once the session is up.
//!
//! The engine's `spawn_session_returns_a_uuid_spawn_id` already covers the plain
//! publish; the value here is the `--wait` *resolution*. We stand up an in-process
//! relay and run the actual binary, then simulate a Dave host with a same-key
//! helper engine on the same relay: it watches for the kind-31989 command, reads
//! its `spawn_id`, and publishes a kind-31988 state carrying that same `spawn_id`
//! (the correlation the CLI waits on). Same identity + relay, so the CLI's own
//! cache syncs the answer and its watch fires.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use agentium_core::Engine;
use agentium_core::messages::Message;
use agentium_core::session_events::{self, AI_SESSION_COMMAND_KIND, build_session_state_event};
use nostrdb::{Config, Ndb};
use tempfile::TempDir;

/// Resolve a path to *this* worktree's freshly-built `agentium` binary, robust to
/// the shared-target uplift hazard that otherwise makes these tests fail under
/// `cargo test --workspace` while passing under `cargo test -p agentium_cli`.
///
/// `env!("CARGO_BIN_EXE_agentium")` names the top-level `target/debug/agentium` —
/// a *single* path that every git worktree sharing this `target/` dir hardlinks
/// its own `agentium` onto. Under `cargo test --workspace` cargo compiles this
/// crate's bin but records its uplift as already-done and won't re-link it, so a
/// sibling worktree's older `agentium` (e.g. one predating `--wait`) can own that
/// path — and, because sibling builds run concurrently, it can be re-clobbered at
/// any instant *during* the test. `-p agentium_cli` re-roots the package and forces
/// the uplift, which is exactly why isolation passes.
///
/// Rather than trust that shared path, we go to the per-fingerprint artifact cargo
/// actually built for this invocation: `target/debug/deps/agentium-<hash>`. That
/// name is unique to a (source + feature) fingerprint, so no sibling overwrites it
/// with *different* code (a matching hash means matching source). We pick the
/// newest such artifact that (a) cargo built through *this* worktree's target path
/// — its `agentium-<hash>.d` dep-info records that absolute path — and (b) is a
/// current build that understands `--wait`, so the test exercises this worktree's
/// code, not a sibling's.
fn agentium_bin() -> PathBuf {
    let uplifted = Path::new(env!("CARGO_BIN_EXE_agentium"));
    let deps = uplifted
        .parent()
        .expect("CARGO_BIN_EXE_agentium has a target/debug parent")
        .join("deps");

    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&deps)
        .expect("read target/debug/deps")
        .flatten()
    {
        let path = entry.path();
        if !is_agentium_exe(&path) || !built_in_this_worktree(&path, &deps) {
            continue;
        }
        if !is_current_build(&path) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
            newest = Some((mtime, path));
        }
    }

    newest.map(|(_, p)| p).unwrap_or_else(|| {
        panic!(
            "no current `agentium` artifact built by this worktree under {} — \
             run `cargo build -p agentium_cli --bin agentium` first",
            deps.display()
        )
    })
}

/// A `deps/agentium-<hash>` runnable executable — the extensionless sibling of the
/// `.d`/`.rmeta`/`.o` files cargo drops next to it under the same stem.
fn is_agentium_exe(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    name.starts_with("agentium-") && !name.contains('.') && path.is_file()
}

/// Cargo's dep-info (`agentium-<hash>.d`) names its target by absolute path through
/// the *building* worktree's `target` symlink, so ours begin with this worktree's
/// own `deps` dir. (A hash shared with a sibling means byte-identical source, so a
/// miss here is never wrong — just deferred to an owned twin or the fallback.)
fn built_in_this_worktree(bin: &Path, deps: &Path) -> bool {
    let Ok(depinfo) = std::fs::read_to_string(bin.with_extension("d")) else {
        return false;
    };
    depinfo
        .lines()
        .next()
        .is_some_and(|first| first.starts_with(&*deps.to_string_lossy()))
}

/// A current build lists both the `spawn` command and its `--wait` flag in its
/// no-arg usage; a stale sibling that predates either lists neither, which is how
/// we tell a freshly-built `agentium` from a leftover one. (No-arg usage goes to
/// stderr, so scan both streams.)
fn is_current_build(bin: &Path) -> bool {
    let Ok(out) = Command::new(bin).output() else {
        return false;
    };
    let mut usage = String::from_utf8_lossy(&out.stdout).into_owned();
    usage.push_str(&String::from_utf8_lossy(&out.stderr));
    usage.contains("spawn") && usage.contains("--wait")
}

/// `[7u8; 32]` as an nsec — the identity the CLI signs/decrypts as, and the same
/// key the helper host uses so their PNS envelopes round-trip.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];

/// The spawn target the CLI names and the helper host answers on.
const HOST: &str = "test-host";
const CWD: &str = "/home/u/proj";
/// The d-tag the helper host mints for the new session.
const SPAWNED_SID: &str = "spawned-session-1";

/// Wait (bounded) for the helper host to see a kind-31989 spawn command in its
/// synced cache, then return its `spawn_id`. `None` if none arrived in time.
async fn await_spawn_id(host: &Engine) -> Option<String> {
    let filter = nostrdb::Filter::new()
        .kinds([AI_SESSION_COMMAND_KIND as u64])
        .build();
    let sub = host.ndb().subscribe(std::slice::from_ref(&filter)).ok()?;
    // The CLI only publishes the command after its own connect + settle, so give
    // the round-trip generous headroom.
    tokio::time::timeout(Duration::from_secs(25), host.ndb().wait_for_notes(sub, 1))
        .await
        .ok()?
        .ok()?;
    let txn = nostrdb::Transaction::new(host.ndb()).ok()?;
    let results = host.ndb().query(&txn, &[filter], 1).ok()?;
    let note = &results.first()?.note;
    session_events::get_tag_value(note, "spawn_id").map(|s| s.to_string())
}

/// The real `agentium spawn --wait --prompt` resolves the new session's ref, and
/// its seeded first message lands on the host — driven by a same-key helper host
/// answering the command over a live relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_wait_resolves_and_prompt_lands() {
    // A real relay backed by its own ndb — the seam every envelope crosses.
    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &Config::new()).expect("relay ndb");
    let relay = nostrdb_net::relay::server::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr"))
        .expect("spawn relay");
    let url = relay.url();

    // The helper "host": a same-identity engine on the same relay that will answer
    // the spawn command. Connect so it both receives the command and can publish.
    let host_dir = TempDir::new().expect("host tmp");
    let mut host =
        Engine::open(host_dir.path().to_str().expect("path"), SECKEY).expect("host engine");
    host.connect(&url).expect("host connect");

    // The CLI needs a cache dir of its own; nothing to seed (the target comes from
    // the explicit flags, not a current session).
    let cli_dir = TempDir::new().expect("cli tmp");
    let db_path = cli_dir.path().to_str().expect("path").to_string();
    let url_for_cli = url.clone();
    let bin = agentium_bin();

    // Run the real binary in a blocking task so the host-answer loop runs
    // concurrently on this task.
    let cli = tokio::task::spawn_blocking(move || {
        Command::new(&bin)
            .args([
                "--nsec",
                NSEC,
                "--db",
                &db_path,
                "--relay",
                &url_for_cli,
                "spawn",
                "--host",
                HOST,
                "--cwd",
                CWD,
                "--prompt",
                "do the first thing",
                "--wait",
            ])
            .env("XDG_DATA_HOME", cli_dir.path())
            .env("HOME", cli_dir.path())
            .output()
            .expect("run agentium spawn")
    });

    // Host side: wait for the command, then publish a kind-31988 state echoing its
    // spawn_id — the correlation the CLI's --wait keys on.
    let spawn_id = await_spawn_id(&host)
        .await
        .expect("host should see the spawn command");
    let state = build_session_state_event(
        SPAWNED_SID,
        "Connecting...",
        None,
        CWD,
        "working",
        None,
        HOST,
        "/home/u",
        "claude",
        "default",
        Some(""),
        Some(&spawn_id),
        1_770_000_000,
        &SECKEY,
    )
    .expect("build state");
    host.publish_event(&state).expect("publish state");
    // Flush the publish through the Session's FIFO so it reaches the relay.
    let _ = tokio::time::timeout(Duration::from_secs(5), host.wait_for_sync()).await;

    // The CLI resolves the ref and exits 0.
    let out = cli.await.expect("join cli");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("spawned agentium:") && stdout.contains(&format!("on {HOST}")),
        "spawn --wait should print the resolved ref on the host:\n{stdout}"
    );
    assert!(
        stdout.contains("sent prompt"),
        "spawn --prompt should report the seeded message:\n{stdout}"
    );

    // The seeded first message lands on the host (same key, same relay).
    let mut watch = host.watch_session(SPAWNED_SID).expect("watch");
    let received = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(Message::User(u)) = host.session_messages(SPAWNED_SID).first()
                && u.text == "do the first thing"
            {
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
        "host should receive the seeded first user message"
    );

    relay.shutdown();
}

/// With no host answering, `spawn --wait` fails loudly (bounded) rather than
/// hanging: the command is still published, but the wait times out with a clear
/// "no host answered" error and a nonzero exit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_wait_times_out_without_a_host() {
    // A reachable but host-less relay: the command publishes fine, nothing answers.
    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &Config::new()).expect("relay ndb");
    let relay = nostrdb_net::relay::server::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr"))
        .expect("spawn relay");
    let url = relay.url();

    let cli_dir = TempDir::new().expect("cli tmp");
    let db_path = cli_dir.path().to_str().expect("path").to_string();
    let url_for_cli = url.clone();
    let bin = agentium_bin();

    let out = tokio::task::spawn_blocking(move || {
        Command::new(&bin)
            .args([
                "--nsec",
                NSEC,
                "--db",
                &db_path,
                "--relay",
                &url_for_cli,
                "spawn",
                "--host",
                HOST,
                "--cwd",
                CWD,
                "--wait",
            ])
            .env("XDG_DATA_HOME", cli_dir.path())
            .env("HOME", cli_dir.path())
            .output()
            .expect("run agentium spawn")
    })
    .await
    .expect("join cli");

    assert!(
        !out.status.success(),
        "a --wait with no host must exit nonzero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no host answered"),
        "should surface the bounded-wait timeout:\n{stderr}"
    );

    relay.shutdown();
}
