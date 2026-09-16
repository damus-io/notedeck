pub mod claude;
mod codex;
mod codex_protocol;
mod openai;
mod remote;
mod session_info;
pub(crate) mod shared;
mod task_tracker;
mod tool_summary;
pub mod traits;

pub(crate) use tool_summary::truncate_output;

pub use claude::ClaudeBackend;
pub use codex::CodexBackend;
pub use openai::OpenAiBackend;
pub use remote::RemoteOnlyBackend;
pub use traits::{AiBackend, BackendType, Model};

#[cfg(test)]
use notedeck::Waker;

/// A [`Waker`] that records how many times production code woke the UI.
///
/// Backend tests otherwise build [`Waker::noop`], which cannot tell a missing
/// `waker.wake()` from a present one: deleting any of the wake calls in
/// `shared`/`claude` left the whole backend suite green while the UI silently
/// stopped repainting. Hand [`waker`](Self::waker) to the code under test and
/// assert on [`wakes`](Self::wakes).
#[cfg(test)]
pub(crate) struct CountingWaker {
    waker: Waker,
    wakes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl CountingWaker {
    pub(crate) fn new() -> Self {
        let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = wakes.clone();
        Self {
            waker: Waker::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }),
            wakes,
        }
    }

    /// The waker to hand to the code under test.
    pub(crate) fn waker(&self) -> &Waker {
        &self.waker
    }

    /// How many times the code under test has woken the UI.
    pub(crate) fn wakes(&self) -> usize {
        self.wakes.load(std::sync::atomic::Ordering::Relaxed)
    }
}
