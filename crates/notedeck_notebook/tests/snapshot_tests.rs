use egui_kittest::Harness;
use notedeck::{App, Notedeck};
use notedeck_notebook::Notebook;

struct NotebookTestState {
    notedeck: Notedeck,
    notebook: Notebook,
    _tmpdir: tempfile::TempDir,
    setup_done: bool,
}

fn render_notebook(ctx: &egui::Context, state: &mut NotebookTestState) {
    // Fonts/styles must be installed before the first real frame; do it once.
    if !state.setup_done {
        state.notedeck.setup(ctx);
        ctx.style_mut(|s| s.animation_time = 0.0);
        state.setup_done = true;
        return;
    }

    let mut app_ctx = state.notedeck.app_context(ctx);
    egui::CentralPanel::default().show(ctx, |ui| {
        state.notebook.render(&mut app_ctx, ui);
    });
}

fn notebook_harness(size: egui::Vec2) -> Harness<'static, NotebookTestState> {
    notebook_harness_with(size, Notebook::new())
}

fn notebook_harness_with(
    size: egui::Vec2,
    notebook: Notebook,
) -> Harness<'static, NotebookTestState> {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let state = NotebookTestState {
        notedeck,
        notebook,
        _tmpdir: tmpdir,
        setup_done: false,
    };

    let mut harness = Harness::builder()
        .with_size(size)
        .with_max_steps(16)
        .renderer(notedeck::software_renderer())
        .build_state(render_notebook, state);

    // First frame installs fonts; pump a few more so the scene lays out. The
    // text nodes' scroll areas keep requesting repaints, so use a fixed step
    // count rather than run()'s convergence check.
    harness.run_steps(4);
    harness
}

/// Render the demo canvas at a desktop viewport and snapshot it. Exercises the
/// markdown rendering in text nodes, edges/arrows, and node frames.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook() {
    let mut harness = notebook_harness(egui::Vec2::new(1200.0, 800.0));
    harness.run_steps(3);
    harness.snapshot("notebook_demo");
}

/// A small canvas placing each preset color (and a hex color) near the origin,
/// so the colored frames/edges are all in the initial viewport. Verifies the
/// JSONCanvas color field is honored for node fill/stroke and edge stroke.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_colors() {
    let canvas: jsoncanvas::JsonCanvas = r##"{
      "nodes": [
        {"id":"n1","type":"text","text":"# Red","x":40,"y":40,"width":200,"height":90,"color":"1"},
        {"id":"n2","type":"text","text":"# Orange","x":300,"y":40,"width":200,"height":90,"color":"2"},
        {"id":"n3","type":"text","text":"# Yellow","x":560,"y":40,"width":200,"height":90,"color":"3"},
        {"id":"n4","type":"text","text":"# Green","x":40,"y":200,"width":200,"height":90,"color":"4"},
        {"id":"n5","type":"text","text":"# Cyan","x":300,"y":200,"width":200,"height":90,"color":"5"},
        {"id":"n6","type":"text","text":"# Purple","x":560,"y":200,"width":200,"height":90,"color":"6"},
        {"id":"n7","type":"text","text":"# Hex #3b82f6","x":300,"y":360,"width":200,"height":90,"color":"#3b82f6"},
        {"id":"n8","type":"text","text":"plain node","x":40,"y":360,"width":200,"height":90}
      ],
      "edges": [
        {"id":"e1","fromNode":"n1","fromSide":"bottom","toNode":"n4","toSide":"top","color":"1"},
        {"id":"e2","fromNode":"n5","fromSide":"bottom","toNode":"n7","toSide":"top","color":"5"}
      ]
    }"##
    .parse()
    .expect("valid canvas");

    let mut harness =
        notebook_harness_with(egui::Vec2::new(820.0, 500.0), Notebook::from_canvas(canvas));
    harness.run_steps(3);
    harness.snapshot("notebook_colors");
}
