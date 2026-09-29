//! End-to-end: `headway review` gathers a commit's metadata from a real git repo
//! and records it on a card, and `show --json` reads it back — the CLI → relay
//! → CLI loop for review records.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use nostrdb::{Config, Ndb};
use serde_json::Value;

/// A [`Config`] with a small mapsize on Windows, where LMDB allocates the full
/// mapsize on disk (mirrors the one in `e2e.rs`).
fn test_config() -> Config {
    if cfg!(target_os = "windows") {
        Config::new().set_mapsize(32 * 1024 * 1024)
    } else {
        Config::new()
    }
}

/// Test signing key (the same all-`0x42` secret `e2e.rs` uses).
const SECRET: [u8; 32] = [0x42; 32];

fn nsec() -> String {
    let hrp = bech32::Hrp::parse("nsec").expect("hrp");
    bech32::encode::<bech32::Bech32>(hrp, &SECRET).expect("encode nsec")
}

/// The `headway` binary this worktree built.
///
/// `env!("CARGO_BIN_EXE_headway")` is the top-level `target/debug/headway`,
/// which every git worktree sharing `target/` hardlinks its own build onto, so
/// under `cargo test --workspace` a sibling's older binary (one without
/// `review`) can own it. Prefer the newest `deps/headway-<hash>` whose dep-info
/// names this worktree's `deps` dir and which knows `review`; fall back to the
/// uplifted binary when it is current (a single CI checkout has no siblings).
/// Same approach as `agentium_cli`'s `spawn_lands::agentium_bin`.
fn headway_bin() -> PathBuf {
    let uplifted = Path::new(env!("CARGO_BIN_EXE_headway"));
    let deps = uplifted.parent().expect("target dir").join("deps");
    let ours = |path: &Path| {
        let Some(stem) = path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|n| n.strip_suffix(std::env::consts::EXE_SUFFIX))
        else {
            return false;
        };
        stem.starts_with("headway-")
            && !stem.contains('.')
            && path.is_file()
            && std::fs::read_to_string(path.with_extension("d")).is_ok_and(|d| {
                // Built through this worktree, and from the CLI crate's sources: the
                // `headway` library's own test harness shares the stem.
                let first = d.lines().next().unwrap_or("");
                first.starts_with(&*deps.to_string_lossy()) && first.contains("headway_cli")
            })
    };
    let newest = std::fs::read_dir(&deps)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| ours(p) && knows_review(p))
        .max_by_key(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH)
        });
    if let Some(path) = newest {
        return path;
    }
    assert!(
        knows_review(uplifted),
        "no current `headway` binary — run `cargo build -p headway_cli` first"
    );
    uplifted.to_path_buf()
}

/// A current build lists `review` in its no-arg usage (printed to stderr).
fn knows_review(bin: &Path) -> bool {
    Command::new(bin)
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stderr).contains("review"))
}

/// Run `headway` against `url`/`db` with `extra`, with the board pinned and a
/// known `$AGENTIUM_SESSION`, so neither the developer's persisted board nor the
/// session running the test leaks in.
fn headway(bin: &Path, url: &str, db: &str, extra: &[&str]) -> std::process::Output {
    let nsec = nsec();
    Command::new(bin)
        .args(["--nsec", &nsec, "--relay", url, "--db", db])
        .args(extra)
        .env("HEADWAY_BOARD", "headway")
        .env("AGENTIUM_SESSION", "agentium:test-review-session")
        .output()
        .expect("run headway")
}

/// Run `git -C <dir> <args>`, panicking on failure; returns trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A temp repo on branch `trunk` holding one commit titled `subject`.
fn one_commit_repo(subject: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("repo dir");
    git(dir.path(), &["init", "-q", "-b", "trunk"]);
    git(
        dir.path(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            subject,
        ],
    );
    dir
}

/// Poll `show --json` until the board holds a card, and return its hex id.
fn card_id_until(bin: &Path, url: &str, db: &str) -> String {
    for _ in 0..50 {
        let out = headway(bin, url, db, &["show", "--json"]);
        if let Ok(board) = serde_json::from_slice::<Value>(&out.stdout)
            && let Some(id) = board["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|c| c["cards"].as_array().into_iter().flatten())
                .find_map(|c| c["id"].as_str())
        {
            return id.to_string();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the added card never showed up");
}

/// Poll `show <card> --json` until the card carries `n` review records.
fn reviews_until(bin: &Path, url: &str, db: &str, card: &str, n: usize) -> Vec<Value> {
    for _ in 0..50 {
        let out = headway(bin, url, db, &["show", card, "--json"]);
        if out.status.success()
            && let Ok(v) = serde_json::from_slice::<Value>(&out.stdout)
            && let Some(reviews) = v[0]["reviews"].as_array()
            && reviews.len() == n
        {
            return reviews.clone();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("card {card} never reached {n} review records");
}

/// `review` records the commit, its subject, branch, toplevel, repo identity,
/// host, the `$AGENTIUM_SESSION` ref and the explainer, and `show --json` reads
/// all of it back.
#[test]
fn review_records_git_metadata_on_a_card() {
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

    let seed = headway(&bin, &url, db, &["seed"]);
    assert!(
        seed.status.success(),
        "seed: {}",
        String::from_utf8_lossy(&seed.stderr)
    );
    let add = headway(&bin, &url, db, &["add", "Review me"]);
    assert!(
        add.status.success(),
        "add: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    // `add` prints the new card's ref only when its immediate re-fold already
    // sees the sealed card, so poll `show` for its id instead.
    let card = card_id_until(&bin, &url, db);

    let repo = one_commit_repo("the reviewed commit");
    let sha = git(repo.path(), &["rev-parse", "HEAD"]);
    let top = git(repo.path(), &["rev-parse", "--show-toplevel"]);
    let explainer = "https://example.com/x";

    let review = headway(
        &bin,
        &url,
        db,
        &[
            "review",
            &card,
            "--repo-dir",
            repo.path().to_str().unwrap(),
            "--explainer",
            explainer,
        ],
    );
    let stdout = String::from_utf8_lossy(&review.stdout);
    assert!(
        review.status.success(),
        "review: {}",
        String::from_utf8_lossy(&review.stderr)
    );
    // The recorded fields are echoed so an agent's transcript shows them.
    assert!(
        stdout.contains(&sha),
        "review output lacks the sha: {stdout}"
    );

    let reviews = reviews_until(&bin, &url, db, &card, 1);
    let r = &reviews[0];
    assert_eq!(r["commit"], sha.as_str());
    assert_eq!(r["title"], "the reviewed commit");
    assert_eq!(r["branch"], "trunk");
    assert_eq!(r["path"], top.as_str());
    // The only commit is its own root, so it is the repo identity.
    assert_eq!(r["repo"], sha.as_str());
    assert_eq!(r["explainer"], explainer);
    assert_eq!(r["agentium"], "agentium:test-review-session");
    assert!(r["host"].as_str().is_some_and(|h| !h.is_empty()), "{r:#}");
    assert!(r["remote"].is_null(), "{r:#}");
}

/// Outside a git repo `review` refuses with git's own message, before touching
/// the relay (the fields are gathered while parsing).
#[test]
fn review_refuses_outside_a_repo() {
    let bin = headway_bin();
    let not_a_repo = tempfile::tempdir().expect("dir");
    let cli_dir = tempfile::tempdir().expect("cli dir");
    let out = headway(
        &bin,
        // Nothing listens here: reaching the relay would be a failure of its own.
        "ws://127.0.0.1:9",
        cli_dir.path().to_str().unwrap(),
        &[
            "review",
            "some-card-ref",
            "--repo-dir",
            not_a_repo.path().to_str().unwrap(),
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("not a git repository"), "{stderr}");
}
