//! Device stepping utilities for advancing frames in E2E tests.

use std::{
    thread,
    time::{Duration, Instant},
};

use crate::cluster::AccountCluster;
use crate::device::DeviceHarness;

/// Advances a device a fixed number of frames to let UI actions settle.
pub fn step_device_frames(device: &mut DeviceHarness, frames: usize) {
    for _ in 0..frames {
        device.step();
    }
}

/// Advances a device for a wall-clock duration.
pub fn step_device_for(device: &mut DeviceHarness, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        device.step();
        thread::sleep(Duration::from_millis(20));
    }
}

/// Steps one device until a condition succeeds or the timeout expires.
pub fn wait_for_device_condition<T>(
    device: &mut DeviceHarness,
    timeout: Duration,
    context: &str,
    mut condition: impl FnMut(&mut DeviceHarness) -> Result<T, String>,
) -> T {
    let deadline = Instant::now() + timeout;

    loop {
        device.step();

        match condition(device) {
            Ok(value) => return value,
            Err(status) => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {context}; {status}"
                );
            }
        }

        thread::sleep(Duration::from_millis(20));
    }
}

/// Asserts that a condition remains true while stepping a fixed number of frames.
pub fn assert_device_condition_stable(
    device: &mut DeviceHarness,
    frames: usize,
    context: &str,
    mut condition: impl FnMut(&mut DeviceHarness) -> Result<(), String>,
) {
    for _ in 0..frames {
        device.step();

        if let Err(status) = condition(device) {
            panic!("{context}; {status}");
        }

        thread::sleep(Duration::from_millis(20));
    }
}

/// Pumps all devices for one frame.
pub fn step_devices(devices: &mut [DeviceHarness]) {
    for device in devices {
        device.step();
    }
}

/// Pumps a borrowed group of named devices for one frame.
pub fn step_device_group(devices: &mut [&mut DeviceHarness]) {
    for device in devices {
        device.step();
    }
}

/// Pumps every device across all clusters for one frame.
pub fn step_clusters(clusters: &mut [&mut AccountCluster]) {
    for cluster in clusters {
        step_devices(&mut cluster.devices);
    }
}

/// Gives all clusters a short startup window to realize subscriptions.
pub fn warm_up_clusters(clusters: &mut [&mut AccountCluster]) {
    step_clusters(clusters);
    thread::sleep(Duration::from_millis(100));
    step_clusters(clusters);
}
