//! End-to-end check that the real `agentium spawn --wait` publishes a kind-31989
//! spawn command, waits for a host to answer with the new session's kind-31988
//! state, and prints its durable `agentium:` ref — plus that `--prompt` rides
//! that command so the host can deliver the first `user` message itself.
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
use agentium_core::session_events::{self, AI_SESSION_COMMAND_KIND, build_session_state_event};
use nostrdb::{Config, Ndb};
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

    if let Some((_, path)) = newest {
        return path;
    }

    // Nothing in `deps` was identifiable as ours. That is a miss by the
    // *heuristic*, not proof the binary is wrong: the scan reads cargo's dep-info
    // to tell our artifacts from a sibling worktree's, and its file naming is not
    // contractual (on Windows CI it identifies nothing at all). The hazard it
    // guards against — several worktrees sharing one `target/` — needs sibling
    // worktrees to exist, which on a CI runner's single fresh checkout they do
    // not. So fall back to the uplifted binary, still gated on the check that
    // actually matters: that it is a current build and so this worktree's code.
    if is_current_build(uplifted) {
        return uplifted.to_path_buf();
    }

    panic!(
        "no `agentium` under {} and no current uplifted binary at {} — \
         run `cargo build -p agentium_cli --bin agentium` first",
        deps.display(),
        uplifted.display()
    )
}

/// A `deps/agentium-<hash>` runnable executable — the sibling of the
/// `.d`/`.rmeta`/`.o` files cargo drops next to it under the same stem.
///
/// The binary is extensionless on unix but carries [`std::env::consts::EXE_SUFFIX`]
/// on Windows, so strip that first and reject a dot only in what remains: matching
/// on the raw name would throw `agentium-<hash>.exe` out with the build artifacts
/// and leave the search with no candidate at all.
fn is_agentium_exe(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let Some(stem) = name.strip_suffix(std::env::consts::EXE_SUFFIX) else {
        return false;
    };
    stem.starts_with("agentium-") && !stem.contains('.') && path.is_file()
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

/// What the helper host reads off the CLI's kind-31989 spawn command: the
/// `spawn_id` it must echo back on the session state, the `prompt` tag the
/// command carries (the first `user` message a real host delivers itself when it
/// materializes the session), and the `permission_mode` tag naming the mode a
/// real host starts that session's backend in.
struct SpawnCommand {
    spawn_id: String,
    prompt: Option<String>,
    permission_mode: Option<String>,
    /// The `idempotency_key` tag naming the *request*, which a host keys its
    /// duplicate-spawn dedupe on. Absent when the CLI was told
    /// `--allow-duplicate`.
    idempotency_key: Option<String>,
}

/// Wait (bounded) for the helper host to see a kind-31989 spawn command in its
/// synced cache, then return what it carries. `None` if none arrived in time.
async fn await_spawn_command(host: &Engine) -> Option<SpawnCommand> {
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
    Some(SpawnCommand {
        spawn_id: session_events::get_tag_value(note, "spawn_id")?.to_string(),
        prompt: session_events::get_tag_value(note, "prompt").map(|s| s.to_string()),
        permission_mode: session_events::get_tag_value(note, "permission_mode")
            .map(|s| s.to_string()),
        idempotency_key: session_events::get_tag_value(note, "idempotency_key")
            .map(|s| s.to_string()),
    })
}

/// The real `agentium spawn --wait --prompt` resolves the new session's ref, and
/// the seeded first message and permission mode ride the published command so the
/// host can deliver one and start the backend in the other — driven by a same-key
/// helper host answering over a live relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_wait_resolves_and_prompt_lands() {
    // A real relay backed by its own ndb — the seam every envelope crosses.
    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &test_config()).expect("relay ndb");
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
                // Deliberately an alias, so this also covers the CLI normalizing
                // to the canonical wire spelling before publishing.
                "--permission-mode",
                "acceptEdits",
                "--wait",
            ])
            .env("XDG_DATA_HOME", cli_dir.path())
            .env("HOME", cli_dir.path())
            .output()
            .expect("run agentium spawn")
    });

    // Host side: wait for the command, then publish a kind-31988 state echoing its
    // spawn_id — the correlation the CLI's --wait keys on.
    let command = await_spawn_command(&host)
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
        Some(&command.spawn_id),
        None,
        None,
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

    // The `--prompt` is not reported on the spawn output — it rides the spawn
    // command and the host delivers it off that command (see `emit_spawn`). The
    // real end-to-end guarantee is that the seeded first message actually lands
    // on the host, asserted below.

    // The seeded first message rode the spawn command itself, so a real host has
    // everything it needs to deliver it whether or not the CLI was still waiting.
    assert_eq!(
        command.prompt.as_deref(),
        Some("do the first thing"),
        "the spawn command should carry --prompt as its `prompt` tag"
    );

    // Likewise the mode: a real host reads this before it starts the session's
    // backend, which is the only moment the starting mode can be chosen.
    assert_eq!(
        command.permission_mode.as_deref(),
        Some("accept_edits"),
        "the spawn command should carry --permission-mode, canonically spelled"
    );

    relay.shutdown();
}

/// `spawn --json` is one line, so `| jq -r .session` (and the `tail -1`/`head -1`
/// variants callers reach for) works instead of silently yielding nothing.
///
/// Driven without `--wait`: the point is the *shape* of the output, and a spawn
/// that only publishes exercises it without waiting on a host.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_json_is_one_line() {
    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &test_config()).expect("relay ndb");
    let relay = nostrdb_net::relay::server::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr"))
        .expect("spawn relay");
    let url = relay.url();

    let cli_dir = TempDir::new().expect("cli tmp");
    let db_path = cli_dir.path().to_str().expect("path").to_string();
    let bin = agentium_bin();
    let home = cli_dir.path().to_path_buf();

    let out = tokio::task::spawn_blocking(move || {
        Command::new(&bin)
            .args([
                "--nsec", NSEC, "--db", &db_path, "--relay", &url, "--json", "spawn", "--host",
                HOST, "--cwd", CWD,
            ])
            .env("XDG_DATA_HOME", &home)
            .env("HOME", &home)
            .output()
            .expect("run agentium spawn --json")
    })
    .await
    .expect("join cli");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "nonzero exit:\n{stdout}\n{stderr}");
    assert_eq!(
        stdout.lines().count(),
        1,
        "spawn --json must be one line, or `| tail -1 | jq -r .session` yields \
         nothing and the caller re-runs the spawn: {stdout:?}"
    );
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON object");
    assert!(
        v["spawn_id"].as_str().is_some(),
        "the one line still carries the record: {stdout}"
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
        Ndb::new(relay_dir.path().to_str().expect("path"), &test_config()).expect("relay ndb");
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

/// Count the kind-31989 spawn commands an engine's cache has seen — the number
/// of *sessions* a host would have materialized from them.
fn spawn_command_count(host: &Engine) -> usize {
    let filter = nostrdb::Filter::new()
        .kinds([AI_SESSION_COMMAND_KIND as u64])
        .build();
    let Ok(txn) = nostrdb::Transaction::new(host.ndb()) else {
        return 0;
    };
    host.ndb().query(&txn, &[filter], 64).map_or(0, |r| r.len())
}

/// The duplicate, for real: run the actual binary twice with identical spawn
/// flags — a caller that mis-read the first run's output and re-ran the same
/// command — and assert the second refuses *before publishing*, so exactly one
/// kind-31989 command exists and a host would materialize exactly one session.
///
/// This is the failure the whole change exists to stop, driven through the real
/// CLI rather than a unit seam: two invocations, one session. `--allow-duplicate`
/// is then exercised on a third run to prove the guard is a guard and not a hard
/// stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_repeated_spawn_publishes_one_command() {
    const TITLE: &str = "Fix the parser";

    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &test_config()).expect("relay ndb");
    let relay = nostrdb_net::relay::server::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr"))
        .expect("spawn relay");
    let url = relay.url();

    let host_dir = TempDir::new().expect("host tmp");
    let mut host =
        Engine::open(host_dir.path().to_str().expect("path"), SECKEY).expect("host engine");
    host.connect(&url).expect("host connect");

    // One cache dir shared by every run, so the second run sees what the first
    // produced — exactly as a real retry from the same shell would.
    let cli_dir = TempDir::new().expect("cli tmp");
    let db_path = cli_dir.path().to_str().expect("path").to_string();
    let bin = agentium_bin();

    // The command the caller runs, and then re-runs verbatim.
    let spawn_run = {
        let bin = bin.clone();
        let db_path = db_path.clone();
        let url = url.clone();
        let home = cli_dir.path().to_path_buf();
        move |extra: Vec<&'static str>| {
            let mut args = vec![
                "--nsec".to_string(),
                NSEC.to_string(),
                "--db".to_string(),
                db_path.clone(),
                "--relay".to_string(),
                url.clone(),
                "spawn".to_string(),
                "--host".to_string(),
                HOST.to_string(),
                "--cwd".to_string(),
                CWD.to_string(),
                "--title".to_string(),
                TITLE.to_string(),
                "--wait".to_string(),
            ];
            args.extend(extra.into_iter().map(str::to_string));
            Command::new(&bin)
                .args(&args)
                .env("XDG_DATA_HOME", &home)
                .env("HOME", &home)
                .output()
                .expect("run agentium spawn")
        }
    };

    // Run 1: a normal spawn, answered by the helper host. The state must carry the
    // title as `custom_title` (what `--title` becomes) and a *current*
    // `created_at`, since the guard only refuses against a recent session.
    let first = {
        let run = spawn_run.clone();
        tokio::task::spawn_blocking(move || run(vec![]))
    };
    let command = await_spawn_command(&host)
        .await
        .expect("host should see the first spawn command");
    // Derived and attached by the CLI with nothing asked of the caller — the key
    // is what lets a host recognize a retry that slips past the guard below.
    assert!(
        command.idempotency_key.is_some(),
        "an ordinary spawn must carry an idempotency key",
    );
    let state = build_session_state_event(
        SPAWNED_SID,
        "Connecting...",
        Some(TITLE),
        CWD,
        "working",
        None,
        HOST,
        "/home/u",
        "claude",
        "default",
        Some(""),
        Some(&command.spawn_id),
        None,
        None,
        session_events::now_secs(),
        &SECKEY,
    )
    .expect("build state");
    host.publish_event(&state).expect("publish state");
    let _ = tokio::time::timeout(Duration::from_secs(5), host.wait_for_sync()).await;

    let out = first.await.expect("join first cli");
    assert!(
        out.status.success(),
        "the first spawn must succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(
        spawn_command_count(&host),
        1,
        "one spawn, one command on the wire"
    );

    // Run 2: the retry. Identical flags, and it must refuse.
    let retry = {
        let run = spawn_run.clone();
        tokio::task::spawn_blocking(move || run(vec![]))
    }
    .await
    .expect("join retry cli");

    let retry_err = String::from_utf8_lossy(&retry.stderr).into_owned();
    assert!(
        !retry.status.success(),
        "a retried spawn must fail rather than quietly create a second session\nstdout:\n{}",
        String::from_utf8_lossy(&retry.stdout),
    );
    assert!(
        retry_err.contains("agentium:") && retry_err.contains("--allow-duplicate"),
        "the refusal must name the existing session and the override:\n{retry_err}",
    );

    // The assertion that matters: nothing new reached the wire, so a host has
    // exactly one session to materialize.
    let _ = tokio::time::timeout(Duration::from_secs(2), host.wait_for_sync()).await;
    assert_eq!(
        spawn_command_count(&host),
        1,
        "the retry must not publish a second spawn command:\n{retry_err}",
    );

    // Run 3: the caller really does want a sibling. The guard yields, and a
    // second command reaches the wire.
    let forced = tokio::task::spawn_blocking(move || spawn_run(vec!["--allow-duplicate"]))
        .await
        .expect("join forced cli");
    // No host answers this one, so `--wait` times out (nonzero) — but the command
    // is published before the wait, which is what we are counting.
    let _ = tokio::time::timeout(Duration::from_secs(30), host.wait_for_sync()).await;
    assert_eq!(
        spawn_command_count(&host),
        2,
        "--allow-duplicate must still publish\nstderr:\n{}",
        String::from_utf8_lossy(&forced.stderr),
    );

    relay.shutdown();
}
