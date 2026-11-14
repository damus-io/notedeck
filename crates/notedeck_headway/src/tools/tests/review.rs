//! The `headway` CLI's `review` e2e cases, ported to `headway_review`, and
//! `headway_diff` read back through the registry as `nd headway diff` runs
//! it: its work left pending for a worker, its patch as the text and where
//! it was found as the notice. Each runs against a live notedeck and a temp
//! git repo.

use super::live::{Live, keys, live, next_second};
use super::*;
use notedeck::Started;
use std::path::Path;
use std::process::Command;

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

/// Commit everything in `dir` as `message`, with a throwaway identity.
fn commit(dir: &Path, message: &str) {
    git(
        dir,
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
            message,
        ],
    );
}

/// A temp repo on branch `trunk` holding one commit titled `subject`, which
/// adds `hello.txt`.
fn one_commit_repo(subject: &str) -> TempDir {
    let dir = TempDir::new().expect("repo dir");
    git(dir.path(), &["init", "-q", "-b", "trunk"]);
    std::fs::write(dir.path().join("hello.txt"), "hello\n").unwrap();
    git(dir.path(), &["add", "hello.txt"]);
    commit(dir.path(), subject);
    dir
}

/// `path` as a string, as `nd` sends a path.
fn path(dir: &Path) -> String {
    dir.to_string_lossy().into_owned()
}

/// The first card on the demo board.
fn first_card(live: &mut Live) -> String {
    live.cards()[0]["ref"].as_str().unwrap().to_owned()
}

/// The card `card`'s review records, once it has `n`.
fn reviews_until(live: &mut Live, card: &str, n: usize) -> Vec<serde_json::Value> {
    let card = live.card_until(card, "the review records", |c| {
        c["reviews"].as_array().is_some_and(|r| r.len() == n)
    });
    card["reviews"].as_array().unwrap().clone()
}

/// What [`start_diff`] came to: the text `nd` prints, the notice it prints
/// to stderr, and the value.
struct Diffed {
    json: serde_json::Value,
    text: String,
    notice: String,
}

/// Start `headway_diff` with `args` as the socket starts it: its work must
/// be left pending, not run on the caller's thread. Then run it, as the
/// socket's worker does.
fn start_diff(live: &mut Live, args: serde_json::Value) -> Result<Diffed, String> {
    let tool = tools()
        .into_iter()
        .find(|tool| tool.name() == "headway_diff")
        .unwrap();
    let mut app = live.notedeck.app_context();
    let mut cx = app.tool_context();
    let pending = match tool.start_rendered(&mut cx, &args) {
        Started::Pending(pending) => pending,
        Started::Done(done) => match done.outcome {
            notedeck::ToolOutcome::Error(err) => return Err(err),
            _ => panic!("headway_diff answered without running its work off the loop"),
        },
    };
    let done = pending.run();
    Ok(Diffed {
        json: done.result?,
        text: done.text.expect("diff renders its patch"),
        notice: done.notice.expect("diff says where it found the commit"),
    })
}

/// `review` records the commit, its subject, branch, toplevel, repo
/// identity, host, the caller's `$AGENTIUM_SESSION` ref and the explainer,
/// echoes them, and `show` reads all of it back. A malformed
/// `$AGENTIUM_SESSION` is left out. (Ports
/// `review_records_git_metadata_on_a_card`.)
#[tokio::test]
async fn review_records_git_metadata_on_a_card() {
    let mut live = live();
    let card = first_card(&mut live);
    let repo = one_commit_repo("the reviewed commit");
    let sha = git(repo.path(), &["rev-parse", "HEAD"]);
    let top = git(repo.path(), &["rev-parse", "--show-toplevel"]);
    let explainer = "https://example.com/x";

    let ack = live.edit(
        "headway_review",
        json!({
            "card": card,
            "repo_dir": path(repo.path()),
            "explainer": explainer,
            "default_agentium": "agentium:test-review-session",
        }),
    );
    assert_eq!(keys(&ack.json), ["events", "ok", "review", "sources"]);
    assert_eq!(ack.json["review"]["commit"], sha.as_str());
    // The recorded fields are echoed so an agent's transcript shows them.
    assert!(ack.text.starts_with("ok ("), "{}", ack.text);
    assert!(
        ack.text.contains(&format!("commit     {sha}")),
        "{}",
        ack.text
    );

    let reviews = reviews_until(&mut live, &card, 1);
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

    // A stray `$AGENTIUM_SESSION` never fails a review; an explicit bad
    // `agentium` does.
    let ack = live.edit(
        "headway_review",
        json!({ "card": card, "repo_dir": path(repo.path()), "default_agentium": "junk" }),
    );
    assert!(ack.json["review"].get("agentium").is_none(), "{}", ack.json);
    let err = live
        .call(
            "headway_review",
            json!({ "card": card, "repo_dir": path(repo.path()), "agentium": "junk" }),
        )
        .unwrap_err();
    assert!(err.contains("--agentium wants agentium:<word-id>"), "{err}");
}

/// The source flags store the files on the new record in the same call, by
/// file name, and `show` reads them back byte for byte; two files of one
/// name are refused before anything is written. (Ports
/// `review_sources_record_and_read_back`.)
#[tokio::test]
async fn review_sources_record_and_read_back() {
    let mut live = live();
    let card = first_card(&mut live);
    let repo = one_commit_repo("the explained commit");
    let book = TempDir::new().expect("book dir");
    let explainer_text = "# What\n\n\"quoted\" and trailing newline\n";
    let code_text = "Walk:\n\n```diff\n+x\n```";
    let md = book.path().join("explain-me.md");
    let code = book.path().join("explain-me.code.md");
    std::fs::write(&md, explainer_text).unwrap();
    std::fs::write(&code, code_text).unwrap();

    let ack = live.edit(
        "headway_review",
        json!({
            "card": card,
            "repo_dir": path(repo.path()),
            "explainer_source": [path(&md)],
            "code_source": [path(&code)],
        }),
    );
    assert!(
        ack.text.contains("explain-me.code.md (code,"),
        "{}",
        ack.text
    );
    assert_eq!(ack.json["events"], json!(3), "{}", ack.json);
    assert_eq!(
        ack.json["sources"],
        json!([
            {"name": "explain-me.md", "role": "explainer", "bytes": explainer_text.len()},
            {"name": "explain-me.code.md", "role": "code", "bytes": code_text.len()},
        ])
    );

    let card_json = live.card_until(&card, "both sources", |c| {
        c["reviews"][0]["sources"]
            .as_array()
            .is_some_and(|s| s.len() == 2)
    });
    let sources = card_json["reviews"][0]["sources"].as_array().unwrap();
    // Sorted by name.
    assert_eq!(sources[0]["name"], "explain-me.code.md");
    assert_eq!(sources[0]["text"], code_text);
    assert_eq!(sources[1]["name"], "explain-me.md");
    assert_eq!(sources[1]["role"], "explainer");
    assert_eq!(sources[1]["text"], explainer_text);

    let other = TempDir::new().expect("other dir");
    let clash = other.path().join("explain-me.md");
    std::fs::write(&clash, "other").unwrap();
    let err = live
        .call(
            "headway_review",
            json!({
                "card": card,
                "repo_dir": path(repo.path()),
                "explainer_source": [path(&md), path(&clash)],
            }),
        )
        .unwrap_err();
    assert!(err.contains("two sources named"), "{err}");
}

/// Outside a git repo `review` refuses with git's own message; with no
/// `repo_dir` at all (a model's call) it says what's missing. (Ports
/// `review_refuses_outside_a_repo`.)
#[tokio::test]
async fn review_refuses_outside_a_repo() {
    let mut live = live();
    let card = first_card(&mut live);
    let not_a_repo = TempDir::new().expect("dir");
    let err = live
        .call(
            "headway_review",
            json!({ "card": card, "repo_dir": path(not_a_repo.path()) }),
        )
        .unwrap_err();
    assert!(err.contains("not a git repository"), "{err}");

    let err = live
        .call("headway_review", json!({ "card": card }))
        .unwrap_err();
    assert!(err.contains("repo_dir is required"), "{err}");
}

/// `diff` reads back the commit a card's record names, `git show`-style, as
/// its text, with where it was found as its notice; `record` picks a record
/// by sha prefix, and a missing explain note is an error naming the commit.
/// Its work is left for a worker, never run on the caller's thread.
#[tokio::test]
async fn diff_reads_back_the_reviewed_commit() {
    let mut live = live();
    let card = first_card(&mut live);
    let repo = one_commit_repo("the diffed commit");
    let first = git(repo.path(), &["rev-parse", "HEAD"]);
    live.edit(
        "headway_review",
        json!({ "card": card, "repo_dir": path(repo.path()) }),
    );
    std::fs::write(repo.path().join("hello.txt"), "hello again\n").unwrap();
    git(repo.path(), &["add", "hello.txt"]);
    commit(repo.path(), "the second commit");
    let second = git(repo.path(), &["rev-parse", "HEAD"]);
    next_second();
    live.edit(
        "headway_review",
        json!({ "card": card, "repo_dir": path(repo.path()) }),
    );
    reviews_until(&mut live, &card, 2);

    // Read from somewhere that isn't the repo: the record's path finds it.
    let elsewhere = TempDir::new().expect("dir");
    let newest = start_diff(
        &mut live,
        json!({ "card": card, "repo_dir": path(elsewhere.path()) }),
    )
    .unwrap();
    assert_eq!(newest.json["commit"], second.as_str());
    assert_eq!(newest.json["found"], "local checkout");
    assert_eq!(newest.json["truncated"], json!(false));
    assert!(
        newest.text.starts_with(&format!("commit {second}\n")),
        "{}",
        newest.text
    );
    assert!(
        newest.text.contains("    the second commit\n"),
        "{}",
        newest.text
    );
    assert!(newest.text.contains("+hello again"), "{}", newest.text);
    assert!(
        newest.notice.starts_with("found locally in "),
        "{}",
        newest.notice
    );

    let older = start_diff(
        &mut live,
        json!({ "card": card, "record": &first[..8], "repo_dir": path(repo.path()) }),
    )
    .unwrap();
    assert_eq!(older.json["commit"], first.as_str());
    assert!(older.text.contains("+hello\n"), "{}", older.text);

    let err = start_diff(
        &mut live,
        json!({ "card": card, "note": "explain", "repo_dir": path(repo.path()) }),
    )
    .err()
    .unwrap();
    assert!(err.contains("has no explain note"), "{err}");
}

/// A card with no review record is found by its `Headway:` trailer in the
/// caller's checkout; with neither, `diff` says so before any work runs.
#[tokio::test]
async fn diff_falls_back_to_the_headway_trailer() {
    let mut live = live();
    let card = first_card(&mut live);
    let repo = one_commit_repo("unrelated");
    let elsewhere = TempDir::new().expect("dir");

    let err = start_diff(
        &mut live,
        json!({ "card": card, "repo_dir": path(elsewhere.path()) }),
    )
    .err()
    .unwrap();
    assert!(err.contains("has no review record"), "{err}");

    commit(repo.path(), &format!("trailered\n\nHeadway: {card}"));
    let sha = git(repo.path(), &["rev-parse", "HEAD"]);
    let found = start_diff(
        &mut live,
        json!({ "card": card, "repo_dir": path(repo.path()) }),
    )
    .unwrap();
    assert_eq!(found.json["commit"], sha.as_str());
    assert_eq!(found.json["found"], "found by Headway trailer");
}

/// The review and diff tools read git in the caller's checkout: `repo_dir`
/// is a path `nd` resolves against its own working directory and sends
/// when left out, and the source files are paths `nd` resolves the same
/// way, one per flag.
#[test]
fn repo_dir_and_sources_resolve_on_the_callers_side() {
    for name in ["headway_review", "headway_diff"] {
        let tool = tools().into_iter().find(|t| t.name() == name).unwrap();
        assert_eq!(tool.spec().command_path().join(" "), name.replace('_', " "));
        let schema = tool.spec().json_schema();
        let repo_dir = &schema["properties"]["repo_dir"];
        assert_eq!(repo_dir["x-nd-kind"], "path", "{name}");
        assert_eq!(repo_dir["x-nd-default-cwd"], json!(true), "{name}");
    }
    let review = tools()
        .into_iter()
        .find(|t| t.name() == "headway_review")
        .unwrap();
    let schema = review.spec().json_schema();
    for source in ["explainer_source", "code_source"] {
        assert_eq!(schema["properties"][source]["x-nd-kind"], "paths");
    }
    assert_eq!(
        schema["properties"]["default_agentium"]["x-nd-env"],
        "AGENTIUM_SESSION"
    );
}
