//! The Notedeck toolset in the native loop: a session run under an agent
//! spec is offered its spec's app tools, and Dave runs each call through
//! the host's tool registry once its approval allows.
//!
//! ```text
//! AgentSpec ── Catalog::build(registry specs) ──▶ SpecTools ─┐
//!                                                             │ on the session
//! model ─ ToolCalls::App ─▶ handle_tool_calls ── approval? ───┤
//!                              auto  ─▶ Waiting(Open)         │
//!                              gated ─▶ permission row        │
//!                                       Waiting(Asked)        │
//! frame: Dave::start_app_tool_calls ─▶ start_app_call ─▶ registry
//!          (fast: answered there; slow: a ToolJob off the frame)
//! ```
//!
//! The tools themselves are the apps' own: the model sees each tool's
//! [`ToolSpec`](notedeck::ToolSpec), and the call runs the same registry
//! entry `nd headway …` runs. Nothing here wraps a tool.
//!
//! ## A UI tool's call waits on the chrome's answer
//!
//! A spec may be offered a UI tool (one with an `nd` command line, see
//! [`Catalog`]). The registry can't run it, since it needs its app's own
//! state, so it answers with an [`Intent`](ToolOutcome::Intent) carrying
//! [`AppAction::CallTool`]. Dave queues that call with a reply
//! ([`AppActionQueue::call_tool`]) and the call becomes a job: the chrome
//! runs the tool in its app at the end of this tick's `update` and answers,
//! and the job waits for that answer on its own thread, never the frame's.
//! So the model hears what the tool said: its data, its error (a malformed
//! note id), or that the window carried out the intent it raised. Headless
//! the chrome still runs the tool, and answers an intent that needs the
//! window with an error.
//!
//! ```text
//! start_app_call ─▶ registry ─▶ Intent(CallTool) ─▶ app_actions.call_tool ─▶ UiToolPending
//!   job thread: wait (bounded, cancellable) ◀── chrome update: Data / Error
//!                                           ◀── chrome render: Applied
//! ```
//!
//! ## A call names its own session
//!
//! A tool argument the `nd` CLI fills from the caller's environment
//! ([`ToolArg::env`](notedeck::ToolArg::env)), such as `agentium_spawn`'s
//! `from_session` or `headway_review`'s agentium session from
//! `$AGENTIUM_SESSION`, has no caller environment here: the tool runs in the
//! notedeck process, which hosts many sessions. So before a call is asked
//! for or started, each such argument the model left out is filled from the
//! calling session's own identity ([`with_session_defaults`]), the same
//! variables a Claude session's CLI is given
//! ([`session_env`](crate::backend::shared::session_env)).
//!
//! ## Web and sub-agent tools run as jobs
//!
//! A spec's Web and SubAgent toolsets have no app behind them
//! ([`ToolSource`](agentium_core::agent_spec::ToolSource)), so their calls
//! never reach the registry or the frame: each runs as a job off the frame
//! ([`crate::web_tools::job`], [`SubAgent::job`]), after the user allows it
//! when its approval is gated, as the butler's `ask_researcher` is.
//!
//! ## Coding tools run on the host
//!
//! A Coding toolset's tools ([`coding`](agentium_core::agent_spec::coding))
//! run as jobs too ([`crate::coding_tools::job`]), through
//! [`Executor`](agentium_core::agent_spec::local_executor::Executor)
//! in the session's working tree. Only that runner exists: a spec naming the
//! Microsandbox runner is refused when its session starts
//! ([`SpecToolsError::NoSandboxRunner`]), since running its calls on the
//! host would present an unconfined session as sandboxed.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use agentium_core::agent_spec::{AgentSpec, Catalog, CatalogError, Runner, Toolset};
use notedeck::{
    AppAction, AppActionQueue, AppContext, Started, ToolArgType, ToolOutcome, ToolSpec,
    UiToolAnswer, UiToolPending,
};

use crate::sub_agent::{SubAgent, SubAgentError};
use crate::tool_round::StartedCall;
use crate::tools::{AppToolCall, AppToolResponse, Tool};
use crate::ToolResponses;

/// What a session run under an agent spec may call: the spec's
/// [`Catalog`], and the same tools as its backend advertises them.
#[derive(Clone)]
pub(crate) struct SpecTools {
    /// Each tool with the approval its call needs.
    pub(crate) catalog: Arc<Catalog>,
    /// The catalog as the backend advertises it, built once.
    pub(crate) model_tools: Arc<HashMap<String, Tool>>,
    /// The spec's sub-agent, resolved, when it has a SubAgent toolset.
    pub(crate) sub_agent: Option<Arc<SubAgent>>,
    /// The SearXNG instance a Web toolset's searches go to.
    pub(crate) search_url: Option<String>,
}

/// Why a spec's tools can't be built.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SpecToolsError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    SubAgent(#[from] SubAgentError),
    /// The spec's Coding toolset runs in a Microsandbox guest, and this
    /// build has no such runner yet.
    #[error("agent spec {0}: this build can't run coding tools in a microsandbox yet")]
    NoSandboxRunner(String),
}

impl SpecTools {
    /// The tools `spec` offers, from `available`: what the host has
    /// ([`AppContext::tool_specs`]). A SubAgent toolset's spec is set up by
    /// `sub_agent`, from its slug; web searches go to `search_url`.
    pub(crate) fn for_spec(
        spec: &AgentSpec,
        available: &[notedeck::ToolSpec],
        sub_agent: impl FnOnce(&str) -> Result<SubAgent, SubAgentError>,
        search_url: Option<String>,
    ) -> Result<Self, SpecToolsError> {
        let catalog = Catalog::build(spec, available)?;
        let sandboxed = spec.toolsets.iter().any(|t| {
            matches!(
                t,
                Toolset::Coding {
                    runner: Runner::Microsandbox { .. }
                }
            )
        });
        if sandboxed {
            return Err(SpecToolsError::NoSandboxRunner(spec.slug.clone()));
        }
        let child = spec.toolsets.iter().find_map(|t| match t {
            Toolset::SubAgent { spec } => Some(spec.as_str()),
            _ => None,
        });
        let sub_agent = child.map(sub_agent).transpose()?.map(Arc::new);
        let model_tools = Arc::new(catalog.model_tools());
        Ok(Self {
            catalog: Arc::new(catalog),
            model_tools,
            sub_agent,
            search_url,
        })
    }
}

/// `call` with each string argument of `spec` that the model left out (or
/// sent empty) and that a command line fills from an environment variable
/// set in `identity` filled from it. `identity` is the calling session's
/// own environment ([`session_env`](crate::backend::shared::session_env)
/// with only its identity), so a call made without its `from_session` still
/// names the session that made it. What the model did pass is kept.
pub(crate) fn with_session_defaults(
    call: &AppToolCall,
    spec: &ToolSpec,
    identity: &BTreeMap<String, String>,
) -> AppToolCall {
    let mut call = call.clone();
    let Some(args) = call.args.as_object_mut() else {
        return call;
    };
    for arg in spec.arguments() {
        if !matches!(arg.typ(), ToolArgType::String) {
            continue;
        }
        let Some(value) = arg.cli().env.and_then(|var| identity.get(var)) else {
            continue;
        };
        let given = args
            .get(arg.name())
            .is_some_and(|v| !v.is_null() && v.as_str() != Some(""));
        if !given {
            args.insert(arg.name().to_owned(), value.clone().into());
        }
    }
    call
}

/// How long a UI tool's call waits on the chrome's answer. Data and errors
/// come back on the tick the call was made; only an intent waits on a
/// render, which a hidden window may not draw.
const UI_TOOL_BOUND: Duration = Duration::from_secs(30);

/// Start the app tool `call` through the host's tool registry: what the
/// frame's start pass runs once the call may start. A UI tool's call is
/// queued for the chrome to answer (see the module docs).
pub(crate) fn start_app_call(ctx: &mut AppContext<'_>, call: &AppToolCall) -> StartedCall {
    // A model-driven call never signs with a comment key (see
    // `ToolContext::comment_signer`).
    let started = ctx.start_tool_rendered(&call.name, &call.args, None);
    started_call(&call.name, started, ctx.app_actions)
}

/// What a registry's `started` call of the tool `name` is to the round: a
/// value answers at once, a slow tool's pending work becomes a job off the
/// frame, and a UI tool's call is queued on `queue` for the chrome to answer,
/// as a job that waits on that answer.
pub(crate) fn started_call(
    name: &str,
    started: Started,
    queue: &mut AppActionQueue,
) -> StartedCall {
    let tool = name.to_owned();
    match started {
        Started::Done(rendered) => match rendered.outcome {
            ToolOutcome::Data(value) => {
                StartedCall::Done(Box::new(ToolResponses::App(AppToolResponse {
                    tool,
                    value,
                })))
            }
            ToolOutcome::Error(err) => StartedCall::Done(Box::new(ToolResponses::Error(err))),
            ToolOutcome::Intent(AppAction::CallTool(invocation)) => {
                wait_on_ui_tool(tool, queue.call_tool(invocation))
            }
            // The registry raises no other intent; one would have no caller
            // to hear what it did.
            ToolOutcome::Intent(_) => StartedCall::Done(Box::new(ToolResponses::Error(format!(
                "'{tool}' answered with an action the agent can't carry out"
            )))),
        },
        Started::Pending(pending) => StartedCall::Pending(Box::new(move |_cancel| {
            pending
                .run()
                .result
                .map(|value| ToolResponses::App(AppToolResponse { tool, value }))
        })),
    }
}

/// A job that waits, on its own thread, for the chrome's answer to the UI
/// tool call `pending`, and hands it to the model.
fn wait_on_ui_tool(tool: String, pending: UiToolPending) -> StartedCall {
    StartedCall::Pending(Box::new(move |job| {
        match pending.wait(UI_TOOL_BOUND, || job.cancel().is_cancelled()) {
            UiToolAnswer::Data(value) => Ok(ToolResponses::App(AppToolResponse { tool, value })),
            UiToolAnswer::Error(err) => Err(err),
            UiToolAnswer::Applied => Ok(ToolResponses::App(AppToolResponse {
                tool,
                value: serde_json::json!({ "applied": true }),
            })),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use notedeck::{Rendered, ToolInvocation};

    /// A registry's answer to a UI tool's call: an intent to call it in its
    /// app.
    fn intent(name: &str) -> Started {
        let call = ToolInvocation::new(name, serde_json::json!({ "id": "x" }));
        Started::Done(Rendered::from(ToolOutcome::Intent(AppAction::CallTool(
            call,
        ))))
    }

    /// Run `call`'s job as the round would, off any round.
    fn run_job(call: StartedCall) -> Result<ToolResponses, String> {
        let StartedCall::Pending(job) = call else {
            panic!("a UI tool's call is a job");
        };
        let cancel = crate::tool_round::ToolCancel::default();
        job(&crate::tool_round::JobCtx::detached("call-1", cancel))
    }

    /// A UI tool's call is queued with a reply for the chrome, and the job
    /// hands the model what the tool answered, not just that it was sent.
    #[test]
    fn a_ui_tools_answer_reaches_the_model() {
        let mut queue = AppActionQueue::default();
        let call = started_call("nostr_open_thread", intent("nostr_open_thread"), &mut queue);
        let calls = queue.take_tool_calls();
        let [queued] = <[_; 1]>::try_from(calls).ok().expect("one call queued");
        assert_eq!(queued.invocation.name, "nostr_open_thread");
        assert_eq!(queued.invocation.args, serde_json::json!({ "id": "x" }));
        assert!(queue.take().is_empty(), "not an unanswered action");

        queued
            .reply
            .send(UiToolAnswer::Data(serde_json::json!({ "opened": 1 })));
        let Ok(ToolResponses::App(AppToolResponse { tool, value })) = run_job(call) else {
            panic!("the model hears the tool's data");
        };
        assert_eq!(tool, "nostr_open_thread");
        assert_eq!(value, serde_json::json!({ "opened": 1 }));
    }

    /// The tool's error (a malformed id) reaches the model as an error, and
    /// an intent the window carried out as `applied`.
    #[test]
    fn a_ui_tools_error_and_applied_reach_the_model() {
        let mut queue = AppActionQueue::default();
        let call = started_call("nostr_open_thread", intent("nostr_open_thread"), &mut queue);
        let [queued] = <[_; 1]>::try_from(queue.take_tool_calls()).ok().unwrap();
        queued
            .reply
            .send(UiToolAnswer::Error("not a note id".to_owned()));
        assert_eq!(run_job(call).err().as_deref(), Some("not a note id"));

        let call = started_call("nostr_zap", intent("nostr_zap"), &mut queue);
        let [queued] = <[_; 1]>::try_from(queue.take_tool_calls()).ok().unwrap();
        queued.reply.send(UiToolAnswer::Applied);
        let Ok(ToolResponses::App(AppToolResponse { value, .. })) = run_job(call) else {
            panic!("an applied intent is an answer");
        };
        assert_eq!(value, serde_json::json!({ "applied": true }));
    }

    /// A call no shell took (a host with no chrome) answers so, rather than
    /// waiting out its bound.
    #[test]
    fn a_ui_tool_call_no_shell_ran_is_an_error() {
        let mut queue = AppActionQueue::default();
        let call = started_call("nostr_open_thread", intent("nostr_open_thread"), &mut queue);
        drop(queue.take_tool_calls());
        let err = run_job(call).expect_err("nothing answered");
        assert!(err.contains("no shell"), "{err}");
    }

    /// A string argument the CLI fills from `$AGENTIUM_SESSION` is filled
    /// from the session's identity when the model leaves it out or sends it
    /// empty, never over a value it gave, and never for a variable the
    /// identity lacks.
    #[test]
    fn a_left_out_session_argument_names_the_caller() {
        use notedeck::ToolArg;
        let spec = ToolSpec::new(
            "agentium_spawn",
            "Spawn.",
            vec![
                ToolArg::new("from_session", ToolArgType::String, "The parent.")
                    .env("AGENTIUM_SESSION"),
                ToolArg::new("board", ToolArgType::String, "A board.").env("HEADWAY_BOARD"),
                ToolArg::new("title", ToolArgType::String, "A title."),
            ],
        );
        let identity =
            BTreeMap::from([("AGENTIUM_SESSION".to_owned(), "agentium:a-b-c".to_owned())]);
        let call = |args| AppToolCall {
            name: "agentium_spawn".into(),
            args,
        };

        let filled =
            with_session_defaults(&call(serde_json::json!({ "title": "x" })), &spec, &identity);
        assert_eq!(
            filled.args,
            serde_json::json!({ "title": "x", "from_session": "agentium:a-b-c" })
        );
        let empty = with_session_defaults(
            &call(serde_json::json!({ "from_session": "" })),
            &spec,
            &identity,
        );
        assert_eq!(empty.args["from_session"], "agentium:a-b-c");
        let given = serde_json::json!({ "from_session": "agentium:x-y-z" });
        assert_eq!(
            with_session_defaults(&call(given.clone()), &spec, &identity).args,
            given
        );
        let none = with_session_defaults(&call(serde_json::json!({})), &spec, &BTreeMap::new());
        assert_eq!(none.args, serde_json::json!({}));
    }

    /// A coder on the host runner builds, offering its six tools to the
    /// model; one on the Microsandbox runner is refused at the start, since
    /// there is no guest to run its calls in.
    #[test]
    fn a_coder_runs_on_the_host_and_a_sandboxed_one_is_refused() {
        let coder = |runner: &str| {
            AgentSpec::from_json(&format!(
                r#"{{
                  "slug": "coder",
                  "model": {{ "provider": "anthropic", "model": "claude-opus-5-5" }},
                  "prompt": "Implement the card.",
                  "toolsets": [{{ "kind": "coding", "runner": {runner} }}],
                  "budget": {{ "max_steps": 10 }}
                }}"#
            ))
            .unwrap()
        };
        let host = coder(r#"{ "kind": "local_executor" }"#);
        let tools = SpecTools::for_spec(&host, &[], |_| unreachable!("no sub-agent"), None)
            .expect("a host coder builds");
        assert_eq!(tools.model_tools.len(), 6);
        assert!(tools.model_tools.contains_key("Bash"));

        let sandboxed = coder(r#"{ "kind": "microsandbox", "git": "read_only" }"#);
        let err = SpecTools::for_spec(&sandboxed, &[], |_| unreachable!("no sub-agent"), None)
            .err()
            .expect("no sandbox runner");
        assert!(matches!(err, SpecToolsError::NoSandboxRunner(slug) if slug == "coder"));
    }
}
