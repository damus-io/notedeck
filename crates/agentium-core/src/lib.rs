//! # agentium-core
//!
//! Platform-neutral, egui-free engine for remote-agentic sessions, extracted
//! from `notedeck_dave`. This crate owns the pure nostr **session protocol**:
//! building and parsing the conversation/archive/state/spawn/run-config events,
//! the JSONL transcript representation, and reconstructing a session's JSONL
//! from nostrdb.
//!
//! It is designed to drop into any platform — local or remote — with no UI or
//! app-host dependencies. See the `agentium-core` epic on the Headway board for
//! the full extraction plan; this is the scaffold with the pure protocol
//! modules moved over.

pub mod config;
pub mod file_update;
pub mod messages;
pub mod session;
pub mod session_converter;
pub mod session_events;
pub mod session_jsonl;
pub mod session_loader;
pub mod session_reconstructor;
pub mod tools;
mod util;
