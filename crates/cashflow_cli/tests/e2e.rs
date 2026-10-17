//! End-to-end: run the real `cashflow` binary against a temp `$CASHFLOW_DIR`.

mod harness;

use harness::{Cashflow, expect_failure};
use serde_json::Value;

/// A value no output may ever contain: it is planted as a secret.
const PLANTED: &str = "planted-secret-value-7f3a";

/// `cashflow --help` lists the commands and exits 0, on stderr like the other
/// CLIs.
#[test]
fn help_lists_commands() {
    let cf = Cashflow::new();
    let out = cf.run(&["--help"]);
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stderr);
    assert!(help.contains("config"), "{help}");
    assert!(help.contains("--dir"), "{help}");

    let page = cf.run(&["help", "config"]);
    assert!(String::from_utf8_lossy(&page.stderr).contains("config path"));

    let options = cf.run(&["help", "options"]);
    assert!(String::from_utf8_lossy(&options.stderr).contains("SECRETS:"));

    let err = expect_failure(cf.run(&["frob"]));
    assert!(err.contains("unknown command 'frob'"), "{err}");
}

/// `config path` names the layout under `$CASHFLOW_DIR`, one path on its own
/// when named, and creates nothing.
#[test]
fn config_path_names_the_layout() {
    let cf = Cashflow::new();
    let root = cf.dir().join("data");
    let root_str = root.to_str().unwrap();

    let all = cf.ok(&["--dir", root_str, "config", "path"]);
    let lines: Vec<&str> = all.lines().collect();
    assert_eq!(lines.len(), 5, "{all}");
    assert!(lines[0].starts_with("dir") && lines[0].ends_with(root_str));
    assert!(all.contains(&root.join("config.toml").display().to_string()));

    let journal = cf.ok(&["--dir", root_str, "config", "path", "journal"]);
    assert_eq!(
        journal.trim_end(),
        root.join("journal").display().to_string()
    );

    let json: Value =
        serde_json::from_str(&cf.ok(&["--dir", root_str, "config", "path", "--json"])).unwrap();
    assert_eq!(json["secrets"], root.join("secrets").display().to_string());

    let err = expect_failure(cf.run(&["--dir", root_str, "config", "path", "nope"]));
    assert!(err.contains("no path named 'nope'"), "{err}");

    assert!(!root.exists(), "config path created {}", root.display());
}

/// `$CASHFLOW_DIR` picks the dir, and `--dir` beats it.
#[test]
fn dir_flag_overrides_env() {
    let cf = Cashflow::new();
    let from_env = cf.ok(&["config", "path", "dir"]);
    assert_eq!(from_env.trim_end(), cf.dir().display().to_string());

    let other = cf.dir().join("other");
    let from_flag = cf.ok(&["config", "path", "dir", "--dir", other.to_str().unwrap()]);
    assert_eq!(from_flag.trim_end(), other.display().to_string());
}

/// With no `$CASHFLOW_DIR` the dir is `<data-local-dir>/cashflow`, which on
/// Linux follows `$XDG_DATA_HOME`.
#[cfg(target_os = "linux")]
#[test]
fn default_dir_is_under_the_data_dir() {
    let cf = Cashflow::new();
    let out = cf
        .command()
        .env_remove("CASHFLOW_DIR")
        .env("XDG_DATA_HOME", cf.dir())
        .args(["config", "path", "dir"])
        .output()
        .unwrap();
    let dir = harness::expect_success(out);
    assert_eq!(
        dir.trim_end(),
        cf.dir().join("cashflow").display().to_string()
    );
}

/// `config show` with nothing on disk is all defaults and no secrets.
#[test]
fn config_show_defaults() {
    let cf = Cashflow::new();
    let text = cf.ok(&["config", "show"]);
    assert!(text.contains("missing: all defaults"), "{text}");
    assert!(text.contains("(none)"), "{text}");

    let json: Value = serde_json::from_str(&cf.ok(&["config", "show", "--json"])).unwrap();
    assert_eq!(json["config_file_exists"], false);
    assert_eq!(json["config"], serde_json::json!({}));
    assert_eq!(json["secrets"], serde_json::json!([]));
}

/// `config show` lists a secret file and a `CASHFLOW_*` variable by name and
/// source, and never prints either value, in text or JSON.
#[test]
fn config_show_redacts_secrets() {
    let cf = Cashflow::new();
    let secrets = cf.dir().join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    std::fs::write(secrets.join("test-token"), PLANTED).unwrap();

    for args in [&["config", "show"][..], &["config", "show", "--json"]] {
        let out = cf
            .command()
            .env("CASHFLOW_TEST_TOKEN", PLANTED)
            .args(args)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let stdout = harness::expect_success(out);
        assert!(stdout.contains("test-token"), "{stdout}");
        assert!(stdout.contains("CASHFLOW_TEST_TOKEN"), "{stdout}");
        assert!(
            !stdout.contains(PLANTED),
            "secret leaked to stdout:\n{stdout}"
        );
        assert!(
            !stderr.contains(PLANTED),
            "secret leaked to stderr:\n{stderr}"
        );
        assert!(!stdout.contains("CASHFLOW_DIR"), "{stdout}");
    }
}

/// An unknown key in `config.toml` fails `config show`, naming the file, but
/// `config path` still works so the file can be found and fixed.
#[test]
fn config_show_rejects_unknown_keys() {
    let cf = Cashflow::new();
    std::fs::write(cf.dir().join("config.toml"), "not_a_setting = 1\n").unwrap();

    let err = expect_failure(cf.run(&["config", "show"]));
    assert!(err.contains("config.toml"), "{err}");
    assert!(err.contains("not_a_setting"), "{err}");

    cf.ok(&["config", "path", "config"]);
}
