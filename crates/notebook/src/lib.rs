//! Core notebook data model, shared by the `notedeck_notebook` egui app and the
//! `notebook_cli` binary — mirroring the `headway` core crate.
//!
//! This is pure logic with no egui/notedeck dependency, so the CLI can link it
//! without pulling in the whole GUI stack:
//!
//! - [`event`] — the nostr-backed vault schema (canvases, nodes, edges, longform
//!   notes) and the reducer that folds events into views;
//! - [`store`] — persistence into nostrdb, translating UI intents into signed
//!   events and sealing the vault into its team-of-one SNS workspace;
//! - [`wordid`] — stable, lossy word-id references to notebook entities.
//!
//! The egui-facing pieces (rendering, the canvas UI, the longform editor, the
//! `jsoncanvas` bridge, the inline reference parser/renderer) stay in
//! `notedeck_notebook`, which re-exports these modules so its own code and the
//! app keep one import path.

pub mod event;
pub mod store;
pub mod wordid;
