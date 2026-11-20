//! Providers: where the native loop sends a spec's model requests.
//!
//! An [`AgentSpec`](super::AgentSpec) names its model as `{provider, model}`
//! and never holds a credential. [`resolve`] turns that name into an
//! [`Endpoint`] (an OpenAI-compatible base URL, the key to send it, and the
//! model id) that the host builds its client from.
//!
//! A provider is one of two things, looked up in this order:
//!
//! 1. **A backend profile** whose backend is
//!    [`OpenAi`](ProfileBackend::OpenAi): the account's kind-31992 record,
//!    PNS-encrypted and synced to every host, with the endpoint in
//!    [`OPENAI_BASE_URL_VAR`] and the key in [`OPENAI_API_KEY_VAR`]. It is set
//!    like any other profile (`nd agentium profile set <slug> --backend
//!    openai`, then `profile secret set <slug> OPENAI_API_KEY`), and it
//!    overrides a built-in of the same slug, so `anthropic` can be pointed at a
//!    proxy.
//! 2. **A built-in** ([`BUILTINS`]): `openai`, `anthropic` and `ollama`, at
//!    their public endpoints, keyed from the process environment the way the
//!    rest of Dave already is.
//!
//! A profile is explicit: its key is the one it holds, never a fallback to
//! the process env, so a work provider can't quietly run on a personal key.
//! A Claude profile of the same slug is an error, not a skip: it configures
//! the Claude harness, and sending the native loop to the default endpoint
//! instead would surprise whoever named it.

use std::collections::BTreeMap;

use super::ModelRef;
use crate::config::{BackendProfile, ProfileBackend, OPENAI_API_KEY_VAR, OPENAI_BASE_URL_VAR};

/// OpenAI's own endpoint: where an OpenAI profile with no base URL goes.
pub const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// A provider every host knows without a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinProvider {
    /// The slug a spec names it by.
    pub slug: &'static str,
    /// Its OpenAI-compatible endpoint.
    pub base_url: &'static str,
    /// The process env var its key comes from; `None` for a provider that
    /// takes none.
    pub key_var: Option<&'static str>,
}

/// The built-in providers, by slug.
///
/// Anthropic serves the OpenAI chat-completions API beside its own at
/// `/v1/`, which is what lets the one native loop run a Claude model.
pub const BUILTINS: [BuiltinProvider; 3] = [
    BuiltinProvider {
        slug: "openai",
        base_url: OPENAI_DEFAULT_BASE_URL,
        key_var: Some("OPENAI_API_KEY"),
    },
    BuiltinProvider {
        slug: "anthropic",
        base_url: "https://api.anthropic.com/v1",
        key_var: Some("ANTHROPIC_API_KEY"),
    },
    BuiltinProvider {
        slug: "ollama",
        base_url: "http://localhost:11434/v1",
        key_var: None,
    },
];

/// The built-in provider named `slug`, if there is one.
pub fn builtin(slug: &str) -> Option<&'static BuiltinProvider> {
    BUILTINS.iter().find(|b| b.slug == slug)
}

/// Where one model request goes: what [`resolve`] answers.
///
/// `Debug` masks the key, so an endpoint can be logged.
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The provider slug it was resolved from.
    pub provider: String,
    /// The OpenAI-compatible base URL, e.g. `https://api.anthropic.com/v1`.
    pub base_url: String,
    /// The key sent as the bearer token; `None` sends none.
    pub api_key: Option<String>,
    /// The model id the provider knows it by.
    pub model: String,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "********"))
            .field("model", &self.model)
            .finish()
    }
}

/// Why a spec's model can't be resolved to an [`Endpoint`]. No variant
/// carries a key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// No profile and no built-in has the slug.
    #[error(
        "no provider `{0}`: it is neither a built-in (openai, anthropic, ollama) nor a \
         backend profile; add one with `nd agentium profile set {0} --backend openai`"
    )]
    Unknown(String),
    /// The slug names a profile for another backend.
    #[error(
        "backend profile `{slug}` configures the {backend} harness, not an OpenAI-compatible \
         provider"
    )]
    WrongBackend {
        /// The provider slug.
        slug: String,
        /// The profile's backend.
        backend: ProfileBackend,
    },
    /// A built-in that needs a key, with its env var unset or empty.
    #[error("provider `{slug}` needs an API key in ${var}, which is unset")]
    NoKey {
        /// The provider slug.
        slug: String,
        /// The env var the key is read from.
        var: &'static str,
    },
}

/// Whether `slug` names a provider [`resolve`] looks up: one of `profiles`
/// or a built-in. Says nothing of whether it resolves (a missing key, a
/// Claude profile): that is `resolve`'s to report. What tells
/// `provider/model` from a model id with a `/` of its own
/// ([`ModelRef::overridden`]).
pub fn is_known(slug: &str, profiles: &BTreeMap<String, BackendProfile>) -> bool {
    profiles.contains_key(slug) || builtin(slug).is_some()
}

/// Resolve `model` to the endpoint its requests go to.
///
/// `profiles` is the account's backend profiles by slug (every backend; the
/// Claude ones are only ever an error here). `env` reads the process
/// environment, passed in so resolution is a pure function a test can drive.
pub fn resolve(
    model: &ModelRef,
    profiles: &BTreeMap<String, BackendProfile>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Endpoint, ProviderError> {
    let slug = model.provider.as_str();
    if let Some(profile) = profiles.get(slug) {
        return from_profile(profile, &model.model);
    }
    let Some(builtin) = builtin(slug) else {
        return Err(ProviderError::Unknown(slug.to_string()));
    };
    let api_key = match builtin.key_var {
        None => None,
        Some(var) => match env(var).filter(|key| !key.is_empty()) {
            Some(key) => Some(key),
            None => {
                return Err(ProviderError::NoKey {
                    slug: slug.to_string(),
                    var,
                })
            }
        },
    };
    Ok(Endpoint {
        provider: slug.to_string(),
        base_url: builtin.base_url.to_string(),
        api_key,
        model: model.model.clone(),
    })
}

/// The endpoint an OpenAI profile describes, for `model`.
fn from_profile(profile: &BackendProfile, model: &str) -> Result<Endpoint, ProviderError> {
    if profile.backend != ProfileBackend::OpenAi {
        return Err(ProviderError::WrongBackend {
            slug: profile.slug.clone(),
            backend: profile.backend,
        });
    }
    // "" is a profile's way of saying "unset" (see `BackendProfile::env`).
    let var = |name: &str| {
        profile
            .env
            .get(name)
            .filter(|value| !value.is_empty())
            .cloned()
    };
    Ok(Endpoint {
        provider: profile.slug.clone(),
        base_url: var(OPENAI_BASE_URL_VAR).unwrap_or_else(|| OPENAI_DEFAULT_BASE_URL.to_string()),
        api_key: var(OPENAI_API_KEY_VAR),
        model: model.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-very-secret";

    fn model(provider: &str, model: &str) -> ModelRef {
        ModelRef {
            provider: provider.into(),
            model: model.into(),
        }
    }

    /// An env with only `ANTHROPIC_API_KEY` set.
    fn anthropic_env(var: &str) -> Option<String> {
        (var == "ANTHROPIC_API_KEY").then(|| KEY.to_string())
    }

    fn no_profiles() -> BTreeMap<String, BackendProfile> {
        BTreeMap::new()
    }

    fn profiles(
        list: impl IntoIterator<Item = BackendProfile>,
    ) -> BTreeMap<String, BackendProfile> {
        list.into_iter().map(|p| (p.slug.clone(), p)).collect()
    }

    fn openai_profile(slug: &str, env: &[(&str, &str)]) -> BackendProfile {
        let mut p = BackendProfile::for_backend(ProfileBackend::OpenAi, slug, slug);
        for (name, value) in env {
            p.env.insert(name.to_string(), value.to_string());
        }
        p
    }

    #[test]
    fn a_builtin_takes_its_key_from_the_env() {
        let endpoint = resolve(
            &model("anthropic", "claude-sonnet-5-5"),
            &no_profiles(),
            anthropic_env,
        )
        .unwrap();
        assert_eq!(
            endpoint,
            Endpoint {
                provider: "anthropic".into(),
                base_url: "https://api.anthropic.com/v1".into(),
                api_key: Some(KEY.into()),
                model: "claude-sonnet-5-5".into(),
            }
        );
    }

    #[test]
    fn a_builtin_without_its_key_is_an_error_not_an_anonymous_request() {
        let err = resolve(&model("openai", "gpt-5.2"), &no_profiles(), anthropic_env).unwrap_err();
        assert_eq!(
            err,
            ProviderError::NoKey {
                slug: "openai".into(),
                var: "OPENAI_API_KEY",
            }
        );
        let empty = |_: &str| Some(String::new());
        assert!(
            resolve(&model("anthropic", "m"), &no_profiles(), empty).is_err(),
            "an empty key is no key"
        );
    }

    #[test]
    fn ollama_needs_no_key() {
        let endpoint = resolve(&model("ollama", "qwen3"), &no_profiles(), |_| None).unwrap();
        assert_eq!(endpoint.base_url, "http://localhost:11434/v1");
        assert_eq!(endpoint.api_key, None);
    }

    #[test]
    fn an_openai_profile_gives_its_own_url_and_key() {
        let local = openai_profile(
            "hydra-oa",
            &[
                ("OPENAI_BASE_URL", "https://llm.example/v1"),
                ("OPENAI_API_KEY", KEY),
            ],
        );
        let endpoint = resolve(
            &model("hydra-oa", "glm-5.3-flash"),
            &profiles([local]),
            |_| panic!("a profile never reads the process env"),
        )
        .unwrap();
        assert_eq!(endpoint.base_url, "https://llm.example/v1");
        assert_eq!(endpoint.api_key.as_deref(), Some(KEY));
        assert_eq!(endpoint.model, "glm-5.3-flash");
    }

    #[test]
    fn a_profile_overrides_the_builtin_of_its_slug() {
        let proxy = openai_profile("anthropic", &[("OPENAI_BASE_URL", "http://proxy/v1")]);
        let endpoint =
            resolve(&model("anthropic", "m"), &profiles([proxy]), anthropic_env).unwrap();
        assert_eq!(endpoint.base_url, "http://proxy/v1");
        assert_eq!(
            endpoint.api_key, None,
            "a profile without a key sends none, not the env's"
        );
    }

    #[test]
    fn a_profile_without_a_url_goes_to_openai_and_empty_means_unset() {
        let bare = openai_profile("bare", &[("OPENAI_BASE_URL", ""), ("OPENAI_API_KEY", "")]);
        let endpoint = resolve(&model("bare", "m"), &profiles([bare]), |_| None).unwrap();
        assert_eq!(endpoint.base_url, OPENAI_DEFAULT_BASE_URL);
        assert_eq!(endpoint.api_key, None);
    }

    #[test]
    fn a_claude_profile_is_not_a_provider() {
        let hydra = BackendProfile::new("hydra", "Hydra");
        let err = resolve(&model("hydra", "m"), &profiles([hydra]), |_| None).unwrap_err();
        assert_eq!(
            err,
            ProviderError::WrongBackend {
                slug: "hydra".into(),
                backend: ProfileBackend::Claude,
            }
        );
        let anthropic = BackendProfile::new("anthropic", "Anthropic");
        assert!(
            resolve(
                &model("anthropic", "m"),
                &profiles([anthropic]),
                anthropic_env
            )
            .is_err(),
            "a Claude profile shadows the built-in rather than falling through to it"
        );
    }

    #[test]
    fn an_unknown_slug_names_the_command_that_adds_it() {
        let err = resolve(&model("nope", "m"), &no_profiles(), |_| None).unwrap_err();
        assert_eq!(err, ProviderError::Unknown("nope".into()));
        assert!(err
            .to_string()
            .contains("nd agentium profile set nope --backend openai"));
    }

    #[test]
    fn debug_masks_the_key() {
        let endpoint = resolve(&model("anthropic", "m"), &no_profiles(), anthropic_env).unwrap();
        let debug = format!("{endpoint:?}");
        assert!(!debug.contains(KEY), "{debug}");
        assert!(debug.contains("********"), "{debug}");
    }
}
