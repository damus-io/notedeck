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

/// The `headway` binary this worktree built, rather than whatever sibling
/// worktree last hardlinked its build (maybe one without `review`) onto the
/// shared `target/debug/headway`. See [`bin_testing::worktree_bin`].
fn headway_bin() -> PathBuf {
    bin_testing::worktree_bin(
        Path::new(env!("CARGO_BIN_EXE_headway")),
        "headway_cli",
        knows_review,
    )
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

    let seed = headway(&bin, &url, db, &["--board", "headway", "seed"]);
    assert!(
        seed.status.success(),
        "seed: {}",
        String::from_utf8_lossy(&seed.stderr)
    );
    let add = headway(&bin, &url, db, &["add", "Review me", "--json"]);
    assert!(
        add.status.success(),
        "add: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    // `seed` makes a sealed board, where the new card only folds in once its
    // envelope is unwrapped; `add` still reports its ref straight away.
    let added: Value = serde_json::from_slice(&add.stdout).expect("add --json");
    let card = added["ref"]
        .as_str()
        .unwrap_or_else(|| panic!("add --json lacks the new card's ref: {added}"))
        .to_string();

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

/// Poll `show <card> --json` until its newest review record carries `n`
/// comments; returns that record.
fn record_with_comments(bin: &Path, url: &str, db: &str, card: &str, n: usize) -> Value {
    for _ in 0..50 {
        let out = headway(bin, url, db, &["show", card, "--json"]);
        if out.status.success()
            && let Ok(v) = serde_json::from_slice::<Value>(&out.stdout)
            && v[0]["reviews"][0]["comments"]
                .as_array()
                .is_some_and(|c| c.len() == n)
        {
            return v[0]["reviews"][0].clone();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("card {card}'s record never reached {n} review comments");
}

/// Run `headway` and panic with its stderr unless it succeeded.
fn ok(bin: &Path, url: &str, db: &str, extra: &[&str]) -> String {
    let out = headway(bin, url, db, extra);
    assert!(
        out.status.success(),
        "{extra:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// An agent reads and answers inline review comments from the CLI: `comment
/// --path/--line` leaves one on the card's record, `show` prints it under the
/// record with its place, `--reply-to` a review comment threads the answer
/// under it on the same record and lines (not the card's thread), and
/// `--record` alone comments on the whole commit.
#[test]
fn review_comments_post_show_and_reply() {
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

    ok(&bin, &url, db, &["--board", "headway", "seed"]);
    let added: Value =
        serde_json::from_str(&ok(&bin, &url, db, &["add", "Comment me", "--json"])).unwrap();
    let card = added["ref"].as_str().expect("new card ref").to_string();

    // No record yet: a line comment has nothing to land on.
    let early = headway(
        &bin,
        &url,
        db,
        &["comment", &card, "--path", "a.rs", "--line", "1", "x"],
    );
    assert!(!early.status.success(), "a line comment needs a record");

    let repo = one_commit_repo("the commented commit");
    let sha = git(repo.path(), &["rev-parse", "HEAD"]);
    ok(
        &bin,
        &url,
        db,
        &["review", &card, "--repo-dir", repo.path().to_str().unwrap()],
    );
    reviews_until(&bin, &url, db, &card, 1);

    ok(
        &bin,
        &url,
        db,
        &[
            "comment", &card, "--path", "src/a.rs", "--line", "3-5", "why", "a", "clone?",
        ],
    );
    let record = record_with_comments(&bin, &url, db, &card, 1);
    let top = &record["comments"][0];
    assert_eq!(top["path"], "src/a.rs");
    assert_eq!(top["line"], "3-5");
    assert_eq!(top["side"], "new");
    assert_eq!(top["commit"], sha.as_str());
    assert_eq!(top["body"], "why a clone?");
    assert!(top["parent"].is_null(), "{top:#}");

    let shown = ok(&bin, &url, db, &["show", &card]);
    assert!(shown.contains("1 review comment"), "{shown}");
    assert!(shown.contains("src/a.rs:3-5"), "{shown}");
    assert!(shown.contains("why a clone?"), "{shown}");

    // Reply by a hex prefix of the review comment's id.
    let top_id = top["id"].as_str().unwrap().to_string();
    ok(
        &bin,
        &url,
        db,
        &[
            "comment",
            &card,
            "--reply-to",
            &top_id[..12],
            "dropped",
            "it",
        ],
    );
    let record = record_with_comments(&bin, &url, db, &card, 2);
    let reply = &record["comments"][1];
    assert_eq!(reply["parent"], top_id.as_str());
    assert_eq!(reply["body"], "dropped it");
    // Under the same lines, so the app's diff shows it beside its parent.
    assert_eq!(reply["path"], "src/a.rs");
    assert_eq!(reply["line"], "3-5");

    let shown = ok(&bin, &url, db, &["show", &card]);
    let reply_line = shown
        .lines()
        .position(|l| l.contains("↳"))
        .unwrap_or_else(|| panic!("no nested reply in:\n{shown}"));
    assert!(
        shown
            .lines()
            .nth(reply_line + 1)
            .unwrap()
            .contains("dropped it"),
        "{shown}"
    );
    // The reply's place is its parent's, so it isn't repeated.
    assert_eq!(shown.matches("src/a.rs:3-5").count(), 1, "{shown}");

    // The review flags don't mix with a reply.
    let mixed = headway(
        &bin,
        &url,
        db,
        &["comment", &card, "--reply-to", &top_id, "--line", "1", "x"],
    );
    assert!(!mixed.status.success(), "a reply takes its parent's lines");

    // `--record` alone: a comment on the whole commit.
    ok(
        &bin,
        &url,
        db,
        &["comment", &card, "--record", &sha[..7], "lgtm", "overall"],
    );
    let record = record_with_comments(&bin, &url, db, &card, 3);
    let whole = &record["comments"][2];
    assert!(
        whole["path"].is_null() && whole["parent"].is_null(),
        "{whole:#}"
    );

    // None of it went into the card's own thread.
    let v: Value = serde_json::from_str(&ok(&bin, &url, db, &["show", &card, "--json"])).unwrap();
    assert_eq!(v[0]["comments"].as_array().map(Vec::len), Some(0), "{v:#}");
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
