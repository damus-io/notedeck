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

use std::collections::HashMap;
use std::sync::Arc;

use agentium_core::agent_spec::{AgentSpec, Catalog, CatalogError};
use notedeck::{AppContext, Started, ToolOutcome};

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
}

impl SpecTools {
    /// The tools `spec` offers, from `available`: what the host has
    /// ([`AppContext::tool_specs`]).
    // The butler's spec (headway:dave/picnic-diagram-dizzy) is the first
    // session to run under one.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn for_spec(
        spec: &AgentSpec,
        available: &[notedeck::ToolSpec],
    ) -> Result<Self, CatalogError> {
        Ok(Self::from_catalog(Catalog::build(spec, available)?))
    }

    /// Wrap a built `catalog`.
    #[cfg_attr(not(test), allow(dead_code))]
    fn from_catalog(catalog: Catalog) -> Self {
        let model_tools = Arc::new(catalog.model_tools());
        Self {
            catalog: Arc::new(catalog),
            model_tools,
        }
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
