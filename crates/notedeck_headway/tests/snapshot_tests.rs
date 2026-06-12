use egui_kittest::Harness;
use notedeck::{App, Notedeck};
use notedeck_headway::Headway;

struct HeadwayTestState {
    notedeck: Notedeck,
    headway: Headway,
    _tmpdir: tempfile::TempDir,
    fonts_installed: bool,
}

fn render_headway(ctx: &egui::Context, state: &mut HeadwayTestState) {
    // Fonts/styles must be installed before the first real frame; do it once.
    if !state.fonts_installed {
        state.notedeck.setup(ctx);
        ctx.style_mut(|s| s.animation_time = 0.0);
        state.fonts_installed = true;
        return;
    }

    let mut app_ctx = state.notedeck.app_context(ctx);
    egui::CentralPanel::default().show(ctx, |ui| {
        state.headway.render(&mut app_ctx, ui);
    });
}

/// Responsive breakpoints to snapshot.
const SIZES: &[(&str, f32, f32)] = &[
    ("headway_mobile", 400.0, 900.0),
    ("headway_tablet", 800.0, 600.0),
    ("headway_desktop", 1200.0, 800.0),
];

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway() {
    let tmpdir = tempfile::TempDir::new().unwrap();

    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let state = HeadwayTestState {
        notedeck,
        headway: Headway::new(),
        _tmpdir: tmpdir,
        fonts_installed: false,
    };

    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(1200.0, 800.0))
        .renderer(notedeck::software_renderer())
        .build_state(render_headway, state);

    // First frame installs fonts; run a couple so layout settles.
    harness.run();

    for &(name, w, h) in SIZES {
        harness.set_size(egui::Vec2::new(w, h));
        harness.run_steps(3);
        harness.snapshot(name);
    }
}
