//! Where cashflow keeps its data, the settings file in it, and its secrets.
//!
//! Everything lives under one directory, `$CASHFLOW_DIR` (default
//! `<data-local-dir>/cashflow`, e.g. `~/.local/share/cashflow` on Linux), which
//! the CLI and the app share:
//!
//! ```text
//! $CASHFLOW_DIR/
//!     config.toml   settings; a missing file means all defaults
//!     journal/      content-addressed ledger segments, the synced truth
//!     staging/      where a sync transport fetches before the journal admits
//!     secrets/      one file per secret (cln-rune, appstore.p8, ...)
//! ```
//!
//! [`CashflowDir`] names those paths. [`Config`] is `config.toml`, read with
//! unknown keys rejected so a typo fails loudly instead of being ignored.
//! Secrets never go in `config.toml`: they come from `secrets/<name>` or a
//! `CASHFLOW_*` environment variable, and are held as a [`Secret`], whose
//! `Debug` is redacted, so a config dump or a log line can't leak one.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The environment variable that overrides the data directory.
pub const DIR_ENV: &str = "CASHFLOW_DIR";

/// The environment variable naming the relay the CLI syncs the vault with,
/// when `--relay` doesn't.
pub const RELAY_ENV: &str = "CASHFLOW_RELAY";

/// The prefix of every cashflow environment variable. Any variable with it
/// that isn't in [`NOT_SECRET_ENV`] is a secret (see [`secret_env_var`]).
pub const ENV_PREFIX: &str = "CASHFLOW_";

/// The `CASHFLOW_*` variables that are settings rather than secrets. A card
/// adding a non-secret variable lists it here, or `config show` reports it as
/// a (redacted) secret — the safe way round.
pub const NOT_SECRET_ENV: &[&str] = &[DIR_ENV, RELAY_ENV];

/// The data directory's name under the platform data dir.
const APP: &str = "cashflow";

/// Why the data directory or its config couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// No `$CASHFLOW_DIR` and the platform has no data dir to default to.
    #[error("no data directory on this platform: set ${DIR_ENV}")]
    NoDataDir,
    /// `config.toml` exists but couldn't be read.
    #[error("reading {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    /// `config.toml` isn't valid, or has a key cashflow doesn't know.
    #[error("{path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    /// A directory under `$CASHFLOW_DIR` couldn't be created.
    #[error("creating {path}: {source}")]
    Create { path: PathBuf, source: io::Error },
    /// A secret file couldn't be written or removed.
    #[error("writing {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
}

/// The data directory and the fixed layout under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CashflowDir {
    root: PathBuf,
}

/// One of the layout's paths, under the name `cashflow config path <name>`
/// takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedPath {
    /// `dir`, `config`, `journal`, `staging` or `secrets`.
    pub name: &'static str,
    pub path: PathBuf,
}

impl CashflowDir {
    /// The layout rooted at `root`, as given (not created).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory a run uses: `$CASHFLOW_DIR` when it is set and non-empty,
    /// else `<data-local-dir>/cashflow`.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::resolve(std::env::var_os(DIR_ENV), dirs::data_local_dir())
    }

    /// [`from_env`](Self::from_env) with its two inputs passed in: `env` is the
    /// value of `$CASHFLOW_DIR`, `data_local` the platform data dir.
    pub fn resolve(
        env: Option<OsString>,
        data_local: Option<PathBuf>,
    ) -> Result<Self, ConfigError> {
        if let Some(dir) = env.filter(|dir| !dir.is_empty()) {
            return Ok(Self::new(dir));
        }
        let base = data_local.ok_or(ConfigError::NoDataDir)?;
        Ok(Self::new(base.join(APP)))
    }

    /// The data directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `config.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// `journal/`: the content-addressed segments.
    pub fn journal(&self) -> PathBuf {
        self.root.join("journal")
    }

    /// `staging/`: what a sync transport fetched, before the journal admits it.
    pub fn staging(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// `secrets/`: one file per secret.
    pub fn secrets(&self) -> PathBuf {
        self.root.join("secrets")
    }

    /// Every path of the layout, by name, in the order `config path` prints
    /// them.
    pub fn paths(&self) -> [NamedPath; 5] {
        [
            NamedPath {
                name: "dir",
                path: self.root.clone(),
            },
            NamedPath {
                name: "config",
                path: self.config_file(),
            },
            NamedPath {
                name: "journal",
                path: self.journal(),
            },
            NamedPath {
                name: "staging",
                path: self.staging(),
            },
            NamedPath {
                name: "secrets",
                path: self.secrets(),
            },
        ]
    }

    /// Create the data directory and its `journal/`, `staging/` and `secrets/`
    /// subdirectories, leaving any that exist alone. On unix `secrets/` is made
    /// owner-only (0700).
    ///
    /// Reading never needs this: a missing directory reads as empty. It is for
    /// the commands that write.
    pub fn create(&self) -> Result<(), ConfigError> {
        for path in [self.journal(), self.staging(), self.secrets()] {
            std::fs::create_dir_all(&path).map_err(|source| ConfigError::Create {
                path: path.clone(),
                source,
            })?;
        }
        restrict_to_owner(&self.secrets())
    }

    /// A secret by name: `$CASHFLOW_<NAME>` when set (see [`secret_env_var`]),
    /// else the file `secrets/<name>` with its trailing newline dropped.
    /// `Ok(None)` when neither exists.
    pub fn secret(&self, name: &str) -> Result<Option<Secret>, ConfigError> {
        if let Ok(value) = std::env::var(secret_env_var(name)) {
            return Ok(Some(Secret(value)));
        }
        let path = self.secrets().join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(Secret(text.trim_end_matches(['\n', '\r']).to_owned()))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ConfigError::Read { path, source }),
        }
    }

    /// Store secret `name` as the file `secrets/<name>`, owner-only (0600 on
    /// unix) from the moment it exists, creating the data dir if need be.
    /// Returns the file's path. A `$CASHFLOW_<NAME>` variable still wins over
    /// it on read (see [`Self::secret`]).
    pub fn write_secret(&self, name: &str, value: &str) -> Result<PathBuf, ConfigError> {
        self.create()?;
        let path = self.secrets().join(name);
        let write = |path: &Path| -> io::Result<()> {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            options
                .open(path)?
                .write_all(format!("{value}\n").as_bytes())
        };
        write(&path).map_err(|source| ConfigError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    /// Delete the file `secrets/<name>`. `Ok(None)` when there was none,
    /// else its path.
    pub fn remove_secret(&self, name: &str) -> Result<Option<PathBuf>, ConfigError> {
        let path = self.secrets().join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(Some(path)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ConfigError::Write { path, source }),
        }
    }

    /// The secrets this run can see, by name and source but never value:
    /// each file in `secrets/`, then each `CASHFLOW_*` variable in `env` that
    /// isn't in [`NOT_SECRET_ENV`]. Each group is sorted by name.
    ///
    /// `env` is the process environment (`std::env::vars_os()`), passed in so a
    /// test needn't set real variables.
    pub fn secret_entries(
        &self,
        env: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Vec<SecretEntry>, ConfigError> {
        let mut files = self.secret_files()?;
        files.sort_by(|a, b| a.name.cmp(&b.name));

        let mut vars: Vec<SecretEntry> = env
            .into_iter()
            .filter_map(|(var, _)| var.into_string().ok())
            .filter(|var| var.starts_with(ENV_PREFIX) && !NOT_SECRET_ENV.contains(&var.as_str()))
            .map(|var| SecretEntry {
                name: var,
                source: SecretSource::Env,
            })
            .collect();
        vars.sort_by(|a, b| a.name.cmp(&b.name));

        files.extend(vars);
        Ok(files)
    }

    /// The files in `secrets/`, unsorted; none when the directory is missing.
    fn secret_files(&self) -> Result<Vec<SecretEntry>, ConfigError> {
        let dir = self.secrets();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(ConfigError::Read { path: dir, source }),
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| ConfigError::Read {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            files.push(SecretEntry {
                name,
                source: SecretSource::File(path),
            });
        }
        Ok(files)
    }
}

/// The environment variable that holds secret `name`: `CASHFLOW_` and the name
/// upper-cased with `-` and `.` as `_`, so `cln-rune` is `CASHFLOW_CLN_RUNE`
/// and `appstore.p8` is `CASHFLOW_APPSTORE_P8`.
pub fn secret_env_var(name: &str) -> String {
    let tail: String = name
        .chars()
        .map(|c| match c {
            '-' | '.' => '_',
            c => c.to_ascii_uppercase(),
        })
        .collect();
    format!("{ENV_PREFIX}{tail}")
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|source| {
        ConfigError::Create {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

/// `config.toml`. Unknown keys are an error, and a missing file is all
/// defaults.
///
/// Each source adds its own table (`[amex]`, `[cln]`, `[btc]`, ...) in the
/// card that imports it; a table left out is that source's defaults. Secrets
/// never go here (see [`CashflowDir::secret`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `[amex]`: the Amex importer's settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amex: Option<AmexConfig>,
    /// `[rbc]`: the RBC importer's settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rbc: Option<RbcConfig>,
    /// `[btc]`: the on-chain bitcoin importer's settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc: Option<BtcConfig>,
}

/// `[amex]` in `config.toml`. Every key is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmexConfig {
    /// The card's liability account [default: `liabilities:amex`].
    pub card: Option<String>,
    /// The commodity amounts are in [default: `CAD`].
    pub currency: Option<String>,
    /// Write SHOUTING merchant names as Title Case in the payee. The id and
    /// the `raw-*` metadata keep the export's spelling either way.
    #[serde(default)]
    pub title_case: bool,
}

/// `[rbc]` in `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RbcConfig {
    /// `[rbc.accounts]`: the ledger account for each RBC account, keyed by
    /// the last four digits of its number, as in `"1234" =
    /// "assets:rbc:chequing"`. Only the last four go here, never the whole
    /// number.
    #[serde(default)]
    pub accounts: BTreeMap<String, String>,
}

/// `[btc]` in `config.toml`: how to reach bitcoind and which wallets to
/// read. Every key is optional.
///
/// The RPC password is a secret, `bitcoind-rpc-password` (see
/// [`CashflowDir::secret`]), used with `user`. With no `user` the importer
/// reads bitcoind's cookie file instead.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BtcConfig {
    /// bitcoind's JSON-RPC endpoint [default: `http://127.0.0.1:8332`].
    pub url: Option<String>,
    /// `rpcuser`, whose password is the `bitcoind-rpc-password` secret.
    pub user: Option<String>,
    /// bitcoind's `.cookie` file, used when there is no `user`
    /// [default: `~/.bitcoin/.cookie`].
    pub cookie: Option<PathBuf>,
    /// The wallets to read [default: every wallet bitcoind has loaded, from
    /// `listwallets`].
    pub wallets: Option<Vec<String>>,
    /// Watch-only wallets (a descriptor or an xpub bitcoind tracks) read as
    /// well, with their watch-only addresses included. Their funds are yours,
    /// so a move between one of them and another wallet is a transfer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_wallets: Vec<String>,
}

impl Config {
    /// Read `dir`'s `config.toml`, or the defaults when there is none.
    pub fn load(dir: &CashflowDir) -> Result<Self, ConfigError> {
        let path = dir.config_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|source| ConfigError::Parse { path, source }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read { path, source }),
        }
    }

    /// Parse the text of a `config.toml`.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// The config as `config.toml` text, every default spelled out.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("a Config always serializes")
    }
}

/// A secret's value. `Debug` prints `Secret(<redacted>)` and there is no
/// `Display`, so the only way to the text is [`expose`](Secret::expose), at the
/// one place that hands it to the service it is for.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// The value, for the call that authenticates with it. Never print it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// A secret a run can see, without its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretEntry {
    /// The file name under `secrets/`, or the environment variable.
    pub name: String,
    pub source: SecretSource,
}

/// Where a [`SecretEntry`] comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretSource {
    /// A file under `secrets/`.
    File(PathBuf),
    /// A `CASHFLOW_*` environment variable.
    Env,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `$CASHFLOW_DIR` wins; unset or empty falls back to the data dir.
    #[test]
    fn dir_resolves_env_then_data_dir() {
        let data = Some(PathBuf::from("/data"));
        let from_env = CashflowDir::resolve(Some("/custom".into()), data.clone()).unwrap();
        assert_eq!(from_env.root(), Path::new("/custom"));

        let default = CashflowDir::resolve(None, data.clone()).unwrap();
        assert_eq!(default.root(), Path::new("/data/cashflow"));

        let empty = CashflowDir::resolve(Some("".into()), data).unwrap();
        assert_eq!(empty.root(), Path::new("/data/cashflow"));

        assert!(matches!(
            CashflowDir::resolve(None, None),
            Err(ConfigError::NoDataDir)
        ));
    }

    /// The layout is fixed under the root.
    #[test]
    fn layout_paths() {
        let dir = CashflowDir::new("/cf");
        let paths = dir.paths();
        let names: Vec<_> = paths.iter().map(|p| p.name).collect();
        assert_eq!(names, ["dir", "config", "journal", "staging", "secrets"]);
        assert_eq!(dir.config_file(), Path::new("/cf/config.toml"));
        assert_eq!(dir.journal(), Path::new("/cf/journal"));
        assert_eq!(dir.staging(), Path::new("/cf/staging"));
        assert_eq!(dir.secrets(), Path::new("/cf/secrets"));
    }

    /// A missing `config.toml` is the defaults, an empty one too, and an
    /// unknown key is an error naming the file.
    #[test]
    fn config_missing_empty_and_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = CashflowDir::new(tmp.path());
        assert_eq!(Config::load(&dir).unwrap(), Config::default());

        std::fs::write(dir.config_file(), "").unwrap();
        assert_eq!(Config::load(&dir).unwrap(), Config::default());

        std::fs::write(dir.config_file(), "home = \"CAD\"\n").unwrap();
        let err = Config::load(&dir).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("config.toml"), "{msg}");
        assert!(msg.contains("home"), "{msg}");
    }

    /// Defaults round-trip through their TOML.
    #[test]
    fn config_toml_round_trips() {
        let config = Config::default();
        assert_eq!(Config::parse(&config.to_toml()).unwrap(), config);

        let amex =
            Config::parse("[amex]\ncard = \"liabilities:card\"\ntitle_case = true\n").unwrap();
        assert_eq!(
            amex.amex,
            Some(AmexConfig {
                card: Some("liabilities:card".to_owned()),
                currency: None,
                title_case: true,
            })
        );
        assert_eq!(Config::parse(&amex.to_toml()).unwrap(), amex);
        assert!(Config::parse("[amex]\ncards = \"x\"\n").is_err());

        let rbc = Config::parse("[rbc.accounts]\n\"1234\" = \"assets:rbc:chequing\"\n").unwrap();
        assert_eq!(
            rbc.rbc.as_ref().unwrap().accounts["1234"],
            "assets:rbc:chequing"
        );
        assert_eq!(Config::parse(&rbc.to_toml()).unwrap(), rbc);

        let btc = Config::parse(
            "[btc]\nuser = \"rpc\"\nwallets = [\"hot\"]\nexternal_wallets = [\"cold\"]\n",
        )
        .unwrap();
        let table = btc.btc.as_ref().unwrap();
        assert_eq!(table.user.as_deref(), Some("rpc"));
        assert_eq!(table.wallets.as_deref(), Some(&["hot".to_owned()][..]));
        assert_eq!(table.external_wallets, ["cold"]);
        assert_eq!(Config::parse(&btc.to_toml()).unwrap(), btc);
        assert!(Config::parse("[btc]\nwallet = [\"x\"]\n").is_err());
    }

    /// `create` makes the subdirectories, `secrets/` owner-only, and is
    /// idempotent.
    #[test]
    fn create_makes_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = CashflowDir::new(tmp.path().join("cf"));
        dir.create().unwrap();
        dir.create().unwrap();
        for path in [dir.journal(), dir.staging(), dir.secrets()] {
            assert!(path.is_dir(), "{}", path.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.secrets())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }

    /// Secret names map to their environment variable.
    #[test]
    fn secret_env_var_names() {
        assert_eq!(secret_env_var("cln-rune"), "CASHFLOW_CLN_RUNE");
        assert_eq!(secret_env_var("appstore.p8"), "CASHFLOW_APPSTORE_P8");
    }

    /// A secret file reads without its trailing newline, a missing one is
    /// `None`, and `Debug` never shows the value.
    #[test]
    fn secret_file_reads_and_redacts() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = CashflowDir::new(tmp.path());
        dir.create().unwrap();
        std::fs::write(dir.secrets().join("test-only-token"), "hunter2\n").unwrap();

        let secret = dir.secret("test-only-token").unwrap().unwrap();
        assert_eq!(secret.expose(), "hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert!(dir.secret("test-only-absent").unwrap().is_none());
    }

    /// A written secret reads back, is owner-only on unix, and removing it
    /// twice reports it gone the second time.
    #[test]
    fn write_secret_round_trips_and_removes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = CashflowDir::new(tmp.path().join("fresh"));
        let path = dir.write_secret("test-only-key", "s3cret").unwrap();
        assert_eq!(
            dir.secret("test-only-key").unwrap().unwrap().expose(),
            "s3cret"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(dir.remove_secret("test-only-key").unwrap(), Some(path));
        assert_eq!(dir.remove_secret("test-only-key").unwrap(), None);
        assert!(dir.secret("test-only-key").unwrap().is_none());
    }

    /// Entries list secret files then `CASHFLOW_*` variables, each sorted,
    /// leaving out `CASHFLOW_DIR`, `CASHFLOW_RELAY` and unrelated variables.
    #[test]
    fn secret_entries_lists_files_then_env() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = CashflowDir::new(tmp.path());
        dir.create().unwrap();
        std::fs::write(dir.secrets().join("b-file"), "x").unwrap();
        std::fs::write(dir.secrets().join("a-file"), "x").unwrap();
        std::fs::create_dir(dir.secrets().join("not-a-secret")).unwrap();

        let env = [
            ("CASHFLOW_ZED", "v"),
            ("CASHFLOW_DIR", "/somewhere"),
            ("CASHFLOW_RELAY", "ws://127.0.0.1:6677"),
            ("HOME", "/home/x"),
            ("CASHFLOW_ALPHA", "v"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));

        let entries = dir.secret_entries(env).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["a-file", "b-file", "CASHFLOW_ALPHA", "CASHFLOW_ZED"]
        );
        assert_eq!(
            entries[0].source,
            SecretSource::File(dir.secrets().join("a-file"))
        );
        assert_eq!(entries[2].source, SecretSource::Env);
    }

    /// No `secrets/` directory means no secret files, not an error.
    #[test]
    fn secret_entries_without_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = CashflowDir::new(tmp.path().join("absent"));
        assert!(dir.secret_entries([]).unwrap().is_empty());
    }
}
