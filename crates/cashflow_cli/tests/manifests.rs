//! End-to-end: segment manifests in the vault. `import` publishes them,
//! `journal publish` backfills them, `journal status` compares them with the
//! journal, and `journal rm` retires a segment for every copy of the journal.
//!
//! Two machines are two data dirs (`--dir`) with their own `$CASHFLOW_HOST`
//! sharing one vault cache (`--db`), standing in for two caches the relay
//! keeps in step. Synthetic books only.

mod harness;

use std::path::{Path, PathBuf};
use std::process::Output;

use harness::{Cashflow, expect_failure, expect_success};
use serde_json::Value;

/// A synthetic key (32 bytes of 0x11); it never signs anything real.
const NSEC: &str = "nsec1zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygs4rm7hz";

/// One machine: its data dir and host name, sharing the harness's vault.
struct Host<'a> {
    cf: &'a Cashflow,
    name: &'static str,
}

impl Host<'_> {
    fn dir(&self) -> PathBuf {
        self.cf.dir().join(self.name)
    }

    fn run(&self, args: &[&str]) -> Output {
        let dir = self.dir();
        let db = self.cf.dir().join("shared-db");
        self.cf
            .command()
            .env("CASHFLOW_NSEC", NSEC)
            .env("CASHFLOW_HOST", self.name)
            .args(["--dir", dir.to_str().unwrap()])
            .args(["--db", db.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        expect_success(self.run(args))
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.ok(args);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
    }

    /// Import one transaction of record `id`, returning the segment's hash.
    fn import(&self, id: &str, payee: &str) -> String {
        std::fs::create_dir_all(self.dir()).unwrap();
        let file = self.dir().join(format!("{payee}.ledger"));
        std::fs::write(
            &file,
            format!(
                "2024-01-02 * {payee}\n    ; id: {id}\n    liabilities:card  -5.00 CAD\n    expenses:unknown\n"
            ),
        )
        .unwrap();
        let out = self.json(&["import", "ledger", file.to_str().unwrap(), "--json"]);
        out["hash"].as_str().unwrap().to_owned()
    }

    fn file_of(&self, hash: &str) -> PathBuf {
        self.dir()
            .join("journal")
            .join(&hash[..2])
            .join(format!("{hash}.ledger"))
    }
}

fn hashes(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|s| s["hash"].as_str().unwrap())
        .collect()
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::copy(from, to).unwrap();
}

/// Each host's imports show up as missing on the other, grouped under the
/// host that has them, until the file arrives; a retire on one drops the
/// segment from the other's books although its file is still there.
#[test]
fn two_hosts_see_whats_missing_where_and_retirements_propagate() {
    let cf = Cashflow::knowing("journal");
    let alpha = Host {
        cf: &cf,
        name: "alpha",
    };
    let beta = Host {
        cf: &cf,
        name: "beta",
    };

    let a1 = alpha.import("shop:1", "Corner");
    let b1 = beta.import("shop:2", "Bakery");

    let status = alpha.json(&["journal", "status", "--json"]);
    assert_eq!(status["logged_in"], true);
    assert_eq!(status["in_sync"], false);
    assert_eq!(hashes(&status["missing_locally"]), [b1.as_str()]);
    assert_eq!(status["missing_locally"][0]["host"], "beta");
    assert_eq!(status["missing_locally"][0]["first"], "2024-01-02");
    assert_eq!(status["missing_locally"][0]["txns"], 1);
    assert!(hashes(&status["unpublished"]).is_empty());
    assert_eq!(status["verify"]["ok"], 1);

    let text = alpha.ok(&["journal", "status"]);
    assert!(text.contains("missing here: 1 segment"), "{text}");
    assert!(text.contains("  on beta: 1 segment, 1 txns"), "{text}");
    assert!(text.contains(&format!("    {b1}  ")), "{text}");
    assert!(text.contains("verify: 1 ok, 0 damaged, 0 bad"), "{text}");

    // A total says it is partial.
    let report = alpha.run(&["report", "bal"]);
    let stderr = String::from_utf8_lossy(&report.stderr).into_owned();
    expect_success(report);
    assert!(
        stderr.contains("1 segment(s) in the vault aren't in this journal yet"),
        "{stderr}"
    );

    // The file arrives, as a sync would bring it.
    copy(&beta.file_of(&b1), &alpha.file_of(&b1));
    copy(&alpha.file_of(&a1), &beta.file_of(&a1));
    let status = alpha.json(&["journal", "status", "--json"]);
    assert_eq!(status["in_sync"], true, "{status}");
    assert!(
        alpha
            .ok(&["journal", "status"])
            .starts_with("in sync: 2 segments here")
    );
    assert!(beta.ok(&["export"]).contains("Corner"));

    // Alpha retires its import: the file goes to its trash, and beta, which
    // still has a copy, leaves it out anyway.
    let rm = alpha.ok(&["journal", "rm", &a1[..12]]);
    assert!(rm.starts_with(&format!("removed {a1} -> ")), "{rm}");
    assert!(rm.contains(&format!("retired {a1} in the vault")), "{rm}");
    assert!(beta.file_of(&a1).is_file());
    let export = beta.ok(&["export"]);
    assert!(
        !export.contains("Corner") && export.contains("Bakery"),
        "{export}"
    );
    let status = beta.json(&["journal", "status", "--json"]);
    assert_eq!(hashes(&status["retired_here"]), [a1.as_str()]);
    assert!(hashes(&status["missing_locally"]).is_empty());
    let alpha_status = alpha.json(&["journal", "status", "--json"]);
    assert_eq!(alpha_status["in_sync"], true, "{alpha_status}");

    // Beta trashes its copy; the manifest is already retired.
    let rm = beta.ok(&["journal", "rm", &a1]);
    assert!(rm.contains(&format!("{a1} was already retired")), "{rm}");
    assert!(!beta.file_of(&a1).exists());
    assert_eq!(beta.json(&["journal", "status", "--json"])["in_sync"], true);

    // Re-importing the same bytes asks for them back.
    alpha.import("shop:1", "Corner");
    assert!(alpha.ok(&["export"]).contains("Corner"));
    let status = beta.json(&["journal", "status", "--json"]);
    assert_eq!(hashes(&status["missing_locally"]), [a1.as_str()]);
}

/// A segment imported while logged out has no manifest until `journal
/// publish` writes one; a dry run lists it and writes nothing.
#[test]
fn publish_backfills_what_was_imported_logged_out() {
    let cf = Cashflow::knowing("journal");
    let books = cf.dir().join("books.ledger");
    std::fs::write(
        &books,
        "2024-01-02 * Shop\n    ; id: t:1\n    liabilities:card  -5.00 CAD\n    expenses:unknown\n",
    )
    .unwrap();
    let import = cf.run(&["import", "ledger", books.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&import.stderr).into_owned();
    expect_success(import);
    assert!(stderr.contains("`cashflow journal publish`"), "{stderr}");
    let status = cf.ok(&["journal", "status"]);
    assert!(status.starts_with("not logged in"), "{status}");
    let err = expect_failure(cf.run(&["journal", "publish"]));
    assert!(err.contains("cashflow login"), "{err}");

    cf.ok(&["login", NSEC]);
    let status: Value = serde_json::from_str(&cf.ok(&["journal", "status", "--json"])).unwrap();
    let hash = hashes(&status["unpublished"])[0].to_owned();

    let dry: Value =
        serde_json::from_str(&cf.ok(&["journal", "publish", "--dry-run", "--json"])).unwrap();
    assert_eq!(hashes(&dry["published"]), [hash.as_str()]);
    let status: Value = serde_json::from_str(&cf.ok(&["journal", "status", "--json"])).unwrap();
    assert_eq!(hashes(&status["unpublished"]), [hash.as_str()]);

    let out = cf.ok(&["journal", "publish"]);
    assert!(out.contains(&format!("published {hash}")), "{out}");
    assert!(out.contains("published 1 manifest as host "), "{out}");
    let status: Value = serde_json::from_str(&cf.ok(&["journal", "status", "--json"])).unwrap();
    assert_eq!(status["in_sync"], true, "{status}");
    assert!(
        cf.ok(&["journal", "publish"])
            .contains("published 0 manifests")
    );
}
