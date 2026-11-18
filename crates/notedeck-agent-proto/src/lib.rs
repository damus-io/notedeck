//! What notedeck's coding agent and its tool executor say to each other.
//!
//! ```text
//!   host: notedeck (model loop,          guest: notedeck-agent (a static musl
//!   permission gate, diff UI)            binary in a microVM, or any child)
//!   ─────────────────────────            ──────────────────────────────────
//!   HostMessage::Ping ───────────────▶   GuestMessage::Pong { version }
//!   HostMessage::Call { id, call } ──▶   GuestMessage::Chunk { id, .. } …
//!                                        GuestMessage::Result { id, text }
//!   HostMessage::Cancel { id } ──────▶     … or Error / Cancelled { id }
//!   HostMessage::Shutdown ───────────▶   (answers what's in flight, exits)
//! ```
//!
//! This crate is compiled into both ends, so there is one definition of each
//! message, and of each [`CodingCall`](call::CodingCall), rather than two that
//! drift. It holds types, and [`lines::LineReassembler`], the one piece of
//! logic both ends must get identically right; nothing that runs a task.
//! Its dependencies are serde, serde_json and thiserror, so it
//! cross-compiles with the guest.

pub mod call;
pub mod lines;
pub mod wire;
