//! cashflow's non-UI core, shared by the `cashflow` CLI (`crates/cashflow_cli`)
//! and the notedeck app (`crates/notedeck_cashflow`).
//!
//! So far it holds where cashflow keeps its data ([`CashflowDir`]), the
//! per-machine settings file in it ([`Config`]), how a secret is read without
//! being printed ([`Secret`]), and the account's private nostr vault
//! ([`vault`]), whose records use the kinds in [`kinds`] and whose first
//! record type is the synced [`settings`]. The journal of segments under the
//! data dir is [`journal`]; [`import`] reads sources into it, [`rules`] reads
//! a local rules file into derive edits, [`prices`] fetches daily exchange
//! rates into it, and [`books`] loads, derives and indexes it for queries. The game derive lands here in a later card; the
//! design all of this follows is on the root epic,
//! headway:cashflow/flame-vault-near.
//!
//! No egui: the app depends on this crate, never the other way round.

pub mod books;
pub mod config;
pub mod import;
pub mod journal;
pub mod kinds;
pub mod prices;
pub mod rules;
pub mod settings;
pub mod vault;

#[cfg(test)]
mod test_util;

pub use books::Books;
pub use config::{
    CashflowDir, Config, ConfigError, DIR_ENV, NamedPath, RELAY_ENV, Secret, SecretEntry,
    SecretSource,
};
pub use settings::{Setting, Settings};
pub use vault::{Record, Stored, Vault, VaultError, Written};
