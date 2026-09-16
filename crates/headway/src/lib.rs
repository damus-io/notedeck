//! Pure board logic for Headway, a Linear/Trello-style issue tracker built on
//! nostr events.
//!
//! This crate is UI- and app-framework-agnostic: it only depends on `nostrdb`
//! and `enostr`. Both the egui app (`notedeck_headway`) and the CLI
//! (`headway_cli`) build on it.
//!
//! - [`event`] — the pure schema: builders, parsers, and the reducer that folds
//!   a set of nostr events into a [`event::BoardView`]. No I/O.
//! - [`store`] — sign + ingest into a local nostrdb, board seeding, and
//!   [`store::apply`], which turns a [`store::BoardAction`] into events.
//! - [`teams`] — the joined-shared-board roster: which SNS channels this account
//!   holds keys for, so both front ends fold and seal the same shared boards.

pub mod event;
pub mod fmt;
pub mod store;
/// Backstop for the tests' nostrdb-ingest waits (see
/// [`nostrdb::SubscriptionStream::wait_for_notes`]). A local ingest commits in
/// microseconds, so seconds of headroom only ever trips when a note is
/// genuinely never coming — a subscription filter that cannot match what the
/// write produced, or a write that silently failed — never on a slow machine.
/// Without it an await like that parks the test thread forever at 0% CPU
/// instead of failing.
#[cfg(test)]
pub(crate) const INGEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub mod teams;
pub mod traversal;
pub mod wordid;
