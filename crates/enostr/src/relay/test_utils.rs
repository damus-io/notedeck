#![cfg(test)]
//! Test utilities for relay testing
//!
//! This module provides mock implementations and helpers for unit and integration tests.

use nostrdb::Filter;

use crate::relay::{OutboxSession, OutboxSubId, OutboxTask};
use crate::Wakeup;

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

/// Returns a task for `id`, panicking when the task is missing.
#[track_caller]
pub fn expect_task<'a>(session: &'a OutboxSession, id: OutboxSubId) -> &'a OutboxTask {
    session
        .tasks
        .get(&id)
        .unwrap_or_else(|| panic!("Expected task for {:?}", id))
}

// ==================== SubRegistry tests ====================

pub fn trivial_filter() -> Vec<Filter> {
    vec![Filter::new().kinds(vec![1]).build()]
}

pub fn filters_json(filters: &[Filter]) -> Vec<String> {
    filters
        .iter()
        .map(|f| f.json().expect("serialize filter to json"))
        .collect()
}
