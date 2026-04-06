//! Core device state and builder functions for E2E test harnesses.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use enostr::FullKeypair;
use nostr::nips::nip19::ToBech32;
use notedeck::Notedeck;
use tempfile::TempDir;

/// One fully booted Notedeck host used as a single test device.
///
/// The tempdir stays alive for the lifetime of the app so its on-disk NostrDB
/// remains valid across the entire scenario.
pub struct DeviceState {
    pub notedeck: Notedeck,
    _data_dir: DeviceDataDir,
}

impl Drop for DeviceState {
    fn drop(&mut self) {
        self.notedeck.shutdown_app();
    }
}

pub enum DeviceDataDir {
    Temp {
        /// Keeps the tempdir alive for the lifetime of the device harness.
        _dir: TempDir,
    },
    External,
}

/// Convenience alias for one full test device.
pub type DeviceHarness = Harness<'static, DeviceState>;

/// Shuts down one device deterministically before dropping the harness.
pub fn shutdown_device(device: DeviceHarness) {
    drop(device);
}

impl eframe::App for DeviceState {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        eframe::App::update(&mut self.notedeck, ctx, frame);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::App::save(&mut self.notedeck, storage);
    }
}

/// App factory: given a fully initialized `Notedeck` and the egui context,
/// install your app. The egui context is provided so apps that need an
/// `AppContext` (like `Damus`) can obtain one via `notedeck.app_context(&ctx)`.
pub type AppFactory = Box<dyn FnOnce(&mut Notedeck, &egui::Context) + Send>;

/// Creates one full device against a fresh temporary data directory.
pub fn build_device(relay: &str, account: &FullKeypair, app_factory: AppFactory) -> DeviceHarness {
    build_device_with_relays(&[relay], account, app_factory)
}

/// Creates one full device against a fresh tempdir with multiple relays.
pub fn build_device_with_relays(
    relays: &[&str],
    account: &FullKeypair,
    app_factory: AppFactory,
) -> DeviceHarness {
    let tmpdir = TempDir::new().expect("tmpdir");
    build_device_in_tmpdir_with_relays(relays, account, tmpdir, app_factory)
}

/// Creates one full device backed by an already-prepared tempdir.
pub fn build_device_in_tmpdir(
    relay: &str,
    account: &FullKeypair,
    tmpdir: TempDir,
    app_factory: AppFactory,
) -> DeviceHarness {
    build_device_in_tmpdir_with_relays(&[relay], account, tmpdir, app_factory)
}

/// Creates one full device backed by an already-prepared tempdir with multiple relays.
pub fn build_device_in_tmpdir_with_relays(
    relays: &[&str],
    account: &FullKeypair,
    tmpdir: TempDir,
    app_factory: AppFactory,
) -> DeviceHarness {
    let data_dir = tmpdir.path().to_path_buf();
    build_device_with_data_dir(
        relays,
        account,
        data_dir,
        DeviceDataDir::Temp { _dir: tmpdir },
        app_factory,
    )
}

/// Creates one full device backed by an externally owned data dir.
pub fn build_device_in_path_with_relays(
    relays: &[&str],
    account: &FullKeypair,
    data_dir: &Path,
    app_factory: AppFactory,
) -> DeviceHarness {
    build_device_with_data_dir(
        relays,
        account,
        data_dir.to_path_buf(),
        DeviceDataDir::External,
        app_factory,
    )
}

fn build_device_with_data_dir(
    relays: &[&str],
    account: &FullKeypair,
    data_dir: PathBuf,
    data_dir_guard: DeviceDataDir,
    app_factory: AppFactory,
) -> DeviceHarness {
    let mut args = vec![
        "notedeck-test".to_owned(),
        "--testrunner".to_owned(),
        "--no-keystore".to_owned(),
        "--nsec".to_owned(),
        account.secret_key.to_bech32().expect("nsec bech32"),
    ];
    for relay in relays {
        args.push("--relay".to_owned());
        args.push((*relay).to_owned());
    }

    // Wrap in Option so we can take() it inside the FnOnce closure
    let mut app_factory = Some(app_factory);

    Harness::builder()
        .with_size(egui::Vec2::new(900.0, 700.0))
        .with_max_steps(24)
        .with_step_dt(0.05)
        .build_eframe(move |cc| {
            let notedeck_ctx = Notedeck::init(&cc.egui_ctx, &data_dir, &args);
            let mut notedeck = notedeck_ctx.notedeck;
            let outbox_session = notedeck_ctx.outbox_session;

            notedeck.setup(&cc.egui_ctx);
            {
                let notedeck_ref = &mut notedeck.notedeck_ref(&cc.egui_ctx, Some(outbox_session));
                notedeck_ref
                    .app_ctx
                    .settings
                    .set_animate_nav_transitions(false);
            }

            // App-specific hook: install the app
            if let Some(factory) = app_factory.take() {
                factory(&mut notedeck, &cc.egui_ctx);
            }

            DeviceState {
                notedeck,
                _data_dir: data_dir_guard,
            }
        })
}
