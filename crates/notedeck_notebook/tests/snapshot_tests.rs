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
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let state = NotebookTestState {
        notedeck,
        notebook: Notebook::new(),
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
