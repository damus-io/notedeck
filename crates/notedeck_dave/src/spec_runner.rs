//! Sessions run under an agent spec: the spec's slug on the session, a
//! runner per resolved spec on Dave.
//!
//! ```text
//! nd agentium spawn --spec butler [--model ..] [--max-steps ..]
//!   ─▶ kind-31989 ["agent-spec","butler"] ["model",..] ["spec-max-steps",..]
//!   host: session.details.agent_spec = "butler", .requested_model, .spec_overrides
//!         (all ride kind-31988 too)
//!
//! each turn: SpecRunners::get_or_build(SessionSpec { "butler", model, overrides })
//!   AgentSpecs::find ─▶ AgentSpec ─▶ SpecOverrides::apply ─▶ resolved AgentSpec
//!   (account record,                                         ┬─ model ─▶ resolve_provider ─▶ Endpoint
//!    else built-in)                                          ├─ toolsets ─▶ SpecTools
//!                                                            └─ prompt, budget
//!                                        ▼
//!   OpenAiBackend::for_endpoint(..).with_system_prompt(..).with_budget(..)
//!   session.spec_tools = runner.tools   ─▶ dispatch_turn on runner.backend
//!   session.details.containment = spec.containment()   (kind-31988 ["containment", ..])
//! ```
//!
//! A runner is keyed on the session's [`SessionSpec`]: its slug, its model
//! and its overrides. Sessions that ask for the same share one; two sessions
//! under one slug with different overrides never do. A spawn's overrides
//! are the session's alone and never touch the account's spec record (see
//! [`agentium_core::agent_spec::overrides`]). A `/model` in a spec session
//! sets its `requested_model` too, so the next turn runs on the runner for
//! that model.
//!
//! A runner is shared by every session that asks for the same spec, so
//! nothing in it is a session's own. What is (its `agentium:` ref) rides each turn's session
//! env: the backend tells the model its ref after the spec's prompt, a
//! left-out `from_session` is filled from it
//! ([`with_session_defaults`](crate::native_tools::with_session_defaults)),
//! and a Bash child gets it as `AGENTIUM_SESSION`
//! ([`child_env`](crate::coding_tools::child_env)).
//!
//! A runner is built the first time a session under its spec dispatches,
//! and kept: its backend holds each session's run (the stop an interrupt
//! sends), so every turn, stop and cleanup of the spec's sessions goes to the
//! same one ([`SessionBackends::for_session`]). A runner that can't be built is never
//! kept: the turn fails with why, and the next turn tries again, so a
//! provider profile created after the failure is picked up.
//!
//! A runner remembers the specs it was built from ([`BuiltFrom`]), the
//! session's overrides applied. When the account's record of either (the
//! spec's own, or its sub-agent's) has changed by the next turn, the runner
//! is rebuilt from the new one, unless another session on that runner is
//! mid-run: its stop lives in the old
//! runner's backend, so the old runner stays until the spec's sessions are
//! idle, and the edit applies on the first turn after.
//!
//! It remembers the host's tools it was offered from the same way
//! ([`HostTools`]): the registry lists a UI tool only while its app runs, so
//! opening Columns after a session started adds `nostr_open_thread`, and
//! the spec's next idle turn is rebuilt with it.

use std::collections::HashMap;

use agentium_core::agent_spec::record::LookupError;
use agentium_core::agent_spec::{
    pricing, provider::ProviderError, AgentSpec, SpecError, SpecOverrides, Toolset,
};
use nostrdb::Ndb;
use notedeck::{AppContext, ToolSpec};

use crate::agent_specs::AgentSpecs;
use crate::backend::OpenAiBackend;
use crate::backend::{AiBackend, BackendType};
use crate::backend_profiles::BackendProfiles;
use crate::native_tools::{SpecTools, SpecToolsError};
use crate::session::ChatSession;
use crate::sub_agent::SubAgent;
use crate::web_tools::SEARCH_URL_VAR;

/// One spec, ready to run sessions: its backend and its tools.
pub(crate) struct SpecRunner {
    /// The native loop on the spec's endpoint, with its prompt and budget.
    pub(crate) backend: Box<dyn AiBackend>,
    /// What its sessions may call.
    pub(crate) tools: SpecTools,
    /// The specs it was built from, to tell when a record changed them.
    pub(crate) built_from: BuiltFrom,
}

/// What a session asks to run under: its spec's slug, its own model (the
/// `model` tag, which a spawn's `--model` and a `/model` set) and its
/// overrides of the spec's prompt and budget. Borrowed from the session, so
/// finding its runner allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionSpec<'a> {
    /// The spec's slug.
    pub(crate) slug: &'a str,
    /// The session's model, `provider/model` or a model on the spec's
    /// provider; `None` keeps the spec's.
    pub(crate) model: Option<&'a str>,
    /// The session's changes to the spec's prompt and budget.
    pub(crate) overrides: &'a SpecOverrides,
}

impl<'a> SessionSpec<'a> {
    /// What `session` runs under, if it runs under a spec.
    pub(crate) fn of(session: &'a ChatSession) -> Option<Self> {
        Some(Self {
            slug: session.details.agent_spec.as_deref()?,
            model: session.details.requested_model.as_deref(),
            overrides: &session.details.spec_overrides,
        })
    }

    /// The slug alone: the spec as its record has it.
    #[cfg(test)]
    pub(crate) fn plain(slug: &'a str) -> Self {
        Self {
            slug,
            model: None,
            overrides: &SpecOverrides::NONE,
        }
    }
}

/// A [`SessionSpec`] a runner was built for, owned so the runner can be
/// kept under it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunnerKey {
    slug: String,
    model: Option<String>,
    overrides: SpecOverrides,
}

impl RunnerKey {
    /// Whether this is the key `want` names.
    fn is(&self, want: &SessionSpec<'_>) -> bool {
        self.slug == want.slug
            && self.model.as_deref() == want.model
            && self.overrides == *want.overrides
    }

    /// `want`, owned.
    fn of(want: &SessionSpec<'_>) -> Self {
        Self {
            slug: want.slug.to_owned(),
            model: want.model.map(str::to_owned),
            overrides: want.overrides.clone(),
        }
    }
}

/// One built runner, under the session spec it was built for.
struct KeyedRunner {
    key: RunnerKey,
    runner: SpecRunner,
}

/// What a runner was built from: the spec, the sub-agent spec its
/// [`Toolset::SubAgent`] names, if it has one, and which copy of the host's
/// tools.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltFrom {
    pub(crate) spec: AgentSpec,
    pub(crate) sub_agent: Option<AgentSpec>,
    /// The [`HostTools::generation`] its Notedeck toolset picked from.
    pub(crate) tools: Option<u64>,
}

impl BuiltFrom {
    /// `want`'s spec as `deps.specs` has it now with the session's model and
    /// overrides applied, its sub-agent's, and the host's tools as `deps`
    /// has them. A sub-agent spec that can't be found is left `None` here:
    /// building the runner reports it.
    fn current(want: &SessionSpec<'_>, deps: &SpecDeps<'_>) -> Result<Self, SpecRunnerError> {
        let base = deps.specs.find(want.slug)?;
        let spec = want
            .overrides
            .apply(&base, want.model, |p| deps.profiles.knows_provider(p))
            .map_err(|err| SpecRunnerError::Override {
                slug: want.slug.to_owned(),
                err,
            })?;
        let sub_agent = spec.toolsets.iter().find_map(|t| match t {
            Toolset::SubAgent { spec } => deps.specs.find(spec).ok(),
            _ => None,
        });
        Ok(Self {
            spec,
            sub_agent,
            tools: deps.host_tools.generation,
        })
    }
}

/// The host's tools ([`AppContext::tool_specs`]), which a spec's Notedeck
/// toolset picks from, copied again whenever the host's tool registry
/// changes. It changes when an app starts or stops, since a UI tool is
/// registered only while its app runs.
#[derive(Default)]
pub(crate) struct HostTools {
    /// The registry's [generation](notedeck::ToolRegistry::generation) the
    /// specs were copied at; `None` before the first copy.
    pub(crate) generation: Option<u64>,
    /// Every tool the host offers.
    pub(crate) specs: Vec<ToolSpec>,
}

impl HostTools {
    /// Copy the host's tools if its registry changed since the last copy.
    /// Called each frame: comparing the generation allocates nothing.
    pub(crate) fn refresh(&mut self, ctx: &AppContext<'_>) {
        let generation = ctx.registries.tools.generation();
        if self.generation == Some(generation) {
            return;
        }
        self.specs = ctx.tool_specs();
        self.generation = Some(generation);
    }
}

/// Why a spec's runner can't be built: the session's turn fails with it.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SpecRunnerError {
    /// Neither the account nor the built-ins have the slug, or the
    /// account's record of it doesn't read.
    #[error(transparent)]
    Lookup(#[from] LookupError),
    /// The spec's model has no endpoint here (a missing key or profile).
    #[error("agent spec {slug:?}: {err}")]
    Provider { slug: String, err: ProviderError },
    /// Its tools can't be built.
    #[error("agent spec {slug:?}: {err}")]
    Tools { slug: String, err: SpecToolsError },
    /// The session's model or overrides make a spec that can't run (a zero
    /// step cap, say).
    #[error("agent spec {slug:?} as this session overrides it: {err}")]
    Override { slug: String, err: SpecError },
}

/// What building a runner reads from the host.
pub(crate) struct SpecDeps<'a> {
    /// The account's spec records over the built-ins.
    pub(crate) specs: &'a AgentSpecs,
    /// The account's provider profiles, for the spec's model and its
    /// sub-agent's.
    pub(crate) profiles: &'a BackendProfiles,
    /// The host's tools, which a Notedeck toolset picks from.
    pub(crate) host_tools: &'a HostTools,
    /// Renders tool answers for the model.
    pub(crate) ndb: &'a Ndb,
    /// The guests a sandboxed spec session's coding calls run in.
    pub(crate) sandboxes: &'a crate::sandbox::Sandboxes,
}

/// Every spec runner this Dave has built, by the [`SessionSpec`] it was
/// built for. A handful at most, so a list scanned in place: finding a
/// session's runner borrows and allocates nothing.
#[derive(Default)]
pub(crate) struct SpecRunners {
    runners: Vec<KeyedRunner>,
}

impl SpecRunners {
    /// The runner for `want`, if one has been built.
    pub(crate) fn get(&self, want: &SessionSpec<'_>) -> Option<&SpecRunner> {
        self.position(want).map(|i| &self.runners[i].runner)
    }

    /// Where `want`'s runner is kept.
    fn position(&self, want: &SessionSpec<'_>) -> Option<usize> {
        self.runners.iter().position(|r| r.key.is(want))
    }

    /// The runner for `want`, built from the spec `deps.specs` has for its
    /// slug with its model and overrides applied on first use, and rebuilt
    /// when that spec (or its sub-agent's) or the host's tools have changed
    /// since, unless `busy`: another session on the cached runner is
    /// mid-run, and must keep its stop.
    pub(crate) fn get_or_build(
        &mut self,
        want: &SessionSpec<'_>,
        deps: &SpecDeps<'_>,
        busy: bool,
    ) -> Result<&SpecRunner, SpecRunnerError> {
        let slug = want.slug;
        let current = BuiltFrom::current(want, deps)?;
        let at = self.position(want);
        let stale = match at.map(|i| &self.runners[i].runner) {
            None => true,
            Some(runner) if runner.built_from == current => false,
            Some(_) if busy => {
                tracing::info!(
                    "agent spec {slug} or the host's tools changed; keeping its runner until its sessions are idle"
                );
                false
            }
            Some(_) => {
                tracing::info!(
                    "agent spec {slug} or the host's tools changed; rebuilding its runner"
                );
                true
            }
        };
        let at = match (at, stale) {
            (Some(i), false) => i,
            (Some(i), true) => {
                self.runners[i].runner = build(current, deps)?;
                i
            }
            (None, _) => {
                let runner = build(current, deps)?;
                self.runners.push(KeyedRunner {
                    key: RunnerKey::of(want),
                    runner,
                });
                self.runners.len() - 1
            }
        };
        Ok(&self.runners[at].runner)
    }

    /// Keep `runner` as `want`'s, for a test to run a spec on a fake
    /// backend.
    #[cfg(test)]
    pub(crate) fn insert(&mut self, want: &SessionSpec<'_>, runner: SpecRunner) {
        match self.position(want) {
            Some(i) => self.runners[i].runner = runner,
            None => self.runners.push(KeyedRunner {
                key: RunnerKey::of(want),
                runner,
            }),
        }
    }
}

/// Build the runner for `from.spec`: its model resolved to an endpoint, its
/// tools from the host's, its sub-agent from `from.sub_agent`, and web
/// searches sent to `$NOTEDECK_SEARXNG_URL`.
fn build(from: BuiltFrom, deps: &SpecDeps<'_>) -> Result<SpecRunner, SpecRunnerError> {
    let spec = &from.spec;
    let slug = spec.slug.as_str();
    let endpoint =
        deps.profiles
            .resolve_provider(&spec.model)
            .map_err(|err| SpecRunnerError::Provider {
                slug: slug.to_owned(),
                err,
            })?;
    let search_url = std::env::var(SEARCH_URL_VAR).ok();
    let sub_agent = |child: &str| {
        SubAgent::resolve(
            child,
            |_| from.sub_agent.clone(),
            deps.profiles.all(),
            |var| std::env::var(var).ok(),
            search_url.clone(),
        )
    };
    let tools = SpecTools::for_spec(spec, &deps.host_tools.specs, sub_agent, search_url.clone())
        .map_err(|err| SpecRunnerError::Tools {
            slug: slug.to_owned(),
            err,
        })?;
    if spec.budget.max_cost_cents.is_some() && pricing::rate(&endpoint.model).is_none() {
        tracing::warn!(
            "agent spec {slug}: no price for model {:?}, so its max_cost_cents isn't enforced",
            endpoint.model
        );
    }
    let backend = OpenAiBackend::for_endpoint(&endpoint, deps.ndb.clone())
        .with_system_prompt(spec.prompt.clone())
        .with_budget(&spec.budget);
    Ok(SpecRunner {
        backend: Box::new(backend),
        tools,
        built_from: from,
    })
}

/// The host's backends, one per [`BackendType`], behind a private map so
/// nothing outside this module picks one by type alone: a session's backend
/// comes from [`SessionBackends::for_session`], and only a caller that has no
/// session (or deliberately wants the host's backend) asks [`Self::for_type`].
/// Remote is always present once the host is built, and is the fallback for
/// a type the host has no backend for.
#[derive(Default)]
pub(crate) struct HostBackends {
    map: HashMap<BackendType, Box<dyn AiBackend>>,
}

impl HostBackends {
    /// Install `backend` as the host's backend for `bt`, replacing any
    /// already there.
    pub(crate) fn insert(&mut self, bt: BackendType, backend: Box<dyn AiBackend>) {
        self.map.insert(bt, backend);
    }

    /// Whether the host has a backend of its own for `bt` (no Remote
    /// fallback): a session of a type the host doesn't run must not be
    /// dispatched, or the fallback disconnects it and it redispatches forever.
    pub(crate) fn runs(&self, bt: BackendType) -> bool {
        self.map.contains_key(&bt)
    }

    /// The host's backend for `bt`, falling back to Remote when the host
    /// has none of that type.
    ///
    /// # Panics
    /// When the host has neither `bt` nor Remote, which a built [`crate::Dave`]
    /// never is.
    pub(crate) fn for_type(&self, bt: BackendType) -> &dyn AiBackend {
        self.map
            .get(&bt)
            .or_else(|| self.map.get(&BackendType::Remote))
            .expect("the host always has a Remote backend")
            .as_ref()
    }

    /// Drop every backend, Remote included, so a test can install exactly
    /// the ones it wants.
    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        self.map.clear();
    }
}

/// Every backend a session can run on: the host's, one per [`BackendType`],
/// and the spec runners'. Call sites that act on a session pick its backend
/// through [`SessionBackends::for_session`], never by its `BackendType`
/// alone, so a spec session's stop, mode change, compact and cleanup reach
/// the runner that holds its run.
#[derive(Clone, Copy)]
pub(crate) struct SessionBackends<'a> {
    host: &'a HostBackends,
    runners: &'a SpecRunners,
}

impl<'a> SessionBackends<'a> {
    /// The host's backends and the spec runners built so far.
    pub(crate) fn new(host: &'a HostBackends, runners: &'a SpecRunners) -> Self {
        Self { host, runners }
    }

    /// The backend that runs `session`: its spec's runner's when it runs
    /// under one, else the host's backend of its type (falling back to
    /// Remote, as [`HostBackends::for_type`] does). A spec session whose
    /// runner isn't built yet has no run to stop or clean up, so the host's
    /// backend answers for it.
    pub(crate) fn for_session(self, session: Option<&ChatSession>) -> &'a dyn AiBackend {
        let spec_backend = session
            .and_then(SessionSpec::of)
            .and_then(|want| self.runners.get(&want))
            .map(|runner| runner.backend.as_ref());
        if let Some(backend) = spec_backend {
            return backend;
        }
        let bt = session.map_or(BackendType::Remote, |s| s.backend_type);
        self.host.for_type(bt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{BackendLog, RecordingBackend, Reply};
    use crate::Message;
    use agentium_core::agent_spec::web::ASK_RESEARCHER;
    use agentium_core::agent_spec::Approval;
    use agentium_core::config::{BackendProfile, ProfileBackend};
    use std::sync::Arc;

    /// The account's `anthropic` provider as an OpenAI profile, so the
    /// built-ins resolve without a key in the test's env.
    fn profiles() -> BackendProfiles {
        let mut anthropic =
            BackendProfile::for_backend(ProfileBackend::OpenAi, "anthropic", "Anthropic");
        anthropic
            .env
            .insert("OPENAI_BASE_URL".into(), "http://127.0.0.1:9/v1".into());
        BackendProfiles::from_profiles([anthropic])
    }

    /// Headway and notebook tools as the host offers them, filed under
    /// their apps as the shell files them, at the generation a Dave that
    /// hasn't copied its host's tools yet has.
    fn host_tools() -> HostTools {
        HostTools {
            generation: None,
            specs: vec![
                ToolSpec::new("headway_show_card", "Show a card.", vec![])
                    .read_only()
                    .command(&["headway", "show"])
                    .in_app("headway"),
                ToolSpec::new("headway_comment_card", "Comment on a card.", vec![])
                    .command(&["headway", "comment"])
                    .in_app("headway"),
                ToolSpec::new("headway_add_card", "Add a card.", vec![])
                    .command(&["headway", "add"])
                    .in_app("headway"),
                ToolSpec::new("notebook_show", "Show the canvas.", vec![])
                    .read_only()
                    .command(&["notebook", "show"])
                    .in_app("notebook"),
            ],
        }
    }

    /// Build `slug`'s runner from the built-ins (no account records).
    fn build_named(slug: &str, ndb: &Ndb) -> Result<SpecRunner, SpecRunnerError> {
        build_for(&SessionSpec::plain(slug), ndb)
    }

    /// Build `want`'s runner from the built-ins (no account records).
    fn build_for(want: &SessionSpec<'_>, ndb: &Ndb) -> Result<SpecRunner, SpecRunnerError> {
        let specs = AgentSpecs::default();
        let profiles = profiles();
        let host_tools = host_tools();
        let deps = SpecDeps {
            specs: &specs,
            profiles: &profiles,
            host_tools: &host_tools,
            ndb,
            sandboxes: &Default::default(),
        };
        build(BuiltFrom::current(want, &deps)?, &deps)
    }

    /// Get or build `slug`'s runner over `specs`, answering what it was
    /// built from.
    fn runner_from(
        runners: &mut SpecRunners,
        specs: &AgentSpecs,
        ndb: &Ndb,
        busy: bool,
    ) -> BuiltFrom {
        runner_over(runners, specs, &host_tools(), ndb, busy)
    }

    /// [`runner_from`] over `host_tools`.
    fn runner_over(
        runners: &mut SpecRunners,
        specs: &AgentSpecs,
        host_tools: &HostTools,
        ndb: &Ndb,
        busy: bool,
    ) -> BuiltFrom {
        let profiles = profiles();
        let deps = SpecDeps {
            specs,
            profiles: &profiles,
            host_tools,
            ndb,
            sandboxes: &Default::default(),
        };
        runners
            .get_or_build(&SessionSpec::plain("butler"), &deps, busy)
            .expect("the butler builds")
            .built_from
            .clone()
    }

    /// An edit to the butler's record, or to its researcher's, rebuilds the
    /// butler's runner on the next turn, and not while one of its sessions
    /// is mid-run (that run's stop is in the old runner).
    #[tokio::test]
    async fn an_edited_record_rebuilds_the_runner_unless_busy() {
        use crate::agent_specs::tests::ingest_spec_record;
        use agentium_core::agent_spec::builtin;
        use agentium_core::agent_spec::record::build_agent_spec_event_at;

        let mut test = crate::tests::TestDave::new();
        let mut specs = AgentSpecs::default();
        specs.subscribe(&mut test.ndb, test.account);
        let mut runners = SpecRunners::default();
        let first = runner_from(&mut runners, &specs, &test.ndb, false);
        assert_eq!(first.spec, builtin::find("butler").unwrap());

        let mut researcher = builtin::find("researcher").unwrap();
        researcher.model.model = "claude-haiku-5-5".into();
        ingest_spec_record(
            &test,
            build_agent_spec_event_at(&researcher, Some(1_770_000_000), &test.sk).unwrap(),
        )
        .await;
        assert!(specs.poll(&test.ndb));
        let busy = runner_from(&mut runners, &specs, &test.ndb, true);
        assert_eq!(busy, first, "kept while a session is mid-run");
        let idle = runner_from(&mut runners, &specs, &test.ndb, false);
        assert_eq!(idle.sub_agent.as_ref(), Some(&researcher), "rebuilt");

        let mut butler = builtin::find("butler").unwrap();
        butler.prompt = "Validate ideas briefly.".into();
        ingest_spec_record(
            &test,
            build_agent_spec_event_at(&butler, Some(1_770_000_001), &test.sk).unwrap(),
        )
        .await;
        assert!(specs.poll(&test.ndb));
        let edited = runner_from(&mut runners, &specs, &test.ndb, false);
        assert_eq!(edited.spec, butler);
        assert_eq!(
            runner_from(&mut runners, &specs, &test.ndb, false),
            edited,
            "an unchanged spec keeps its runner"
        );
    }

    /// Two sessions under one slug with different overrides each get a
    /// runner built for theirs, while a session asking for the same as
    /// another shares its runner; the account's record is never touched
    /// (headway:dave/west-try-slab).
    #[test]
    fn overrides_key_their_own_runner() {
        use agentium_core::agent_spec::builtin;

        let test = crate::tests::TestDave::new();
        let specs = AgentSpecs::default();
        let profiles = profiles();
        let host_tools = host_tools();
        let deps = SpecDeps {
            specs: &specs,
            profiles: &profiles,
            host_tools: &host_tools,
            ndb: &test.ndb,
            sandboxes: &Default::default(),
        };
        let terse = SpecOverrides {
            prompt: Some("Be terse.".into()),
            max_steps: Some(3),
            ..Default::default()
        };
        let plain = SessionSpec::plain("butler");
        let overridden = SessionSpec {
            slug: "butler",
            model: Some("anthropic/claude-haiku-5-5"),
            overrides: &terse,
        };
        let mut runners = SpecRunners::default();
        let butler = builtin::find("butler").unwrap();
        let built = |runners: &mut SpecRunners, want| {
            runners
                .get_or_build(&want, &deps, false)
                .expect("builds")
                .built_from
                .spec
                .clone()
        };

        assert_eq!(built(&mut runners, plain), butler);
        let resolved = built(&mut runners, overridden);
        assert_eq!(resolved.prompt, "Be terse.");
        assert_eq!(resolved.budget.max_steps, 3);
        assert_eq!(resolved.model.provider, "anthropic");
        assert_eq!(resolved.model.model, "claude-haiku-5-5");
        assert_eq!(resolved.toolsets, butler.toolsets);
        assert_eq!(runners.runners.len(), 2, "one runner each");

        let same = SpecOverrides {
            prompt: Some("Be terse.".into()),
            max_steps: Some(3),
            ..Default::default()
        };
        let again = SessionSpec {
            overrides: &same,
            ..overridden
        };
        assert_eq!(built(&mut runners, again), resolved);
        assert_eq!(runners.runners.len(), 2, "the same ask shares its runner");
        assert_eq!(
            built(&mut runners, plain),
            butler,
            "the plain one is untouched"
        );
        assert_eq!(specs.find("butler").unwrap(), butler, "no record written");
    }

    /// An override that makes a spec that can't run fails the build with
    /// the spec named, and keeps no runner.
    #[test]
    fn a_bad_override_fails_the_build() {
        let test = crate::tests::TestDave::new();
        let specs = AgentSpecs::default();
        let profiles = profiles();
        let host_tools = host_tools();
        let deps = SpecDeps {
            specs: &specs,
            profiles: &profiles,
            host_tools: &host_tools,
            ndb: &test.ndb,
            sandboxes: &Default::default(),
        };
        let zero = SpecOverrides {
            max_steps: Some(0),
            ..Default::default()
        };
        let want = SessionSpec {
            slug: "butler",
            model: None,
            overrides: &zero,
        };
        let mut runners = SpecRunners::default();
        let err = runners
            .get_or_build(&want, &deps, false)
            .err()
            .expect("refused");
        assert!(matches!(err, SpecRunnerError::Override { .. }), "{err}");
        assert!(err.to_string().contains("\"butler\""), "{err}");
        assert!(runners.get(&want).is_none());
    }

    /// A change to the host's tools (an app started, so its UI tools are
    /// registered) rebuilds the runner on its next idle turn, with the new
    /// tools offered; a mid-run session keeps the old one.
    #[test]
    fn new_host_tools_rebuild_the_runner_unless_busy() {
        let test = crate::tests::TestDave::new();
        let specs = AgentSpecs::default();
        let mut runners = SpecRunners::default();
        let mut tools = host_tools();
        tools.generation = Some(1);
        let first = runner_over(&mut runners, &specs, &tools, &test.ndb, false);
        assert_eq!(first.tools, Some(1));

        tools.generation = Some(2);
        tools.specs.push(
            ToolSpec::new("headway_open", "Open a card.", vec![])
                .ui()
                .agent()
                .in_app("headway"),
        );
        let busy = runner_over(&mut runners, &specs, &tools, &test.ndb, true);
        assert_eq!(busy, first, "kept while a session is mid-run");
        assert!(runners
            .get(&SessionSpec::plain("butler"))
            .unwrap()
            .tools
            .catalog
            .get("headway_open")
            .is_none());

        let idle = runner_over(&mut runners, &specs, &tools, &test.ndb, false);
        assert_eq!(idle.tools, Some(2), "rebuilt");
        assert!(runners
            .get(&SessionSpec::plain("butler"))
            .unwrap()
            .tools
            .catalog
            .get("headway_open")
            .is_some());
    }

    /// The butler builds from its built-in spec: the account's provider,
    /// the host's headway tools at the spec's approvals (its comments run
    /// unasked, its other writes are asked), and the built-in researcher as
    /// its gated sub-agent.
    #[test]
    fn the_butler_builds_with_its_tools_and_researcher() {
        let test = crate::tests::TestDave::new();
        let runner = build_named("butler", &test.ndb).expect("the butler builds");
        let approval = |name: &str| {
            runner
                .tools
                .catalog
                .get(name)
                .unwrap_or_else(|| panic!("{name} offered"))
                .approval
        };
        assert_eq!(approval("headway_show_card"), Approval::Auto);
        assert_eq!(approval("headway_comment_card"), Approval::Auto);
        assert_eq!(approval("headway_add_card"), Approval::Gated);
        assert_eq!(approval(ASK_RESEARCHER), Approval::Gated);
        let researcher = runner.tools.sub_agent.as_ref().expect("a sub-agent");
        assert_eq!(researcher.slug(), "researcher");
    }

    /// A slug no spec has fails with the built-ins named, so the fix is in
    /// the error.
    #[test]
    fn an_unknown_spec_names_the_builtins() {
        let test = crate::tests::TestDave::new();
        let err = build_named("scout", &test.ndb).err().expect("no such spec");
        let text = err.to_string();
        assert!(text.contains("\"scout\""), "{text}");
        assert!(text.contains("butler, researcher"), "{text}");
    }

    /// A Dave whose one session runs under `spec`, with the butler's runner
    /// on a [`RecordingBackend`] and the host's OpenAI backend one too;
    /// `sid` has sent "go".
    struct SpecSession {
        test: crate::tests::TestDave,
        sid: crate::session::SessionId,
        runner: Arc<BackendLog>,
        host: Arc<BackendLog>,
    }

    fn send_under(spec: &str) -> SpecSession {
        let mut test = crate::tests::TestDave::new();
        let [sid] = test.add_sessions(["/tmp/butler"], BackendType::OpenAI);
        test.dave
            .session_manager
            .get_mut(sid)
            .unwrap()
            .details
            .agent_spec = Some(spec.into());

        let butler = build_named("butler", &test.ndb).unwrap();
        let says_ok = || RecordingBackend::new().replying(Reply::Says("ok".into()));
        let runner_backend = says_ok();
        let runner = runner_backend.log();
        test.dave.spec_runners.insert(
            &SessionSpec::plain("butler"),
            SpecRunner {
                backend: Box::new(runner_backend),
                tools: butler.tools,
                built_from: butler.built_from,
            },
        );
        let host_backend = says_ok();
        let host = host_backend.log();
        test.dave
            .backends
            .insert(BackendType::OpenAI, Box::new(host_backend));

        let parts = test.parts();
        assert!(parts
            .dave
            .add_user_message_for_session(sid, &parts.host, "go".into(), Vec::new()));
        parts.dave.send_user_message_for(sid, &parts.host);
        parts.dave.process_events(&parts.host);
        SpecSession {
            test,
            sid,
            runner,
            host,
        }
    }

    /// The host answers a type it has no backend for with its Remote
    /// backend, and [`HostBackends::runs`] says it has none of its own.
    #[test]
    fn host_backends_for_type_falls_back_to_remote() {
        let remote = RecordingBackend::new();
        let remote_log = remote.log();
        let claude = RecordingBackend::new();
        let claude_log = claude.log();
        let mut host = HostBackends::default();
        host.insert(BackendType::Remote, Box::new(remote));
        host.insert(BackendType::Claude, Box::new(claude));

        assert!(host.runs(BackendType::Claude));
        assert!(!host.runs(BackendType::OpenAI));

        let waker = notedeck::Waker::noop();
        host.for_type(BackendType::OpenAI)
            .interrupt_session("s".into(), waker.clone());
        assert_eq!(remote_log.interrupts(), 1, "OpenAI fell back to Remote");
        host.for_type(BackendType::Claude)
            .interrupt_session("s".into(), waker);
        assert_eq!(claude_log.interrupts(), 1, "Claude answered for itself");
        assert_eq!(remote_log.interrupts(), 1);
    }

    /// A butler session's turn goes to the butler's runner, offered the
    /// butler's tools (never the host backend or Dave's own tools), and its
    /// stop reaches the same runner.
    #[test]
    fn a_spec_sessions_turn_and_stop_go_to_its_runner() {
        let run = send_under("butler");
        assert!(run.host.turns().is_empty(), "host idle");
        let turns = run.runner.turns();
        let [turn] = turns.as_slice() else {
            panic!("one turn on the runner, got {}", turns.len());
        };
        let tools = &turn.tools;
        assert!(tools.contains(&"headway_comment_card".to_string()));
        assert!(tools.contains(&ASK_RESEARCHER.to_string()));
        assert!(
            run.test
                .dave
                .session_manager
                .get(run.sid)
                .unwrap()
                .spec_tools
                .is_some(),
            "the session's calls are checked against the spec's catalog"
        );

        let backend = SessionBackends::new(&run.test.dave.backends, &run.test.dave.spec_runners)
            .for_session(run.test.dave.session_manager.get(run.sid));
        backend.interrupt_session("dave-session-x".into(), run.test.waker.clone());
        assert_eq!(run.runner.interrupts(), 1);
        assert_eq!(run.host.interrupts(), 0);
    }

    /// A session with overrides runs on the runner built for them, not the
    /// plain spec's, and its turn asks for no model of its own: the runner
    /// was built on the overridden one, which may be on another provider
    /// than the raw `provider/model` names. Its stop reaches that runner.
    #[test]
    fn an_overridden_sessions_turn_goes_to_its_own_runner() {
        let mut test = crate::tests::TestDave::new();
        let [sid] = test.add_sessions(["/tmp/butler"], BackendType::OpenAI);
        let terse = SpecOverrides {
            max_steps: Some(3),
            ..Default::default()
        };
        {
            let details = &mut test.dave.session_manager.get_mut(sid).unwrap().details;
            details.agent_spec = Some("butler".into());
            details.requested_model = Some("anthropic/claude-haiku-5-5".into());
            details.spec_overrides = terse.clone();
        }
        let says_ok = || RecordingBackend::new().replying(Reply::Says("ok".into()));
        let mut install = |want: &SessionSpec<'_>| {
            let built = build_for(want, &test.ndb).unwrap();
            let backend = says_ok();
            let log = backend.log();
            test.dave.spec_runners.insert(
                want,
                SpecRunner {
                    backend: Box::new(backend),
                    tools: built.tools,
                    built_from: built.built_from,
                },
            );
            log
        };
        let plain = install(&SessionSpec::plain("butler"));
        let own = install(&SessionSpec {
            slug: "butler",
            model: Some("anthropic/claude-haiku-5-5"),
            overrides: &terse,
        });

        let parts = test.parts();
        assert!(parts
            .dave
            .add_user_message_for_session(sid, &parts.host, "go".into(), Vec::new()));
        parts.dave.send_user_message_for(sid, &parts.host);
        parts.dave.process_events(&parts.host);

        assert!(
            plain.turns().is_empty(),
            "the plain butler's runner is idle"
        );
        let turns = own.turns();
        let [turn] = turns.as_slice() else {
            panic!("one turn on the overridden runner, got {}", turns.len());
        };
        assert_eq!(turn.model, None, "the runner's own model goes out");

        let backend = SessionBackends::new(&test.dave.backends, &test.dave.spec_runners)
            .for_session(test.dave.session_manager.get(sid));
        backend.interrupt_session("dave-session-x".into(), test.waker.clone());
        assert_eq!(own.interrupts(), 1);
        assert_eq!(plain.interrupts(), 0);
    }

    /// A turn on a spec's runner records where the spec's tools run on the
    /// session, and its kind-31988 state carries it as its own tag beside
    /// the spec's slug (headway:dave/forest-squeeze-manage).
    #[test]
    fn a_spec_turn_records_where_its_tools_run() {
        use agentium_core::agent_spec::Containment;

        let run = send_under("butler");
        let session = run.test.dave.session_manager.get(run.sid).unwrap();
        assert_eq!(session.details.containment, Some(Containment::InProcess));

        let state =
            crate::publish::session_state_snapshot(session, "working".into(), "h".into(), 1)
                .expect("an agentic session");
        assert_eq!(state.agent_spec.as_deref(), Some("butler"));
        assert_eq!(state.containment, Some(Containment::InProcess));
    }

    /// Another device's stop and mode change of a butler session (kind-1988
    /// `interrupt` and `set_permission_mode`, as `nd agentium interrupt`
    /// sends) reach the butler's runner, which holds the run, and never the
    /// host's backend of the session's type
    /// (headway:dave/smile-vote-novel).
    #[tokio::test]
    async fn a_remote_stop_of_a_spec_session_reaches_its_runner() {
        use agentium_core::session_events::{
            build_interrupt_event, build_set_permission_mode_event, ThreadingState,
        };

        let mut run = send_under("butler");
        let test = &mut run.test;
        test.dave.conversation_action_sub = Some(
            crate::conversation::subscribe_conversation_events(&test.ndb, test.account)
                .expect("the action subscription"),
        );
        let dtag = test
            .dave
            .session_manager
            .get(run.sid)
            .and_then(|s| s.agentic.as_ref())
            .map(|a| a.event_session_id().to_owned())
            .expect("an agentic session");

        let mut threading = ThreadingState::new();
        let interrupt = build_interrupt_event(&dtag, &mut threading, &test.sk).unwrap();
        let mode =
            build_set_permission_mode_event("plan", &dtag, &mut threading, &test.sk).unwrap();
        let stored = test
            .ndb
            .subscribe(&[nostrdb::Filter::new()
                .kinds([crate::session_events::AI_CONVERSATION_KIND as u64])
                .build()])
            .unwrap();
        for built in [&interrupt, &mode] {
            assert!(crate::publish::pns_ingest(
                &test.ndb,
                &built.note_json,
                &test.sk
            ));
        }
        // Both notes: `wait_for_notes` returns on the first that lands.
        test.ndb
            .wait_for_all_notes_within(stored, 2, std::time::Duration::from_secs(5))
            .await
            .unwrap();

        // The action subscription is told of a write after `stored` may
        // be, so apply until both actions reached a backend. Each note is
        // polled once, so a double apply still fails the counts below.
        let ndb = test.ndb.clone();
        let waker = test.waker.clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            test.dave.apply_remote_conversation_actions(&ndb, &waker);
            let interrupts = run.runner.interrupts() + run.host.interrupts();
            let modes = run.runner.mode_changes().len() + run.host.mode_changes().len();
            if interrupts >= 1 && modes >= 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the remote stop and mode change never reached a backend"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert_eq!(run.runner.interrupts(), 1);
        assert_eq!(run.runner.mode_changes().len(), 1);
        assert_eq!(run.host.interrupts(), 0);
        assert_eq!(run.host.mode_changes().len(), 0);
    }

    /// A session under a spec this host lacks fails its turn with why, and
    /// never runs on the host's backend instead.
    #[test]
    fn an_unknown_spec_fails_the_turn() {
        let run = send_under("scout");
        assert!(run.host.turns().is_empty(), "host idle");
        assert!(run.runner.turns().is_empty(), "butler idle");
        let session = run.test.dave.session_manager.get(run.sid).unwrap();
        let error = session
            .chat
            .iter()
            .find_map(|m| match m {
                Message::Error(e) => Some(e.as_str()),
                _ => None,
            })
            .expect("the turn shows an error");
        assert!(error.contains("\"scout\""), "error: {error}");
    }

    /// Dave Code is a picker row on a host with no account records: picking
    /// it starts a native-loop session under `dave-code`, whose first turn
    /// offers the coding tools and every app's notedeck tools and records
    /// that its tools run on this machine (headway:dave/much-donkey-police).
    #[test]
    fn picking_dave_code_starts_a_host_coder() {
        use crate::DaveOverlay;
        use agentium_core::agent_spec::Containment;

        let mut test = crate::tests::TestDave::new();
        test.dave.available_backends = vec![BackendType::Claude];
        let cwd = tempfile::TempDir::new().unwrap();
        test.dave
            .create_or_pick_backend(cwd.path().to_path_buf(), None);
        let DaveOverlay::BackendPicker { entries, .. } =
            std::mem::take(&mut test.dave.active_overlay)
        else {
            panic!("a lone Claude plus the built-in specs shows the picker");
        };
        let row = entries
            .iter()
            .find(|e| e.key.spec.as_deref() == Some("dave-code"))
            .expect("Dave Code has a row (its label is backend_picker's test)");
        test.dave
            .create_or_resume_session(cwd.path().to_path_buf(), row.choice(0));
        let session = test.dave.session_manager.get_active().unwrap();
        let sid = session.id;
        assert_eq!(session.backend_type, BackendType::OpenAI);
        assert_eq!(session.details.agent_spec.as_deref(), Some("dave-code"));
        assert_eq!(session.details.containment, None, "not before a turn");

        let built = build_named("dave-code", &test.ndb).expect("dave-code builds");
        let backend = RecordingBackend::new().replying(Reply::Says("ok".into()));
        let runner = backend.log();
        test.dave.spec_runners.insert(
            &SessionSpec::plain("dave-code"),
            SpecRunner {
                backend: Box::new(backend),
                tools: built.tools,
                built_from: built.built_from,
            },
        );
        let parts = test.parts();
        assert!(parts
            .dave
            .add_user_message_for_session(sid, &parts.host, "go".into(), Vec::new()));
        parts.dave.send_user_message_for(sid, &parts.host);
        parts.dave.process_events(&parts.host);

        let turns = runner.turns();
        let [turn] = turns.as_slice() else {
            panic!("one turn on Dave Code's runner, got {}", turns.len());
        };
        for tool in ["Bash", "Edit", "headway_comment_card", "notebook_show"] {
            assert!(turn.tools.contains(&tool.to_string()), "{tool} offered");
        }
        let session = test.dave.session_manager.get(sid).unwrap();
        assert_eq!(session.details.containment, Some(Containment::Host));
    }

    /// The Sandboxed Coder is a picker row on every host, but where Dave
    /// can't boot a guest (a test build without the `sandbox` feature)
    /// the row carries the same reason its first turn would fail with,
    /// rather than starting a session that can't run.
    #[test]
    fn the_sandboxed_coder_row_says_why_this_host_cant_run_it() {
        use crate::DaveOverlay;

        let mut test = crate::tests::TestDave::new();
        test.dave.available_backends = vec![BackendType::Claude];
        let cwd = tempfile::TempDir::new().unwrap();
        test.dave
            .create_or_pick_backend(cwd.path().to_path_buf(), None);
        let DaveOverlay::BackendPicker { entries, .. } =
            std::mem::take(&mut test.dave.active_overlay)
        else {
            panic!("a lone Claude plus the built-in specs shows the picker");
        };
        let row = entries
            .iter()
            .find(|e| e.key.spec.as_deref() == Some("sandboxed-coder"))
            .expect("the Sandboxed Coder has a row");
        // A `sandbox` build on a host with the runtime has an ordinary row.
        let why = crate::sandbox::ready().err().map(|e| e.to_string());
        assert_eq!(row.unavailable(), why.as_deref());
        let dave_code = entries
            .iter()
            .find(|e| e.key.spec.as_deref() == Some("dave-code"))
            .unwrap();
        assert_eq!(dave_code.unavailable(), None, "Dave Code needs no guest");
    }
}
