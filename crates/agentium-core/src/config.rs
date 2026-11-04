//! Run-configuration and backend-profile protocol types used by the session
//! engine.
//!
//! This is the platform-neutral subset of `notedeck_dave`'s `config.rs`: only
//! the pieces the pure nostr session protocol depends on (the run-config and
//! backend-profile event kinds and their [`RunConfig`] / [`BackendProfile`]
//! payloads). UI/model/provider configuration stays in the host app.

use std::collections::BTreeMap;

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

/// Nostr event kind for backend profiles (parameterized replaceable, NIP-33).
/// One event per profile; d-tag is the profile's slug.
///
/// Unlike run configs, a profile is account-wide (no `hostname` tag), so a
/// session naming one resolves on any host of the account. It is only ever
/// published PNS-wrapped: the inner note carries the profile's secrets.
pub const AI_BACKEND_PROFILE_KIND: u32 = 31992;

/// The backend every profile overlays for now; carried as the event's
/// `backend` tag so profiles for other backends can come later.
pub const BACKEND_PROFILE_BACKEND: &str = "claude";

/// The longest slug [`validate_slug`] accepts. Below 64 so a slug can never be
/// a 64-char hex string, which nostrdb would store as a 32-byte id that
/// `get_str` cannot read back.
pub const MAX_SLUG_LEN: usize = 63;

/// A named env overlay on the Claude backend: the endpoint, credentials and
/// models a session runs against, e.g. a work backend that speaks the
/// Anthropic API.
///
/// `Debug` is written by hand and masks every value whose variable name looks
/// secret ([`is_secret_var`]); the UI and CLI print through
/// [`BackendProfile::redacted_env`] for the same reason. There is deliberately
/// no `Serialize`, so a profile can't be dumped with its secrets by accident.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct BackendProfile {
    /// Stable identifier, persisted as the event's d-tag ([`validate_slug`]).
    pub slug: String,
    /// Picker text, e.g. "Hydra".
    pub label: String,
    /// Env vars the session subprocess gets. A `""` value means *remove* the
    /// variable from the inherited environment.
    pub env: BTreeMap<String, String>,
    /// Model ids offered in the picker for this profile, in order; may be empty.
    pub models: Vec<String>,
    /// The model a new session starts on, if not the backend's own default.
    pub default_model: Option<String>,
    /// Claude config dir for the session (exported as `CLAUDE_CONFIG_DIR`).
    /// `None` shares the user's `~/.claude`.
    pub config_dir: Option<String>,
    /// `created_at` of the revision this was read from (0 if unpublished).
    pub updated_at: u64,
}

/// Why a [`BackendProfile`] can't be published.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendProfileError {
    /// The slug isn't 1..=[`MAX_SLUG_LEN`] chars of `[a-z0-9-]` starting with
    /// a letter or digit.
    #[error("invalid profile slug {0:?}: use 1-63 of a-z, 0-9 and -, not starting with -")]
    Slug(String),
    /// The env var name isn't `[A-Za-z_][A-Za-z0-9_]*`.
    #[error("invalid env var name {0:?}: use A-Z, a-z, 0-9 and _, not starting with a digit")]
    EnvName(String),
}

/// Check that `slug` can name a profile: 1..=[`MAX_SLUG_LEN`] chars of
/// `[a-z0-9-]`, starting with a letter or digit.
pub fn validate_slug(slug: &str) -> Result<(), BackendProfileError> {
    let valid = !slug.is_empty()
        && slug.len() <= MAX_SLUG_LEN
        && !slug.starts_with('-')
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(())
    } else {
        Err(BackendProfileError::Slug(slug.to_string()))
    }
}

/// Check that `name` is a portable env var name, `[A-Za-z_][A-Za-z0-9_]*`.
/// It can therefore never contain the `=` the event's `NAME=VALUE` tag splits
/// on.
pub fn validate_env_name(name: &str) -> Result<(), BackendProfileError> {
    let mut bytes = name.bytes();
    let valid = bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(BackendProfileError::EnvName(name.to_string()))
    }
}

/// Whether env var `name` holds a secret: its name contains TOKEN, KEY, SECRET
/// or PASSWORD, ignoring case. Such values are never shown or logged.
pub fn is_secret_var(name: &str) -> bool {
    const MARKERS: [&[u8]; 4] = [b"TOKEN", b"KEY", b"SECRET", b"PASSWORD"];
    let name = name.as_bytes();
    MARKERS.iter().any(|marker| {
        name.windows(marker.len())
            .any(|window| window.eq_ignore_ascii_case(marker))
    })
}

/// One env var of a profile, safe to display: see [`BackendProfile::redacted_env`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedactedVar<'a> {
    /// The variable's name, always shown.
    pub name: &'a str,
    /// Its value, or what stands in for it.
    pub value: RedactedValue<'a>,
}

/// What a [`RedactedVar`] shows for its value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RedactedValue<'a> {
    /// A non-secret value, shown as is.
    Plain(&'a str),
    /// A secret value ([`is_secret_var`]), never shown.
    Masked,
    /// `""`: the variable is removed from the inherited environment.
    Unset,
}

impl std::fmt::Display for RedactedValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RedactedValue::Plain(value) => f.write_str(value),
            RedactedValue::Masked => f.write_str("********"),
            RedactedValue::Unset => f.write_str("(unset)"),
        }
    }
}

impl std::fmt::Debug for RedactedValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RedactedValue::Plain(value) => write!(f, "{value:?}"),
            other => std::fmt::Display::fmt(other, f),
        }
    }
}

impl BackendProfile {
    /// An empty profile named `slug`, labelled `label`.
    pub fn new(slug: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            label: label.into(),
            ..Default::default()
        }
    }

    /// Check the slug and every env var name, as publishing requires.
    pub fn validate(&self) -> Result<(), BackendProfileError> {
        validate_slug(&self.slug)?;
        self.env.keys().try_for_each(|name| validate_env_name(name))
    }

    /// The env vars in name order with every secret value masked: what the UI,
    /// CLI and logs show instead of [`env`](Self::env).
    pub fn redacted_env(&self) -> impl Iterator<Item = RedactedVar<'_>> {
        self.env.iter().map(|(name, value)| RedactedVar {
            name,
            value: if value.is_empty() {
                RedactedValue::Unset
            } else if is_secret_var(name) {
                RedactedValue::Masked
            } else {
                RedactedValue::Plain(value)
            },
        })
    }
}

impl std::fmt::Debug for BackendProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct Env<'a>(&'a BackendProfile);
        impl std::fmt::Debug for Env<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_map()
                    .entries(self.0.redacted_env().map(|var| (var.name, var.value)))
                    .finish()
            }
        }
        f.debug_struct("BackendProfile")
            .field("slug", &self.slug)
            .field("label", &self.label)
            .field("env", &Env(self))
            .field("models", &self.models)
            .field("default_model", &self.default_model)
            .field("config_dir", &self.config_dir)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validation() {
        for ok in ["hydra", "work-2", "a", "0x", &"a".repeat(MAX_SLUG_LEN)] {
            assert_eq!(validate_slug(ok), Ok(()), "{ok}");
        }
        let hex64 = "ab".repeat(32);
        for bad in ["", "Hydra", "-x", "a b", "a_b", "é", "a/b", hex64.as_str()] {
            assert!(validate_slug(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn env_name_validation() {
        for ok in ["ANTHROPIC_AUTH_TOKEN", "_X", "a1"] {
            assert_eq!(validate_env_name(ok), Ok(()), "{ok}");
        }
        for bad in ["", "1A", "A=B", "A-B", "A B"] {
            assert!(validate_env_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn secret_vars_match_case_insensitively() {
        for secret in [
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "my_secret",
            "DB_Password",
        ] {
            assert!(is_secret_var(secret), "{secret}");
        }
        for plain in ["ANTHROPIC_BASE_URL", "ANTHROPIC_MODEL", "CLAUDE_CONFIG_DIR"] {
            assert!(!is_secret_var(plain), "{plain}");
        }
        // A plain substring match over-masks, on purpose: a context-size knob
        // is hidden rather than risk showing a real token.
        assert!(is_secret_var("CLAUDE_CODE_MAX_CONTEXT_TOKENS"));
    }

    #[test]
    fn debug_masks_secret_values() {
        let mut profile = BackendProfile::new("hydra", "Hydra");
        profile
            .env
            .insert("ANTHROPIC_AUTH_TOKEN".into(), "sk-very-secret".into());
        profile
            .env
            .insert("ANTHROPIC_BASE_URL".into(), "https://llm.example".into());
        profile
            .env
            .insert("ANTHROPIC_API_KEY".into(), String::new());

        let debug = format!("{profile:?}");
        assert!(!debug.contains("sk-very-secret"), "{debug}");
        assert!(
            debug.contains("\"ANTHROPIC_AUTH_TOKEN\": ********"),
            "{debug}"
        );
        assert!(debug.contains("https://llm.example"), "{debug}");
        assert!(debug.contains("\"ANTHROPIC_API_KEY\": (unset)"), "{debug}");

        let shown: Vec<String> = profile
            .redacted_env()
            .map(|var| format!("{}={}", var.name, var.value))
            .collect();
        assert_eq!(
            shown,
            [
                "ANTHROPIC_API_KEY=(unset)",
                "ANTHROPIC_AUTH_TOKEN=********",
                "ANTHROPIC_BASE_URL=https://llm.example",
            ]
        );
    }
}
