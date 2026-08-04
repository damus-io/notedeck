//! Platform-agnostic UI wake-up callback.
//!
//! Backends run their streaming work on background tasks and need a way to nudge
//! whatever is driving the UI to look at newly arrived data. On desktop that is
//! `egui::Context::request_repaint`, but the engine must not depend on egui (it
//! has to drop into iOS and headless hosts), so it speaks to a [`Waker`]: a
//! cheap-to-clone, thread-safe handle wrapping a `wake` callback the host
//! supplies. Desktop `notedeck_dave` builds one that calls `request_repaint`;
//! a headless or test host uses [`Waker::noop`].

use std::sync::Arc;

/// A cloneable, thread-safe handle that nudges the host to advance its UI.
///
/// Cloning is cheap (an `Arc` bump), so a `Waker` is passed by value into
/// spawned tasks and session commands exactly where an `egui::Context` used to
/// be. Calling [`wake`](Self::wake) invokes the host-supplied callback — on
/// desktop that requests a repaint; on a headless host it may do nothing.
#[derive(Clone)]
pub struct Waker(Arc<dyn Fn() + Send + Sync>);

impl Waker {
    /// Build a waker from a host-supplied callback (for example one that calls
    /// `egui::Context::request_repaint`).
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(wake))
    }

    /// A waker that does nothing, for headless hosts and tests where there is no
    /// UI to nudge.
    pub fn noop() -> Self {
        Self::new(|| {})
    }

    /// Nudge the host to advance its UI so it picks up newly arrived data.
    pub fn wake(&self) {
        (self.0)()
    }
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waker")
    }
}
