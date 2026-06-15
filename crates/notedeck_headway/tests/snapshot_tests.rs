use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
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

/// Open a card's detail view and snapshot it on both a wide and a narrow
/// viewport to exercise the responsive modal/sheet behaviour.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_detail() {
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

    harness.run();

    // `click()` would dispatch an accesskit action to the (non-interactive)
    // title label and do nothing; `simulate_click()` issues a real pointer
    // click at that position, which lands on the drag-source card surface
    // underneath and opens the detail view.
    harness
        .get_by_label("Define nostr event model for boards")
        .simulate_click();
    harness.run();

    for &(name, w, h) in &[
        ("headway_detail_desktop", 1200.0, 800.0),
        ("headway_detail_mobile", 400.0, 900.0),
    ] {
        harness.set_size(egui::Vec2::new(w, h));
        harness.run_steps(3);
        harness.snapshot(name);
    }
}

/// Drive the add-column flow through the real UI: open the composer, type a
/// title, commit, and confirm a column was added and the composer closed.
/// This exercises the full button → BoardAction → model → re-render path that
/// the static snapshots don't touch.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn add_column_flow() {
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

    // Wide enough that all four seeded columns plus the "+ Add column"
    // affordance are on-screen, so the simulated clicks land on them.
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(1600.0, 800.0))
        .renderer(notedeck::software_renderer())
        .build_state(render_headway, state);
    harness.run();

    // Precondition: the seeded board has four columns.
    harness.get_by_label("7 cards · 4 columns");

    // Open the add-column composer.
    harness.get_by_label("+ Add column").simulate_click();
    harness.run();

    // Type into the (auto-focused) composer field, then commit via "Add". The
    // field has no label, so target it by its text-input role.
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .type_text("Ideas");
    harness.run();
    harness.get_by_label("Add").simulate_click();
    harness.run_steps(2);

    // A fifth column now exists (asserted via the always-visible board summary,
    // since the new column itself renders off-screen to the right), and the
    // composer has closed.
    harness.get_by_label("7 cards · 5 columns");
    assert!(
        harness.query_by_label("Add").is_none(),
        "composer should close after adding a column"
    );
}
