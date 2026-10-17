//! The harness every Cashflow UI test builds on: a real [`Notedeck`] host in a
//! throwaway data dir, driving one [`CashflowApp`] through `update` and
//! `render` the way the chrome does.
//!
//! Later cards extend it — a synthetic journal in the data dir, an edits vault
//! in the host's nostrdb — rather than each suite growing its own.

#![allow(dead_code)]

use egui_kittest::Harness;
use notedeck::{App, Notedeck};
use notedeck_cashflow::CashflowApp;

/// A desktop window, wide enough for the nav rail.
pub const DESKTOP: egui::Vec2 = egui::Vec2::new(1200.0, 800.0);

/// A phone window, below `notedeck::ui::is_narrow`'s breakpoint, so the nav
/// collapses to a tab strip.
pub const MOBILE: egui::Vec2 = egui::Vec2::new(400.0, 800.0);

/// What a Cashflow harness holds between frames.
pub struct CashflowTestState {
    /// The host: nostrdb, accounts, job pool, all in [`Self::tmpdir`].
    pub notedeck: Notedeck,
    /// The app under test.
    pub app: CashflowApp,
    /// The host's data dir; dropped (and deleted) with the harness.
    pub tmpdir: tempfile::TempDir,
    /// Whether the first frame has installed notedeck's fonts and styles.
    setup_done: bool,
}

/// One frame: install notedeck's look on the first, then `update` and
/// `render` the app on every later one.
fn render_cashflow(ui: &mut egui::Ui, state: &mut CashflowTestState) {
    notedeck::test_harness::full_window(ui, |ui| {
        if !state.setup_done {
            let ctx = ui.ctx().clone();
            state.notedeck.setup(&ctx);
            ctx.global_style_mut(|s| s.animation_time = 0.0);
            state.setup_done = true;
            // Fonts installed during a frame apply from the next one.
            ctx.request_repaint();
            return;
        }

        let mut app_ctx = state.notedeck.app_context();
        state.app.update(&mut app_ctx);
        // Edge to edge, as the chrome hands an app its `Ui`.
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                state.app.render(&mut app_ctx, ui);
            });
    });
}

/// A booted host in a fresh data dir, running `app`.
pub fn test_state(app: CashflowApp) -> CashflowTestState {
    let tmpdir = tempfile::TempDir::new().expect("tmpdir");
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);
    CashflowTestState {
        notedeck,
        app,
        tmpdir,
        setup_done: false,
    }
}

/// A harness of `size` running `app`, with no renderer: for behavioural tests
/// that query and click, and run under a plain `cargo test`.
pub fn harness(app: CashflowApp, size: egui::Vec2) -> Harness<'static, CashflowTestState> {
    let mut harness = Harness::builder()
        .with_size(size)
        .build_ui_state(render_cashflow, test_state(app));
    harness.run();
    harness
}

/// A harness of `size` running `app` on the software renderer, for
/// snapshots. Needs lavapipe: run its tests via `scripts/snapshot-test`.
pub fn snapshot_harness(app: CashflowApp, size: egui::Vec2) -> Harness<'static, CashflowTestState> {
    let mut harness = Harness::builder()
        .with_size(size)
        .renderer(notedeck::software_renderer())
        .build_ui_state(render_cashflow, test_state(app));
    harness.run();
    harness
}
