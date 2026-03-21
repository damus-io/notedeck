//! Device stepping utilities for advancing frames in E2E tests.

use std::time::Duration;

use crate::cluster::AccountCluster;
use crate::device::DeviceHarness;

/// Advances a device a fixed number of frames to let UI actions settle.
pub fn step_device_frames(device: &mut DeviceHarness, frames: usize) {
    for _ in 0..frames {
        device.step();
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
    std::thread::sleep(Duration::from_millis(100));
    step_clusters(clusters);
}
