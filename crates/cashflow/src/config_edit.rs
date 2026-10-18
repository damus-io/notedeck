//! Editing `config.toml` one key at a time, for `cashflow config set` and
//! `config unset`.
//!
//! The file is edited as a [`toml_edit`] document rather than round-tripped
//! through [`Config`], so the user's comments, key order and spacing survive.
//! Every edit is checked by [`Config::parse`] before it is written, so a
//! typo'd key or a value of the wrong type is an error that changes nothing,
//! and the write goes to a temp file renamed over `config.toml`, so a crash
//! never leaves half a file.

use std::io;
use std::path::PathBuf;

use toml_edit::{DocumentMut, Item, Table, TableLike, Value};

use crate::config::{CashflowDir, Config, ConfigError};

/// Why a `config.toml` edit was refused. Nothing is written when one is.
#[derive(Debug, thiserror::Error)]
pub enum ConfigEditError {
    /// The key isn't `<table>.<key>`.
    #[error("'{0}' is not a config key: give <table>.<key>, as in cln.node_id")]
    Key(String),
    /// A part of the key names something in the file that isn't a table.
    #[error("'{prefix}' in config.toml is not a table, so it has no '{key}'")]
    NotATable { prefix: String, key: String },
    /// `config.toml` isn't TOML at all.
    #[error("{path}: {source}")]
    Toml {
        path: PathBuf,
        source: toml_edit::TomlError,
    },
    /// The edited file wouldn't load: an unknown key, or a value of the
    /// wrong type.
    #[error("{key}: refused, config.toml is unchanged: {source}")]
    Invalid {
        key: String,
        source: toml::de::Error,
    },
    /// Reading or writing `config.toml` failed.
    #[error(transparent)]
    Config(#[from] ConfigError),
}

/// A `config.toml` edited by [`set_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetEdit {
    /// The whole file, edited.
    pub text: String,
    /// The value as it was written, in TOML (`"x"` for a string).
    pub value: String,
}

/// `config.toml` written by [`CashflowDir::set_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetConfig {
    /// The file written.
    pub path: PathBuf,
    /// The value as it was written, in TOML.
    pub value: String,
}

/// `text` with `key` (`<table>.<key>`, or deeper as in `rbc.accounts.1234`)
/// set to `raw`.
///
/// `raw` is read as a TOML value when it is one (`true`, `3`, `["a", "b"]`,
/// `"quoted"`), else as a string. When the TOML value doesn't load (`1234`
/// where the key wants a string) the string is tried before giving up. A
/// missing table is created; an existing value keeps its place in the file and
/// any comment after it.
pub fn set_key(text: &str, key: &str, raw: &str) -> Result<SetEdit, ConfigEditError> {
    let segments = segments(key)?;
    let doc = parse_doc(text)?;

    let typed = raw.parse::<Value>().ok();
    let typed_is_str = typed.as_ref().is_some_and(Value::is_str);
    let as_string = (!typed_is_str).then(|| Value::from(raw));

    let mut first_error = None;
    for value in typed.into_iter().chain(as_string) {
        let mut edited = doc.clone();
        let written = put(edited.as_table_mut(), key, &segments, value)?;
        let text = edited.to_string();
        match Config::parse(&text) {
            Ok(_) => {
                return Ok(SetEdit {
                    text,
                    value: written,
                });
            }
            Err(source) => {
                first_error.get_or_insert(source);
            }
        }
    }
    let source = first_error.expect("a raw value is always tried as a string");
    Err(ConfigEditError::Invalid {
        key: key.to_owned(),
        source,
    })
}

/// `text` with `key` removed, and any table that leaves empty removed with
/// it. `Ok(None)` when `key` isn't set.
pub fn unset_key(text: &str, key: &str) -> Result<Option<String>, ConfigEditError> {
    let segments = segments(key)?;
    let mut doc = parse_doc(text)?;
    if remove(doc.as_table_mut(), key, &segments, 0)?.is_none() {
        return Ok(None);
    }
    let text = doc.to_string();
    Config::parse(&text).map_err(|source| ConfigEditError::Invalid {
        key: key.to_owned(),
        source,
    })?;
    Ok(Some(text))
}

impl CashflowDir {
    /// Set `key` to `raw` in `config.toml` (see [`set_key`]), creating the
    /// file and the data dir if need be.
    pub fn set_config(&self, key: &str, raw: &str) -> Result<SetConfig, ConfigEditError> {
        let edit = set_key(&self.read_config_text()?, key, raw)?;
        let path = self.write_config_text(&edit.text)?;
        Ok(SetConfig {
            path,
            value: edit.value,
        })
    }

    /// Remove `key` from `config.toml` (see [`unset_key`]). `Ok(None)` when
    /// it wasn't set, and then nothing is written; else the file's path.
    pub fn unset_config(&self, key: &str) -> Result<Option<PathBuf>, ConfigEditError> {
        let Some(text) = unset_key(&self.read_config_text()?, key)? else {
            return Ok(None);
        };
        Ok(Some(self.write_config_text(&text)?))
    }

    /// `config.toml`'s text, empty when there is no file.
    fn read_config_text(&self) -> Result<String, ConfigError> {
        let path = self.config_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(text),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            Err(source) => Err(ConfigError::Read { path, source }),
        }
    }

    /// Replace `config.toml` with `text`: write a temp file beside it, then
    /// rename it over the old one. Returns the file's path.
    fn write_config_text(&self, text: &str) -> Result<PathBuf, ConfigError> {
        let root = self.root();
        std::fs::create_dir_all(root).map_err(|source| ConfigError::Create {
            path: root.to_path_buf(),
            source,
        })?;
        let path = self.config_file();
        let temp = root.join(format!(".config.toml.{}.tmp", std::process::id()));
        let written = std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, &path));
        written.map_err(|source| {
            let _ = std::fs::remove_file(&temp);
            ConfigError::Write {
                path: path.clone(),
                source,
            }
        })?;
        Ok(path)
    }
}

/// `key` split on `.`: a table, then at least one key under it.
fn segments(key: &str) -> Result<Vec<&str>, ConfigEditError> {
    let segments: Vec<&str> = key.split('.').collect();
    if segments.len() < 2 || segments.iter().any(|s| s.is_empty()) {
        return Err(ConfigEditError::Key(key.to_owned()));
    }
    Ok(segments)
}

/// `text` as an editable document, named as `config.toml` in an error.
fn parse_doc(text: &str) -> Result<DocumentMut, ConfigEditError> {
    text.parse().map_err(|source| ConfigEditError::Toml {
        path: PathBuf::from("config.toml"),
        source,
    })
}

/// Set the last of `segments` to `value` under the tables the others name,
/// creating those that are missing. Returns the value as written.
fn put(
    root: &mut dyn TableLike,
    key: &str,
    segments: &[&str],
    mut value: Value,
) -> Result<String, ConfigEditError> {
    let (name, tables) = segments.split_last().expect("segments has at least two");
    let mut table = root;
    for (depth, segment) in tables.iter().enumerate() {
        if !table.contains_key(segment) {
            // Implicit, so a table that only holds another gets no header.
            let mut new = Table::new();
            new.set_implicit(true);
            table.insert(segment, Item::Table(new));
        }
        let item = table.get_mut(segment).expect("inserted above");
        table = item
            .as_table_like_mut()
            .ok_or_else(|| not_a_table(key, segments, depth))?;
    }

    let mut written = value.clone();
    written.decor_mut().clear();
    let written = written.to_string().trim().to_owned();

    match table.get_mut(name) {
        Some(Item::Value(old)) => {
            // Keep the old value's spacing and trailing comment.
            let decor = old.decor().clone();
            *old = value;
            *old.decor_mut() = decor;
        }
        _ => {
            value.decor_mut().clear();
            table.insert(name, Item::Value(value));
        }
    }
    Ok(written)
}

/// Remove `segments[depth..]` from `table`, dropping each table the removal
/// leaves empty. `Ok(None)` when it wasn't there.
fn remove(
    table: &mut dyn TableLike,
    key: &str,
    segments: &[&str],
    depth: usize,
) -> Result<Option<Item>, ConfigEditError> {
    let segment = segments[depth];
    if depth + 1 == segments.len() {
        return Ok(table.remove(segment));
    }
    let Some(item) = table.get_mut(segment) else {
        return Ok(None);
    };
    let child = item
        .as_table_like_mut()
        .ok_or_else(|| not_a_table(key, segments, depth))?;
    let removed = remove(child, key, segments, depth + 1)?;
    if removed.is_some() && child.is_empty() {
        table.remove(segment);
    }
    Ok(removed)
}

/// The error for `segments[..=depth]` naming something that isn't a table.
fn not_a_table(key: &str, segments: &[&str], depth: usize) -> ConfigEditError {
    ConfigEditError::NotATable {
        prefix: segments[..=depth].join("."),
        key: key.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A string goes in quoted, a TOML value as itself, and a new table gets
    /// its header.
    #[test]
    fn set_reads_toml_values_else_strings() {
        let edit = set_key("", "cln.node_id", "03f3c1").unwrap();
        assert_eq!(edit.text, "[cln]\nnode_id = \"03f3c1\"\n");
        assert_eq!(edit.value, "\"03f3c1\"");

        let edit = set_key("", "btc.wallets", r#"["hot","cold"]"#).unwrap();
        assert_eq!(edit.value, r#"["hot","cold"]"#);
        let config = Config::parse(&edit.text).unwrap();
        assert_eq!(
            config.btc.unwrap().wallets.unwrap(),
            ["hot".to_owned(), "cold".to_owned()]
        );

        let edit = set_key("", "goals.auto_done", "false").unwrap();
        assert_eq!(
            Config::parse(&edit.text).unwrap().goals.unwrap().auto_done,
            Some(false)
        );

        let edit = set_key("", "cln.host", "\"10.0.0.1:9735\"").unwrap();
        assert_eq!(edit.value, "\"10.0.0.1:9735\"");
    }

    /// A number where the key wants a string is taken as the string.
    #[test]
    fn set_falls_back_to_a_string() {
        let edit = set_key("", "amex.card", "1234").unwrap();
        assert_eq!(edit.value, "\"1234\"");
        let config = Config::parse(&edit.text).unwrap();
        assert_eq!(config.amex.unwrap().card.as_deref(), Some("1234"));
    }

    /// A key three deep goes in a nested table, with no empty header for its
    /// parent.
    #[test]
    fn set_nested_key() {
        let edit = set_key("", "rbc.accounts.1234", "assets:rbc:chequing").unwrap();
        assert_eq!(
            edit.text,
            "[rbc.accounts]\n1234 = \"assets:rbc:chequing\"\n"
        );
        let config = Config::parse(&edit.text).unwrap();
        assert_eq!(config.rbc.unwrap().accounts["1234"], "assets:rbc:chequing");
    }

    /// Comments, order and an existing value's trailing comment survive.
    #[test]
    fn set_keeps_comments_and_order() {
        let text = "\
# my node
[cln]
host = \"a:1\" # the vps
timezone = \"utc\"

# cards
[amex]
title_case = true
";
        let edit = set_key(text, "cln.host", "b:2").unwrap();
        assert_eq!(edit.text, text.replace("\"a:1\"", "\"b:2\""));

        let edit = set_key(text, "cln.node_id", "03ab").unwrap();
        assert!(
            edit.text
                .starts_with("# my node\n[cln]\nhost = \"a:1\" # the vps\ntimezone = \"utc\"\nnode_id = \"03ab\"\n"),
            "{}",
            edit.text
        );
        assert!(edit.text.contains("# cards\n[amex]"), "{}", edit.text);
    }

    /// An unknown key or a wrong type is refused.
    #[test]
    fn set_rejects_what_config_wont_load() {
        let err = set_key("", "cln.nod_id", "x").unwrap_err();
        assert!(matches!(err, ConfigEditError::Invalid { .. }), "{err}");
        assert!(err.to_string().contains("nod_id"), "{err}");

        let err = set_key("", "goals.auto_done", "yes").unwrap_err();
        assert!(matches!(err, ConfigEditError::Invalid { .. }), "{err}");

        let err = set_key("", "frob.x", "1").unwrap_err();
        assert!(err.to_string().contains("frob"), "{err}");
    }

    /// A key needs a table, and the table must be one.
    #[test]
    fn set_rejects_bad_keys() {
        for key in ["cln", "", "cln.", ".x", "a..b"] {
            assert!(
                matches!(set_key("", key, "x"), Err(ConfigEditError::Key(_))),
                "{key}"
            );
        }
        let text = "[[sync.transport]]\nkind = \"rsync\"\n";
        let err = set_key(text, "sync.transport.kind", "git").unwrap_err();
        assert!(
            matches!(&err, ConfigEditError::NotATable { prefix, .. } if prefix == "sync.transport"),
            "{err}"
        );
    }

    /// Unset removes the key, then any table it leaves empty, and leaves the
    /// rest alone.
    #[test]
    fn unset_drops_empty_tables() {
        let text = "# keep\n[cln]\nhost = \"a:1\"\n\n[amex]\ntitle_case = true\n";
        let edited = unset_key(text, "cln.host").unwrap().unwrap();
        assert!(!edited.contains("[cln]"), "{edited}");
        assert!(edited.contains("[amex]\ntitle_case = true"), "{edited}");

        let nested = set_key("", "rbc.accounts.1234", "assets:rbc").unwrap().text;
        assert_eq!(
            unset_key(&nested, "rbc.accounts.1234").unwrap().unwrap(),
            ""
        );

        let two = "[cln]\nhost = \"a:1\"\ntimezone = \"utc\"\n";
        assert_eq!(
            unset_key(two, "cln.host").unwrap().unwrap(),
            "[cln]\ntimezone = \"utc\"\n"
        );
    }

    /// Unsetting what isn't there changes nothing.
    #[test]
    fn unset_missing_is_none() {
        assert_eq!(unset_key("", "cln.host").unwrap(), None);
        assert_eq!(
            unset_key("[cln]\nhost = \"a\"\n", "cln.timezone").unwrap(),
            None
        );
        assert_eq!(unset_key("[cln]\n", "rbc.accounts.1").unwrap(), None);
    }

    /// The file round-trips through the data dir, and a refused edit leaves
    /// it byte-identical.
    #[test]
    fn set_and_unset_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = CashflowDir::new(tmp.path().join("cf"));
        let set = dir.set_config("cln.timezone", "utc").unwrap();
        assert_eq!(set.path, dir.config_file());
        let before = std::fs::read(dir.config_file()).unwrap();

        assert!(dir.set_config("cln.timezon", "utc").is_err());
        assert_eq!(std::fs::read(dir.config_file()).unwrap(), before);

        let config = Config::load(&dir).unwrap();
        assert_eq!(config.cln.unwrap().timezone.as_deref(), Some("utc"));

        assert_eq!(
            dir.unset_config("cln.timezone").unwrap(),
            Some(dir.config_file())
        );
        assert_eq!(dir.unset_config("cln.timezone").unwrap(), None);
        assert_eq!(Config::load(&dir).unwrap(), Config::default());

        let leftovers: Vec<_> = std::fs::read_dir(dir.root())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["config.toml"]);
    }
}
