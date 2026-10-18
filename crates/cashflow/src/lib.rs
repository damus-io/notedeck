//! cashflow's non-UI core, shared by the `cashflow` CLI (`crates/cashflow_cli`)
//! and the notedeck app (`crates/notedeck_cashflow`).
//!
//! So far it holds where cashflow keeps its data ([`CashflowDir`]), the
//! per-machine settings file in it ([`Config`]), how a secret is read without
//! being printed ([`Secret`]), and the account's private nostr vault
//! ([`vault`]), whose records use the kinds in [`kinds`] and whose first
//! record type is the synced [`settings`], among them the [`zone`] that dates
//! a source's unix times. The journal of segments under the
//! data dir is [`journal`]; [`import`] reads sources into it (the CSV ones
//! with [`csv`]). [`manifest`] tells the vault which segments exist, which
//! host has each and which are retired, and [`sync`] compares that with the
//! local journal and moves the missing files through rsync. [`vault_rules`] keeps the categorization rules in the
//! vault, ordered by [`rank`], and [`rules`] reads a local rules file into
//! the same derive edits. [`vault_edits`] keeps the per-transaction
//! overrides, splits and transfer links, and [`chart`] the chart of
//! accounts: each account's metadata (display, project) and the aliases
//! that rename subtrees. [`edits_cache`] folds all of those into what
//! derive takes, once for the CLI or kept live, with a generation that
//! moves only when they change, for the app. [`prices`] fetches daily exchange
//! rates into it, and [`books`] loads, derives and indexes it for queries.
//! [`transfers`] finds the moves between the owner's own accounts that two
//! sources each recorded, and [`unknown`] groups what nothing has
//! categorized yet into a triage queue, which [`triage`] counts down and
//! writes the inbox's edits for. [`net`] is the one definition of
//! income, expenses and net every report totals with. [`goals`] reads the
//! financial goals that live as `cashflow-goal` blocks on Headway cards,
//! and measures each one's progress and whether it is on track.
//! [`board_progress`] reads how the work moves on the boards a project
//! links. [`adventure`] levels the Adventure view's skills from the books
//! on the OSRS curve. The design all of this follows is on the root epic,
//! headway:cashflow/flame-vault-near.
//!
//! No egui: the app depends on this crate, never the other way round.

pub mod adventure;
pub mod board_progress;
pub mod books;
pub mod chart;
pub mod config;
pub mod csv;
pub mod edits_cache;
pub mod goals;
pub mod import;
pub mod journal;
pub mod kinds;
pub mod manifest;
pub mod net;
pub mod prices;
pub mod projects;
pub mod rank;
pub mod rules;
pub mod settings;
pub mod sync;
pub mod transfers;
pub mod triage;
pub mod unknown;
pub mod vault;
pub mod vault_edits;
pub mod vault_rules;
pub mod zone;

#[cfg(test)]
mod test_util;

pub use books::Books;
pub use config::{
    AmexConfig, AppStoreConfig, BtcConfig, CashflowDir, Config, ConfigError, DIR_ENV, GoalsConfig,
    NamedPath, PayrollConfig, RELAY_ENV, RbcConfig, Secret, SecretEntry, SecretSource, SyncConfig,
    TransportConfig,
};
pub use edits_cache::{DeriveInputs, EditsCache};
pub use settings::{Setting, Settings};
pub use vault::{Record, Stored, Vault, VaultError, Written};
