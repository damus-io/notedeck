//! `cashflow config path|show`: where the data lives, and the config a run
//! reads. Neither creates or changes anything.

use cashflow::{CashflowDir, Config, SecretEntry, SecretSource};
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
