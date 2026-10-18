//! End-to-end: `cashflow rule` against a temp data dir and vault cache, with
//! synthetic books and rules only. Each step is its own `cashflow` process,
//! so every read comes back out of the vault's notes.

mod harness;

use std::path::{Path, PathBuf};

use harness::{Cashflow, expect_failure, expect_success};
use serde_json::Value;

/// A synthetic key (32 bytes of 0x11); it never signs anything real.
const NSEC: &str = "nsec1zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygs4rm7hz";

/// Four card purchases and a lightning receipt.
const BOOKS: &str = "\
2024-01-02 * Corner Grocer
    ; id: test:1
    liabilities:card  -45.50 CAD
    expenses:unknown

2024-01-03 * Grocer Airline
    ; id: test:2
    liabilities:card  -300.00 CAD
    expenses:unknown

2024-01-04 * Bakery
    ; id: test:3
    ; raw-city: Montreal
    liabilities:card  -3.00 CAD
    expenses:unknown

2024-01-05 * Night Market
    ; id: test:4
    liabilities:card  -12.00 CAD
    expenses:unknown
";

/// An amex.toml-shaped file: two rules, one with two patterns.
const RULES: &str = "\
card = \"liabilities:card\"
unknown = \"expenses:unknown\"
currency = \"CAD\"

[[rule]]
account = \"expenses:travel\"
match = [\"airline\"]

[[rule]]
account = \"expenses:food\"
match = [\"grocer\", \"market\"]
";

/// A logged-in harness with [`BOOKS`] imported.
fn setup() -> Cashflow {
    let cf = Cashflow::knowing("rule");
    cf.ok(&["login", NSEC]);
    let books = write(&cf, "books.ledger", BOOKS);
    cf.ok(&["import", "ledger", s(&books)]);
    cf
}

fn write(cf: &Cashflow, name: &str, text: &str) -> PathBuf {
    let path = cf.dir().join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// `rule ls --json`: the rules in order.
fn ls(cf: &Cashflow) -> Vec<Value> {
    json(&cf.ok(&["rule", "ls", "--json"]))
        .as_array()
        .unwrap()
        .clone()
}

fn ids(rules: &[Value]) -> Vec<String> {
    rules
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect()
}

/// The account each record's counter leg went to, in `export`'s text.
fn counter_of(export: &str, record: &str) -> String {
    let block = export
        .split("\n\n")
        .find(|b| b.contains(&format!("; id: {record}\n")))
        .unwrap_or_else(|| panic!("no {record} in\n{export}"));
    block
        .lines()
        .last()
        .unwrap()
        .trim()
        .split('\t')
        .next()
        .unwrap()
        .to_owned()
}

/// The migration's check, in small: importing a file into the vault keeps
/// its order, and `export` with the vault's rules equals `export --rules`
/// with the file. A re-run imports nothing.
#[test]
fn import_keeps_order_and_derives_like_the_file() {
    let cf = setup();
    let rules = write(&cf, "amex.toml", RULES);

    let dry = cf.ok(&["rule", "import", s(&rules), "--dry-run"]);
    assert!(dry.contains("would import 2 rules"), "{dry}");
    assert!(dry.contains("would move 3 transactions"), "{dry}");
    assert!(ls(&cf).is_empty(), "a dry run wrote");

    let imported = cf.ok(&["rule", "import", s(&rules)]);
    assert!(imported.contains("imported 2 rules"), "{imported}");
    let listed = ls(&cf);
    let accounts: Vec<&str> = listed
        .iter()
        .map(|r| r["account"].as_str().unwrap())
        .collect();
    assert_eq!(accounts, ["expenses:travel", "expenses:food"]);
    assert_eq!(listed[1]["match"], serde_json::json!(["grocer", "market"]));
    // Grocer Airline is the travel rule's: it comes first.
    let matches: Vec<u64> = listed
        .iter()
        .map(|r| r["matches"].as_u64().unwrap())
        .collect();
    assert_eq!(matches, [1, 2]);

    assert_eq!(cf.ok(&["export"]), cf.ok(&["export", "--rules", s(&rules)]));
    let again = cf.ok(&["rule", "import", s(&rules), "--json"]);
    let again = json(&again);
    assert_eq!(
        (again["imported"].as_u64(), again["skipped"].as_u64()),
        (Some(0), Some(2))
    );
    assert_eq!(ls(&cf).len(), 2);
}

/// `add` previews what it takes, including from an existing rule, and a
/// rule put first takes the rows it shares; `mv` and `rm` each touch one
/// rule; `test` names the rule and the pattern.
#[test]
fn add_mv_rm_and_test() {
    let cf = setup();
    let rules = write(&cf, "amex.toml", RULES);
    cf.ok(&["rule", "import", s(&rules)]);
    let [travel, food] = <[String; 2]>::try_from(ids(&ls(&cf))).unwrap();

    // Bakery: from the default.
    let added = json(&cf.ok(&[
        "rule",
        "add",
        "expenses:bakery",
        "--match",
        "bakery",
        "--meta",
        "raw-city=montreal",
        "--json",
    ]));
    assert_eq!(added["moved"], 1);
    assert_eq!(
        added["moves"],
        serde_json::json!([{"from": "expenses:unknown", "to": "expenses:bakery", "rows": 1}])
    );
    let bakery = added["added"].as_str().unwrap().to_owned();

    // A rule ahead of `food` would take one of its rows: a dry run says so
    // and writes nothing.
    let dry = json(&cf.ok(&[
        "rule",
        "add",
        "expenses:shopping",
        "--match",
        "corner",
        "--before",
        &food,
        "--dry-run",
        "--json",
    ]));
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["taken"], serde_json::json!([{"rule": food, "rows": 1}]));
    assert_eq!(ls(&cf).len(), 3);
    let text = cf.ok(&[
        "rule",
        "add",
        "expenses:shopping",
        "--match",
        "zzz",
        "--dry-run",
    ]);
    assert!(
        text.contains("would move 0 transactions") && text.contains("dry run"),
        "{text}"
    );

    // `food` first: it takes Grocer Airline from `travel`.
    let moved = json(&cf.ok(&["rule", "mv", &food, "--first", "--json"]));
    assert_eq!(
        moved["taken"],
        serde_json::json!([{"rule": travel, "rows": 1}])
    );
    assert_eq!(
        ids(&ls(&cf)),
        [food.clone(), travel.clone(), bakery.clone()]
    );
    assert_eq!(counter_of(&cf.ok(&["export"]), "test:2"), "expenses:food");

    let tested = json(&cf.ok(&["rule", "test", "test:2", "--json"]));
    assert_eq!(tested["decided_by"], "rule");
    assert_eq!(tested["rule"]["id"], food.as_str());
    assert_eq!(tested["pattern"], "grocer");
    let tested = cf.ok(&["rule", "test", "test:4"]);
    assert!(
        tested.contains("matched on payee pattern 'market'"),
        "{tested}"
    );

    // Removing `food` hands its rows back: Grocer Airline to `travel`.
    let removed = json(&cf.ok(&["rule", "rm", &food[..6], "--json"]));
    assert_eq!(removed["moved"], 3);
    assert_eq!(ids(&ls(&cf)), [travel.clone(), bakery]);
    let exported = cf.ok(&["export"]);
    assert_eq!(counter_of(&exported, "test:2"), "expenses:travel");
    assert_eq!(counter_of(&exported, "test:1"), "expenses:unknown");
    let tested = json(&cf.ok(&["rule", "test", "test:1", "--json"]));
    assert_eq!(tested["decided_by"], "default");
    assert_eq!(tested["account"], "expenses:unknown");

    let err = expect_failure(cf.run(&["rule", "rm", &food]));
    assert!(err.contains("no rule"), "{err}");
    let err = expect_failure(cf.run(&["rule", "test", "test:9"]));
    assert!(err.contains("no transaction"), "{err}");
    let err = expect_failure(cf.run(&["rule", "add", "expenses:travel", "--match", "airline"]));
    assert!(
        err.contains(&format!("rule {travel} already does this")),
        "{err}"
    );
    let err = expect_failure(cf.run(&["rule", "add", "expenses:x"]));
    assert!(err.contains("needs a condition"), "{err}");
    let err = expect_failure(cf.run(&["rule", "add", "expenses::x", "--match", "a"]));
    assert!(err.contains("valid account"), "{err}");
}

/// A rule added from an agentium session says so, and `--by` keeps that
/// session's; a malformed `$AGENTIUM_SESSION` is left out.
#[test]
fn rules_record_the_session_that_added_them() {
    let cf = setup();
    let add = |session: &str, pattern: &str| {
        expect_success(
            cf.command()
                .env("AGENTIUM_SESSION", session)
                .args(["rule", "add", "expenses:food", "--match", pattern])
                .output()
                .unwrap(),
        )
    };
    add("agentium:oak-flat-lamp", "grocer");
    add("not a ref", "market");
    let listed = ls(&cf);
    assert_eq!(listed[0]["agentium"], "agentium:oak-flat-lamp");
    assert_eq!(listed[1]["agentium"], Value::Null);
    let by: Vec<Value> = json(&cf.ok(&["rule", "ls", "--by", "agentium:oak-flat-lamp", "--json"]))
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(ids(&by), [ids(&listed)[0].clone()]);
    assert!(cf.ok(&["rule", "ls"]).contains("by agentium:oak-flat-lamp"));
}

/// Without a key, `export` still runs, as imported, and says why.
#[test]
fn export_without_a_key_categorizes_nothing() {
    let cf = Cashflow::knowing("rule");
    let books = write(&cf, "books.ledger", BOOKS);
    cf.ok(&["import", "ledger", s(&books)]);
    let out = cf.run(&["export"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let exported = expect_success(out);
    assert!(stderr.contains("no signing key"), "{stderr}");
    assert_eq!(counter_of(&exported, "test:1"), "expenses:unknown");
}
