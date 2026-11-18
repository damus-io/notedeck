//! Sessions run under an agent spec: the spec's slug on the session, a
//! runner per slug on Dave.
//!
//! ```text
//! nd agentium spawn --spec butler ─▶ kind-31989 ["agent-spec","butler"]
//!   host: session.details.agent_spec = "butler"   (rides kind-31988 too)
//!
//! each turn: SpecRunners::get_or_build("butler")
//!   AgentSpecs::find ─▶ AgentSpec ─┬─ model ─▶ BackendProfiles::resolve_provider ─▶ Endpoint
//!   (account record,               ├─ toolsets ─▶ SpecTools (Catalog + researcher)
//!    else built-in)                └─ prompt, budget
//!                                        ▼
//!   OpenAiBackend::for_endpoint(..).with_system_prompt(..).with_budget(..)
//!   session.spec_tools = runner.tools   ─▶ dispatch_turn on runner.backend
//! ```
//!
//! A runner is built the first time a session under its spec dispatches,
//! and kept: its backend holds each session's run (the stop an interrupt
//! sends), so every turn, stop and cleanup of the spec's sessions goes to the
//! same one ([`SessionBackends::for_session`]). A runner that can't be built is never
//! kept: the turn fails with why, and the next turn tries again, so a
//! provider profile created after the failure is picked up.
//!
//! A runner remembers the specs it was built from ([`BuiltFrom`]). When the
//! account's record of either (the spec's own, or its sub-agent's) has
//! changed by the next turn, the runner is rebuilt from the new one, unless
//! another session under the spec is mid-run: its stop lives in the old
//! runner's backend, so the old runner stays until the spec's sessions are
//! idle, and the edit applies on the first turn after.

use std::collections::HashMap;

use agentium_core::agent_spec::record::LookupError;
use agentium_core::agent_spec::{pricing, provider::ProviderError, AgentSpec, Toolset};
use nostrdb::Ndb;
use notedeck::ToolSpec;

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

/// The specs a runner was built from: the spec, and the sub-agent spec its
/// [`Toolset::SubAgent`] names, if it has one.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltFrom {
    pub(crate) spec: AgentSpec,
    pub(crate) sub_agent: Option<AgentSpec>,
}

impl BuiltFrom {
    /// `slug`'s spec and its sub-agent's, as `specs` has them now. A
    /// sub-agent spec that can't be found is left `None` here: building the
    /// runner reports it.
    fn current(slug: &str, specs: &AgentSpecs) -> Result<Self, SpecRunnerError> {
        let spec = specs.find(slug)?;
        let sub_agent = spec.toolsets.iter().find_map(|t| match t {
            Toolset::SubAgent { spec } => specs.find(spec).ok(),
            _ => None,
        });
        Ok(Self { spec, sub_agent })
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
}

/// What building a runner reads from the host.
pub(crate) struct SpecDeps<'a> {
    /// The account's spec records over the built-ins.
    pub(crate) specs: &'a AgentSpecs,
    /// The account's provider profiles, for the spec's model and its
    /// sub-agent's.
    pub(crate) profiles: &'a BackendProfiles,
    /// The host's tools ([`notedeck::AppContext::tool_specs`]), which a
    /// Notedeck toolset picks from.
    pub(crate) host_tools: &'a [ToolSpec],
    /// Renders tool answers for the model.
    pub(crate) ndb: &'a Ndb,
}

/// Every spec runner this Dave has built, by slug.
#[derive(Default)]
pub(crate) struct SpecRunners {
    runners: HashMap<String, SpecRunner>,
}

impl SpecRunners {
    /// The runner for `slug`, if one has been built.
    pub(crate) fn get(&self, slug: &str) -> Option<&SpecRunner> {
        self.runners.get(slug)
    }

    /// The runner for `slug`, built from the spec `deps.specs` has for it on
    /// first use, and rebuilt when that spec (or its sub-agent's) has
    /// changed since, unless `busy`: another session under `slug` is
    /// mid-run on the cached runner, which must keep its stop.
    pub(crate) fn get_or_build(
        &mut self,
        slug: &str,
        deps: &SpecDeps<'_>,
        busy: bool,
    ) -> Result<&SpecRunner, SpecRunnerError> {
        let current = BuiltFrom::current(slug, deps.specs)?;
        let stale = match self.runners.get(slug) {
            None => true,
            Some(runner) if runner.built_from == current => false,
            Some(_) if busy => {
                tracing::info!(
                    "agent spec {slug} changed; keeping its runner until its sessions are idle"
                );
                false
            }
            Some(_) => {
                tracing::info!("agent spec {slug} changed; rebuilding its runner");
                true
            }
        };
        if stale {
            let runner = build(current, deps)?;
            self.runners.insert(slug.to_owned(), runner);
        }
        Ok(&self.runners[slug])
    }

    /// Keep `runner` as `slug`'s, for a test to run a spec on a fake
    /// backend.
    #[cfg(test)]
    pub(crate) fn insert(&mut self, slug: &str, runner: SpecRunner) {
        self.runners.insert(slug.to_owned(), runner);
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
    let tools = SpecTools::for_spec(spec, deps.host_tools, sub_agent, search_url.clone()).map_err(
        |err| SpecRunnerError::Tools {
            slug: slug.to_owned(),
            err,
        },
    )?;
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

/// Every backend a session can run on: the host's, one per [`BackendType`],
/// and the spec runners'. Call sites that act on a session pick its backend
/// through [`SessionBackends::for_session`], never by its `BackendType`
/// alone, so a spec session's stop, mode change, compact and cleanup reach
/// the runner that holds its run.
#[derive(Clone, Copy)]
pub(crate) struct SessionBackends<'a> {
    /// The host's backends, by type.
    pub(crate) host: &'a HashMap<BackendType, Box<dyn AiBackend>>,
    /// The spec runners built so far.
    pub(crate) runners: &'a SpecRunners,
}

impl<'a> SessionBackends<'a> {
    /// The backend that runs `session`: its spec's runner's when it runs
    /// under one, else the host's backend of its type (falling back to
    /// Remote, as [`crate::get_backend`] does). A spec session whose runner
    /// isn't built yet has no run to stop or clean up, so the host's backend
    /// answers for it.
    pub(crate) fn for_session(self, session: Option<&ChatSession>) -> &'a dyn AiBackend {
        let spec_backend = session
            .and_then(|s| s.details.agent_spec.as_deref())
            .and_then(|slug| self.runners.get(slug))
            .map(|runner| runner.backend.as_ref());
        if let Some(backend) = spec_backend {
            return backend;
        }
        let bt = session.map_or(BackendType::Remote, |s| s.backend_type);
        crate::get_backend(self.host, bt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ResponseStream;
    use crate::messages::DaveApiResponse;
    use crate::tools::Tool;
    use crate::Message;
    use agentium_core::agent_spec::web::ASK_RESEARCHER;
    use agentium_core::agent_spec::Approval;
    use agentium_core::config::{BackendProfile, ProfileBackend};
    use notedeck::Waker;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

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

    /// Headway and notebook tools as the host offers them.
    fn host_tools() -> Vec<ToolSpec> {
        vec![
            ToolSpec::new("headway_show_card", "Show a card.", vec![])
                .read_only()
                .command(&["headway", "show"]),
            ToolSpec::new("headway_comment_card", "Comment on a card.", vec![])
                .command(&["headway", "comment"]),
            ToolSpec::new("headway_add_card", "Add a card.", vec![]).command(&["headway", "add"]),
            ToolSpec::new("notebook_show", "Show the canvas.", vec![])
                .read_only()
                .command(&["notebook", "show"]),
        ]
    }

    /// Build `slug`'s runner from the built-ins (no account records).
    fn build_named(slug: &str, ndb: &Ndb) -> Result<SpecRunner, SpecRunnerError> {
        let specs = AgentSpecs::default();
        let profiles = profiles();
        let host_tools = host_tools();
        let deps = SpecDeps {
            specs: &specs,
            profiles: &profiles,
            host_tools: &host_tools,
            ndb,
        };
        build(BuiltFrom::current(slug, &specs)?, &deps)
    }

    /// Get or build `slug`'s runner over `specs`, answering what it was
    /// built from.
    fn runner_from(
        runners: &mut SpecRunners,
        specs: &AgentSpecs,
        ndb: &Ndb,
        busy: bool,
    ) -> BuiltFrom {
        let profiles = profiles();
        let host_tools = host_tools();
        let deps = SpecDeps {
            specs,
            profiles: &profiles,
            host_tools: &host_tools,
            ndb,
        };
        runners
            .get_or_build("butler", &deps, busy)
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

        let test = crate::tests::TestDave::new();
        let mut specs = AgentSpecs::default();
        specs.subscribe(&test.ndb, test.account);
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
        specs.poll(&test.ndb, &test.account);
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
        specs.poll(&test.ndb, &test.account);
        let edited = runner_from(&mut runners, &specs, &test.ndb, false);
        assert_eq!(edited.spec, butler);
        assert_eq!(
            runner_from(&mut runners, &specs, &test.ndb, false),
            edited,
            "an unchanged spec keeps its runner"
        );
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

    /// What a probe backend saw.
    #[derive(Default)]
    struct Seen {
        /// The tool names each turn offered, sorted.
        offered: Mutex<Vec<Vec<String>>>,
        interrupts: AtomicUsize,
        mode_changes: AtomicUsize,
    }

    /// A backend that records each turn's tools, each interrupt and each
    /// permission-mode change.
    struct Probe(Arc<Seen>);

    impl AiBackend for Probe {
        fn stream_request(
            &self,
            _messages: Vec<Message>,
            tools: Arc<HashMap<String, Tool>>,
            _model: Option<String>,
            _user_id: String,
            _session_id: String,
            _session_env: BTreeMap<String, String>,
            _cwd: Option<std::path::PathBuf>,
            _resume_session_id: Option<String>,
            _permission_mode: claude_agent_sdk_rs::PermissionMode,
            _waker: Waker,
        ) -> ResponseStream {
            let mut names: Vec<String> = tools.keys().cloned().collect();
            names.sort();
            self.0.offered.lock().unwrap().push(names);
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = tx.send(DaveApiResponse::Token("ok".into()));
            ResponseStream {
                rx: Some(rx),
                task: None,
            }
        }

        fn cleanup_session(&self, _session_id: String) {}

        fn interrupt_session(&self, _session_id: String, _waker: Waker) {
            self.0.interrupts.fetch_add(1, Ordering::SeqCst);
        }

        fn set_permission_mode(
            &self,
            _session_id: String,
            _mode: claude_agent_sdk_rs::PermissionMode,
            _waker: Waker,
        ) {
            self.0.mode_changes.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A Dave whose one session runs under `spec`, with the butler's runner
    /// on a probe and every host backend a probe too; `sid` has sent "go".
    struct SpecSession {
        test: crate::tests::TestDave,
        sid: crate::session::SessionId,
        runner: Arc<Seen>,
        host: Arc<Seen>,
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
        let runner = Arc::<Seen>::default();
        test.dave.spec_runners.insert(
            "butler",
            SpecRunner {
                backend: Box::new(Probe(Arc::clone(&runner))),
                tools: butler.tools,
                built_from: butler.built_from,
            },
        );
        let host = Arc::<Seen>::default();
        test.dave
            .backends
            .insert(BackendType::OpenAI, Box::new(Probe(Arc::clone(&host))));

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

    /// A butler session's turn goes to the butler's runner, offered the
    /// butler's tools (never the host backend or Dave's own tools), and its
    /// stop reaches the same runner.
    #[test]
    fn a_spec_sessions_turn_and_stop_go_to_its_runner() {
        let run = send_under("butler");
        assert!(run.host.offered.lock().unwrap().is_empty(), "host idle");
        let offered = run.runner.offered.lock().unwrap();
        let [tools] = offered.as_slice() else {
            panic!("one turn on the runner, got {}", offered.len());
        };
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

        let backend = SessionBackends {
            host: &run.test.dave.backends,
            runners: &run.test.dave.spec_runners,
        }
        .for_session(run.test.dave.session_manager.get(run.sid));
        backend.interrupt_session("dave-session-x".into(), run.test.waker.clone());
        assert_eq!(run.runner.interrupts.load(Ordering::SeqCst), 1);
        assert_eq!(run.host.interrupts.load(Ordering::SeqCst), 0);
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
        let _ = test.ndb.wait_for_notes(stored, 2).await.unwrap();

        let ndb = test.ndb.clone();
        let waker = test.waker.clone();
        test.dave.apply_remote_conversation_actions(&ndb, &waker);

        assert_eq!(run.runner.interrupts.load(Ordering::SeqCst), 1);
        assert_eq!(run.runner.mode_changes.load(Ordering::SeqCst), 1);
        assert_eq!(run.host.interrupts.load(Ordering::SeqCst), 0);
        assert_eq!(run.host.mode_changes.load(Ordering::SeqCst), 0);
    }

    /// A session under a spec this host lacks fails its turn with why, and
    /// never runs on the host's backend instead.
    #[test]
    fn an_unknown_spec_fails_the_turn() {
        let run = send_under("scout");
        assert!(run.host.offered.lock().unwrap().is_empty(), "host idle");
        assert!(run.runner.offered.lock().unwrap().is_empty(), "butler idle");
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
}
