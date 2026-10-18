//! End-to-end: `cashflow sync` over the rsync transport. Two machines are
//! two data dirs (`--dir`) with their own `$CASHFLOW_HOST`, sharing one
//! vault cache (`--db`) as the relay would keep them, and a hub directory
//! on localhost as the remote both list in `config.toml`. Synthetic books
//! only; skipped when `rsync` isn't on PATH.

mod harness;

use std::path::PathBuf;
use std::process::{Command, Output};

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

    fn json(&self, args: &[&str]) -> Value {
        let out = expect_success(self.run(args));
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
    }

    /// Point `config.toml` at `body`.
    fn configure(&self, body: &str) {
        std::fs::create_dir_all(self.dir()).unwrap();
        std::fs::write(self.dir().join("config.toml"), body).unwrap();
    }

    /// Import one transaction of record `id`, returning the segment's hash.
    fn import(&self, id: &str, payee: &str) -> String {
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

    fn has(&self, hash: &str) -> bool {
        segment_path(self.dir().join("journal"), hash).is_file()
    }
}

/// Where segment `hash` sits in the journal at `dir`.
fn segment_path(dir: PathBuf, hash: &str) -> PathBuf {
    dir.join(&hash[..2]).join(format!("{hash}.ledger"))
}

/// The hashes in a JSON list of strings, sorted.
fn sorted(list: &Value) -> Vec<String> {
    let mut hashes: Vec<String> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.as_str().unwrap().to_owned())
        .collect();
    hashes.sort();
    hashes
}

fn have_rsync() -> bool {
    Command::new("rsync")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Alpha pushes to the hub, beta fetches what its manifests say it lacks
/// and pushes its own, alpha fetches that back, and both end in sync. A
/// retired segment is never fetched, a dry run moves nothing, and a file
/// corrupted on the hub is rejected without reaching beta's journal.
#[test]
fn two_hosts_sync_through_an_rsync_hub() {
    if !have_rsync() {
        eprintln!("skipping: rsync is not on PATH");
        return;
    }
    let cf = Cashflow::knowing("sync");
    let hub = cf.dir().join("hub");
    let config = format!(
        "[[sync.transport]]\nname = \"hub\"\nkind = \"rsync\"\nremote = \"{}\"\n",
        hub.display()
    );
    let alpha = Host {
        cf: &cf,
        name: "alpha",
    };
    let beta = Host {
        cf: &cf,
        name: "beta",
    };
    alpha.configure(&config);
    beta.configure(&config);

    let a1 = alpha.import("shop:1", "Corner");
    let a2 = alpha.import("shop:2", "Kiosk");
    let b1 = beta.import("shop:3", "Bakery");

    // Alpha: the hub has nothing, so b1 stays missing; a1 and a2 go up.
    let out = alpha.json(&["sync", "--json"]);
    let hub_report = &out["transports"][0];
    assert_eq!(hub_report["name"], "hub");
    assert_eq!(sorted(&hub_report["pushed"]), {
        let mut v = vec![a1.clone(), a2.clone()];
        v.sort();
        v
    });
    assert_eq!(sorted(&out["still_missing"]), std::slice::from_ref(&b1));
    assert!(segment_path(hub.clone(), &a1).is_file());

    // Alpha retires a2 everywhere before beta syncs.
    expect_success(alpha.run(&["journal", "rm", &a2]));

    // A dry run says what would move and moves nothing.
    let dry = beta.json(&["sync", "--dry-run", "--json"]);
    assert_eq!(dry["dry_run"], true);
    assert_eq!(
        sorted(&dry["transports"][0]["fetched"]),
        std::slice::from_ref(&a1)
    );
    assert!(!beta.has(&a1));
    assert!(!segment_path(hub.clone(), &b1).exists());

    // Beta: a1 arrives (a2 is retired, so never fetched), b1 goes up.
    let text = expect_success(beta.run(&["sync"]));
    assert!(text.contains("fetched 1 segment, admitted 1"), "{text}");
    assert!(text.contains(&format!("    admitted {a1}")), "{text}");
    assert!(text.contains(&format!("    pushed {b1}")), "{text}");
    assert!(beta.has(&a1));
    assert!(!beta.has(&a2));

    // Alpha fetches b1 back; both are now in sync.
    let out = alpha.json(&["sync", "--json"]);
    assert_eq!(
        sorted(&out["transports"][0]["admitted"]),
        std::slice::from_ref(&b1)
    );
    assert!(sorted(&out["still_missing"]).is_empty());
    for host in [&alpha, &beta] {
        let status = host.json(&["journal", "status", "--json"]);
        assert_eq!(status["in_sync"], true, "{}: {status}", host.name);
    }

    // A file corrupted on the hub never reaches beta's journal.
    let a3 = alpha.import("shop:4", "Market");
    alpha.json(&["sync", "--json"]);
    std::fs::write(segment_path(hub.clone(), &a3), "; not the segment\n").unwrap();
    let out = beta.run(&["sync", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = expect_failure(out);
    assert!(err.contains("served a bad file"), "{err}");
    let report: Value = serde_json::from_str(&stdout).unwrap();
    let rejected = report["transports"][0]["rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 1, "{report}");
    assert!(
        rejected[0].as_str().unwrap().contains("hash to"),
        "{report}"
    );
    assert_eq!(sorted(&report["still_missing"]), std::slice::from_ref(&a3));
    assert!(!beta.has(&a3));
    assert!(
        segment_path(beta.dir().join("staging").join("hub"), &a3).is_file(),
        "the rejected file waits in staging"
    );
}

/// A config that can't be synced is an error before any relay is dialled:
/// no transports, an unknown kind (naming the known ones), and a
/// `--transport` nothing is called.
#[test]
fn sync_config_errors() {
    let cf = Cashflow::knowing("sync");
    let host = Host {
        cf: &cf,
        name: "alpha",
    };
    host.configure("");
    let err = expect_failure(host.run(&["sync"]));
    assert!(err.contains("no [[sync.transport]]"), "{err}");

    host.configure("[[sync.transport]]\nkind = \"blossom\"\nremote = \"x\"\n");
    let err = expect_failure(host.run(&["sync"]));
    assert!(
        err.contains("unknown kind \"blossom\" (known kinds: rsync)"),
        "{err}"
    );

    host.configure("[[sync.transport]]\nkind = \"rsync\"\nremote = \"/x\"\n");
    let err = expect_failure(host.run(&["sync", "--transport", "usb"]));
    assert!(
        err.contains("no transport named \"usb\" (configured: rsync)"),
        "{err}"
    );
}
