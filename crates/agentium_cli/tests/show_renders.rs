//! End-to-end checks that the real `agentium show` renders one session's detail,
//! and that a bare `show` falls back to `$AGENTIUM_SESSION`.
//!
//! Each test seeds a signed kind-31988 state, a short kind-1988 conversation
//! (including one unanswered `permission_request`) and a kind-31991 run config on
//! the session's host+cwd into a cache dir, then runs the actual binary against
//! it. The relay points at a closed port so connect fails fast and the bounded
//! sync-settle elapses into a cache read — mirroring `list_renders.rs`.

use std::process::{Command, Output};

use agentium_core::config::{AI_RUN_CONFIG_KIND, RunConfig};
use agentium_core::session_events::{
    ThreadingState, build_permission_request_event, build_run_config_event_at,
};
use nostrdb::{Config, Ndb, NoteBuilder};
use tempfile::TempDir;

/// A [`Config`] with a small mapsize, for tests (see `list_renders.rs`).
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

/// The seeded session's stable id, host and cwd — the run config is registered
/// on the same host+cwd so `show` picks it up.
const SESSION: &str = "sess-show";
const HOST: &str = "macbook";
const CWD: &str = "/home/u/proj";

/// A second session, seeded alongside [`SESSION`] so a test can tell which one a
/// selector (or `$AGENTIUM_SESSION`) actually resolved to.
const OTHER: &str = "sess-other";

fn ingest_json(ndb: &Ndb, note_json: &str) {
    ndb.process_client_event(&format!(r#"["EVENT",{note_json}]"#))
        .expect("ingest");
}

/// Seed a signed kind-31988 state for `d` with the tags `SessionState::from_note`
/// reads.
fn seed_state(ndb: &Ndb, d: &str, title: &str, status: &str) {
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
        .tag_str(HOST)
        .start_tag()
        .tag_str("cwd")
        .tag_str(CWD)
        .start_tag()
        .tag_str("home_dir")
        .tag_str("/home/u")
        .start_tag()
        .tag_str("backend")
        .tag_str("claude")
        .start_tag()
        .tag_str("permission-mode")
        .tag_str("plan")
        .sign(&SECKEY)
        .build()
        .expect("build state note");
    ingest_json(ndb, &note.json().expect("note json"));
}

/// Seed a signed kind-1988 conversation note on [`SESSION`] with the
/// `d`/`role`/`seq`/`ms` tags the loader reads.
fn seed_message(ndb: &Ndb, role: &str, content: &str, seq: u32, ms: u64) {
    let note = NoteBuilder::new()
        .kind(KIND_CONVERSATION)
        .content(content)
        .start_tag()
        .tag_str("d")
        .tag_str(SESSION)
        .start_tag()
        .tag_str("role")
        .tag_str(role)
        .start_tag()
        .tag_str("seq")
        .tag_str(&seq.to_string())
        .start_tag()
        .tag_str("ms")
        .tag_str(&ms.to_string())
        .sign(&SECKEY)
        .build()
        .expect("build conv note");
    ingest_json(ndb, &note.json().expect("note json"));
}

/// Open a fresh cache in `dir` holding [`SESSION`] (two chat messages, one
/// pending `Bash` permission request, one run config on its host+cwd) and a bare
/// [`OTHER`] session, and wait until all of it is queryable. Returns the db path
/// and the pending request's perm-id; the db handle is dropped so the subprocess
/// opens the cache cleanly.
async fn seed_cache(dir: &TempDir) -> (String, uuid::Uuid) {
    let db_path = dir.path().to_str().expect("path").to_string();
    let ndb = Ndb::new(&db_path, &test_config()).expect("ndb");
    let filter = nostrdb::Filter::new()
        .kinds([
            KIND_SESSION_STATE as u64,
            KIND_CONVERSATION as u64,
            AI_RUN_CONFIG_KIND as u64,
        ])
        .build();
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .expect("subscribe");

    seed_state(&ndb, SESSION, "Ship the show tests", "needs_input");
    seed_state(&ndb, OTHER, "Some other session", "idle");
    seed_message(&ndb, "user", "please run the tests", 0, 1_000);
    seed_message(&ndb, "assistant", "on it", 1, 2_000);

    let perm_id = uuid::Uuid::new_v4();
    let mut threading = ThreadingState::new();
    let req = build_permission_request_event(
        &perm_id,
        "Bash",
        &serde_json::json!({ "command": "cargo test" }),
        SESSION,
        &mut threading,
        &SECKEY,
    )
    .expect("request event");
    ingest_json(&ndb, &req.note_json);

    let config = RunConfig {
        id: "cfg-build".into(),
        name: "build".into(),
        command: "cargo build --release".into(),
        updated_at: 0,
    };
    let rc = build_run_config_event_at(&config, CWD, HOST, Some(1_000), &SECKEY).expect("config");
    ingest_json(&ndb, &rc.note_json);

    // 2 states + 2 messages + 1 request + 1 run config.
    ndb.wait_for_all_notes(sub, 6)
        .await
        .expect("ingest seeded notes");
    (db_path, perm_id)
}

/// Run the real `agentium` binary against `db_path` with `dir` redirecting
/// XDG/HOME so the run can't touch real config. `session_env` sets
/// `$AGENTIUM_SESSION` for the child; `None` removes it, so a test can't inherit
/// the ref of the Dave session running the suite.
fn run_agentium(dir: &TempDir, db_path: &str, session_env: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agentium"));
    cmd.args([
        "--nsec",
        NSEC,
        "--db",
        db_path,
        "--relay",
        "ws://127.0.0.1:1",
    ])
    .args(args)
    .env("XDG_DATA_HOME", dir.path())
    .env("HOME", dir.path());
    match session_env {
        Some(v) => cmd.env("AGENTIUM_SESSION", v),
        None => cmd.env_remove("AGENTIUM_SESSION"),
    };
    cmd.output().expect("run agentium")
}

/// `out`'s stdout, after asserting a clean exit.
fn success_stdout(out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "nonzero exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    stdout
}

/// The plain-text detail carries every section: the header (URI + status +
/// title), the kind-31988 state fields, the host+cwd run config, the
/// conversation summary, and the pending permission with the short id
/// `approve`/`deny --request` takes. No usage section — nothing seeded a
/// kind-1989 turn.
#[tokio::test]
async fn show_renders_every_section() {
    let dir = TempDir::new().expect("tmp dir");
    let (db_path, perm_id) = seed_cache(&dir).await;

    let stdout = success_stdout(&run_agentium(&dir, &db_path, None, &["show", SESSION]));

    let uri = agentium_core::wordid::session_ref(SESSION);
    let short = &perm_id.to_string()[..8];
    for needle in [
        uri.as_str(),
        "Needs Input",
        "Ship the show tests",
        SESSION,
        HOST,
        "~/proj",
        "claude",
        "plan",
        "run configs (host+cwd)",
        "build",
        "cargo build --release",
        "conversation",
        "pending permissions",
        short,
        "Bash",
    ] {
        assert!(
            stdout.contains(needle),
            "missing {needle:?} in output:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("usage"),
        "no archived turn, so no usage section:\n{stdout}"
    );
    assert!(
        !stdout.contains("Some other session"),
        "show must describe only the selected session:\n{stdout}"
    );
    assert!(!stdout.contains('\x1b'), "no ANSI when piped:\n{stdout:?}");
}

/// `show --json` is one object: the flattened state plus its `agentium_uri`, the
/// host+cwd run configs, and the conversation summary with the full perm-id.
/// `usage` is omitted, not null, when the archive holds no turn.
#[tokio::test]
async fn show_json_shape() {
    let dir = TempDir::new().expect("tmp dir");
    let (db_path, perm_id) = seed_cache(&dir).await;

    let stdout = success_stdout(&run_agentium(
        &dir,
        &db_path,
        None,
        &["show", SESSION, "--json"],
    ));
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON object");

    let session = &json["session"];
    assert_eq!(session["claude_session_id"], SESSION);
    assert_eq!(
        session["agentium_uri"],
        agentium_core::wordid::session_ref(SESSION)
    );
    assert_eq!(session["hostname"], HOST);
    assert_eq!(session["cwd"], CWD);

    assert_eq!(
        json["run_configs"],
        serde_json::json!([{
            "id": "cfg-build",
            "name": "build",
            "command": "cargo build --release",
        }])
    );

    assert!(
        json.get("usage").is_none(),
        "usage omitted when absent: {json}"
    );

    let conv = &json["conversation"];
    let pending = conv["pending_permissions"].as_array().expect("array");
    assert_eq!(pending.len(), 1, "{conv}");
    assert_eq!(pending[0]["perm_id"], perm_id.to_string());
    assert_eq!(pending[0]["tool_name"], "Bash");
    assert_eq!(pending[0]["is_question"], false);
    // The two chat messages plus the request itself.
    assert_eq!(conv["message_count"], 3, "{conv}");
}

/// A bare `show` — no selector — targets `$AGENTIUM_SESSION`, the `agentium:`
/// ref a running Dave session exports. The env names the word-id form, so this
/// also covers resolving a sayable ref, not just the raw id. An explicit
/// selector still wins over the env.
#[tokio::test]
async fn show_defaults_to_agentium_session_env() {
    let dir = TempDir::new().expect("tmp dir");
    let (db_path, _) = seed_cache(&dir).await;
    let uri = agentium_core::wordid::session_ref(SESSION);

    let bare = success_stdout(&run_agentium(&dir, &db_path, Some(&uri), &["show"]));
    assert!(
        bare.contains("Ship the show tests") && !bare.contains("Some other session"),
        "bare show should resolve $AGENTIUM_SESSION:\n{bare}"
    );

    let explicit = success_stdout(&run_agentium(&dir, &db_path, Some(&uri), &["show", OTHER]));
    assert!(
        explicit.contains("Some other session") && !explicit.contains("Ship the show tests"),
        "an explicit selector overrides $AGENTIUM_SESSION:\n{explicit}"
    );
}

/// With neither a selector nor `$AGENTIUM_SESSION` (an empty value counts as
/// unset), `show` fails and says how to name a session.
#[tokio::test]
async fn show_without_selector_or_env_errors() {
    let dir = TempDir::new().expect("tmp dir");
    let (db_path, _) = seed_cache(&dir).await;

    for env in [None, Some("")] {
        let out = run_agentium(&dir, &db_path, env, &["show"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "env {env:?}: should fail");
        assert!(
            stderr.contains("AGENTIUM_SESSION"),
            "env {env:?}: error should point at $AGENTIUM_SESSION:\n{stderr}"
        );
    }
}
