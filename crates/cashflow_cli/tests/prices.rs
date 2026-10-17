//! End-to-end: `prices fetch` against a temp `$CASHFLOW_DIR`, limited to
//! what needs no network: the fixed unit relations, and refusing to guess a
//! start day for an empty journal. The fetchers themselves are tested with
//! recorded responses in `cashflow::prices`.

mod harness;

use harness::{Cashflow, expect_failure};
use serde_json::Value;

/// The unit relations are written once, and again is a no-op.
#[test]
fn units_are_written_once() {
    let cf = Cashflow::knowing("prices");
    let first: Value =
        serde_json::from_str(&cf.ok(&["prices", "fetch", "--source", "units", "--json"])).unwrap();
    let segment = &first[0]["segments"][0];
    assert_eq!(first[0]["source"], "units");
    assert_eq!(segment["new"], true);
    assert_eq!(segment["prices"], 2);
    assert_eq!(segment["imported"], 0);

    let again = cf.ok(&["prices", "fetch", "--source", "units"]);
    let hash = segment["hash"].as_str().unwrap();
    assert!(again.starts_with(&format!("unchanged {hash}")), "{again}");

    let exported = cf.ok(&["export"]);
    assert_eq!(
        exported,
        "P 1970-01-01 msat 0.001 sat\nP 1970-01-01 sat 0.00000001 btc\n"
    );
}

/// With no `--since` and nothing in the journal there is no day to resume
/// from: the run fails before any request and writes nothing.
#[test]
fn an_empty_journal_needs_since() {
    let cf = Cashflow::knowing("prices");
    let err = expect_failure(cf.run(&["prices", "fetch"]));
    assert!(err.contains("boc: the journal is empty"), "{err}");
    assert_eq!(cf.ok(&["journal", "ls"]), "");

    let err = expect_failure(cf.run(&["prices", "fetch", "--source", "frob"]));
    assert!(err.contains("sources: units, boc, bitstamp"), "{err}");
}
