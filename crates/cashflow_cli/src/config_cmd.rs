//! `cashflow config path|show|set|unset` and `cashflow secret set|rm`: where
//! the data lives, the config a run reads, and the edits to it. `path` and
//! `show` create or change nothing.

use std::io::{IsTerminal, Read};

use cashflow::{CashflowDir, Config, SecretEntry, SecretSource, config::secret_env_var};
use serde_json::{Map, Value, json};

/// `config path [<name>]`: every path of the layout, or the one named.
pub fn path(dir: &CashflowDir, name: Option<&str>, json: bool) -> Result<(), String> {
    let paths = dir.paths();
    let Some(name) = name else {
        if json {
            let object: Map<String, Value> = paths
                .iter()
                .map(|p| (p.name.to_owned(), path_value(&p.path)))
                .collect();
            print_json(&Value::Object(object));
        } else {
            for p in &paths {
                println!("{:<8} {}", p.name, p.path.display());
            }
        }
        return Ok(());
    };

    let Some(found) = paths.iter().find(|p| p.name == name) else {
        let names: Vec<_> = paths.iter().map(|p| p.name).collect();
        return Err(format!(
            "no path named '{name}' (one of: {})",
            names.join(", ")
        ));
    };
    if json {
        print_json(&json!({ found.name: path_value(&found.path) }));
    } else {
        println!("{}", found.path.display());
    }
    Ok(())
}

/// `config show`: the effective `config.toml` and the secrets this run can
/// see, by name and source only.
pub fn show(dir: &CashflowDir, json: bool) -> Result<(), String> {
    let config = Config::load(dir).map_err(|e| e.to_string())?;
    let secrets = dir
        .secret_entries(std::env::vars_os())
        .map_err(|e| e.to_string())?;
    let config_file = dir.config_file();
    let exists = config_file.is_file();

    if json {
        let secrets: Vec<Value> = secrets.iter().map(secret_value).collect();
        print_json(&json!({
            "dir": path_value(dir.root()),
            "config_file": path_value(&config_file),
            "config_file_exists": exists,
            "config": serde_json::to_value(&config).map_err(|e| e.to_string())?,
            "secrets": secrets,
        }));
        return Ok(());
    }

    println!("dir      {}", dir.root().display());
    let missing = if exists {
        ""
    } else {
        " (missing: all defaults)"
    };
    println!("config   {}{missing}", config_file.display());

    println!("\nconfig.toml:");
    let toml = config.to_toml();
    if toml.trim().is_empty() {
        println!("    (all defaults)");
    }
    for line in toml.lines() {
        println!("    {line}");
    }

    println!("\nsecrets (values never printed):");
    if secrets.is_empty() {
        println!("    (none)");
    }
    for secret in &secrets {
        match &secret.source {
            SecretSource::File(path) => {
                println!("    {:<24} file {}", secret.name, path.display())
            }
            SecretSource::Env => println!("    {:<24} env", secret.name),
        }
    }
    Ok(())
}

/// `config set <table>.<key> <value>`: write one key of `config.toml`,
/// keeping its comments, once the edited file is known to load.
pub fn set(dir: &CashflowDir, key: &str, value: &str) -> Result<(), String> {
    let set = dir.set_config(key, value).map_err(|e| e.to_string())?;
    println!("set {key} = {} in {}", set.value, set.path.display());
    Ok(())
}

/// `config unset <table>.<key>`: remove one key of `config.toml`, and its
/// table if that leaves it empty.
pub fn unset(dir: &CashflowDir, key: &str) -> Result<(), String> {
    match dir.unset_config(key).map_err(|e| e.to_string())? {
        Some(path) => println!("unset {key} in {}", path.display()),
        None => println!("{key} is not set"),
    }
    Ok(())
}

/// `secret set <name>`: store stdin as `secrets/<name>`, owner-only. From a
/// terminal one line is read; from a pipe, all of it, so a multi-line key
/// file can be piped in. The value is never printed.
pub fn secret_set(dir: &CashflowDir, name: &str) -> Result<(), String> {
    let mut stdin = std::io::stdin();
    let mut value = String::new();
    let read = if stdin.is_terminal() {
        eprint!("{name}: ");
        stdin.read_line(&mut value).map(|_| ())
    } else {
        stdin.read_to_string(&mut value).map(|_| ())
    };
    read.map_err(|e| format!("reading {name} from stdin: {e}"))?;
    let value = value.trim_end_matches(['\n', '\r']);
    if value.is_empty() {
        return Err(format!("no value for {name} on stdin; nothing stored"));
    }

    let path = dir.write_secret(name, value).map_err(|e| e.to_string())?;
    println!("stored {name} in {}", path.display());
    let var = secret_env_var(name);
    if std::env::var_os(&var).is_some() {
        eprintln!("cashflow: note: ${var} is set and still wins over the file");
    }
    Ok(())
}

/// `secret rm <name>`: delete `secrets/<name>`. A `$CASHFLOW_<NAME>` variable
/// is untouched.
pub fn secret_rm(dir: &CashflowDir, name: &str) -> Result<(), String> {
    match dir.remove_secret(name).map_err(|e| e.to_string())? {
        Some(path) => println!("removed {name} at {}", path.display()),
        None => println!("no stored secret named {name}"),
    }
    Ok(())
}

/// A secret for `--json`: its name and source, never its value.
fn secret_value(secret: &SecretEntry) -> Value {
    match &secret.source {
        SecretSource::File(path) => {
            json!({ "name": secret.name, "source": "file", "path": path_value(path) })
        }
        SecretSource::Env => json!({ "name": secret.name, "source": "env" }),
    }
}

/// A path as a JSON string.
fn path_value(path: &std::path::Path) -> Value {
    Value::String(path.to_string_lossy().into_owned())
}

/// Print `value` as indented JSON.
fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("a Value always serializes")
    );
}
