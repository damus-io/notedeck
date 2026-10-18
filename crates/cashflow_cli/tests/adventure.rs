//! End-to-end: `cashflow quests` and `cashflow achievements` against a temp
//! data dir and vault cache, with synthetic books only.

mod harness;

use harness::{Cashflow, expect_failure};
use serde_json::Value;

/// A synthetic key (32 bytes of 0x11); it never signs anything real.
const NSEC: &str = "nsec1zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygs4rm7hz";

/// Two months of salary, the second beating the first.
const BOOKS: &str = "\
2024-01-05 * Salary
    ; id: test:1
    assets:bank  1000.00 CAD
    income:job

2024-02-05 * Salary
    ; id: test:2
    assets:bank  2000.00 CAD
    income:job
";

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// The books' history unlocks achievements on the days it first met
/// them, and February's beaten net is a completed quest.
#[test]
fn achievements_and_quests_read_the_books() {
    let cf = Cashflow::knowing("achievements");
    cf.ok(&["login", NSEC]);
    let books = cf.dir().join("books.ledger");
    std::fs::write(&books, BOOKS).unwrap();
    cf.ok(&["import", "ledger", books.to_str().unwrap()]);

    let achievements = json(&cf.ok(&["achievements", "--json"]));
    let unlocked = |id: &str| {
        achievements["achievements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .unwrap()["unlocked"]
            .clone()
    };
    assert_eq!(unlocked("income-1k"), "2024-01-31");
    assert_eq!(unlocked("quest-1"), "2024-02-29");
    assert_eq!(unlocked("income-10k"), Value::Null);
    assert!(achievements["points"].as_u64().unwrap() > 0);

    let quests = json(&cf.ok(&["quests", "--json"]));
    assert_eq!(quests["completed"]["count"], 1, "{quests}");
    assert!(
        quests["quests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|q| q["kind"] == "monthly" && q["id"].as_str().unwrap().contains("beat-net")),
        "{quests}"
    );

    let table = cf.ok(&["achievements"]);
    assert!(table.contains("Four Figures"), "{table}");
    assert!(table.contains("points: "), "{table}");
    let err = expect_failure(cf.run(&["quests", "extra"]));
    assert!(err.contains("takes no arguments"), "{err}");
}
