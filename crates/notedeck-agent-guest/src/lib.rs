//! notedeck's coding tool executor.
//!
//! One body, two ways to run it:
//!
//! ```text
//!   host coder ── agentium-core Runner::LocalExecutor ──▶ Executor::run, in notedeck
//!
//!   sandboxed agent ── HostMessage lines ──▶ notedeck-agent (this crate's binary,
//!                     ◀── GuestMessage lines ──   in a microVM) ──▶ serve ──▶ Executor::run
//! ```
//!
//! [`Executor`] runs the six coding tools against a working tree. [`serve`]
//! is the binary's loop: it reads [`HostMessage`]s, runs each call
//! concurrently, and answers [`GuestMessage`]s, using the wire types in
//! `notedeck-agent-proto` so host and guest share one definition.
//!
//! ## Building the guest binary
//!
//! libkrun virtualises rather than emulates, so a guest's architecture is
//! its host's, and the binary has to exist for both arm64 and x86-64.
//! `scripts/build-guest-agent` builds both, statically linked against musl,
//! to `target/<triple>/small/notedeck-agent`, and stamps the commit into the
//! binary as `NOTEDECK_AGENT_BUILD`, which it answers a ping with. The
//! cross-compile needs only `rustup target add` and rust-lld
//! (`.cargo/config.toml`), which holds as long as nothing in this crate's
//! graph builds C.
//!
//! [`HostMessage`]: notedeck_agent_proto::wire::HostMessage
//! [`GuestMessage`]: notedeck_agent_proto::wire::GuestMessage

pub mod executor;
#[cfg(unix)]
pub mod ipc_bridge;
mod serve;

pub use executor::{ChildEnv, ExecError, Executor};
pub use serve::serve;
