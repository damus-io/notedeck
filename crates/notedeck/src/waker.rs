//! The host's wake signal: how background work asks for another pass.
//!
//! Notedeck's hosts do not share a way to schedule a frame. The GUI host has an
//! eframe integration behind `egui::Context::request_repaint`; the `--headless`
//! host has no window and no integration, only a run loop asleep on a
//! `tokio::sync::Notify`. Everything that produces data off the render thread —
//! a streaming AI session, a relay bridge, nostrdb's ingester, an
//! [`AsyncLoader`](crate::AsyncLoader) worker — has to reach whichever one is
//! running without knowing which it is, so it speaks to a [`Waker`]: a
//! cheap-to-clone, thread-safe handle wrapping the callback that host supplied.
//!
//! Every host must supply a real one. A host that cannot be woken will sit on
//! completed work until something else happens to tick it, which is how the
//! headless run loop came to stall dave mid-stream (that bug is
//! headway:notedeck/ginger-twice-gate). [`Waker::noop`] exists for tests, which
//! have no loop to wake, and is not an answer for a host.

use std::sync::Arc;

/// A cloneable, thread-safe handle that asks the host for another pass.
///
/// Cloning is cheap (an `Arc` bump), so a `Waker` is passed by value into
/// spawned tasks and session commands exactly where an `egui::Context` used to
/// be. Calling [`wake`](Self::wake) invokes the host-supplied callback — in the
/// GUI that requests a repaint, headless it signals the run loop.
#[derive(Clone)]
pub struct Waker(Arc<dyn Fn() + Send + Sync>);

impl Waker {
    /// Build a waker from a host-supplied callback.
    ///
    /// The callback runs on whatever thread called [`wake`](Self::wake), so it
    /// must be cheap and non-blocking: a `request_repaint`, a `notify_one`.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(wake))
    }

    /// A waker backed by an egui context, for the GUI host: waking requests a
    /// repaint, which is what eframe schedules the next frame off.
    ///
    /// Holds a clone of the context (itself an `Arc` handle), so the waker
    /// outlives the borrow it was built from.
    pub fn egui(ctx: &egui::Context) -> Self {
        let ctx = ctx.clone();
        Self::new(move || ctx.request_repaint())
    }

    /// A waker that does nothing. **Tests only** — a test has no run loop to
    /// wake, so dropping the signal costs it nothing.
    ///
    /// Not for a headless or embedded host: those have a loop that only advances
    /// when woken, and handing them this silently converts "there is work now"
    /// into "wait for the idle cap", or into a hang. See the module docs.
    pub fn noop() -> Self {
        Self::new(|| {})
    }

    /// Ask the host for another pass, so it picks up newly arrived data.
    pub fn wake(&self) {
        (self.0)()
    }
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waker")
    }
}
