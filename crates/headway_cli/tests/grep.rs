//! End-to-end: the real `headway grep` searches card text — titles,
//! descriptions and comments — across a board in one run, against a real
//! embedded relay.
//!
//! The load-bearing assertions are the ones a `show --json` + `show <card>`
//! loop can't make cheaply: one invocation covers every card and its comment
//! thread across every board, archived cards included, while `--board` narrows
//! to one board and `--in` to a subtree.

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
/// worktree last hardlinked its build (maybe one without `grep`) onto the
/// shared `target/debug/headway`. See [`bin_testing::worktree_bin`].
fn headway_bin() -> PathBuf {
    bin_testing::worktree_bin(
        Path::new(env!("CARGO_BIN_EXE_headway")),
        "headway_cli",
        knows_grep,
    )
}

/// A current build lists `grep` in its no-arg usage (printed to stderr).
fn knows_grep(bin: &Path) -> bool {
    Command::new(bin)
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stderr).contains("grep"))
}

/// Run `headway` against `url`/`db` with `extra`, the default board pinned so
/// the developer's persisted board can't leak in. Asserts a clean exit and
/// returns stdout.
fn headway(bin: &Path, url: &str, db: &str, extra: &[&str]) -> String {
    headway_env(bin, url, db, extra, &[])
}

/// [`headway`] with `envs` set on the run too.
fn headway_env(bin: &Path, url: &str, db: &str, extra: &[&str], envs: &[(&str, &str)]) -> String {
    let nsec = nsec();
    let out = Command::new(bin)
        .args(["--nsec", &nsec, "--relay", url, "--db", db])
        .args(extra)
        .env("HEADWAY_BOARD", "headway")
        .env_remove("HEADWAY_COMMENT_NSEC")
        .env_remove("HEADWAY_COMMENT_NSEC_FILE")
        .env_remove("HEADWAY_PAGER")
        .env_remove("PAGER")
        .envs(envs.iter().copied())
        .output()
        .expect("run headway");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "headway {extra:?} failed\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// `add` a card with `extra` flags, returning its `headway:<board>/<word-id>`.
fn add(bin: &Path, url: &str, db: &str, extra: &[&str]) -> String {
    let mut args = vec!["--json", "add"];
    args.extend_from_slice(extra);
    let out = headway(bin, url, db, &args);
    let v: Value = serde_json::from_str(&out).expect("add --json");
    v["ref"].as_str().expect("new card ref").to_string()
}

/// `grep --json` with `args`, as the parsed array of per-card rows.
fn grep(bin: &Path, url: &str, db: &str, args: &[&str]) -> Vec<Value> {
    let mut argv = vec!["--json", "grep"];
    argv.extend_from_slice(args);
    let out = headway(bin, url, db, &argv);
    serde_json::from_str::<Value>(&out)
        .expect("grep --json")
        .as_array()
        .expect("array")
        .clone()
}

/// Poll `grep --json` with `args` until it returns `n` cards — a sealed
/// board's edits fold only once nostrdb has peeled their envelopes.
fn grep_until(bin: &Path, url: &str, db: &str, args: &[&str], n: usize) -> Vec<Value> {
    for _ in 0..50 {
        let rows = grep(bin, url, db, args);
        if rows.len() == n {
            return rows;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("grep {args:?} never reached {n} cards");
}

/// The row for card `card_ref` in a `grep --json` result.
fn row<'a>(rows: &'a [Value], card_ref: &str) -> &'a Value {
    rows.iter()
        .find(|r| r["ref"] == card_ref)
        .unwrap_or_else(|| panic!("no row for {card_ref}: {rows:?}"))
}

/// A row's matches, each as `<field>: <text>`.
fn matches(row: &Value) -> Vec<String> {
    row["matches"]
        .as_array()
        .expect("matches")
        .iter()
        .map(|m| {
            format!(
                "{}: {}",
                m["field"].as_str().unwrap(),
                m["text"].as_str().unwrap()
            )
        })
        .collect()
}

#[test]
fn grep_searches_card_text_across_the_board() {
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
    let (bin, url) = (bin.as_path(), url.as_str());

    headway(bin, url, db, &["--board", "headway", "seed"]);
    let epic = add(
        bin,
        url,
        db,
        &[
            "Relay reconnect epic",
            "--desc",
            "first line, no hit\nthe RELAY stalls past the cap",
        ],
    );
    let child = add(bin, url, db, &["child task", "--parent", &epic]);
    headway(
        bin,
        url,
        db,
        &["comment", &child, "the relay looks fine now"],
    );
    let quiet = add(bin, url, db, &["Quiet card", "--desc", "nothing to find"]);
    let outside = add(bin, url, db, &["Relay docs"]);
    let archived = add(bin, url, db, &["Archived relay card"]);
    headway(bin, url, db, &["archive", &archived]);
    headway(bin, url, db, &["--board", "other", "seed"]);
    let elsewhere = add(
        bin,
        url,
        db,
        &["--board", "other", "relay on another board"],
    );

    // Wait for every edit to fold: the comment and the archive are the last.
    grep_until(bin, url, db, &["fine now"], 1);
    let rows = grep_until(bin, url, db, &["relay"], 5);

    // One run covers every card on the board: titles, the description line
    // that matches (not the one that doesn't), and the comment thread. The
    // all-lowercase pattern folds case, so `RELAY` comes along.
    assert_eq!(
        matches(row(&rows, &epic)),
        [
            "title: Relay reconnect epic",
            "desc: the RELAY stalls past the cap",
        ]
    );
    assert_eq!(
        matches(row(&rows, &child)),
        ["comment: the relay looks fine now"]
    );
    assert_eq!(row(&rows, &child)["column"], "Backlog");
    assert_eq!(row(&rows, &child)["board"], "headway");
    // Each match carries when its text was written, as unix seconds.
    assert!(
        row(&rows, &child)["matches"][0]["created_at"]
            .as_u64()
            .is_some_and(|t| t > 0),
        "{rows:?}"
    );
    // No match, no row. Everything else is searched by default: the
    // archived card and the other board's card too, though the run's current
    // board (`$HEADWAY_BOARD`) is `headway`.
    assert!(rows.iter().all(|r| r["ref"] != quiet), "{rows:?}");
    assert!(rows.iter().any(|r| r["ref"] == outside));
    assert_eq!(row(&rows, &archived)["column"], "archived");
    assert_eq!(row(&rows, &elsewhere)["board"], "other");

    // Case written into the pattern is honoured, and `-i` overrides it.
    let rows = grep(bin, url, db, &["RELAY"]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["ref"], epic);
    assert_eq!(grep(bin, url, db, &["-i", "RELAY"]).len(), 5);
    assert!(
        grep(bin, url, db, &["-s", "relay"])
            .iter()
            .all(|r| r["ref"] != outside)
    );

    // `--in` keeps the epic and its subtree, and nothing beside it.
    let rows = grep(bin, url, db, &["relay", "--in", &epic]);
    let refs: Vec<&str> = rows.iter().map(|r| r["ref"].as_str().unwrap()).collect();
    assert_eq!(refs.len(), 2, "{refs:?}");
    assert!(refs.contains(&epic.as_str()) && refs.contains(&child.as_str()));

    // `--board` narrows to one board, archived cards still included.
    let rows = grep(bin, url, db, &["--board", "headway", "relay"]);
    assert!(rows.iter().all(|r| r["ref"] != elsewhere), "{rows:?}");
    assert_eq!(row(&rows, &archived)["column"], "archived");
    assert_eq!(rows.len(), 4, "{rows:?}");
    let rows = grep(bin, url, db, &["--board", "other", "relay"]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["ref"], elsewhere);

    // The old opt-in flags are still accepted, and change nothing.
    assert_eq!(
        grep(bin, url, db, &["relay", "--all", "--archived"]).len(),
        5
    );

    // The plain rendering: each card headed by its full ref, its matching
    // lines beneath with the field they came from and how long ago they were
    // written. Piped, it is neither colored nor paged.
    let out = headway(bin, url, db, &["grep", "fine now"]);
    assert!(
        out.starts_with(&format!("{child}  child task  (Backlog)\n")),
        "{out}"
    );
    // The time is "just now" unless this run was slow enough to cross a
    // minute, so only its shape is pinned.
    let line = out
        .lines()
        .find(|l| l.starts_with("  comment  "))
        .unwrap_or_else(|| panic!("a comment row: {out}"));
    let when = line["  comment  ".len()..]
        .strip_suffix("  the relay looks fine now")
        .unwrap_or_else(|| panic!("time, then the line: {line:?}"));
    assert!(
        when == "just now" || when.ends_with(" ago"),
        "a relative time: {when:?}"
    );
    assert!(!out.contains('\x1b'), "no color into a pipe: {out:?}");
    assert_eq!(
        headway(bin, url, db, &["grep", "zzz-no-such-text"]).trim(),
        "no matches"
    );

    // `--color always` keeps the highlight into a pipe: the matched span is
    // painted bold red, grep's own convention.
    let out = headway(bin, url, db, &["grep", "fine now", "--color", "always"]);
    assert!(
        out.contains(&format!("\x1b[{}mfine now\x1b[0m", cli_term::SGR_MATCH)),
        "{out:?}"
    );

    // `--pager` pages even into a pipe, through `$HEADWAY_PAGER` — here a pager
    // that marks every line it is fed, so its output proves the text went
    // through it. Unix only: the stand-in pager is `sed`.
    if !cfg!(unix) {
        return;
    }
    let paged = headway_env(
        bin,
        url,
        db,
        &["grep", "fine now", "--pager"],
        &[("HEADWAY_PAGER", "sed s/^/paged:/")],
    );
    // Colored, too: `--color auto` follows the effective sink, and the default
    // pager (`less -R`) renders color.
    assert!(
        paged.starts_with(&format!(
            "paged:\x1b[33m{child}\x1b[0m  \x1b[1mchild task\x1b[0m"
        )),
        "{paged:?}"
    );
    assert!(paged.lines().all(|l| l.starts_with("paged:")), "{paged}");
}
