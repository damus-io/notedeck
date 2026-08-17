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
pub mod teams;
pub mod traversal;
pub mod wordid;
