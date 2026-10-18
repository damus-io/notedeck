//! End-to-end: `cashflow categorize`, `uncategorize`, `split`, `transfer`,
//! `edits` and `unknown` against a temp data dir and vault cache, with
//! synthetic books only. Each step is its own `cashflow` process, so every
//! read comes back out of the vault's notes.

mod harness;

use harness::{Cashflow, expect_failure, expect_success};
use serde_json::{Value, json};

/// A synthetic key (32 bytes of 0x11); it never signs anything real.
const NSEC: &str = "nsec1zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygs4rm7hz";

/// Card purchases, a card payment from chequing (both sides), and a
/// lightning receipt.
const BOOKS: &str = "\
2024-01-02 * Corner Grocer #12
    ; id: test:1
    liabilities:card  -45.50 CAD
    expenses:unknown

2024-01-03 * CORNER GROCER #7
    ; id: test:2
    liabilities:card  -100.00 CAD
    expenses:unknown

2024-01-04 * Bakery
    ; id: test:3
    liabilities:card  -3.00 CAD
    expenses:unknown

2024-01-05 * Pay card
    ; id: bank:1
    assets:chequing  -500.00 CAD
    expenses:unknown

2024-01-07 * Payment received
    ; id: test:4
    liabilities:card  500.00 CAD
    income:unknown
";

/// A logged-in harness with [`BOOKS`] imported.
fn setup() -> Cashflow {
    let cf = Cashflow::knowing("unknown");
    cf.ok(&["login", NSEC]);
    let books = cf.dir().join("books.ledger");
    std::fs::write(&books, BOOKS).unwrap();
    cf.ok(&["import", "ledger", books.to_str().unwrap()]);
    cf
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// The postings of `record` in `export`'s text, `account amount` each.
fn legs_of(export: &str, record: &str) -> Vec<String> {
    let block = export
        .split("\n\n")
        .find(|b| b.contains(&format!("; id: {record}\n")))
        .unwrap_or_else(|| panic!("no {record} in\n{export}"));
    block
        .lines()
        .filter(|l| l.starts_with('\t') && !l.trim_start().starts_with(';'))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// `edits --json`.
fn edits(cf: &Cashflow) -> Vec<Value> {
    json(&cf.ok(&["edits", "--json"]))
        .as_array()
        .unwrap()
        .clone()
}

/// An override changes the derive, a split replaces it, `uncategorize`
/// clears it, and a dry run writes nothing.
#[test]
fn categorize_split_and_uncategorize() {
    let cf = setup();

    let dry = json(&cf.ok(&[
        "categorize",
        "test:1",
        "expenses:food",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["d"], "ov:test:1");
    let change = &dry["changes"][0];
    assert_eq!(change["before"]["decided_by"], "default");
    assert_eq!(
        change["before"]["legs"],
        json!([{"account": "expenses:unknown", "amount": "45.50 CAD"}])
    );
    assert_eq!(change["after"]["decided_by"], "override");
    assert_eq!(change["after"]["legs"][0]["account"], "expenses:food");
    assert!(edits(&cf).is_empty(), "a dry run wrote");

    let text = cf.ok(&["categorize", "test:1", "expenses:food"]);
    assert!(text.contains("wrote ov:test:1"), "{text}");
    // Writing it again says what it replaced.
    let again = json(&cf.ok(&["categorize", "test:1", "expenses:food", "--json"]));
    assert_eq!(
        again["replaced"],
        json!({"kind": "override", "d": "ov:test:1", "account": "expenses:food"})
    );
    assert_eq!(
        legs_of(&cf.ok(&["export"]), "test:1"),
        ["liabilities:card -45.50 CAD", "expenses:food"]
    );

    // A split replaces the override: one note written, one cleared.
    let split = json(&cf.ok(&[
        "split",
        "test:1",
        "expenses:food=40",
        "expenses:home",
        "--json",
    ]));
    assert_eq!(split["d"], "split:test:1");
    assert_eq!(split["replaced"]["kind"], "override");
    assert_eq!(split["changes"][0]["before"]["decided_by"], "override");
    assert_eq!(
        split["changes"][0]["after"]["legs"],
        json!([
            {"account": "expenses:food", "amount": "40.00 CAD"},
            {"account": "expenses:home", "amount": "5.50 CAD"},
        ])
    );
    let listed = edits(&cf);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["kind"], "split");
    assert_eq!(
        listed[0]["legs"],
        json!([{"account": "expenses:food", "qty": 4000}, {"account": "expenses:home", "qty": 550}])
    );
    assert_eq!(
        legs_of(&cf.ok(&["export"]), "test:1"),
        [
            "liabilities:card -45.50 CAD",
            "expenses:food 40.00 CAD",
            "expenses:home 5.50 CAD"
        ]
    );

    // Splits are checked against the transaction before anything is stored.
    let err = expect_failure(cf.run(&["split", "test:2", "expenses:food=60"]));
    assert!(
        err.contains("add up to 60.00 CAD") && err.contains("100.00 CAD"),
        "{err}"
    );
    let err = expect_failure(cf.run(&["split", "test:2", "expenses:food=60 USD", "a:b"]));
    assert!(err.contains("in CAD"), "{err}");
    let err = expect_failure(cf.run(&["split", "test:2", "expenses:food=0.001", "a:b"]));
    assert!(err.contains("expenses:food=0.001"), "{err}");
    let err = expect_failure(cf.run(&["split", "test:2", "a", "b"]));
    assert!(err.contains("bare <account>"), "{err}");
    let err = expect_failure(cf.run(&["categorize", "test:9", "expenses:food"]));
    assert!(err.contains("no transaction"), "{err}");
    let err = expect_failure(cf.run(&["categorize", "test:2", "expenses::x"]));
    assert!(err.contains("valid account"), "{err}");

    let cleared = cf.ok(&["uncategorize", "test:1"]);
    assert!(cleared.contains("cleared split:test:1"), "{cleared}");
    assert!(edits(&cf).is_empty());
    assert_eq!(
        legs_of(&cf.ok(&["export"]), "test:1"),
        ["liabilities:card -45.50 CAD", "expenses:unknown"]
    );
    let err = expect_failure(cf.run(&["uncategorize", "test:1"]));
    assert!(err.contains("no override or split"), "{err}");
}

/// `suggest` finds the card payment, `--accept-all --dry-run` writes
/// nothing, `--accept-all` links it, a linked row can't be categorized, and
/// `unlink` in either order clears the one canonical note.
#[test]
fn transfers_suggest_link_and_unlink() {
    let cf = setup();
    let suggested = json(&cf.ok(&["transfer", "suggest", "--json"]));
    let pairs = suggested["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 1, "{suggested}");
    assert_eq!(pairs[0]["d"], "xfer:bank:1|test:4");
    assert_eq!(pairs[0]["days"], 2);
    assert_eq!(pairs[0]["a"]["account"], "assets:chequing");
    assert_eq!(pairs[0]["b"]["amount"], "500.00 CAD");

    let dry = json(&cf.ok(&["transfer", "suggest", "--accept-all", "--dry-run", "--json"]));
    assert_eq!(dry["linked"], 0);
    assert!(edits(&cf).is_empty());
    let err = expect_failure(cf.run(&["transfer", "suggest", "--dry-run"]));
    assert!(err.contains("--accept-all"), "{err}");

    let linked = cf.ok(&["transfer", "suggest", "--accept-all"]);
    assert!(linked.contains("linked 1 transfers"), "{linked}");
    let listed = edits(&cf);
    assert_eq!(
        (listed[0]["kind"].as_str(), listed[0]["d"].as_str()),
        (Some("transfer"), Some("xfer:bank:1|test:4"))
    );
    let exported = cf.ok(&["export"]);
    assert_eq!(legs_of(&exported, "bank:1")[1], "transfers");
    assert_eq!(legs_of(&exported, "test:4")[1], "transfers");
    assert!(
        json(&cf.ok(&["transfer", "suggest", "--json"]))["pairs"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let err = expect_failure(cf.run(&["categorize", "test:4", "income:salary"]));
    assert!(err.contains("transfer unlink test:4 bank:1"), "{err}");
    let err = expect_failure(cf.run(&["transfer", "link", "test:4", "test:1"]));
    assert!(err.contains("linked as a transfer"), "{err}");

    // Unlink names the pair in the other order: the same note.
    let unlinked = json(&cf.ok(&["transfer", "unlink", "test:4", "bank:1", "--json"]));
    assert_eq!(unlinked["d"], "xfer:bank:1|test:4");
    assert_eq!(unlinked["changes"][1]["after"]["decided_by"], "default");
    assert!(edits(&cf).is_empty());

    // An unbalanced pair is refused before it is stored, as is a link to a
    // categorized row.
    let err = expect_failure(cf.run(&["transfer", "link", "test:1", "test:4"]));
    assert!(err.contains("TransferUnbalanced"), "{err}");
    cf.ok(&["categorize", "test:1", "expenses:food"]);
    let err = expect_failure(cf.run(&["transfer", "link", "test:1", "bank:1"]));
    assert!(err.contains("uncategorize test:1"), "{err}");

    let linked = json(&cf.ok(&["transfer", "link", "test:4", "bank:1", "--json"]));
    assert_eq!(linked["d"], "xfer:bank:1|test:4");
}

/// The triage queue groups by payee and default, biggest first, and its
/// suggested rule takes the whole group; an edit takes its row out.
#[test]
fn unknown_groups_and_suggests_rules() {
    let cf = setup();
    let queue = json(&cf.ok(&["unknown", "--json"]));
    assert_eq!(queue["rows"], 5);
    let groups = queue["groups"].as_array().unwrap();
    let summary: Vec<(&str, &str, u64)> = groups
        .iter()
        .map(|g| {
            (
                g["payee"].as_str().unwrap(),
                g["account"].as_str().unwrap(),
                g["rows"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("pay card", "expenses:unknown", 1),
            ("payment received", "income:unknown", 1),
            ("corner grocer", "expenses:unknown", 2),
            ("bakery", "expenses:unknown", 1),
        ]
    );
    let grocer = &groups[2];
    assert_eq!(grocer["total"], "145.50 CAD");
    assert_eq!(grocer["record_ids"], json!(["test:1", "test:2"]));
    assert_eq!(
        grocer["rule_add"],
        json!([
            "cashflow",
            "rule",
            "add",
            "<account>",
            "--match",
            "corner grocer",
            "--source",
            "ledger"
        ])
    );
    let text = cf.ok(&["unknown", "--limit", "1"]);
    assert!(
        text.contains("5 unknown transactions in 4 groups; the 1 biggest:"),
        "{text}"
    );
    assert!(text.contains("--match 'pay card'"), "{text}");

    // Running the suggestion (with an account) empties the group.
    let mut argv: Vec<String> = grocer["rule_add"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|a| a.as_str().unwrap().to_owned())
        .collect();
    argv[2] = "expenses:food".to_owned();
    let added = json(&cf.ok(&[argv, vec!["--json".to_owned()]].concat()));
    assert_eq!(added["moved"], 2);
    cf.ok(&["categorize", "test:3", "expenses:food"]);
    let queue = json(&cf.ok(&["unknown", "--json"]));
    assert_eq!(queue["rows"], 2);

    let none = json(&cf.ok(&["unknown", "--source", "amex", "--json"]));
    assert_eq!(none["rows"], 0);
}

/// An override wins over every rule, so a rule change doesn't count the
/// row it decides as moved.
#[test]
fn rules_leave_edited_rows_alone() {
    let cf = setup();
    cf.ok(&["categorize", "test:1", "expenses:treats"]);
    let added = json(&cf.ok(&[
        "rule",
        "add",
        "expenses:food",
        "--match",
        "grocer",
        "--json",
    ]));
    assert_eq!(added["moved"], 1);
    let listed = json(&cf.ok(&["rule", "ls", "--json"]));
    assert_eq!(listed[0]["matches"], 1);
    let tested = json(&cf.ok(&["rule", "test", "test:1", "--json"]));
    assert_eq!(tested["decided_by"], "override");
}

/// Every edit records the session that wrote it, and `edits --by` keeps
/// that session's.
#[test]
fn edits_record_the_session_that_wrote_them() {
    let cf = setup();
    let run = |session: Option<&str>, args: &[&str]| {
        let mut command = cf.command();
        if let Some(session) = session {
            command.env("AGENTIUM_SESSION", session);
        }
        expect_success(command.args(args).output().unwrap())
    };
    run(
        Some("agentium:oak-flat-lamp"),
        &["categorize", "test:1", "expenses:food"],
    );
    run(None, &["categorize", "test:2", "expenses:food"]);
    run(
        Some("agentium:oak-flat-lamp"),
        &["split", "test:3", "expenses:food=1", "expenses:home"],
    );
    let all = edits(&cf);
    assert_eq!(all.len(), 3);
    let by: Vec<Value> = json(&cf.ok(&["edits", "--by", "agentium:oak-flat-lamp", "--json"]))
        .as_array()
        .unwrap()
        .clone();
    let records: Vec<&str> = by
        .iter()
        .map(|e| e["record_id"].as_str().unwrap())
        .collect();
    assert_eq!(records, ["test:1", "test:3"]);
    let text = cf.ok(&["edits"]);
    assert!(text.contains("by agentium:oak-flat-lamp"), "{text}");
}
