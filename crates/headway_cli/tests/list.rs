//! End-to-end: the real `headway list` filters cards across every board in
//! one run, against a real embedded relay, with the query syntax the app's
//! board filter shares (`p:high`, `label:`, `is:done`) and `--in`/`--board`
//! narrowing it.

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
/// worktree last hardlinked its build (maybe one without `list`) onto the
/// shared `target/debug/headway`. See [`bin_testing::worktree_bin`].
fn headway_bin() -> PathBuf {
    bin_testing::worktree_bin(
        Path::new(env!("CARGO_BIN_EXE_headway")),
        "headway_cli",
        knows_list,
    )
}

/// A current build lists `list` in its no-arg usage (printed to stderr).
fn knows_list(bin: &Path) -> bool {
    Command::new(bin).output().is_ok_and(|out| {
        String::from_utf8_lossy(&out.stderr).contains("List the cards a query matches")
    })
}

/// Run `headway` against `url`/`db` with `extra`, the default board pinned so
/// the developer's persisted board can't leak in. Asserts a clean exit and
/// returns stdout.
fn headway(bin: &Path, url: &str, db: &str, extra: &[&str]) -> String {
    let nsec = nsec();
    let out = Command::new(bin)
        .args(["--nsec", &nsec, "--relay", url, "--db", db])
        .args(extra)
        .env("HEADWAY_BOARD", "headway")
        .env_remove("HEADWAY_COMMENT_NSEC")
        .env_remove("HEADWAY_COMMENT_NSEC_FILE")
        .env_remove("HEADWAY_PAGER")
        .env_remove("PAGER")
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

/// The refs `list --json` with `args` returns, in list order.
fn list(bin: &Path, url: &str, db: &str, args: &[&str]) -> Vec<String> {
    let mut argv = vec!["--json", "list"];
    argv.extend_from_slice(args);
    let out = headway(bin, url, db, &argv);
    serde_json::from_str::<Value>(&out)
        .expect("list --json")
        .as_array()
        .expect("array")
        .iter()
        .map(|r| r["ref"].as_str().expect("ref").to_string())
        .collect()
}

/// Poll `list --json` with `args` until it returns `n` cards, so every edit
/// has folded before the assertions run.
fn list_until(bin: &Path, url: &str, db: &str, args: &[&str], n: usize) -> Vec<String> {
    for _ in 0..50 {
        let rows = list(bin, url, db, args);
        if rows.len() == n {
            return rows;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("list {args:?} never reached {n} cards");
}

#[test]
fn list_filters_cards_across_boards() {
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
    let epic = add(bin, url, db, &["List epic", "-l", "ui"]);
    let child = add(bin, url, db, &["child task", "--parent", &epic]);
    headway(bin, url, db, &["priority", &child, "high"]);
    let shipped = add(bin, url, db, &["Shipped card", "-l", "ui"]);
    headway(bin, url, db, &["priority", &shipped, "high"]);
    headway(bin, url, db, &["move", &shipped, "--col", "done"]);
    let archived = add(bin, url, db, &["Archived card"]);
    headway(bin, url, db, &["archive", &archived]);
    headway(bin, url, db, &["--board", "other", "seed"]);
    let elsewhere = add(
        bin,
        url,
        db,
        &["--board", "other", "Card on another board", "-l", "ui"],
    );
    headway(bin, url, db, &["priority", &elsewhere, "high"]);

    // Every board, open cards only by default: the done card and the archived
    // one are left out, though the run's current board is `headway`.
    list_until(bin, url, db, &["is:done"], 1);
    let all_open = list_until(bin, url, db, &["p:high"], 2);
    assert_eq!(all_open, [child.as_str(), elsewhere.as_str()]);
    assert_eq!(
        list(bin, url, db, &[]),
        [epic.as_str(), child.as_str(), elsewhere.as_str()]
    );

    // Terms AND; `--all` brings the done card back; `is:done` asks for it.
    assert_eq!(
        list(bin, url, db, &["p:high", "label:ui"]),
        [elsewhere.as_str()]
    );
    assert_eq!(
        list(bin, url, db, &["p:high", "label:ui", "--all"]),
        [shipped.as_str(), elsewhere.as_str()]
    );
    assert_eq!(list(bin, url, db, &["is:done"]), [shipped.as_str()]);
    assert_eq!(
        list(bin, url, db, &["col:backlog", "--board", "headway"]),
        [epic.as_str(), child.as_str()]
    );

    // `--in` keeps the epic and its subtree; `--board` narrows to one board.
    assert_eq!(
        list(bin, url, db, &["--in", &epic]),
        [epic.as_str(), child.as_str()]
    );
    assert_eq!(
        list(bin, url, db, &["--board", "other"]),
        [elsewhere.as_str()]
    );
    // A pasted ref names that one card.
    assert_eq!(list(bin, url, db, &[&elsewhere]), [elsewhere.as_str()]);

    // `--json` carries the card's fields plus where it sits.
    let out = headway(bin, url, db, &["--json", "list", "is:done"]);
    let rows: Value = serde_json::from_str(&out).expect("list --json");
    assert_eq!(rows[0]["column"], "Done");
    assert_eq!(rows[0]["board"], "headway");
    assert_eq!(rows[0]["priority"], "high");

    // `is:archived` lists the archived card, in an `archived` column, and
    // nothing live; no other state term reaches it.
    assert_eq!(list(bin, url, db, &["is:archived"]), [archived.as_str()]);
    assert!(!list(bin, url, db, &["is:any"]).contains(&archived));
    let out = headway(bin, url, db, &["--json", "list", "is:archived"]);
    let rows: Value = serde_json::from_str(&out).expect("list --json");
    assert_eq!(rows[0]["column"], "archived");
    let out = headway(bin, url, db, &["list", "is:archived"]);
    assert!(out.contains("[archived]  Archived card"), "{out:?}");

    // The plain rendering: one row per card, its ref and column before the
    // title, uncolored into a pipe; `-n` caps it.
    let out = headway(bin, url, db, &["list", "p:high", "-n", "1"]);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.starts_with(&format!("↑ {child}")), "{out:?}");
    assert!(out.contains("[Backlog]  child task"), "{out:?}");
    assert!(!out.contains('\x1b'), "no color into a pipe: {out:?}");
    assert_eq!(
        headway(bin, url, db, &["list", "zzz-nothing"]).trim(),
        "no cards"
    );

    // Display properties: `--hide labels` drops the label suffix and keeps
    // the rest; `--show` prints just what it names beside the title.
    let out = headway(bin, url, db, &["list", "--board", "other"]);
    assert!(out.contains("Card on another board  [ui]"), "{out:?}");
    let out = headway(
        bin,
        url,
        db,
        &["list", "--board", "other", "--hide", "labels"],
    );
    assert!(!out.contains("[ui]"), "{out:?}");
    assert!(out.contains(&elsewhere), "the ref stays: {out:?}");
    let out = headway(
        bin,
        url,
        db,
        &["list", "--board", "other", "--show", "status"],
    );
    assert_eq!(out.trim_end(), "[Backlog]  Card on another board");

    // Grouping: `--group status` heads each column, same-named columns of
    // both boards in one group, and its rows drop the `[column]` the header
    // already says.
    let out = headway(bin, url, db, &["list", "--group", "status", "--all"]);
    let headers: Vec<&str> = out.lines().filter(|l| l.starts_with("## ")).collect();
    assert_eq!(headers, ["## Backlog (3)", "## Done (1)"], "{out}");
    assert!(!out.contains("[Backlog]"), "{out:?}");
    // `--empty` adds the columns no card is in; sub-groups indent.
    let out = headway(
        bin,
        url,
        db,
        &["list", "--group", "status", "--empty", "--board", "headway"],
    );
    assert!(out.contains("## In Progress (0)"), "{out}");
    let out = headway(
        bin,
        url,
        db,
        &["list", "--group", "board", "--sub-group", "priority"],
    );
    assert!(out.contains("\n  ## High (1)\n"), "{out}");

    // `--json` grouped: one object per group, its rows and its sub-groups.
    let out = headway(
        bin,
        url,
        db,
        &["--json", "list", "--group", "label", "--sub-group", "board"],
    );
    let groups: Value = serde_json::from_str(&out).expect("list --json --group");
    assert_eq!(groups[0]["group"], "ui");
    assert_eq!(groups[0]["count"], 2);
    assert_eq!(groups[0]["rows"][0]["ref"], epic.as_str());
    assert_eq!(groups[0]["subgroups"][1]["group"], "other");
    assert_eq!(groups[0]["subgroups"][1]["count"], 1);
    assert_eq!(groups[1]["group"], "No label");
    assert_eq!(groups[1]["rows"][0]["ref"], child.as_str());

    // Ordering: by priority the unprioritised epic goes last; by due date
    // the one dated card leads and the undated keep board order after it,
    // and `-n` keeps the first rows of the sorted list.
    let by_priority = list(bin, url, db, &["--sort", "priority"]);
    assert_eq!(by_priority.last(), Some(&epic), "{by_priority:?}");
    headway(bin, url, db, &["due", &epic, "2026-11-01"]);
    let by_due = list_until(bin, url, db, &["due:any"], 1);
    assert_eq!(by_due, [epic.as_str()]);
    assert_eq!(
        list(bin, url, db, &["--sort", "due", "--descending"]),
        [epic.as_str(), child.as_str(), elsewhere.as_str()]
    );
    assert_eq!(
        list(
            bin,
            url,
            db,
            &["--sort", "priority", "--ascending", "-n", "2"]
        )
        .len(),
        2
    );
    assert_eq!(
        list(bin, url, db, &["--sort", "due", "-n", "1"]),
        [epic.as_str()]
    );

    // Work order marks the ready frontier, the rows `next --ready` hands
    // out: the child is ready, its epic waits on it.
    let out = headway(
        bin,
        url,
        db,
        &["list", "--board", "headway", "--sort", "work-order"],
    );
    let lines: Vec<&str> = out.lines().collect();
    // The ready and priority slots, both blank for the epic.
    assert!(lines[0].starts_with(&format!("    {epic}")), "{out:?}");
    assert!(lines[1].starts_with(&format!("▶ ↑ {child}")), "{out:?}");
    let out = headway(
        bin,
        url,
        db,
        &[
            "--json",
            "list",
            "--board",
            "headway",
            "--sort",
            "work-order",
        ],
    );
    let rows: Value = serde_json::from_str(&out).expect("list --json --sort");
    assert_eq!(rows[0]["ready"], false);
    assert_eq!(rows[1]["ready"], true);

    // Saved views: save the run's query and layout as a view, list the
    // views, list through it (the run's words narrow it), and delete it.
    let out = headway(
        bin,
        url,
        db,
        &[
            "list",
            "label:ui",
            "--group",
            "board",
            "--save-view",
            "UI work",
        ],
    );
    assert_eq!(out.trim(), "saved view 'UI work' (ui-work)");
    let out = headway(bin, url, db, &["list", "--views"]);
    let names: Vec<&str> = out
        .lines()
        .map(|l| l.split("  ").next().unwrap_or("").trim())
        .collect();
    assert_eq!(
        names,
        ["Active", "Backlog", "All issues", "UI work"],
        "{out}"
    );
    assert!(out.contains("label:ui  group:board"), "{out}");
    assert_eq!(
        list(bin, url, db, &["--view", "ui work", "--group", "none"]),
        [epic.as_str(), elsewhere.as_str()]
    );
    assert_eq!(
        list(
            bin,
            url,
            db,
            &["--view", "ui-work", "--group", "none", "p:high"]
        ),
        [elsewhere.as_str()]
    );
    let out = headway(bin, url, db, &["list", "--view", "UI work"]);
    assert!(
        out.starts_with("## headway (1)"),
        "the view's grouping: {out}"
    );
    // A built-in view, and `--all` over it.
    assert_eq!(
        list(bin, url, db, &["--view", "all", "p:high", "label:ui"]),
        [shipped.as_str(), elsewhere.as_str()]
    );
    assert_eq!(
        list(bin, url, db, &["--view", "backlog", "--board", "other"]),
        [elsewhere.as_str()]
    );
    let out = headway(bin, url, db, &["list", "--delete-view", "UI work"]);
    assert_eq!(out.trim(), "deleted view 'UI work'");
    let out = headway(bin, url, db, &["list", "--views"]);
    assert!(!out.contains("UI work"), "{out}");
}
