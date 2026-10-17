//! End-to-end: `import`, `journal ls`/`verify`/`rm`, `export` and `report`
//! against a temp `$CASHFLOW_DIR`, with synthetic books only.

mod harness;

use std::path::{Path, PathBuf};

use harness::{Cashflow, expect_failure};
use serde_json::Value;

/// A small synthetic journal: a price, two card purchases, a paycheque in
/// USD.
const BOOKS: &str = "\
P 2024-01-01 USD 1.35 CAD

2024-01-02 * Corner Grocer
    ; id: test:1
    ; raw-merchant: corner grocer
    liabilities:card  -45.50 CAD
    expenses:unknown

2024-01-05 * Airline Co
    ; id: test:2
    liabilities:card  -300.00 CAD
    expenses:unknown

2024-02-01 * Payroll
    ; id: test:3
    assets:bank  1000.00 USD
    income:unknown
";

/// [`BOOKS`] as `export` prints it: ledgerdb's canonical form.
const CANONICAL: &str = "\
P 2024-01-01 USD 1.35 CAD

2024-01-02 * Corner Grocer
\t; id: test:1
\t; raw-merchant: corner grocer
\tliabilities:card\t-45.50 CAD
\texpenses:unknown

2024-01-05 * Airline Co
\t; id: test:2
\tliabilities:card\t-300.00 CAD
\texpenses:unknown

2024-02-01 * Payroll
\t; id: test:3
\tassets:bank\t1000.00 USD
\tincome:unknown
";

const RULES: &str = "\
[[rule]]
account = \"expenses:food\"
match = [\"GROCER\"]

[[rule]]
account = \"income:salary\"
match = [\"payroll\"]
";

/// A harness for a build that has `export`.
fn cashflow() -> Cashflow {
    Cashflow::knowing("export")
}

/// `text` written to `name` beside the harness's data dir.
fn write(cf: &Cashflow, name: &str, text: &str) -> PathBuf {
    let path = cf.dir().join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Importing writes one segment, a second identical import writes nothing,
/// and `journal ls` lists it.
#[test]
fn import_then_ls_lists_one_segment() {
    let cf = cashflow();
    let books = write(&cf, "books.ledger", BOOKS);

    let first: Value = serde_json::from_str(&cf.ok(&[
        "import",
        "ledger",
        s(&books),
        "--source",
        "test",
        "--layer",
        "cards",
        "--json",
    ]))
    .unwrap();
    assert_eq!(first["new"], true);
    assert_eq!(first["txns"], 3);
    assert_eq!(first["prices"], 1);
    let hash = first["hash"].as_str().unwrap().to_owned();

    let again = cf.ok(&[
        "import",
        "ledger",
        s(&books),
        "--source",
        "test",
        "--layer",
        "cards",
    ]);
    assert!(again.starts_with("nothing new"), "{again}");
    assert!(
        again.contains("txns 0 new/0 changed/3 unchanged  prices 0 new/0 changed/1 unchanged"),
        "{again}"
    );

    let ls = cf.ok(&["journal", "ls"]);
    let lines: Vec<&str> = ls.lines().collect();
    assert_eq!(lines.len(), 1, "{ls}");
    assert!(lines[0].starts_with(&hash), "{ls}");
    assert!(lines[0].contains("cards  test  3 txns  1 prices"), "{ls}");

    let json: Value = serde_json::from_str(&cf.ok(&["journal", "ls", "--json"])).unwrap();
    assert_eq!(json[0]["hash"], hash.as_str());
    assert_eq!(json[0]["source"], "test");
    assert_eq!(json[0]["layer"], "cards");

    let file = cf
        .dir()
        .join("journal")
        .join(&hash[..2])
        .join(format!("{hash}.ledger"));
    assert!(file.is_file(), "{}", file.display());
}

/// `verify` passes a clean journal and fails, naming the file, once one is
/// edited.
#[test]
fn verify_catches_an_edited_segment() {
    let cf = cashflow();
    let books = write(&cf, "books.ledger", BOOKS);
    let out = cf.ok(&["journal", "verify"]);
    assert_eq!(out.trim(), "ok: 0 segments");

    let imported: Value =
        serde_json::from_str(&cf.ok(&["import", "ledger", s(&books), "--json"])).unwrap();
    assert_eq!(cf.ok(&["journal", "verify"]).trim(), "ok: 1 segments");

    let hash = imported["hash"].as_str().unwrap();
    let file = cf
        .dir()
        .join("journal")
        .join(&hash[..2])
        .join(format!("{hash}.ledger"));
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace("45.50", "45.51")).unwrap();

    let out = cf.run(&["journal", "verify"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = expect_failure(out);
    assert!(
        stdout.contains(hash) && stdout.contains("corrupt"),
        "{stdout}"
    );
    assert!(stderr.contains("1 of 1 segments failed"), "{stderr}");
}

/// `export` prints the canonical text, and re-importing an export into a
/// fresh journal exports the same bytes again.
#[test]
fn export_round_trips_through_import() {
    let cf = cashflow();
    let books = write(&cf, "books.ledger", BOOKS);
    cf.ok(&["import", "ledger", s(&books)]);
    let exported = cf.ok(&["export"]);
    assert_eq!(exported, CANONICAL);

    let other = cashflow();
    let copy = write(&other, "exported.ledger", &exported);
    other.ok(&["import", "ledger", s(&copy)]);
    assert_eq!(other.ok(&["export"]), exported);
}

/// `--rules` moves the counter legs, layers include or leave out
/// segments, and a range keeps the prices before its end.
#[test]
fn export_applies_rules_layers_and_range() {
    let cf = cashflow();
    let books = write(&cf, "books.ledger", BOOKS);
    let rules = write(&cf, "rules.toml", RULES);
    let extra = write(
        &cf,
        "btc.ledger",
        "2024-01-03 * Wallet\n  ; id: btc:1\n  assets:btc  1 BTC\n  income:unknown\n",
    );
    cf.ok(&["import", "ledger", s(&books), "--layer", "cards"]);
    cf.ok(&["import", "ledger", s(&extra), "--layer", "btc"]);

    let derived = cf.ok(&["export", "--rules", s(&rules), "--except", "btc"]);
    assert_eq!(
        derived,
        CANONICAL
            .replace(
                "-45.50 CAD\n\texpenses:unknown",
                "-45.50 CAD\n\texpenses:food"
            )
            .replace("income:unknown", "income:salary")
    );
    assert!(cf.ok(&["export"]).contains("assets:btc"));
    let only_btc = cf.ok(&["export", "--layer", "btc"]);
    assert!(only_btc.starts_with("2024-01-03 * Wallet"), "{only_btc}");

    let range = cf.ok(&["export", "--begin", "2024-01-03", "--end", "2024-02-01"]);
    assert_eq!(
        range,
        "P 2024-01-01 USD 1.35 CAD\n\n\
         2024-01-03 * Wallet\n\t; id: btc:1\n\tassets:btc\t1 BTC\n\tincome:unknown\n\n\
         2024-01-05 * Airline Co\n\t; id: test:2\n\tliabilities:card\t-300.00 CAD\n\texpenses:unknown\n"
    );

    let err = expect_failure(cf.run(&["export", "--rules", s(&cf.dir().join("missing.toml"))]));
    assert!(err.contains("missing.toml"), "{err}");
}

/// `report bal`, `top` and `reg` over the derived books, in CAD by default.
#[test]
fn reports_read_the_derived_books() {
    let cf = cashflow();
    let books = write(&cf, "books.ledger", BOOKS);
    let rules = write(&cf, "rules.toml", RULES);
    cf.ok(&["import", "ledger", s(&books)]);

    let bal = cf.ok(&["report", "bal", "--rules", s(&rules)]);
    let line = |account: &str| {
        bal.lines()
            .find(|l| l.ends_with(&format!("  {account}")))
            .unwrap_or_else(|| panic!("no {account} in\n{bal}"))
            .trim()
            .to_owned()
    };
    assert_eq!(line("assets:bank"), "1350.00 CAD  assets:bank");
    assert_eq!(line("expenses:food"), "45.50 CAD  expenses:food");
    assert_eq!(line("income:salary"), "-1350.00 CAD  income:salary");

    let native = cf.ok(&["report", "bal", "assets", "--native"]);
    assert_eq!(
        native.lines().map(str::trim).collect::<Vec<_>>(),
        ["1000.00 USD  assets", "1000.00 USD  assets:bank"]
    );
    let before = cf.ok(&["report", "bal", "liabilities", "--end", "2024-01-03"]);
    assert!(before.contains("-45.50 CAD  liabilities:card"), "{before}");

    let top = cf.ok(&["report", "top", "--rules", s(&rules)]);
    let rows: Vec<&str> = top.lines().map(str::trim).collect();
    assert_eq!(
        rows,
        [
            "300.00 CAD   86.8%  expenses:unknown",
            "45.50 CAD   13.2%  expenses:food",
            "345.50 CAD          total",
        ]
    );

    let reg = cf.ok(&["report", "reg", "liabilities", "--payee", "airline"]);
    assert_eq!(
        reg,
        "2024-01-05  Airline Co  liabilities:card  -300.00 CAD\n"
    );

    let err = expect_failure(cf.run(&["report", "bal", "--in", "EUR"]));
    assert!(err.contains("no EUR"), "{err}");
    let err = expect_failure(cf.run(&["report", "reg", "nowhere"]));
    assert!(err.contains("no account named 'nowhere'"), "{err}");
}

/// What `import` refuses, it refuses whole, writing nothing.
#[test]
fn import_refusals_write_nothing() {
    let cf = cashflow();
    let err = expect_failure(cf.run(&["import", "frob"]));
    assert!(err.contains("no importer named 'frob'"), "{err}");
    let err = expect_failure(cf.run(&["import", "ledger"]));
    assert!(err.contains("exactly one <file>"), "{err}");
    assert_eq!(cf.ok(&["journal", "ls"]), "");
}

/// A legacy ledger file with no ids, a raw line before its first
/// transaction and an elided payroll-style posting: the bad line is reported
/// by number, the rest imports with legacy ids, and `journal rm` retires the
/// segment again.
#[test]
fn legacy_import_reports_skips_and_rm_retires_it() {
    let cf = cashflow();
    let legacy = write(
        &cf,
        "legacy.ledger",
        "2024-01-01\tdeadbeef\t5000\n\
         2024-01-02\tundefined\n\trouted:in\t-90 msat\n\tassets:cln\t90 msat\n\n\
         2024-01-02\tundefined\n\trouted:in\t-90 msat\n\tassets:cln\t90 msat\n\n\
         2024-01-15 Payroll\n    assets:bank  1000.00 CAD ; net\n    income:salary\n",
    );

    let out = cf.run(&["import", "ledger", s(&legacy), "--source", "old"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains("3 txns  0 prices"), "{stdout}");
    assert!(stdout.contains("1 skipped"), "{stdout}");
    assert!(
        stderr.contains(&format!("skipped {}:1:", legacy.display())),
        "{stderr}"
    );

    let exported = cf.ok(&["export"]);
    let ids: Vec<&str> = exported
        .lines()
        .filter_map(|line| line.trim().strip_prefix("; id: "))
        .collect();
    assert_eq!(ids.len(), 3, "{exported}");
    assert!(ids.iter().all(|id| id.starts_with("old:")), "{ids:?}");
    assert_eq!(ids[1], format!("{}:2", ids[0]), "identical txns numbered");

    let ls: Value = serde_json::from_str(&cf.ok(&["journal", "ls", "--json"])).unwrap();
    let hash = ls[0]["hash"].as_str().unwrap().to_owned();
    let err = expect_failure(cf.run(&["journal", "rm", "zz"]));
    assert!(err.contains("not a segment hash"), "{err}");
    let removed = cf.ok(&["journal", "rm", &hash[..10]]);
    assert!(removed.starts_with(&format!("removed {hash}")), "{removed}");
    assert!(
        cf.dir()
            .join("journal/.trash")
            .join(format!("{hash}.ledger"))
            .is_file()
    );
    assert_eq!(cf.ok(&["journal", "ls"]), "");
    assert_eq!(cf.ok(&["journal", "verify"]).trim(), "ok: 0 segments");
    let err = expect_failure(cf.run(&["journal", "rm", &hash]));
    assert!(err.contains("no segment"), "{err}");
}
