//! `AgentSpec`: one agentic loop, set up per job.
//!
//! Notedeck runs one native agent loop. How much an agent can do is answered
//! by the spec it runs under, not by building a different agent for each job:
//! which model, which prompt, which toolsets, where each toolset's tools run,
//! what the policy loosens or tightens, and how far it may go before it stops.
//!
//! ```text
//! AgentSpec { slug, model, prompt, toolsets, policy, budget }
//!               │                     │          │
//!               │      Toolset ── Runner ── Containment ─┐
//!               │      ToolSpec ── Capability ───────────┤
//!               │                                 base_approval (the table)
//!               └──────────────────────────────── Policy (widen, tighten)
//!                                                        ▼
//!                                                     Approval
//! ```
//!
//! ## A spec is local config, not wire format
//!
//! A spec never travels in a session event. `BackendType` is serialized into
//! kind-31988 tags, so a spec is **not** a new variant of it; a session run
//! under a spec carries two separate optional tags instead,
//! [`SPEC_TAG`] and [`CONTAINMENT_TAG`], which an old reader ignores. Specs
//! are read from JSON ([`AgentSpec::from_json`]). They are kept apart from
//! backend profiles (`config::BackendProfile`, kind 31992): a profile is an
//! env overlay for an external harness whose loop and tools we don't own,
//! while a spec configures our own loop. What the two share is a credential,
//! and a spec only names one ([`ModelRef::provider`]), never holds it.
//!
//! ## Private data and egress never share a context
//!
//! An agent that can read private boards and also fetch arbitrary URLs is an
//! exfiltration path: an injected page tells it to put board contents in the
//! next URL. [`AgentSpec::validate`] refuses a spec whose toolsets hold
//! private data and reach the network together. Research runs as a
//! [`Toolset::SubAgent`]: another spec, with no private data, that returns a
//! summary.

pub mod catalog;
pub mod policy;
pub mod provider;
#[cfg(test)]
mod tests;
pub mod web;

use serde::{Deserialize, Serialize};

use crate::config::validate_slug;
use crate::tool::{Capability, ToolSpec};
pub use catalog::{Catalog, CatalogError, CatalogTool, ToolSource};
pub use policy::{base_approval, Approval, Containment, Policy, PolicyError, Tighten, Widen};

/// The kind-31988 tag naming the spec a session runs under: `["agent-spec",
/// <slug>]`. Separate from the `backend` tag, so an old reader still sees the
/// backend and simply doesn't know the spec.
pub const SPEC_TAG: &str = "agent-spec";

/// The kind-31988 tag naming where the session's tools run:
/// `["containment", <Containment::as_str>]`.
pub const CONTAINMENT_TAG: &str = "containment";

/// One job's agent: the model, the prompt, the tools, where they run, what
/// they may do unasked, and how far it may go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    /// Stable name, e.g. `butler`; the value of a session's [`SPEC_TAG`].
    pub slug: String,
    /// The model and the provider that serves it.
    pub model: ModelRef,
    /// The system prompt: the agent's role.
    pub prompt: String,
    /// The tools it may call, grouped by where they come from.
    pub toolsets: Vec<Toolset>,
    /// Adjustments to the approval table.
    #[serde(default)]
    pub policy: Policy,
    /// When the loop stops on its own.
    pub budget: Budget,
}

/// The model a spec runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    /// The provider's slug: an OpenAI backend profile of the account's, or a
    /// built-in (`openai`, `anthropic`, `ollama`), either of which holds the
    /// endpoint and the credential. A spec never carries a secret;
    /// [`provider::resolve`] finds them.
    pub provider: String,
    /// The model id the provider knows it by.
    pub model: String,
}

/// A group of tools a spec may call, and where they run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Toolset {
    /// Notedeck's own app tools (headway, notebook), run in-process over the
    /// user's nostr data.
    Notedeck {
        /// Which apps' tools, e.g. `["headway", "notebook"]`; never empty.
        apps: Vec<String>,
    },
    /// Web search and fetch, in-process.
    Web,
    /// File and shell tools over a working tree, run by `runner`.
    Coding {
        /// Where the tools execute.
        runner: Runner,
    },
    /// Another spec, called as a tool: a question in, its summary out. The
    /// child keeps its own tools and budget; the parent sees only the
    /// answer.
    SubAgent {
        /// The child spec's slug.
        spec: String,
    },
}

/// What a [`Toolset`] is, without its settings: for duplicate checks and
/// error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolsetKind {
    Notedeck,
    Web,
    Coding,
    SubAgent,
}

/// Where a [`Toolset::Coding`] toolset's tools execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Runner {
    /// The tool executor as a host subprocess: no VM.
    LocalExecutor,
    /// The tool executor in a microVM guest.
    Microsandbox {
        /// Whether the guest may write the repository's `.git`.
        git: SandboxGit,
    },
}

/// What a sandboxed guest may do to the repository's `.git`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxGit {
    /// Mounted read-only: commits, branches and rebases fail in the kernel.
    ReadOnly,
    /// Mounted read-write: the agent can commit.
    ReadWrite,
}

impl Runner {
    /// The table column this runner puts its tools in.
    pub fn containment(self) -> Containment {
        match self {
            Self::LocalExecutor => Containment::Host,
            Self::Microsandbox {
                git: SandboxGit::ReadOnly,
            } => Containment::SandboxedReview,
            Self::Microsandbox {
                git: SandboxGit::ReadWrite,
            } => Containment::SandboxedCommit,
        }
    }
}

impl Toolset {
    /// What kind of toolset this is.
    pub fn kind(&self) -> ToolsetKind {
        match self {
            Self::Notedeck { .. } => ToolsetKind::Notedeck,
            Self::Web => ToolsetKind::Web,
            Self::Coding { .. } => ToolsetKind::Coding,
            Self::SubAgent { .. } => ToolsetKind::SubAgent,
        }
    }

    /// The table column this toolset's tools run in. Only a coding toolset
    /// has a runner; the rest run inside notedeck.
    pub fn containment(&self) -> Containment {
        match self {
            Self::Coding { runner } => runner.containment(),
            Self::Notedeck { .. } | Self::Web | Self::SubAgent { .. } => Containment::InProcess,
        }
    }

    /// Whether its tools read data the user hasn't published: private
    /// boards and notebooks, a private repository.
    pub fn holds_private_data(&self) -> bool {
        match self {
            Self::Notedeck { .. } | Self::Coding { .. } => true,
            Self::Web | Self::SubAgent { .. } => false,
        }
    }

    /// Whether its tools reach the network outside notedeck. A sub-agent
    /// call doesn't: its child does, in a context with none of the parent's
    /// data.
    pub fn has_egress(&self) -> bool {
        matches!(self, Self::Web)
    }
}

/// When the loop stops on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Most tool-call steps in one run; at least 1.
    pub max_steps: u32,
    /// Most spend in one run, in US cents, when the provider reports cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_cents: Option<u32>,
    /// Wall-clock limit for one run, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl Budget {
    /// The step cap a loop runs under when no spec gives one: room for a
    /// real search-and-read task, and a stop for a model that keeps calling
    /// tools without ever answering.
    pub const DEFAULT_MAX_STEPS: u32 = 25;
}

/// Why an [`AgentSpec`] can't be used.
#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    /// The JSON doesn't parse as a spec.
    #[error("agent spec doesn't parse: {0}")]
    Parse(#[from] serde_json::Error),
    /// The slug isn't a valid name.
    #[error("invalid agent spec slug {0:?}: use 1-63 of a-z, 0-9 and -, not starting with -")]
    Slug(String),
    /// `model.provider` or `model.model` is empty.
    #[error("agent spec {0} names no model or provider")]
    Model(String),
    /// The spec has no tools at all.
    #[error("agent spec {0} has no toolsets")]
    NoToolsets(String),
    /// A toolset kind appears twice.
    #[error("agent spec {spec} lists the {kind:?} toolset twice")]
    DuplicateToolset { spec: String, kind: ToolsetKind },
    /// A Notedeck toolset names no apps.
    #[error("agent spec {0}: a notedeck toolset names its apps")]
    NoApps(String),
    /// A sub-agent toolset calls the spec itself.
    #[error("agent spec {0} calls itself as a sub-agent")]
    SelfSubAgent(String),
    /// Private data and egress in one context.
    #[error(
        "agent spec {0} reads private data and reaches the network in one context: \
         move the web toolset into a sub-agent spec"
    )]
    PrivateDataWithEgress(String),
    /// `budget.max_steps` is zero.
    #[error("agent spec {0} has a zero step budget")]
    ZeroSteps(String),
    /// A spec called as a sub-agent holds private data, so the question's
    /// answer could carry it out (see [`AgentSpec::check_sub_agent`]).
    #[error("agent spec {0} can't be a sub-agent: it reads private data")]
    SubAgentHoldsPrivateData(String),
    /// A spec called as a sub-agent calls one of its own.
    #[error("agent spec {0} can't be a sub-agent: it calls a sub-agent of its own")]
    NestedSubAgent(String),
    /// The policy is malformed.
    #[error("agent spec {spec}: {source}")]
    Policy {
        spec: String,
        #[source]
        source: PolicyError,
    },
}

impl AgentSpec {
    /// Parse a spec from JSON and [`validate`](Self::validate) it. Unknown
    /// fields are an error, so a misspelled `tighten` never silently drops a
    /// restriction.
    pub fn from_json(json: &str) -> Result<Self, SpecError> {
        let spec: Self = serde_json::from_str(json)?;
        spec.validate()?;
        Ok(spec)
    }

    /// Check the spec can run: a valid slug, a model, at least one toolset
    /// and none twice, apps named, no self-call, never private data beside
    /// egress, a non-zero step budget, and a well-formed policy.
    pub fn validate(&self) -> Result<(), SpecError> {
        let name = || self.slug.clone();
        validate_slug(&self.slug).map_err(|_| SpecError::Slug(name()))?;
        if self.model.provider.trim().is_empty() || self.model.model.trim().is_empty() {
            return Err(SpecError::Model(name()));
        }
        if self.toolsets.is_empty() {
            return Err(SpecError::NoToolsets(name()));
        }
        for (i, toolset) in self.toolsets.iter().enumerate() {
            let kind = toolset.kind();
            if self.toolsets[..i].iter().any(|t| t.kind() == kind) {
                return Err(SpecError::DuplicateToolset { spec: name(), kind });
            }
            match toolset {
                Toolset::Notedeck { apps } if apps.is_empty() => {
                    return Err(SpecError::NoApps(name()))
                }
                Toolset::SubAgent { spec } if *spec == self.slug => {
                    return Err(SpecError::SelfSubAgent(name()))
                }
                _ => {}
            }
        }
        let private = self.toolsets.iter().any(Toolset::holds_private_data);
        let egress = self.toolsets.iter().any(Toolset::has_egress);
        if private && egress {
            return Err(SpecError::PrivateDataWithEgress(name()));
        }
        if self.budget.max_steps == 0 {
            return Err(SpecError::ZeroSteps(name()));
        }
        self.policy.validate().map_err(|source| SpecError::Policy {
            spec: name(),
            source,
        })
    }

    /// Check this spec can be called as another's sub-agent: it holds no
    /// private data, since whatever it reads goes back to the parent and
    /// whatever its context holds an injected page could send out, and it
    /// calls no sub-agent itself, so isolation is one level deep and easy
    /// to see.
    pub fn check_sub_agent(&self) -> Result<(), SpecError> {
        if self.toolsets.iter().any(Toolset::holds_private_data) {
            return Err(SpecError::SubAgentHoldsPrivateData(self.slug.clone()));
        }
        if self
            .toolsets
            .iter()
            .any(|t| t.kind() == ToolsetKind::SubAgent)
        {
            return Err(SpecError::NestedSubAgent(self.slug.clone()));
        }
        Ok(())
    }

    /// What happens when this spec's agent calls `tool` from `toolset`: the
    /// table's cell for the tool's class under the toolset's containment,
    /// adjusted by the spec's policy.
    pub fn approval(&self, toolset: &Toolset, tool: &ToolSpec) -> Approval {
        self.approval_for(tool.name(), tool.capability_class(), toolset.containment())
    }

    /// [`approval`](Self::approval) from the parts, for a tool known only by
    /// name and class (a sub-agent call, a tool from another process).
    pub fn approval_for(
        &self,
        tool: &str,
        class: Capability,
        containment: Containment,
    ) -> Approval {
        self.policy.resolve(tool, class, containment)
    }

    /// Where this spec's tools run, for the session's [`CONTAINMENT_TAG`]:
    /// its coding toolset's, else in-process.
    pub fn containment(&self) -> Containment {
        self.toolsets
            .iter()
            .find_map(|t| match t {
                Toolset::Coding { runner } => Some(runner.containment()),
                _ => None,
            })
            .unwrap_or(Containment::InProcess)
    }

    /// The session tags naming this spec and its containment.
    pub fn session_tags(&self) -> SessionSpecTags<'_> {
        SessionSpecTags {
            spec: &self.slug,
            containment: self.containment(),
        }
    }
}

/// The values a session run under a spec carries, beside (never in) its
/// `backend` tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSpecTags<'a> {
    /// The [`SPEC_TAG`] value.
    pub spec: &'a str,
    /// The [`CONTAINMENT_TAG`] value, via [`Containment::as_str`].
    pub containment: Containment,
}
