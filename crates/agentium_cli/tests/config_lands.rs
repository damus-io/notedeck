//! End-to-end checks for `agentium config`: `list`/`show` read seeded
//! kind-31991 run configs out of the cache, and `add`/`edit`/`rm` publish
//! revisions that reach a same-identity verifier engine over a real relay.
//!
//! The sibling of `permission_lands.rs`. The read cases run with `--no-sync`,
//! since they are pure cache reads.

use std::process::{Command, Output};
use std::time::Duration;

use agentium_core::config::{AI_RUN_CONFIG_KIND, RunConfig};
use agentium_core::session_events::build_run_config_event_at;
use agentium_core::session_loader::load_run_configs_from_ndb;
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

/// `[7u8; 32]` as an nsec — the key the seeded events are signed with.
const NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";
const SECKEY: [u8; 32] = [7u8; 32];

/// The pubkey [`SECKEY`] signs as.
fn author() -> nostrdb_net::Pubkey {
    let (_, pk) = nostrdb_net::relay::sync::parse_nsec(NSEC).expect("nsec");
    nostrdb_net::Pubkey::new(*pk.bytes())
}

/// Open a fresh sender cache in `dir` holding one plain (un-wrapped) kind-31991
/// note per `(id, name, command, host, cwd)`, and wait until all of them are
/// queryable. The db handle is dropped so the subprocess opens the cache cleanly.
async fn seed_cache(dir: &TempDir, configs: &[(&str, &str, &str, &str, &str)]) -> String {
    let db_path = dir.path().to_str().expect("path").to_string();
    let ndb = Ndb::new(&db_path, &test_config()).expect("ndb");
    let filter = nostrdb::Filter::new()
        .kinds([AI_RUN_CONFIG_KIND as u64])
        .build();
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .expect("subscribe");

    for (id, name, command, host, cwd) in configs {
        let config = RunConfig {
            id: id.to_string(),
            name: name.to_string(),
            command: command.to_string(),
            updated_at: 0,
        };
        let built =
            build_run_config_event_at(&config, cwd, host, Some(1_000), &SECKEY).expect("build");
        ndb.process_client_event(&format!(r#"["EVENT",{}]"#, built.note_json))
            .expect("ingest");
    }
    if !configs.is_empty() {
        ndb.wait_for_all_notes(sub, configs.len() as u32)
            .await
            .expect("ingest seeded notes");
    }
    db_path
}

/// Run the real `agentium` binary against `db_path`/`relay`, with `dir`
/// redirecting XDG/HOME so the run can't touch real config.
fn run_agentium(dir: &TempDir, db_path: &str, relay: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agentium"))
        .args(["--nsec", NSEC, "--db", db_path, "--relay", relay])
        .args(args)
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .env_remove("AGENTIUM_SESSION")
        .output()
        .expect("run agentium")
}

/// Assert a run succeeded and return its stdout.
fn ok_stdout(out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    stdout
}

/// `list` shows every host, grouped host → cwd; `--host` narrows it; `--json`
/// is the flat array.
#[tokio::test]
async fn list_groups_every_host() {
    let dir = TempDir::new().expect("tmp");
    let db = seed_cache(
        &dir,
        &[
            ("aaaa1111-0", "build", "cargo build", "mac", "/home/u/proj"),
            ("bbbb2222-0", "serve", "npm start", "linux", "/srv/app"),
        ],
    )
    .await;

    let out = ok_stdout(&run_agentium(
        &dir,
        &db,
        "ws://127.0.0.1:1",
        &["--no-sync", "config", "list"],
    ));
    let linux = out.find("linux\n  /srv/app\n    bbbb2222  serve");
    let mac = out.find("mac\n  /home/u/proj\n    aaaa1111  build");
    assert!(
        linux.is_some() && mac.is_some() && linux < mac,
        "both hosts, grouped and in host order:\n{out}"
    );

    let out = ok_stdout(&run_agentium(
        &dir,
        &db,
        "ws://127.0.0.1:1",
        &["--no-sync", "--host", "MAC", "--json", "config"],
    ));
    let rows: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        rows,
        serde_json::json!([{
            "id": "aaaa1111-0",
            "name": "build",
            "command": "cargo build",
            "host": "mac",
            "cwd": "/home/u/proj",
            "updated_at": 1000,
        }])
    );
}

/// `show` resolves by id prefix or exact name, and an ambiguous name errors with
/// the candidates.
#[tokio::test]
async fn show_resolves_prefix_and_name() {
    let dir = TempDir::new().expect("tmp");
    let db = seed_cache(
        &dir,
        &[
            ("aaaa1111-0", "build", "cargo build", "mac", "/p"),
            ("bbbb2222-0", "build", "make", "linux", "/p"),
            ("cccc3333-0", "serve", "npm start", "linux", "/p"),
        ],
    )
    .await;
    let run = |args: &[&str]| {
        let mut all = vec!["--no-sync", "config", "show"];
        all.extend_from_slice(args);
        run_agentium(&dir, &db, "ws://127.0.0.1:1", &all)
    };

    let out = ok_stdout(&run(&["bbbb"]));
    assert!(out.contains("command  make\n"), "{out}");
    assert!(out.contains("host     linux\n"), "{out}");

    let out = ok_stdout(&run(&["serve"]));
    assert!(out.contains("id       cccc3333-0\n"), "{out}");

    let out = run(&["build"]);
    assert!(!out.status.success(), "ambiguous name must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("aaaa1111 build (mac:/p)") && stderr.contains("bbbb2222 build (linux:/p)"),
        "should list both candidates:\n{stderr}"
    );
}

/// `add` with no --host/--cwd and no current session has nowhere to put the
/// config, and says so before publishing anything.
#[tokio::test]
async fn add_without_a_place_errors() {
    let dir = TempDir::new().expect("tmp");
    let db = seed_cache(&dir, &[]).await;
    let out = run_agentium(
        &dir,
        &db,
        "ws://127.0.0.1:1",
        &["config", "add", "--name", "x", "--command", "true"],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("pass --host"), "{stderr}");
}

/// Wait (bounded) until `landed` holds on the verifier's cache, waking on each
/// new kind-31991 event.
async fn wait_until(verifier: &agentium_core::Engine, landed: impl Fn(&Ndb) -> bool) -> bool {
    let ndb = verifier.ndb();
    let filter = nostrdb::Filter::new()
        .kinds([AI_RUN_CONFIG_KIND as u64])
        .build();
    let sub = ndb.subscribe(&[filter]).expect("subscribe");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if landed(ndb) {
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

/// The configs the desktop on host `mac` would load for `/p`, as
/// `(id, name, command)`.
fn mac_configs(ndb: &Ndb) -> Vec<(String, String, String)> {
    let txn = Transaction::new(ndb).expect("txn");
    load_run_configs_from_ndb(ndb, &txn, &author(), "mac")
        .remove(std::path::Path::new("/p"))
        .unwrap_or_default()
        .into_iter()
        .map(|c| (c.id, c.name, c.command))
        .collect()
}

/// `add` → `edit` → `rm`, each reaching the verifier as the desktop would read
/// it: the config appears, changes in place under the same id, then is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_edit_rm_land_on_the_verifier() {
    let relay_dir = TempDir::new().expect("relay tmp");
    let relay_ndb =
        Ndb::new(relay_dir.path().to_str().expect("path"), &test_config()).expect("relay ndb");
    let relay = nostrdb_net::relay::server::spawn(relay_ndb, "127.0.0.1:0".parse().expect("addr"))
        .expect("spawn relay");
    let url = relay.url();

    let sender_dir = TempDir::new().expect("sender tmp");
    let db = seed_cache(&sender_dir, &[]).await;

    let verifier_dir = TempDir::new().expect("verifier tmp");
    let mut verifier =
        agentium_core::Engine::open(verifier_dir.path().to_str().expect("path"), SECKEY)
            .expect("verifier engine");
    verifier.connect(&url).expect("verifier connect");
    let _ = tokio::time::timeout(Duration::from_secs(5), verifier.wait_for_sync()).await;

    // add
    let out = ok_stdout(&run_agentium(
        &sender_dir,
        &db,
        &url,
        &[
            "--json",
            "--host",
            "mac",
            "--cwd",
            "/p",
            "config",
            "add",
            "--name",
            "build",
            "--command",
            "cargo build",
        ],
    ));
    let added: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    assert_eq!(added["action"], "added");
    let id = added["config"]["id"].as_str().expect("id").to_string();
    let landed = wait_until(&verifier, |ndb| {
        mac_configs(ndb) == [(id.clone(), "build".into(), "cargo build".into())]
    })
    .await;
    assert!(landed, "the added config should reach the verifier");

    // edit, by id prefix
    let out = ok_stdout(&run_agentium(
        &sender_dir,
        &db,
        &url,
        &["config", "edit", &id[..8], "--command", "cargo build -r"],
    ));
    assert!(
        out.starts_with(&format!("updated run config {} build", &id[..8])),
        "{out}"
    );
    let landed = wait_until(&verifier, |ndb| {
        mac_configs(ndb) == [(id.clone(), "build".into(), "cargo build -r".into())]
    })
    .await;
    assert!(
        landed,
        "the edit should replace the config under the same id"
    );

    // rm, by name
    let out = ok_stdout(&run_agentium(
        &sender_dir,
        &db,
        &url,
        &["config", "rm", "build"],
    ));
    assert!(
        out.starts_with(&format!("removed run config {}", &id[..8])),
        "{out}"
    );
    let landed = wait_until(&verifier, |ndb| mac_configs(ndb).is_empty()).await;
    assert!(
        landed,
        "the tombstone should remove the config on the verifier"
    );

    relay.shutdown();
}
