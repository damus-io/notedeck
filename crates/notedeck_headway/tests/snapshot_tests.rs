use egui_kittest::Harness;
use egui_kittest::kittest::{Key, Queryable};
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

/// Build a harness at `size` with the seeded board, fonts installed and one
/// frame run. Use a width wide enough to keep every column plus the
/// "+ Add column" affordance on-screen when driving clicks.
fn headway_harness(size: egui::Vec2) -> Harness<'static, HeadwayTestState> {
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
        .with_size(size)
        .renderer(notedeck::software_renderer())
        .build_state(render_headway, state);
    harness.run();
    harness
}

/// Open the first column's "⋯" overflow menu (there's one per column, so query
/// all and take the leftmost) and run a frame so the popup is present.
fn open_first_column_menu(harness: &mut Harness<'static, HeadwayTestState>) {
    harness
        .get_all_by_label("⋯")
        .next()
        .expect("at least one column menu")
        .simulate_click();
    harness.run();
}

/// Drive the add-column flow through the real UI: open the composer, type a
/// title, commit, and confirm a column was added and the composer closed.
/// This exercises the full button → BoardAction → model → re-render path that
/// the static snapshots don't touch.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn add_column_flow() {
    // Wide enough that all four seeded columns plus the "+ Add column"
    // affordance are on-screen, so the simulated clicks land on them.
    let mut harness = headway_harness(egui::Vec2::new(1600.0, 800.0));

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

/// Rename a column via its "⋯" menu: open menu → Rename → replace the inline
/// field's text → commit with Enter, and confirm the new title replaced the old.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn rename_column_flow() {
    let mut harness = headway_harness(egui::Vec2::new(1600.0, 800.0));

    harness.get_by_label("Backlog"); // precondition

    open_first_column_menu(&mut harness);
    harness.get_by_label("Rename").simulate_click();
    harness.run();

    // The header is now an inline field seeded with "Backlog". Select all
    // (Command+A maps to egui's select-all), replace it, and commit with Enter.
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .key_combination(&[Key::Command, Key::A]);
    harness.run();
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .type_text("Inbox");
    harness.run();
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .key_press(Key::Enter);
    harness.run_steps(2);

    harness.get_by_label("Inbox");
    assert!(
        harness.query_by_label("Backlog").is_none(),
        "old column title should be gone after rename"
    );
}

/// Reorder a column via its "⋯" menu: Move right shifts Backlog past Todo.
/// Asserted by comparing the columns' on-screen x positions.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn reorder_column_flow() {
    let mut harness = headway_harness(egui::Vec2::new(1600.0, 800.0));

    let backlog_x = harness.get_by_label("Backlog").bounding_box().unwrap().x0;
    let todo_x = harness.get_by_label("Todo").bounding_box().unwrap().x0;
    assert!(backlog_x < todo_x, "precondition: Backlog is left of Todo");

    open_first_column_menu(&mut harness);
    harness.get_by_label("Move right").simulate_click();
    harness.run_steps(2);

    let backlog_x = harness.get_by_label("Backlog").bounding_box().unwrap().x0;
    let todo_x = harness.get_by_label("Todo").bounding_box().unwrap().x0;
    assert!(
        backlog_x > todo_x,
        "Backlog should sit right of Todo after Move right"
    );
}

/// Delete a column via its "⋯" menu. Backlog holds three cards, so removing it
/// drops the board to three columns and four cards.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn delete_column_flow() {
    let mut harness = headway_harness(egui::Vec2::new(1600.0, 800.0));

    harness.get_by_label("7 cards · 4 columns"); // precondition

    open_first_column_menu(&mut harness);
    harness.get_by_label("Delete column").simulate_click();
    harness.run_steps(2);

    assert!(
        harness.query_by_label("Backlog").is_none(),
        "deleted column should be gone"
    );
    harness.get_by_label("4 cards · 3 columns");
}
