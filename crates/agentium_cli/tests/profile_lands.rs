//! End-to-end checks for `agentium profile`: `set`, `secret set` (value on
//! stdin) and `rm` publish kind-31992 revisions over a real relay, and `show`
//! and `list` print the profile without ever printing the secret.
//!
//! Because nothing the CLI prints may carry the token, a same-identity verifier
//! engine on the relay is what proves `secret set` stored it: it loads the
//! profile the way Dave will and reads the value back.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use agentium_core::config::{AI_BACKEND_PROFILE_KIND, BackendProfile};
use agentium_core::session_loader::load_backend_profiles_from_ndb;
use nostrdb::{Config, Ndb, Transaction};
use tempfile::TempDir;

/// A [`Config`] with a small mapsize, for tests (see `interrupt_lands.rs`).
fn test_config() -> Config {
    if cfg!(target_os = "windows") {
        Config::new().set_mapsize(32 * 1024 * 1024) // 32 MiB
    } else {
        Config::new()
    }
}

/// `[7u8; 32]` as an nsec, shared by the CLI and the verifier so their PNS
/// envelopes round-trip.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];

/// The secret `secret set` stores. Distinctive, so a leak is unmistakable.
const TOKEN: &str = "hydra-tok-4f1c2e9b7a0d63e5";

/// The pubkey [`SECKEY`] signs as.
fn author() -> nostrdb_net::Pubkey {
    let (_, pk) = nostrdb_net::relay::sync::parse_nsec(NSEC).expect("nsec");
    nostrdb_net::Pubkey::new(*pk.bytes())
}

/// *This* worktree's freshly-built `agentium` (see [`bin_testing::worktree_bin`]).
fn agentium_bin() -> PathBuf {
    bin_testing::worktree_bin(
        Path::new(env!("CARGO_BIN_EXE_agentium")),
        "agentium_cli",
        is_current_build,
    )
}

/// A current build has a `profile` command: `agentium profile --help` exits 0,
/// where a stale sibling that predates it rejects the name. Read from the exit
/// status, not the help text, so a rewording can't make the probe miss.
fn is_current_build(bin: &Path) -> bool {
    Command::new(bin)
        .args(["profile", "--help"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// A real relay, the CLI's cache dir, and a same-key verifier engine.
struct World {
    relay: nostrdb_net::relay::server::RelayHandle,
    url: String,
    verifier: agentium_core::Engine,
    cli_dir: TempDir,
    db: String,
    _relay_dir: TempDir,
    _verifier_dir: TempDir,
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

        let cli_dir = TempDir::new().expect("cli tmp");
        let db = cli_dir
            .path()
            .join("db")
            .to_str()
            .expect("path")
            .to_string();
        std::fs::create_dir_all(&db).expect("db dir");

        let verifier_dir = TempDir::new().expect("verifier tmp");
        let mut verifier =
            agentium_core::Engine::open(verifier_dir.path().to_str().expect("path"), SECKEY)
                .expect("verifier engine");
        verifier.connect(&url).expect("verifier connect");
        let _ = tokio::time::timeout(Duration::from_secs(5), verifier.wait_for_sync()).await;

        World {
            relay,
            url,
            verifier,
            cli_dir,
            db,
            _relay_dir: relay_dir,
            _verifier_dir: verifier_dir,
        }
    }

    /// Run the real `agentium` with `stdin` piped in, XDG/HOME redirected so
    /// the run can't touch real config.
    fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = Command::new(agentium_bin())
            .args(["--nsec", NSEC, "--db", &self.db, "--relay", &self.url])
            .args(args)
            .env("XDG_DATA_HOME", self.cli_dir.path())
            .env("HOME", self.cli_dir.path())
            .env_remove("AGENTIUM_SESSION")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn agentium");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("write stdin");
        child.wait_with_output().expect("run agentium")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_stdin(args, "")
    }

    /// Wait (bounded) until `landed` holds on the verifier's profiles, waking
    /// on each new kind-31992 event.
    async fn wait_until(&self, landed: impl Fn(&BackendProfileMap) -> bool) -> bool {
        let ndb = self.verifier.ndb();
        let filter = nostrdb::Filter::new()
            .kinds([AI_BACKEND_PROFILE_KIND as u64])
            .build();
        let sub = ndb.subscribe(&[filter]).expect("subscribe");
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let profiles = {
                    let txn = Transaction::new(ndb).expect("txn");
                    load_backend_profiles_from_ndb(ndb, &txn, &author())
                };
                if landed(&profiles) {
                    return true;
                }
                if ndb.wait_for_notes(sub, 1).await.is_err() {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false)
    }
}

type BackendProfileMap = std::collections::BTreeMap<String, BackendProfile>;

/// Assert a run succeeded, that neither stream carries [`TOKEN`], and return
/// its stdout.
fn ok_stdout(out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        !stdout.contains(TOKEN) && !stderr.contains(TOKEN),
        "the token leaked:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}

/// `set` → `secret set` from a pipe → `show`/`list` (token masked, the rest
/// shown) → `rm` (gone from `list`), each write reaching the verifier.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_secret_show_rm() {
    let w = World::new().await;

    let out = ok_stdout(&w.run(&[
        "profile",
        "set",
        "hydra",
        "--label",
        "Hydra",
        "--env",
        "ANTHROPIC_BASE_URL=https://llm.hydra.host",
        "--env",
        "ANTHROPIC_API_KEY=",
        "--model",
        "glm-5.3-flash",
        "--model",
        "qwen3.8-flash-next",
        "--default-model",
        "glm-5.3-flash",
    ]));
    assert!(out.starts_with("created profile hydra"), "{out}");

    let out = ok_stdout(&w.run_with_stdin(
        &["profile", "secret", "set", "hydra", "ANTHROPIC_AUTH_TOKEN"],
        &format!("{TOKEN}\n"),
    ));
    assert!(
        out.starts_with("stored ANTHROPIC_AUTH_TOKEN on profile hydra"),
        "{out}"
    );
    let landed = w
        .wait_until(|profiles| {
            profiles.get("hydra").is_some_and(|p| {
                p.env.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str) == Some(TOKEN)
                    && p.env.get("ANTHROPIC_BASE_URL").map(String::as_str)
                        == Some("https://llm.hydra.host")
                    && p.models == ["glm-5.3-flash", "qwen3.8-flash-next"]
            })
        })
        .await;
    assert!(
        landed,
        "the verifier should load the profile with the token and the set's fields kept"
    );

    let out = ok_stdout(&w.run(&["profile", "show", "hydra"]));
    assert!(out.contains("ANTHROPIC_AUTH_TOKEN  ********"), "{out}");
    assert!(out.contains("https://llm.hydra.host"), "{out}");
    assert!(out.contains("ANTHROPIC_API_KEY     (unset)"), "{out}");
    assert!(
        out.contains("glm-5.3-flash (default), qwen3.8-flash-next"),
        "{out}"
    );

    let out = ok_stdout(&w.run(&["--json", "profile", "show", "hydra"]));
    let json: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(json["label"], "Hydra");
    assert_eq!(json["default_model"], "glm-5.3-flash");

    let out = ok_stdout(&w.run(&["profile", "list"]));
    assert!(out.contains("hydra  Hydra  (claude)"), "{out}");
    assert!(out.contains("ANTHROPIC_AUTH_TOKEN"), "{out}");

    let out = ok_stdout(&w.run(&["profile", "rm", "hydra"]));
    assert!(out.starts_with("removed profile hydra"), "{out}");
    let out = ok_stdout(&w.run(&["profile", "list"]));
    assert!(!out.contains("hydra"), "{out}");
    let gone = w.wait_until(|profiles| profiles.is_empty()).await;
    assert!(
        gone,
        "the tombstone should remove the profile on the verifier"
    );

    w.relay.shutdown();
}

/// A secret var can't be set through argv, and `secret set` refuses a value
/// passed as an argument; neither publishes anything.
#[test]
fn secrets_stay_off_argv() {
    let dir = TempDir::new().expect("tmp");
    let run = |args: &[&str]| {
        Command::new(agentium_bin())
            .args(["--nsec", NSEC, "--relay", "ws://127.0.0.1:1"])
            .args(args)
            .env("XDG_DATA_HOME", dir.path())
            .env("HOME", dir.path())
            .output()
            .expect("run agentium")
    };

    let out = run(&[
        "profile",
        "set",
        "hydra",
        "--env",
        &format!("ANTHROPIC_AUTH_TOKEN={TOKEN}"),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("profile secret set"), "{stderr}");
    assert!(
        !stderr.contains(TOKEN),
        "the refusal must not echo it: {stderr}"
    );

    let out = run(&[
        "profile",
        "secret",
        "set",
        "hydra",
        "ANTHROPIC_AUTH_TOKEN",
        TOKEN,
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("stdin"), "{stderr}");
    assert!(!stderr.contains(TOKEN), "{stderr}");
}
