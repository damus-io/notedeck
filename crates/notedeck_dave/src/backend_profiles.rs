//! Backend profiles (kind-31992) on this host: the account's named env
//! overlays, loaded from nostrdb and kept live by a subscription, for a
//! session that runs under one to look its profile up at dispatch.
//!
//! Two kinds share the record. A Claude profile overlays the Claude harness:
//! the backend picker offers it and a Claude session runs under it
//! ([`BackendProfiles::claude`]). An OpenAI profile is a provider for the
//! native loop, which an agent spec names as `model.provider`
//! ([`BackendProfiles::resolve_provider`]). Neither ever stands in for the
//! other.
//!
//! Profiles are account-wide (unlike run configs, nothing scopes them to a
//! host), so a session spawned on any host of the account under `hydra` finds
//! the same `hydra` here. A session naming a profile this host lacks fails its
//! turn ([`missing_profile_error`]) instead of running on the plain backend.

use agentium_core::agent_spec::provider::{self, Endpoint, ProviderError};
use agentium_core::agent_spec::ModelRef;
use agentium_core::config::BackendProfile;
use agentium_core::session_loader::load_backend_profiles_from_ndb;
use std::collections::BTreeMap;

/// The selected account's backend profiles, keyed by slug, and the ndb
/// subscription that refreshes them.
#[derive(Default)]
pub(crate) struct BackendProfiles {
    profiles: BTreeMap<String, BackendProfile>,
    sub: Option<nostrdb::Subscription>,
}

impl BackendProfiles {
    /// The Claude profile named `slug`, if the account has one. An OpenAI
    /// profile of that slug is no answer: a Claude session can't run under it.
    pub(crate) fn claude(&self, slug: &str) -> Option<&BackendProfile> {
        self.profiles.get(slug).filter(|p| p.is_claude())
    }

    /// Every Claude profile, in slug order: the backend picker's rows.
    pub(crate) fn claude_profiles(&self) -> impl Iterator<Item = &BackendProfile> {
        self.profiles.values().filter(|p| p.is_claude())
    }

    /// Where an agent spec's `model` sends its requests: an OpenAI profile of
    /// the account's, else a built-in keyed from this process's env. See
    /// [`provider::resolve`].
    // The spec runner (headway:dave/picnic-diagram-dizzy) is its first caller.
    #[allow(dead_code)]
    pub(crate) fn resolve_provider(&self, model: &ModelRef) -> Result<Endpoint, ProviderError> {
        provider::resolve(model, &self.profiles, |var| std::env::var(var).ok())
    }

    /// Whether the live kind-31992 subscription is set up.
    pub(crate) fn is_subscribed(&self) -> bool {
        self.sub.is_some()
    }

    /// Subscribe to `account`'s kind-31992 profile notes (the PNS-unwrapped
    /// inner notes), so [`poll`](Self::poll) sees every revision and
    /// tombstone as it lands.
    pub(crate) fn subscribe(&mut self, ndb: &nostrdb::Ndb, account: nostrdb_net::Pubkey) {
        let filter = nostrdb::Filter::new()
            .kinds([agentium_core::config::AI_BACKEND_PROFILE_KIND as u64])
            .authors([account.bytes()])
            .build();
        match ndb.subscribe(&[filter]) {
            Ok(sub) => {
                self.sub = Some(sub);
                tracing::info!("subscribed for backend profile events in ndb");
            }
            Err(e) => {
                tracing::warn!("failed to subscribe for backend profile events: {:?}", e);
            }
        }
    }

    /// Replace the profiles with `account`'s newest live revision of each, as
    /// nostrdb has them now.
    pub(crate) fn load(&mut self, ndb: &nostrdb::Ndb, account: &nostrdb_net::Pubkey) {
        let txn = match nostrdb::Transaction::new(ndb) {
            Ok(txn) => txn,
            Err(err) => {
                tracing::warn!("failed to open txn for backend profile load: {err:?}");
                return;
            }
        };
        self.profiles = load_backend_profiles_from_ndb(ndb, &txn, account);
        // Slugs only: a profile's env holds its secrets.
        tracing::info!("loaded {} backend profiles from ndb", self.profiles.len());
    }

    /// Reload when a profile note (an upsert or a tombstone) has landed since
    /// the last poll. A whole reload rather than a fold of the new notes: the
    /// loader already resolves each slug's newest revision and its
    /// tombstones, and an account has a handful of profiles at most.
    pub(crate) fn poll(&mut self, ndb: &nostrdb::Ndb, account: &nostrdb_net::Pubkey) {
        let Some(sub) = self.sub else {
            return;
        };
        if ndb.poll_for_notes(sub, 64).is_empty() {
            return;
        }
        self.load(ndb, account);
    }

    /// Profiles for a test, without ndb.
    #[cfg(test)]
    pub(crate) fn from_profiles(profiles: impl IntoIterator<Item = BackendProfile>) -> Self {
        Self {
            profiles: profiles.into_iter().map(|p| (p.slug.clone(), p)).collect(),
            sub: None,
        }
    }
}

/// The chat error a turn fails with when its session runs under profile
/// `slug` and `hostname` has no such profile. Names the command that fixes it.
pub(crate) fn missing_profile_error(slug: &str, hostname: &str) -> String {
    format!(
        "backend profile `{slug}` is not configured on {hostname}; set it with \
         `agentium profile set {slug} …`. Not falling back to the default backend."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::ingest_built_event;
    use agentium_core::config::AI_BACKEND_PROFILE_KIND;
    use agentium_core::session_events::{
        build_backend_profile_delete_event_at, build_backend_profile_event_at,
    };
    use std::time::Duration;

    /// Wait until one more kind-31992 note is indexed. Subscribed before the
    /// ingest it waits for; each wait unsubscribes on return.
    async fn await_profile_note(ndb: &nostrdb::Ndb, sub: nostrdb::Subscription) {
        ndb.wait_for_all_notes_within(sub, 1, Duration::from_secs(5))
            .await
            .unwrap();
    }

    fn profile_sub(ndb: &nostrdb::Ndb) -> nostrdb::Subscription {
        let filter = nostrdb::Filter::new()
            .kinds([AI_BACKEND_PROFILE_KIND as u64])
            .build();
        ndb.subscribe(&[filter]).unwrap()
    }

    /// A profile published to the account's PNS (as `agentium profile set`
    /// does from any host) reaches a running Dave through the poll, and its
    /// tombstone takes it away again.
    #[tokio::test]
    async fn profiles_follow_upserts_and_tombstones() {
        let test = crate::tests::TestDave::new();
        let mut profiles = BackendProfiles::default();
        profiles.subscribe(&test.ndb, test.account);
        profiles.load(&test.ndb, &test.account);
        assert!(profiles.claude("hydra").is_none());

        let mut hydra = BackendProfile::new("hydra", "Hydra");
        hydra.env.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            "https://llm.hydra.host".to_string(),
        );
        let waiter = profile_sub(&test.ndb);
        ingest_built_event(
            build_backend_profile_event_at(&hydra, Some(1_770_000_000), &test.sk),
            "backend-profile",
            &test.ndb,
            &test.sk,
        );
        await_profile_note(&test.ndb, waiter).await;
        profiles.poll(&test.ndb, &test.account);
        let loaded = profiles.claude("hydra").expect("the upsert arrives");
        assert_eq!(loaded.env["ANTHROPIC_BASE_URL"], "https://llm.hydra.host");

        let waiter = profile_sub(&test.ndb);
        ingest_built_event(
            build_backend_profile_delete_event_at("hydra", Some(1_770_000_001), &test.sk),
            "backend-profile-delete",
            &test.ndb,
            &test.sk,
        );
        await_profile_note(&test.ndb, waiter).await;
        profiles.poll(&test.ndb, &test.account);
        assert!(
            profiles.claude("hydra").is_none(),
            "the tombstone removes it"
        );
    }

    /// An OpenAI profile is a provider, never a Claude overlay: the picker and
    /// a Claude session don't see it, and a spec naming it resolves to its
    /// endpoint. A Claude profile is the other way round.
    #[test]
    fn openai_profiles_are_providers_and_claude_ones_are_overlays() {
        use agentium_core::config::ProfileBackend;
        let mut local = BackendProfile::for_backend(ProfileBackend::OpenAi, "local", "Local");
        local
            .env
            .insert("OPENAI_BASE_URL".into(), "http://127.0.0.1:8080/v1".into());
        let profiles =
            BackendProfiles::from_profiles([BackendProfile::new("hydra", "Hydra"), local]);

        assert!(profiles.claude("local").is_none());
        assert!(profiles.claude("hydra").is_some());
        let rows: Vec<&str> = profiles
            .claude_profiles()
            .map(|p| p.slug.as_str())
            .collect();
        assert_eq!(rows, ["hydra"]);

        let model = |provider: &str| ModelRef {
            provider: provider.into(),
            model: "qwen3".into(),
        };
        let endpoint = profiles.resolve_provider(&model("local")).unwrap();
        assert_eq!(endpoint.base_url, "http://127.0.0.1:8080/v1");
        assert_eq!(endpoint.model, "qwen3");
        assert!(matches!(
            profiles.resolve_provider(&model("hydra")),
            Err(ProviderError::WrongBackend { .. })
        ));
    }

    /// The missing-profile error names the profile, the host and the command
    /// that sets it.
    #[test]
    fn missing_profile_error_names_the_fix() {
        let error = missing_profile_error("hydra", "monad");
        assert!(error.contains("`hydra`"));
        assert!(error.contains("monad"));
        assert!(error.contains("agentium profile set hydra"));
    }
}
