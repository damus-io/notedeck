//! Generic UI interaction helpers for E2E tests.

use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;

use crate::device::DeviceHarness;

/// Waits until a labeled UI node appears on the given device.
pub fn wait_for_label(device: &mut DeviceHarness, label: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;

    loop {
        device.step();
        if device.query_all_by_label(label).next().is_some() {
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
pub fn click_enabled_label(device: &DeviceHarness, label: &str) {
    let node = device
        .query_all_by_label(label)
        .find(|node| !node.is_disabled())
        .unwrap_or_else(|| panic!("no enabled UI node found for label {label:?}"));
    node.click();
}
