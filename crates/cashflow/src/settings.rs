//! App settings, kept in the vault so they follow the account across
//! machines.
//!
//! One note per setting ([`SettingNote`], `d = set:<name>`), so two machines
//! changing different settings never overwrite each other. [`Settings::load`]
//! folds them into one struct with defaults filled in.
//!
//! Which journal layers load is a setting too, one note per layer
//! (`d = set:layer:<name>`, [`set_layer`]), so switching `cln-chain` on at
//! one desk switches it on at every desk without touching the other
//! layers. A layer with no note is on, unless it is one of
//! [`DEFAULT_OFF_LAYERS`].
//!
//! So is which Headway boards track each project's work, one note per
//! project (`d = set:boards:<project>`, [`set_project_boards`]): the
//! Projects view shows those boards' progress beside the project's revenue.
//!
//! `config.toml` (see [`crate::config`]) is the other place settings live:
//! that one is per machine and never synced. A setting that should be the
//! same everywhere, like the home currency, belongs here.

use nostrdb::{Ndb, Tags, Transaction};
use nostrdb_net::Pubkey;

use crate::kinds;
use crate::vault::{self, Record, Stored, TagWriter, Vault, VaultError, Written, tag_value};
use crate::zone::Zone;

/// The home commodity when none is set: CAD, per the root epic.
pub const DEFAULT_HOME_COMMODITY: &str = "CAD";

/// What [`Setting::Timezone`] reads as when it was never set: each
/// machine's own zone, which is the value the setting exists to replace,
/// so it can't be set to it.
pub const DEFAULT_TIMEZONE: &str = "local";

/// The layers that start switched off: on-chain BTC as CLN's chain moves
/// see it overlaps the wallet's own view, so counting both doubles it.
pub const DEFAULT_OFF_LAYERS: &[&str] = &["cln-chain"];

/// The record-id prefix of a layer's switch, under the settings prefix:
/// `set:layer:<name>`.
const LAYER_ID: &str = "layer:";

/// The record-id prefix of a project's boards, under the settings prefix:
/// `set:boards:<project>`. The CLI takes `boards:<project>` as a setting
/// name for the same reason.
pub const BOARDS_PREFIX: &str = "boards:";

/// A layer switch's value when the layer loads.
const LAYER_ON: &str = "on";

/// A layer switch's value when the layer is left out.
const LAYER_OFF: &str = "off";

/// A setting the vault holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// The commodity every amount is converted into for reports.
    Home,
    /// The zone a source that gives only a unix time (CLN) is dated in:
    /// see [`crate::zone`]. `[cln] timezone` in `config.toml` overrides it
    /// on one machine.
    Timezone,
}

impl Setting {
    /// Every setting, in the order `settings show` lists them.
    pub const ALL: [Setting; 2] = [Setting::Home, Setting::Timezone];

    /// The setting's name on the command line, and its record id.
    pub fn name(self) -> &'static str {
        match self {
            Setting::Home => "home",
            Setting::Timezone => "timezone",
        }
    }

    /// The setting called `name`, if there is one.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Check `value` is one this setting can hold.
    fn validate(self, value: &str) -> Result<(), SettingsError> {
        match self {
            Setting::Home if is_commodity(value) => Ok(()),
            Setting::Home => Err(SettingsError::Invalid {
                setting: self.name(),
                value: value.to_owned(),
                expected: "a commodity symbol, like CAD or USD",
            }),
            Setting::Timezone if Zone::parse(value).is_ok_and(|z| z != Zone::Local) => Ok(()),
            Setting::Timezone => Err(SettingsError::Invalid {
                setting: self.name(),
                value: value.to_owned(),
                expected: "a zone like America/Vancouver, utc, or an offset like -07:00 \
                           (not `local`: that differs from machine to machine)",
            }),
        }
    }
}

/// A commodity symbol cashflow can write unquoted in a journal: letters, or
/// `$`, at most 16 of them.
fn is_commodity(value: &str) -> bool {
    (1..=16).contains(&value.chars().count())
        && value.chars().all(|c| c.is_alphabetic() || c == '$')
}

/// One setting's value: the body of its vault note, the tag
/// `["value", <value>]`, borrowed from the note when read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingNote<'a> {
    /// The value, as `cashflow settings set` took it.
    pub value: &'a str,
}

impl<'a> Record<'a> for SettingNote<'a> {
    const KIND: u32 = kinds::SETTING;
    const PREFIX: &'static str = "set";

    fn write_tags<'b>(&self, tags: TagWriter<'b>) -> TagWriter<'b> {
        tags.tag("value", &[self.value])
    }

    fn from_tags(tags: &Tags<'a>) -> Option<Self> {
        Some(SettingNote {
            value: tag_value(tags, "value")?,
        })
    }
}

/// Why a setting wasn't written.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The value isn't one the setting can hold.
    #[error("'{value}' isn't a valid {setting}: expected {expected}")]
    Invalid {
        setting: &'static str,
        value: String,
        expected: &'static str,
    },
    /// The vault refused the write.
    #[error(transparent)]
    Vault(#[from] VaultError),
}

/// Every setting, as the vault holds them, borrowed from the transaction
/// they were read in. A field is `None` when the setting was never set; its
/// accessor then gives the default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings<'a> {
    /// [`Setting::Home`]'s note.
    pub home_commodity: Option<Stored<'a, SettingNote<'a>>>,
    /// [`Setting::Timezone`]'s note.
    pub timezone: Option<Stored<'a, SettingNote<'a>>>,
    /// Every layer switch ever set, newest revision of each, in no
    /// particular order: see [`Self::layer_switches`].
    pub layers: Vec<Stored<'a, SettingNote<'a>>>,
    /// Every project's boards ever set, newest revision of each, sorted by
    /// project: see [`Self::project_boards`].
    pub project_boards: Vec<Stored<'a, SettingNote<'a>>>,
}

/// The Headway boards one project's work is tracked on, as the vault
/// links them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectBoards<'a> {
    /// The project, as the chart of accounts names it.
    pub project: &'a str,
    /// The boards' slugs, separated by whitespace: see [`Self::boards`].
    pub slugs: &'a str,
}

impl<'a> ProjectBoards<'a> {
    /// Each board's slug, in the order they were linked. None once the
    /// project is unlinked.
    pub fn boards(&self) -> impl Iterator<Item = &'a str> + 'a {
        self.slugs.split_whitespace()
    }
}

/// One layer switched on or off in the vault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerSwitch<'a> {
    /// The layer, as segment headers name it.
    pub layer: &'a str,
    /// Whether it loads.
    pub on: bool,
}

impl<'a> Settings<'a> {
    /// Fold `author`'s setting notes.
    pub fn load(ndb: &Ndb, txn: &'a Transaction, author: &Pubkey) -> Self {
        let mut settings = Settings::default();
        for stored in vault::latest_all::<SettingNote>(ndb, txn, author) {
            match Setting::from_name(stored.id) {
                Some(Setting::Home) => settings.home_commodity = Some(stored),
                Some(Setting::Timezone) => settings.timezone = Some(stored),
                None if stored.id.starts_with(LAYER_ID) => settings.layers.push(stored),
                None if stored.id.starts_with(BOARDS_PREFIX) => {
                    settings.project_boards.push(stored)
                }
                // A setting a newer cashflow wrote; nothing here reads it.
                None => {}
            }
        }
        settings
    }

    /// The home commodity, or [`DEFAULT_HOME_COMMODITY`].
    pub fn home_commodity(&self) -> &'a str {
        self.value(Setting::Home).unwrap_or(DEFAULT_HOME_COMMODITY)
    }

    /// Every layer the vault switches on or off. A value other than `on`
    /// or `off` (a newer cashflow's) is skipped, leaving the default.
    pub fn layer_switches(&self) -> impl Iterator<Item = LayerSwitch<'a>> + '_ {
        self.layers.iter().filter_map(|stored| {
            let layer = stored.id.strip_prefix(LAYER_ID)?;
            let on = match stored.body.value {
                LAYER_ON => true,
                LAYER_OFF => false,
                _ => return None,
            };
            Some(LayerSwitch { layer, on })
        })
    }

    /// Every project linked to Headway boards, sorted by project. A project
    /// whose links were all removed is left out.
    pub fn project_boards(&self) -> impl Iterator<Item = ProjectBoards<'a>> + '_ {
        self.project_boards.iter().filter_map(|stored| {
            let links = ProjectBoards {
                project: stored.id.strip_prefix(BOARDS_PREFIX)?,
                slugs: stored.body.value,
            };
            links.boards().next().is_some().then_some(links)
        })
    }

    /// `setting`'s stored value, or `None` when it was never set.
    pub fn value(&self, setting: Setting) -> Option<&'a str> {
        let stored = match setting {
            Setting::Home => self.home_commodity.as_ref(),
            Setting::Timezone => self.timezone.as_ref(),
        };
        stored.map(|s| s.body.value)
    }

    /// `setting`'s value, falling back to its default.
    pub fn effective(&self, setting: Setting) -> &'a str {
        match setting {
            Setting::Home => self.home_commodity(),
            Setting::Timezone => self.value(setting).unwrap_or(DEFAULT_TIMEZONE),
        }
    }
}

/// Write `value` as `setting`'s newest value in the vault.
pub fn set(
    vault: &Vault,
    ndb: &Ndb,
    setting: Setting,
    value: &str,
) -> Result<Written, SettingsError> {
    setting.validate(value)?;
    Ok(vault.write(ndb, setting.name(), &SettingNote { value }, 0)?)
}

/// Write whether `layer` loads as its newest switch in the vault.
pub fn set_layer(
    vault: &Vault,
    ndb: &Ndb,
    layer: &str,
    on: bool,
) -> Result<Written, SettingsError> {
    if layer.is_empty() || layer.chars().any(char::is_whitespace) {
        return Err(SettingsError::Invalid {
            setting: "layer",
            value: layer.to_owned(),
            expected: "a layer name, as a segment header gives it",
        });
    }
    let value = if on { LAYER_ON } else { LAYER_OFF };
    let id = format!("{LAYER_ID}{layer}");
    Ok(vault.write(ndb, &id, &SettingNote { value }, 0)?)
}

/// Link `project` to the Headway boards `slugs` as its newest boards in the
/// vault, replacing the ones linked before. No slugs unlinks it.
///
/// The slugs are one `value`, separated by spaces: a slug never has
/// whitespace in it, and one value keeps a project's links one replaceable
/// note, so relinking it never leaves a stale board behind.
pub fn set_project_boards(
    vault: &Vault,
    ndb: &Ndb,
    project: &str,
    slugs: &[&str],
) -> Result<Written, SettingsError> {
    let bad = |setting, value: &str, expected| SettingsError::Invalid {
        setting,
        value: value.to_owned(),
        expected,
    };
    if project.is_empty() || project.chars().any(char::is_whitespace) {
        return Err(bad(
            "project",
            project,
            "a project name with no spaces, as the chart of accounts gives it",
        ));
    }
    if let Some(slug) = slugs
        .iter()
        .find(|slug| slug.is_empty() || slug.chars().any(char::is_whitespace))
    {
        return Err(bad("board", slug, "a Headway board's slug, like damus-ios"));
    }
    let value = slugs.join(" ");
    let id = format!("{BOARDS_PREFIX}{project}");
    Ok(vault.write(ndb, &id, &SettingNote { value: &value }, 0)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    use nostrdb::Filter;

    use crate::test_util::{INGEST_TIMEOUT, TempNdb};

    const SECRET: [u8; 32] = [5u8; 32];

    #[tokio::test]
    async fn home_defaults_to_cad_and_round_trips() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);

        {
            let txn = Transaction::new(ndb).unwrap();
            let settings = Settings::load(ndb, &txn, vault.author());
            assert_eq!(settings.value(Setting::Home), None);
            assert_eq!(settings.home_commodity(), "CAD");
        }

        let sub = ndb
            .subscribe(&[Filter::new().kinds([kinds::SETTING as u64]).build()])
            .unwrap();
        set(&vault, ndb, Setting::Home, "USD").unwrap();
        ndb.wait_for_all_notes_within(sub, 1, INGEST_TIMEOUT)
            .await
            .unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let settings = Settings::load(ndb, &txn, vault.author());
        assert_eq!(settings.value(Setting::Home), Some("USD"));
        assert_eq!(settings.effective(Setting::Home), "USD");
    }

    /// A layer switch round-trips, a later one replaces it, and the plain
    /// settings don't see it.
    #[tokio::test]
    async fn layer_switches_round_trip() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = ndb
            .subscribe(&[Filter::new().kinds([kinds::SETTING as u64]).build()])
            .unwrap();

        let first = set_layer(&vault, ndb, "cln-chain", true).unwrap();
        set_layer(&vault, ndb, "amex", false).unwrap();
        ndb.wait_for_all_notes_within(sub, 2, INGEST_TIMEOUT)
            .await
            .unwrap();
        let sub = ndb
            .subscribe(&[Filter::new().kinds([kinds::SETTING as u64]).build()])
            .unwrap();
        let id = format!("{LAYER_ID}cln-chain");
        vault
            .write(
                ndb,
                &id,
                &SettingNote { value: LAYER_OFF },
                first.created_at,
            )
            .unwrap();
        ndb.wait_for_all_notes_within(sub, 1, INGEST_TIMEOUT)
            .await
            .unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let settings = Settings::load(ndb, &txn, vault.author());
        let mut switches: Vec<LayerSwitch> = settings.layer_switches().collect();
        switches.sort_by_key(|s| s.layer);
        assert_eq!(
            switches,
            [
                LayerSwitch {
                    layer: "amex",
                    on: false
                },
                LayerSwitch {
                    layer: "cln-chain",
                    on: false
                },
            ]
        );
        assert_eq!(settings.value(Setting::Home), None);
        assert!(set_layer(&vault, ndb, "two words", true).is_err());
    }

    /// A project's boards round-trip, a relink replaces them, an unlink
    /// drops the project, and the plain settings and layers don't see them.
    #[tokio::test]
    async fn project_boards_round_trip() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = ndb
            .subscribe(&[Filter::new().kinds([kinds::SETTING as u64]).build()])
            .unwrap();
        let first = set_project_boards(&vault, ndb, "damus", &["damus-ios"]).unwrap();
        set_project_boards(&vault, ndb, "notedeck", &["notedeck", "headway"]).unwrap();
        set_project_boards(&vault, ndb, "gone", &["old"]).unwrap();
        ndb.wait_for_all_notes_within(sub, 3, INGEST_TIMEOUT)
            .await
            .unwrap();
        let sub = ndb
            .subscribe(&[Filter::new().kinds([kinds::SETTING as u64]).build()])
            .unwrap();
        let id = format!("{BOARDS_PREFIX}damus");
        let relinked = SettingNote {
            value: "damus-ios damus-android",
        };
        vault.write(ndb, &id, &relinked, first.created_at).unwrap();
        set_project_boards(&vault, ndb, "gone", &[]).unwrap();
        ndb.wait_for_all_notes_within(sub, 2, INGEST_TIMEOUT)
            .await
            .unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let settings = Settings::load(ndb, &txn, vault.author());
        let mut links: Vec<(&str, Vec<&str>)> = settings
            .project_boards()
            .map(|l| (l.project, l.boards().collect()))
            .collect();
        links.sort();
        assert_eq!(
            links,
            [
                ("damus", vec!["damus-ios", "damus-android"]),
                ("notedeck", vec!["notedeck", "headway"]),
            ]
        );
        assert_eq!(settings.layer_switches().count(), 0);
        assert_eq!(settings.value(Setting::Home), None);
        assert!(set_project_boards(&vault, ndb, "two words", &["a"]).is_err());
        assert!(set_project_boards(&vault, ndb, "damus", &["a b"]).is_err());
    }

    #[test]
    fn rejects_values_that_are_not_commodities() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        for bad in ["", "C D", "12", "a-b", "ABCDEFGHIJKLMNOPQ"] {
            assert!(
                matches!(
                    set(&vault, ndb, Setting::Home, bad),
                    Err(SettingsError::Invalid { .. })
                ),
                "{bad:?}"
            );
        }
        assert!(is_commodity("$"));
        assert!(is_commodity("BTC"));
    }

    #[test]
    fn timezone_takes_a_shared_zone_but_never_local() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        for bad in ["local", "LOCAL", "Mars/Olympus", "-25:00", ""] {
            assert!(
                matches!(
                    set(&vault, ndb, Setting::Timezone, bad),
                    Err(SettingsError::Invalid { .. })
                ),
                "{bad:?}"
            );
        }
        for good in ["America/Vancouver", "utc", "-07:00"] {
            assert!(set(&vault, ndb, Setting::Timezone, good).is_ok(), "{good}");
        }
        assert_eq!(Settings::default().effective(Setting::Timezone), "local");
    }

    #[test]
    fn setting_names_round_trip() {
        for setting in Setting::ALL {
            assert_eq!(Setting::from_name(setting.name()), Some(setting));
        }
        assert_eq!(Setting::from_name("nope"), None);
    }
}
