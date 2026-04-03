//! Shared E2E test harness for Notedeck apps.
//!
//! Provides generic device management, stepping utilities, account cluster
//! management, UI helpers, and fixture builders that any Notedeck app crate
//! can use in its integration tests.

pub mod cluster;
pub mod device;
pub mod fixtures;
pub mod relay;
pub mod stepping;
pub mod ui;

pub use cluster::AccountCluster;
pub use device::{
    build_device_minimal, shutdown_device, AppFactory, DeviceDataDir, DeviceHarness, DeviceState,
};
pub use relay::LocalRelayExt;

use std::sync::Once;

static TRACING_INIT: Once = Once::new();

/// Initializes tracing once for local debugging of end-to-end failures.
pub fn init_tracing() {
    TRACING_INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("notedeck=debug".parse().expect("directive"))
                    .add_directive("enostr=info".parse().expect("directive")),
            )
            .with_test_writer()
            .try_init();
    });
}
