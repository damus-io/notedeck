//! Generic UI interaction helpers for E2E tests.

use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

/// Waits until a labeled UI node appears on the given harness.
pub fn wait_for_label<S>(harness: &mut Harness<'_, S>, label: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;

    loop {
        harness.step();
        if harness.query_all_by_label(label).next().is_some() {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for UI label {label:?}"
        );

        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Clicks the enabled UI node matching the given label.
pub fn click_enabled_label<S>(harness: &Harness<'_, S>, label: &str) {
    let node = harness
        .query_all_by_label(label)
        .find(|node| !node.is_disabled())
        .unwrap_or_else(|| panic!("no enabled UI node found for label {label:?}"));
    node.click();
}
