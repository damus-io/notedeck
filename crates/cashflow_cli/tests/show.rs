//! End-to-end: `cashflow show <record-id>` against a temp data dir and
//! vault cache, with synthetic books only.

mod harness;

use harness::{Cashflow, expect_failure, expect_success};
use serde_json::{Value, json};

/// A synthetic key (32 bytes of 0x11); it never signs anything real.
const NSEC: &str = "nsec1zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygs4rm7hz";

/// A grocer with an importer's raw metadata, a USD charge with no rate, and
/// a bakery a rule takes.
const BOOKS: &str = "\
2024-01-02 * Corner Grocer #12
    ; id: test:1
    ; raw-merchant: corner grocer
    ; raw-city: toronto
    liabilities:card  -45.50 CAD
    expenses:unknown

2024-01-03 * US Store
    ; id: test:2
    liabilities:card  -10.00 USD
    expenses:unknown

2024-01-04 * Bakery
    ; id: test:3
    liabilities:card  -3.00 CAD
    expenses:unknown
";

/// A logged-in harness with [`BOOKS`] imported.
fn setup() -> Cashflow {
    let cf = Cashflow::knowing("show");
    cf.ok(&["login", NSEC]);
    let books = cf.dir().join("books.ledger");
    std::fs::write(&books, BOOKS).unwrap();
    cf.ok(&["import", "ledger", books.to_str().unwrap()]);
    cf
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// An unknown row shows its legs, raw metadata and default; an override
/// written from a session shows its account and that session; a rule shows
/// its id and patterns.
#[test]
fn show_prints_the_facts_and_the_why() {
    let cf = setup();

    let unknown = json(&cf.ok(&["show", "test:1", "--json"]));
    assert_eq!(unknown["record_id"], "test:1");
    assert_eq!(unknown["date"], "2024-01-02");
    assert_eq!(unknown["payee"], "Corner Grocer #12");
    assert_eq!(unknown["source"], "ledger");
    assert_eq!(
        unknown["postings"][0],
        json!({"account": "liabilities:card", "amount": "-45.50 CAD", "qty": -4550,
               "home": "-45.50 CAD", "own": true, "project": null})
    );
    assert_eq!(
        unknown["meta"],
        json!([{"key": "raw-merchant", "value": "corner grocer"},
               {"key": "raw-city", "value": "toronto"}])
    );
    assert_eq!(
        unknown["decided"],
        json!({"by": "default", "account": "expenses:unknown"})
    );
    let usd = json(&cf.ok(&["show", "test:2", "--json"]));
    assert_eq!(usd["postings"][0]["home"], Value::Null, "no USD rate");

    let mut categorize = cf.command();
    categorize
        .args(["categorize", "test:1", "expenses:food"])
        .env("AGENTIUM_SESSION", "agentium:oak-flat-lamp");
    expect_success(categorize.output().unwrap());
    cf.ok(&["rule", "add", "expenses:treats", "--match", "bakery"]);

    let over = json(&cf.ok(&["show", "test:1", "--json"]));
    assert_eq!(over["decided"]["by"], "override");
    assert_eq!(over["decided"]["account"], "expenses:food");
    assert_eq!(
        over["decided"]["written"]["agentium"],
        "agentium:oak-flat-lamp"
    );
    assert!(over["decided"]["written"]["at"].as_u64().unwrap() > 0);

    let ruled = json(&cf.ok(&["show", "test:3", "--json"]));
    assert_eq!(ruled["decided"]["by"], "rule");
    assert_eq!(ruled["decided"]["account"], "expenses:treats");
    assert_eq!(ruled["decided"]["patterns"], json!(["bakery"]));
    assert!(ruled["decided"]["rule_id"].is_string());
    assert_eq!(ruled["decided"]["written"]["agentium"], Value::Null);

    let text = cf.ok(&["show", "test:1"]);
    for want in [
        "2024-01-02 * Corner Grocer #12",
        "id       test:1",
        "expenses:food",
        "why: override to expenses:food",
        "by agentium:oak-flat-lamp",
        "raw-merchant: corner grocer",
    ] {
        assert!(text.contains(want), "no {want:?} in\n{text}");
    }
    let text = cf.ok(&["show", "test:3"]);
    assert!(text.contains("matching \"bakery\""), "{text}");
}

/// A record the books don't hold is an error that says so.
#[test]
fn an_unknown_record_fails() {
    let cf = setup();
    let err = expect_failure(cf.run(&["show", "test:99"]));
    assert!(err.contains("no transaction with id 'test:99'"), "{err}");
    let err = expect_failure(cf.run(&["show"]));
    assert!(err.contains("`show` takes <record-id>"), "{err}");
}
