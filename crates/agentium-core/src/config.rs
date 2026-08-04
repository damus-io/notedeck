//! Run-configuration protocol types used by the session engine.
//!
//! This is the platform-neutral subset of `notedeck_dave`'s `config.rs`: only
//! the pieces the pure nostr session protocol depends on (the run-config event
//! kind and the [`RunConfig`] payload). UI/model/provider configuration stays
//! in the host app.

use serde::{Deserialize, Serialize};

/// Nostr event kind for per-config run configurations (parameterized replaceable, NIP-33).
/// One event per config; d-tag is the config's stable UUID.
pub const AI_RUN_CONFIG_KIND: u32 = 31991;

/// A named run configuration: a label + shell command to execute.
///
/// Each config has a stable UUID that is generated once at creation time and
/// persisted to Nostr as the d-tag of its kind-31991 event. This ID survives
/// renames, command edits, reloads, and cross-device sync.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunConfig {
    /// Stable identifier persisted as the Nostr event d-tag.
    /// Generated once on creation, never changes.
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(skip)]
    pub updated_at: u64,
}

impl RunConfig {
    /// Create a new RunConfig with a fresh UUID.
    pub fn new(name: String, command: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            command,
            updated_at: 0,
        }
    }

    /// Sort run configs by name for deterministic ordering.
    pub fn sort_by_name(configs: &mut [RunConfig]) {
        configs.sort_by(|a, b| a.name.cmp(&b.name));
    }
}
