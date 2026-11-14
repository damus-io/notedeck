//! Pure board logic for Headway, a Linear/Trello-style issue tracker built on
//! nostr events.
//!
//! This crate is UI- and app-framework-agnostic: it only depends on `nostrdb`
//! and `enostr`. Both the egui app (`notedeck_headway`) and the CLI
//! (`headway_cli`) build on it.
//!
//! - [`event`] — the pure schema: builders, parsers, and the reducer that folds
//!   a set of nostr events into a [`event::BoardView`]. No I/O.
//! - [`graph`] — the pure dependency-graph model: folds an epic's subtree +
//!   blocking edges out of a [`event::BoardView`] into nodes + directed edges for
//!   the graph view's layout and rendering. No egui.
//! - [`edit`] — reading an edit's arguments against a board: columns, review
//!   records, due dates, estimates and what a comment answers, for the CLI and
//!   the app's tools alike.
//! - [`grep`] and [`list`] — `headway grep`'s search and `headway list`'s
//!   listing and saved views, each as JSON or text, shared by the CLI and
//!   the app's tools.
//! - [`query`] — the issue-list query model: the shared filter parser and
//!   predicate behind the board filter, the list view and `headway list`.
//! - [`render`] — the human text view of a board, a card, the `next`
//!   frontier and the board list, shared by the CLI and the app's tools.
//! - [`store`] — sign + ingest into a local nostrdb, board seeding, and
//!   [`store::apply`], which turns a [`store::BoardAction`] into events.
//! - [`git`] — thin `git` shell-outs (commit, toplevel, repo identity) for
//!   review records, and the resolver that finds (or fetches) a record's
//!   commit on this host and reads it back as a patch.
//! - [`review`] and [`diff`] — `review`'s record gathered from git in the
//!   caller's checkout, and `diff`'s read-back of a record's commit, for the
//!   CLI and the app's tools alike.
//! - [`title`] — splits a card title into `CODE: headline — detail` for
//!   surfaces that show it in little room.
//! - [`views`] — saved list views: an account's [`query::ViewSpec`]s, kept
//!   PNS-wrapped in nostrdb, behind the list's tabs and `headway list --view`.
//! - [`teams`] — the joined-shared-board roster: which SNS channels this account
//!   holds keys for, so both front ends fold and seal the same shared boards.
//! - [`boards`] — the [`boards::Roster`] built from [`teams`], and loading one
//!   board (or all of them) by whichever transport it uses, for consumers that
//!   read boards outside the egui app.

pub mod boards;
pub mod diff;
pub mod edit;
pub mod event;
pub mod fmt;
pub mod git;
pub mod graph;
pub mod grep;
pub mod list;
pub mod query;
pub mod render;
pub mod review;
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

#[cfg(test)]
pub(crate) use ndb_testing::test_config;

pub mod teams;
pub mod title;
pub mod traversal;
pub mod views;
pub mod wordid;
