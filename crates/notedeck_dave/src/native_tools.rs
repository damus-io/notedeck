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
//! [`LocalExecutor`](agentium_core::agent_spec::local_executor::LocalExecutor)
//! in the session's working tree. Only that runner exists: a spec naming the
//! Microsandbox runner is refused when its session starts
//! ([`SpecToolsError::NoSandboxRunner`]), since running its calls on the
//! host would present an unconfined session as sandboxed.

use std::collections::HashMap;
use std::sync::Arc;

use agentium_core::agent_spec::{AgentSpec, Catalog, CatalogError, Runner, Toolset};
use notedeck::{AppContext, Started, ToolOutcome};

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

/// Start the app tool `call` through the host's tool registry: what the
/// frame's start pass runs once the call may start.
pub(crate) fn start_app_call(ctx: &mut AppContext<'_>, call: &AppToolCall) -> StartedCall {
    // A model-driven call never signs with a comment key (see
    // `ToolContext::comment_signer`).
    started_call(
        &call.name,
        ctx.start_tool_rendered(&call.name, &call.args, None),
    )
}

/// What a registry's `started` call of the tool `name` is to the round: a
/// value answers at once, a slow tool's pending work becomes a job off the
/// frame.
pub(crate) fn started_call(name: &str, started: Started) -> StartedCall {
    let tool = name.to_owned();
    match started {
        Started::Done(rendered) => StartedCall::Done(Box::new(match rendered.outcome {
            ToolOutcome::Data(value) => ToolResponses::App(AppToolResponse { tool, value }),
            ToolOutcome::Error(err) => ToolResponses::Error(err),
            ToolOutcome::Intent(_) => ToolResponses::Error(format!(
                "'{tool}' acts on the window, which an agent can't use"
            )),
        })),
        Started::Pending(pending) => StartedCall::Pending(Box::new(move |_cancel| {
            pending
                .run()
                .result
                .map(|value| ToolResponses::App(AppToolResponse { tool, value }))
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
