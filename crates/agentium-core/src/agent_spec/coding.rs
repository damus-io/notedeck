//! What the coding toolset's tools can reach, ahead of its catalog.
//!
//! The coding tools themselves (Read, Write, Edit, Grep, Glob, Bash) are
//! declared by the coding catalog. [`AgentSpec::validate`](super::AgentSpec::validate)
//! needs one fact about them before that exists: which ones reach the
//! network. A shell does: a host subprocess has the user's network, and no
//! runner yet gives a guest none, so `curl` works wherever Bash runs. A
//! spec closes a coding toolset's egress by making every tool listed here
//! impossible, and only then may it hold private data beside it.

use crate::tool::Capability;

/// A coding tool that can reach the network, by the name and class its
/// [`ToolSpec`](crate::tool::ToolSpec) declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkTool {
    /// The tool's name as the model sees it.
    pub name: &'static str,
    /// Its capability class, which a class tighten matches.
    pub class: Capability,
}

/// Every coding tool that reaches the network. The coding catalog adds any
/// tool of its own that can (a fetch, a package install) here, or validate
/// lets it sit beside private data.
pub const NETWORK_TOOLS: &[NetworkTool] = &[NetworkTool {
    name: "Bash",
    class: Capability::MutateWorkspace,
}];
