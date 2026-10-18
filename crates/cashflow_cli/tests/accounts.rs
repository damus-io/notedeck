//! End-to-end: `cashflow account ls/set/alias/unalias` and `report
//! projects` against a temp data dir and vault cache, with synthetic books
//! only. Each step is its own `cashflow` process, so every read comes back
//! out of the vault's notes.

mod harness;

use harness::{Cashflow, expect_failure};
use serde_json::{Value, json};

/// A synthetic key (32 bytes of 0x11); it never signs anything real.
const NSEC: &str = "nsec1zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygs4rm7hz";

/// Income for two projects and a job, and some spending.
const BOOKS: &str = "\
2024-01-02 * Zap
    ; id: test:1
    assets:wallet  21.00 CAD
    income:alpha:zaps

2024-01-03 * Subscription
    ; id: test:2
    assets:wallet  9.00 CAD
    income:alpha:subs

2024-01-04 * Tip for a side thing
    ; id: test:3
    assets:wallet  4.00 CAD
    income:alpha:zaps:side

2024-01-05 * Salary
    ; id: test:4
    assets:bank  1000.00 CAD
    income:job

2024-01-06 * Misc
    ; id: test:5
    assets:bank  1.00 CAD
    income:other

2024-01-07 * Grocer
    ; id: test:6
    assets:bank  -30.00 CAD
    expenses:food:produce
";

/// A logged-in harness with [`BOOKS`] imported.
fn setup() -> Cashflow {
    let cf = Cashflow::knowing("account");
    cf.ok(&["login", NSEC]);
    let books = cf.dir().join("books.ledger");
    std::fs::write(&books, BOOKS).unwrap();
    cf.ok(&["import", "ledger", books.to_str().unwrap()]);
    cf
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// `account ls --json`'s row for `account`.
fn row(cf: &Cashflow, args: &[&str], account: &str) -> Value {
    let mut all = vec!["account", "ls", "--json"];
    all.extend_from_slice(args);
    let out = json(&cf.ok(&all));
    out["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account"] == account)
        .cloned()
        .unwrap_or(Value::Null)
}

/// Metadata set on an account is inherited below it, merges on a second
/// `set`, clears with `--clear`, and a hidden subtree leaves `ls`.
#[test]
fn set_inherits_merges_clears_and_hides() {
    let cf = setup();

    let dry = json(&cf.ok(&[
        "account",
        "set",
        "income:alpha",
        "--project",
        "alpha",
        "--icon",
        "🅰",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(dry["d"], "acct:income:alpha");
    assert_eq!(dry["after"]["project"], "alpha");
    assert!(
        row(&cf, &[], "income:alpha")["project"].is_null(),
        "a dry run wrote"
    );

    let text = cf.ok(&[
        "account",
        "set",
        "income:alpha",
        "--project",
        "alpha",
        "--icon",
        "🅰",
    ]);
    assert!(text.contains("set acct:income:alpha"), "{text}");
    cf.ok(&["account", "set", "income:alpha", "--name", "Alpha"]);

    let alpha = row(&cf, &[], "income:alpha");
    assert_eq!(alpha["name"], "Alpha");
    assert_eq!(alpha["icon"], json!({"value": "🅰", "from": "income:alpha"}));
    let zaps = row(&cf, &[], "income:alpha:zaps");
    assert_eq!(
        zaps["project"],
        json!({"value": "alpha", "from": "income:alpha"})
    );
    assert!(zaps["name"].is_null(), "names don't inherit");
    assert_eq!(zaps["balance"], "-25.00 CAD");

    cf.ok(&["account", "set", "income:alpha", "--clear", "icon"]);
    assert!(row(&cf, &[], "income:alpha:zaps")["icon"].is_null());
    assert_eq!(row(&cf, &[], "income:alpha")["name"], "Alpha");

    // An account name with a space is stored behind an escaped id.
    cf.ok(&["account", "set", "expenses:coffee shop", "--icon", "☕"]);
    let ls = json(&cf.ok(&["account", "ls", "--json"]));
    assert_eq!(
        ls["not_in_ledger"][0]["account"], "expenses:coffee shop",
        "{ls}"
    );

    cf.ok(&["account", "set", "income:alpha", "--hide"]);
    assert!(row(&cf, &[], "income:alpha:zaps").is_null());
    assert_eq!(
        row(&cf, &["--all"], "income:alpha:zaps")["hidden"]["value"],
        true
    );
    cf.ok(&["account", "set", "income:alpha:zaps", "--show"]);
    assert!(row(&cf, &[], "income:alpha").is_null());
    assert!(!row(&cf, &[], "income:alpha:zaps").is_null());

    let err = expect_failure(cf.run(&["account", "set", "income:alpha", "--color", "red"]));
    assert!(err.contains("#rrggbb"), "{err}");
    let err = expect_failure(cf.run(&["account", "set", "income:alpha"]));
    assert!(err.contains("needs a field"), "{err}");
}

/// `report projects` sums income by project, a nested project taking its
/// subtree out of the one around it.
#[test]
fn report_projects_groups_by_project() {
    let cf = setup();
    cf.ok(&["account", "set", "income:alpha", "--project", "alpha"]);
    cf.ok(&[
        "account",
        "set",
        "income:alpha:zaps:side",
        "--project",
        "side",
    ]);
    cf.ok(&["account", "set", "income:job", "--project", "job"]);

    let out = cf.ok(&["report", "projects"]);
    let lines: Vec<String> = out
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(
        lines,
        [
            "-1000.00 CAD 96.6% job",
            "-30.00 CAD 2.9% alpha",
            "-4.00 CAD 0.4% side",
            "-1.00 CAD 0.1% (unattributed)",
            "-1035.00 CAD total",
        ],
        "{out}"
    );
}

/// An alias renames a subtree in every reading command, prints what moves
/// before writing, and `unalias` puts it back.
#[test]
fn alias_renames_and_unalias_restores() {
    let cf = setup();

    let dry = json(&cf.ok(&[
        "account",
        "alias",
        "expenses:food",
        "expenses:groceries",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(dry["d"], "alias:expenses:food");
    assert_eq!(
        dry["moves"],
        json!([{"from": "expenses:food:produce", "to": "expenses:groceries:produce", "postings": 1}])
    );
    assert!(cf.ok(&["export"]).contains("expenses:food:produce"));

    let text = cf.ok(&["account", "alias", "expenses:food", "expenses:groceries"]);
    assert!(text.contains("aliased alias:expenses:food"), "{text}");
    let export = cf.ok(&["export"]);
    assert!(export.contains("expenses:groceries:produce"), "{export}");
    assert!(!export.contains("expenses:food"), "{export}");
    let ls = json(&cf.ok(&["account", "ls", "--json"]));
    assert_eq!(
        ls["aliases"],
        json!([{"from": "expenses:food", "to": "expenses:groceries", "agentium": null}])
    );
    // The renamed-away account has no postings left, so `ls` drops it.
    assert!(row(&cf, &[], "expenses:food").is_null());

    let err = expect_failure(cf.run(&["account", "alias", "expenses:food", "expenses:food"]));
    assert!(err.contains("different"), "{err}");

    cf.ok(&["account", "unalias", "expenses:food"]);
    assert!(cf.ok(&["export"]).contains("expenses:food:produce"));
    let err = expect_failure(cf.run(&["account", "unalias", "expenses:food"]));
    assert!(err.contains("no alias"), "{err}");
}
