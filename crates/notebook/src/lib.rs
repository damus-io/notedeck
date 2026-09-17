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
/// Backstop for the tests' nostrdb-ingest waits (see
/// [`nostrdb::SubscriptionStream::wait_for_notes`]). A local ingest commits in
/// microseconds, so seconds of headroom only ever trips when a note is
/// genuinely never coming — a subscription filter that cannot match what the
/// write produced, or a write that silently failed — never on a slow machine.
/// Without it an await like that parks the test thread forever at 0% CPU
/// instead of failing.
#[cfg(test)]
pub(crate) const INGEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A [`nostrdb::Config`] with a small mapsize, for tests.
///
/// On Windows LMDB actually allocates the full mapsize on disk rather than only
/// mapping it virtually, so tests taking nostrdb's large default exhaust the
/// disk on CI runners. Mirrors `notedeck::test_util::test_config`, which this
/// crate can't reach (it doesn't depend on notedeck).
#[cfg(test)]
pub(crate) fn test_config() -> nostrdb::Config {
    if cfg!(target_os = "windows") {
        nostrdb::Config::new().set_mapsize(32 * 1024 * 1024) // 32 MiB
    } else {
        nostrdb::Config::new()
    }
}

/// `block_on` for the tests, on a current-thread runtime with timers enabled.
///
/// The ingest waits below are bounded with [`tokio::time::timeout`], which needs
/// a Tokio timer; `pollster::block_on` drives a future with no reactor at all, so
/// a wait under it panics with "there is no reactor running".
#[cfg(test)]
pub(crate) fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("current-thread runtime")
        .block_on(fut)
}
