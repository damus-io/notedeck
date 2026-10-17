//! App settings, kept in the vault so they follow the account across
//! machines.
//!
//! One note per setting ([`SettingNote`], `d = set:<name>`), so two machines
//! changing different settings never overwrite each other. [`Settings::load`]
//! folds them into one struct with defaults filled in.
//!
//! `config.toml` (see [`crate::config`]) is the other place settings live:
//! that one is per machine and never synced. A setting that should be the
//! same everywhere, like the home currency, belongs here.

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;
use serde::{Deserialize, Serialize};

use crate::kinds;
use crate::vault::{self, Record, Stored, Vault, VaultError, Written};

/// The home commodity when none is set: CAD, per the root epic.
pub const DEFAULT_HOME_COMMODITY: &str = "CAD";

/// A setting the vault holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// The commodity every amount is converted into for reports.
    Home,
}

impl Setting {
    /// Every setting, in the order `settings show` lists them.
    pub const ALL: [Setting; 1] = [Setting::Home];

    /// The setting's name on the command line, and its record id.
    pub fn name(self) -> &'static str {
        match self {
            Setting::Home => "home",
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
        }
    }
}

/// A commodity symbol cashflow can write unquoted in a journal: letters, or
/// `$`, at most 16 of them.
fn is_commodity(value: &str) -> bool {
    (1..=16).contains(&value.chars().count())
        && value.chars().all(|c| c.is_alphabetic() || c == '$')
}

/// One setting's value: the body of its vault note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingNote {
    /// The value, as `cashflow settings set` took it.
    pub value: String,
}

impl Record for SettingNote {
    const KIND: u32 = kinds::SETTING;
    const PREFIX: &'static str = "set";
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

/// Every setting, as the vault holds them. A field is `None` when the
/// setting was never set; its accessor then gives the default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// [`Setting::Home`]'s note.
    pub home_commodity: Option<Stored<SettingNote>>,
}

impl Settings {
    /// Fold `author`'s setting notes.
    pub fn load(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Self {
        let mut settings = Settings::default();
        for stored in vault::latest_all::<SettingNote>(ndb, txn, author) {
            match Setting::from_name(&stored.id) {
                Some(Setting::Home) => settings.home_commodity = Some(stored),
                // A setting a newer cashflow wrote; nothing here reads it.
                None => {}
            }
        }
        settings
    }

    /// The home commodity, or [`DEFAULT_HOME_COMMODITY`].
    pub fn home_commodity(&self) -> &str {
        self.value(Setting::Home).unwrap_or(DEFAULT_HOME_COMMODITY)
    }

    /// `setting`'s stored value, or `None` when it was never set.
    pub fn value(&self, setting: Setting) -> Option<&str> {
        let stored = match setting {
            Setting::Home => self.home_commodity.as_ref(),
        };
        stored.map(|s| s.body.value.as_str())
    }

    /// `setting`'s value, falling back to its default.
    pub fn effective(&self, setting: Setting) -> &str {
        match setting {
            Setting::Home => self.home_commodity(),
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
    let note = SettingNote {
        value: value.to_owned(),
    };
    Ok(vault.write(ndb, setting.name(), &note, 0)?)
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
    fn setting_names_round_trip() {
        for setting in Setting::ALL {
            assert_eq!(Setting::from_name(setting.name()), Some(setting));
        }
        assert_eq!(Setting::from_name("nope"), None);
    }
}
