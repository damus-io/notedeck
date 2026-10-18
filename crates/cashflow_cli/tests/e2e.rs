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

/// `config set` writes keys `config show` then reads back, in text and JSON,
/// and `config unset` takes them out again, table and all.
#[test]
fn config_set_then_show_round_trips() {
    let cf = Cashflow::knowing("secret");
    let node = "03f3c1aa";
    let out = cf.ok(&["config", "set", "cln.node_id", node]);
    assert!(out.contains("set cln.node_id = \"03f3c1aa\""), "{out}");
    cf.ok(&["config", "set", "cln.timezone", "-07:00"]);
    cf.ok(&["config", "set", "btc.wallets", r#"["hot","cold"]"#]);
    cf.ok(&["config", "set", "rbc.accounts.1234", "assets:rbc:chequing"]);
    cf.ok(&["config", "set", "goals.auto_done", "false"]);

    let json: Value = serde_json::from_str(&cf.ok(&["config", "show", "--json"])).unwrap();
    let config = &json["config"];
    assert_eq!(config["cln"]["node_id"], node);
    assert_eq!(config["cln"]["timezone"], "-07:00");
    assert_eq!(config["btc"]["wallets"], serde_json::json!(["hot", "cold"]));
    assert_eq!(config["rbc"]["accounts"]["1234"], "assets:rbc:chequing");
    assert_eq!(config["goals"]["auto_done"], false);

    let text = cf.ok(&["config", "show"]);
    assert!(text.contains("node_id = \"03f3c1aa\""), "{text}");

    cf.ok(&["config", "unset", "rbc.accounts.1234"]);
    let out = cf.ok(&["config", "unset", "rbc.accounts.1234"]);
    assert!(out.contains("is not set"), "{out}");
    let file = std::fs::read_to_string(cf.dir().join("config.toml")).unwrap();
    assert!(!file.contains("rbc"), "{file}");
    let json: Value = serde_json::from_str(&cf.ok(&["config", "show", "--json"])).unwrap();
    assert!(json["config"].get("rbc").is_none(), "{json}");
}

/// A key `Config` doesn't know, or a value of the wrong type, fails and
/// leaves `config.toml` byte-identical.
#[test]
fn config_set_unknown_key_changes_nothing() {
    let cf = Cashflow::knowing("secret");
    let path = cf.dir().join("config.toml");
    let original = "# hand-written\n[cln]\nhost = \"a:9735\"\n";
    std::fs::write(&path, original).unwrap();

    let err = expect_failure(cf.run(&["config", "set", "cln.nod_id", "03ab"]));
    assert!(err.contains("nod_id"), "{err}");
    assert!(err.contains("unchanged"), "{err}");
    let err = expect_failure(cf.run(&["config", "set", "frob.x", "1"]));
    assert!(err.contains("frob"), "{err}");
    let err = expect_failure(cf.run(&["config", "set", "goals.auto_done", "yes"]));
    assert!(err.contains("auto_done"), "{err}");
    let err = expect_failure(cf.run(&["config", "set", "cln", "x"]));
    assert!(err.contains("<table>.<key>"), "{err}");

    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let names: Vec<_> = std::fs::read_dir(cf.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["config.toml"], "a temp file was left behind");
}

/// A comment in `config.toml`, before a table or after a value, survives a
/// set, and so does the order of the keys.
#[test]
fn config_set_keeps_comments() {
    let cf = Cashflow::knowing("secret");
    let path = cf.dir().join("config.toml");
    std::fs::write(
        &path,
        "# my lightning node\n[cln]\nhost = \"a:9735\" # the vps\ntimezone = \"utc\"\n\n# cards\n[amex]\ntitle_case = true\n",
    )
    .unwrap();

    cf.ok(&["config", "set", "cln.host", "b:9735"]);
    cf.ok(&["config", "set", "cln.node_id", "03ab"]);
    let file = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        file,
        "# my lightning node\n[cln]\nhost = \"b:9735\" # the vps\ntimezone = \"utc\"\nnode_id = \"03ab\"\n\n# cards\n[amex]\ntitle_case = true\n"
    );
}

/// `secret set` reads the value from stdin into `secrets/<name>`, 0600, and
/// `config show` names it without printing it; `secret rm` removes it.
#[test]
fn secret_set_from_stdin() {
    let cf = Cashflow::knowing("secret");
    let out = cf.run_with_stdin(&["secret", "set", "cln-rune"], &format!("{PLANTED}\n"));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let stdout = harness::expect_success(out);
    assert!(stdout.contains("stored cln-rune"), "{stdout}");
    assert!(
        !stdout.contains(PLANTED) && !stderr.contains(PLANTED),
        "{stdout}{stderr}"
    );

    let path = cf.dir().join("secrets").join("cln-rune");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{PLANTED}\n")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    for args in [&["config", "show"][..], &["config", "show", "--json"]] {
        let shown = cf.ok(args);
        assert!(shown.contains("cln-rune"), "{shown}");
        assert!(!shown.contains(PLANTED), "secret leaked:\n{shown}");
    }

    let err = expect_failure(cf.run_with_stdin(&["secret", "set", "cln-rune"], ""));
    assert!(err.contains("nothing stored"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{PLANTED}\n")
    );
    let err = expect_failure(cf.run_with_stdin(&["secret", "set", "../config.toml"], "x"));
    assert!(err.contains("not a secret name"), "{err}");

    let out = cf.ok(&["secret", "rm", "cln-rune"]);
    assert!(out.contains("removed cln-rune"), "{out}");
    assert!(!path.exists());
    let out = cf.ok(&["secret", "rm", "cln-rune"]);
    assert!(out.contains("no stored secret"), "{out}");
}
