//! The Coding toolset's tools, run on the host: what
//! [`Runner::LocalExecutor`](super::Runner::LocalExecutor) means.
//!
//! The executor lives in `notedeck-agent-guest`, because the same body runs
//! inside a sandbox guest as the `notedeck-agent` binary. On the host
//! notedeck calls [`Executor::run`] in process: Read, Write and Edit on
//! `std::fs`, Grep and Glob on ripgrep's walker, and only Bash as a child,
//! in its own process group.
//!
//! Nothing here confines a call: a path may be absolute and anywhere, and
//! Bash can do what the user can. On the host the session's permission mode
//! is the only guard (the [`Containment::Host`](super::Containment) column of
//! the approval table), so a session using this runner must never be
//! presented as sandboxed.

pub use notedeck_agent_guest::{ChildEnv, ExecError, Executor};
