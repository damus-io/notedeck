//! cashflow's non-UI core, shared by the `cashflow` CLI (`crates/cashflow_cli`)
//! and the notedeck app (`crates/notedeck_cashflow`).
//!
//! For now this is only where cashflow keeps its data ([`CashflowDir`]), the
//! settings file in it ([`Config`]) and how a secret is read without being
//! printed ([`Secret`]). Importers, the journal store, the edits vault and the
//! game derive land here in later cards; the design they follow is on the root
//! epic, headway:cashflow/flame-vault-near.
//!
//! No egui: the app depends on this crate, never the other way round.

pub mod config;

pub use config::{
    CashflowDir, Config, ConfigError, DIR_ENV, NamedPath, Secret, SecretEntry, SecretSource,
};
