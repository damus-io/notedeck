//! End-to-end: `login`, `logout` and the vault's `settings`, each run a
//! separate `cashflow` process against one temp data dir and cache.

mod harness;

use std::io::Write;
use std::process::Stdio;
use std::time::Duration;

use harness::{Cashflow, expect_failure, expect_success};
use nostrdb::{Config, Filter, Ndb, Transaction};
use nostrdb_net::sns::SNS_ENVELOPE_KIND;
use serde_json::Value;

/// A synthetic key (32 bytes of 0x11); it never signs anything real.
const NSEC: &str = "nsec1zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygs4rm7hz";

/// A harness that knows `settings`, logged in with [`NSEC`].
fn logged_in() -> Cashflow {
    let cf = Cashflow::knowing("settings");
    cf.ok(&["login", NSEC]);
    cf
}

/// `login` stores the key as the secret `nsec`, which `config show` lists by
/// name only; `logout` removes it, and a second `logout` says so.
#[test]
fn login_stores_the_key_and_logout_forgets_it() {
    let cf = Cashflow::knowing("login");
    let stored = cf.ok(&["login", NSEC]);
    assert!(stored.contains("stored signing key for"), "{stored}");
    assert!(!stored.contains(NSEC), "login echoed the key: {stored}");
    let path = cf.dir().join("secrets").join("nsec");
    assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), NSEC);

    let shown = cf.ok(&["config", "show"]);
    assert!(shown.contains("nsec"), "{shown}");
    assert!(!shown.contains(NSEC), "config show leaked the key: {shown}");

    assert!(cf.ok(&["logout"]).contains("removed stored signing key"));
    assert!(!path.exists());
    assert!(cf.ok(&["logout"]).contains("no stored signing key"));

    let err = expect_failure(cf.run(&["login", "nsec1notakey"]));
    assert!(err.contains("nsec"), "{err}");
    assert!(!path.exists(), "a bad key was stored");
}

/// With no argument, `login` reads the key from stdin.
#[test]
fn login_reads_stdin() {
    let cf = Cashflow::knowing("login");
    let mut child = cf
        .command()
        .arg("login")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{NSEC}").unwrap();
    expect_success(child.wait_with_output().unwrap());
    assert_eq!(
        std::fs::read_to_string(cf.dir().join("secrets/nsec"))
            .unwrap()
            .trim(),
        NSEC
    );
}

/// A vault command with no key says how to get one.
#[test]
fn settings_need_a_key() {
    let cf = Cashflow::knowing("settings");
    let err = expect_failure(cf.run(&["settings"]));
    assert!(err.contains("cashflow login"), "{err}");
}

/// The card's loop: `settings set home` in one process, read back in the
/// next, both sharing the default cache dir. A fresh `--db` has never seen
/// the write, so the value came from the cache, not from anywhere global.
#[test]
fn settings_set_then_read_back_in_another_process() {
    let cf = logged_in();
    assert_eq!(cf.ok(&["settings", "get", "home"]).trim(), "CAD");
    assert!(cf.ok(&["settings"]).contains("(default)"));

    let set = cf.ok(&["settings", "set", "home", "USD"]);
    assert!(set.contains("home = USD"), "{set}");
    assert!(set.contains("offline"), "{set}");

    assert_eq!(cf.ok(&["settings", "get", "home"]).trim(), "USD");
    let json: Value = serde_json::from_str(&cf.ok(&["settings", "show", "--json"])).unwrap();
    assert_eq!(
        json,
        serde_json::json!([
            {"name": "home", "value": "USD", "default": false},
            {"name": "timezone", "value": "local", "default": true},
        ])
    );

    // A later set wins, even within the same second.
    cf.ok(&["settings", "set", "home", "CAD"]);
    assert_eq!(cf.ok(&["settings", "get", "home"]).trim(), "CAD");
    cf.ok(&["settings", "set", "timezone", "America/Vancouver"]);
    assert!(!cf.ok(&["settings"]).contains("(default)"));

    let elsewhere = cf.dir().join("other-db");
    let fresh = cf.ok(&["--db", elsewhere.to_str().unwrap(), "settings"]);
    assert!(fresh.contains("(default)"), "{fresh}");
}

/// `boards:<project>` links a project to Headway boards: `show` lists it,
/// `get` prints its slugs, and an empty value unlinks it.
#[test]
fn settings_link_a_project_to_boards() {
    let cf = logged_in();
    assert_eq!(cf.ok(&["settings", "get", "boards:damus"]).trim(), "");
    let set = cf.ok(&[
        "settings",
        "set",
        "boards:damus",
        "damus-ios, damus-android",
    ]);
    assert!(
        set.contains("boards:damus = damus-ios damus-android"),
        "{set}"
    );
    assert_eq!(
        cf.ok(&["settings", "get", "boards:damus"]).trim(),
        "damus-ios damus-android"
    );
    assert!(cf.ok(&["settings"]).contains("boards:damus"));
    let json: Value = serde_json::from_str(&cf.ok(&["settings", "show", "--json"])).unwrap();
    assert_eq!(
        json[2],
        serde_json::json!({"name": "boards:damus", "value": "damus-ios damus-android", "default": false})
    );

    let unlinked = cf.ok(&["settings", "set", "boards:damus", ""]);
    assert!(unlinked.contains("(unlinked)"), "{unlinked}");
    assert!(!cf.ok(&["settings"]).contains("boards:damus"));
    let err = expect_failure(cf.run(&["settings", "set", "boards:two words", "x"]));
    assert!(err.contains("project"), "{err}");
}

/// A bad setting name or value fails, naming what's wrong, and stores
/// nothing.
#[test]
fn settings_reject_bad_names_and_values() {
    let cf = logged_in();
    let err = expect_failure(cf.run(&["settings", "set", "nope", "x"]));
    assert!(err.contains("no setting named 'nope'"), "{err}");
    assert!(err.contains("home"), "{err}");

    let err = expect_failure(cf.run(&["settings", "set", "home", "12"]));
    assert!(err.contains("commodity"), "{err}");
    assert_eq!(cf.ok(&["settings", "get", "home"]).trim(), "CAD");

    let err = expect_failure(cf.run(&["settings", "set", "timezone", "local"]));
    assert!(err.contains("America/Vancouver"), "{err}");
    assert_eq!(cf.ok(&["settings", "get", "timezone"]).trim(), "local");
}

/// The kind settings are stored under (`cashflow::kinds::SETTING`).
const SETTING_KIND: u64 = 30700;

/// Two machines meet through a relay: one sets `home` with its own cache,
/// the other, with a fresh cache, reconciles and reads it. The relay holds
/// only the sealed envelope, never the setting itself.
#[test]
fn settings_travel_through_a_relay_sealed() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let relay_dir = tempfile::tempdir().unwrap();
    let relay_ndb = Ndb::new(relay_dir.path().to_str().unwrap(), &relay_config()).unwrap();
    // Subscribe before anything is published, so the wait below can't miss it.
    let envelopes = relay_ndb
        .subscribe(&[Filter::new().kinds([SNS_ENVELOPE_KIND as u64]).build()])
        .unwrap();
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(relay_ndb.clone(), "127.0.0.1:0".parse().unwrap())
            .unwrap();
    let url = relay.url();

    let cf = logged_in();
    let laptop = cf.dir().join("laptop-db");
    let desktop = cf.dir().join("desktop-db");
    let on = |db: &std::path::Path, args: &[&str]| {
        let mut full = vec!["--relay", url.as_str(), "--db", db.to_str().unwrap()];
        full.extend_from_slice(args);
        cf.ok(&full)
    };

    let set = on(&laptop, &["settings", "set", "home", "USD"]);
    assert!(!set.contains("offline"), "{set}");
    rt.block_on(relay_ndb.wait_for_all_notes_within(envelopes, 1, Duration::from_secs(30)))
        .unwrap();

    {
        let txn = Transaction::new(&relay_ndb).unwrap();
        let plain = relay_ndb
            .query(&txn, &[Filter::new().kinds([SETTING_KIND]).build()], 10)
            .unwrap();
        assert!(plain.is_empty(), "the relay can read the setting");
    }

    assert_eq!(on(&desktop, &["settings", "get", "home"]).trim(), "USD");
}

/// A [`Config`] with a small mapsize on Windows, where LMDB allocates the
/// whole map on disk.
fn relay_config() -> Config {
    if cfg!(target_os = "windows") {
        Config::new().set_mapsize(32 * 1024 * 1024)
    } else {
        Config::new()
    }
}
