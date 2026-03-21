//! Account cluster: one logical account with several independent devices.

use enostr::FullKeypair;

use crate::device::{build_device_with_relays, AppFactory, DeviceHarness};

/// One logical account with several independent devices.
pub struct AccountCluster {
    /// Human-readable label used in assertion messages.
    pub name: &'static str,
    /// Shared account keypair for every device in the cluster.
    pub account: FullKeypair,
    /// Cached `npub` used for UI actions.
    pub npub: String,
    /// Independent full Notedeck hosts for this account.
    pub devices: Vec<DeviceHarness>,
}

impl AccountCluster {
    /// Builds an account cluster with `device_count` independent hosts.
    ///
    /// The `app_factory_fn` is called once per device to produce the app factory closure.
    pub fn new<F>(name: &'static str, relay: &str, device_count: usize, app_factory_fn: F) -> Self
    where
        F: Fn() -> AppFactory,
    {
        let account = FullKeypair::generate();
        let npub = account.pubkey.npub().expect("account npub");
        let devices = (0..device_count)
            .map(|_| build_device_with_relays(&[relay], &account, app_factory_fn()))
            .collect();

        Self {
            name,
            account,
            npub,
            devices,
        }
    }

    /// Returns one specific device in the cluster for targeted UI actions.
    pub fn device(&mut self, index: usize) -> &mut DeviceHarness {
        self.devices
            .get_mut(index)
            .unwrap_or_else(|| panic!("missing device {index} for cluster {}", self.name))
    }
}
