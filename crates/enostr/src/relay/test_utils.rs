//! Test utilities for relay unit tests.

use nostrdb::Filter;

mod enostr_api {
    pub use crate::relay::NormRelayUrl;
}

#[path = "../../../enostr_test_support/src/relay.rs"]
mod capture_relay;
pub use capture_relay::{
    create_filtered_capture_relay_with_handler, create_req_capture_relay,
    create_text_capture_relay, CaptureNotify, CaptureRelayResponse, CapturedTextFrames,
};

pub trait Wakeup: Send + Sync + Clone + 'static {
    fn wake(&self);
}

/// A mock Wakeup implementation that tracks how many times wake() was called.
///
/// This is useful for unit tests to verify that wakeups are triggered correctly
/// without needing a real UI/event loop.
#[derive(Clone)]
pub struct MockWakeup {}

impl MockWakeup {
    /// Create a new MockWakeup with zero wakeup count.
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for MockWakeup {
    fn default() -> Self {
        Self::new()
    }
}

impl Wakeup for MockWakeup {
    fn wake(&self) {}
}

// ==================== Filter test helpers ====================

pub fn trivial_filter() -> Vec<Filter> {
    vec![Filter::new().kinds(vec![1]).build()]
}

pub fn filters_json(filters: &[Filter]) -> Vec<String> {
    filters
        .iter()
        .map(|f| f.json().expect("serialize filter to json"))
        .collect()
}
