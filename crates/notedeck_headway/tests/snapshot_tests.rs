use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::{Key, Queryable};
use enostr::{FullKeypair, Keypair};
use nostrdb::Transaction;
use notedeck::{App, Notedeck};
use notedeck_headway::Headway;

struct HeadwayTestState {
    notedeck: Notedeck,
    headway: Headway,
    /// Signing account injected on first frame so Headway can seed + edit its
    /// event-backed board.
    account: FullKeypair,
    _tmpdir: tempfile::TempDir,
    fonts_installed: bool,
}

fn render_headway(ctx: &egui::Context, state: &mut HeadwayTestState) {
    // Fonts/styles must be installed before the first real frame; do it once,
    // and take the same first frame to inject a signing account.
    if !state.fonts_installed {
        state.notedeck.setup(ctx);
        ctx.style_mut(|s| s.animation_time = 0.0);

        let secret = state.account.secret_key.clone();
        let pubkey = state.account.pubkey;
        let app_ctx = &mut state.notedeck.app_context(ctx);
        if let Some(resp) = app_ctx.accounts.add_account(Keypair::from_secret(secret)) {
            let txn = Transaction::new(app_ctx.ndb).expect("txn");
            resp.unk_id_action
                .process_action(app_ctx.unknown_ids, app_ctx.ndb, &txn);
        }
        app_ctx.select_account(&pubkey);

        state.fonts_installed = true;
        return;
    }

    let mut app_ctx = state.notedeck.app_context(ctx);
    egui::CentralPanel::default().show(ctx, |ui| {
        state.headway.render(&mut app_ctx, ui);
    });
}

/// Build a harness at `size` with fonts installed, a signing account injected,
/// and the default board seeded + materialised.
fn headway_harness(size: egui::Vec2) -> Harness<'static, HeadwayTestState> {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    // `--testrunner` hands a fresh account an empty bootstrap relay set, so
    // selecting it never opens a relay websocket and the outbox has nothing to
    // flush on `AppContext` drop — no Tokio runtime required.
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let state = HeadwayTestState {
        notedeck,
        headway: Headway::new(),
        account: FullKeypair::generate(),
        _tmpdir: tmpdir,
        fonts_installed: false,
    };

    // `wake()` schedules an 8-frame `request_repaint_after` burst to poll for
    // async ndb ingests; the harness's simulated clock elapses each delay
    // immediately, so a single `run()` can take ~8 steps. Lift the default cap
    // of 4 above that burst so the wait loops don't spuriously panic.
    let mut harness = Harness::builder()
        .with_size(size)
        .with_max_steps(16)
        .renderer(notedeck::software_renderer())
        .build_state(render_headway, state);

    wait_for_board(&mut harness);
    harness
}

/// The board is seeded by ingesting events into nostrdb, which lands on an async
/// writer thread, and each card folds in across several events. Wait for the
/// header's full-count summary rather than just the first column, so every test
/// starts from a fully-materialised board instead of a half-ingested one.
fn wait_for_board(harness: &mut Harness<'static, HeadwayTestState>) {
    wait_for_label(harness, "7 cards · 4 columns");
}

/// Pump frames (with small sleeps, since ndb ingest is async) until a widget
/// with `label` appears, or panic after a deadline.
fn wait_for_label(harness: &mut Harness<'static, HeadwayTestState>, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run();
        if harness.query_by_label(label).is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Pump frames until no widget with `label` is present, or panic after a deadline.
fn wait_for_absent(harness: &mut Harness<'static, HeadwayTestState>, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run();
        if harness.query_by_label(label).is_none() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {label:?} to vanish"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
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
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));

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
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));

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

/// The inline card widget must render its content left-aligned even though the
/// notebook lays node content out centered (egui's `Ui::put` →
/// `centered_and_justified`). Reproduce that centered context and snapshot the
/// card, guarding against a regression to centered content.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_inline_card() {
    use notedeck_headway::{card_inline_ui, event::CardView};

    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let mut notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let card = CardView {
        id: enostr::NoteId::new([1u8; 32]),
        title: "Update headway-cli to use negentropy for sync".to_string(),
        description: String::new(),
        labels: vec!["headway".to_string()],
        rank: String::new(),
        placed_at: 0,
    };

    let mut installed = false;
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(420.0, 160.0))
        .renderer(notedeck::software_renderer())
        .build_ui(move |ui| {
            if !installed {
                notedeck.setup(ui.ctx());
                ui.ctx().style_mut(|s| s.animation_time = 0.0);
                installed = true;
            }
            let theme = notedeck::ColorTheme::current(ui.ctx());
            // Mimic the notebook's centered node layout.
            let rect = ui.available_rect_before_wrap();
            ui.put(rect, |ui: &mut egui::Ui| card_inline_ui(ui, &theme, &card));
        });

    harness.run();
    harness.snapshot("inline_card");
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
/// This exercises the full button → BoardAction → event ingest → reload path.
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

    // A fifth column now exists (asserted via the always-visible board summary,
    // since the new column itself renders off-screen to the right). The ingest
    // is async, so wait for the reload.
    wait_for_label(&mut harness, "7 cards · 5 columns");
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

    wait_for_label(&mut harness, "Inbox");
    wait_for_absent(&mut harness, "Backlog");
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

    // Wait for the reordered board to materialise (Backlog moves right of Todo).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run();
        let backlog_x = harness.get_by_label("Backlog").bounding_box().unwrap().x0;
        let todo_x = harness.get_by_label("Todo").bounding_box().unwrap().x0;
        if backlog_x > todo_x {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Backlog never moved right of Todo"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Delete a column via its "⋯" menu. Unlike the old in-memory model, deleting a
/// column doesn't destroy its cards: they're separate events and fall back to
/// the first column, so the board keeps all seven cards but drops to three
/// columns.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn delete_column_flow() {
    let mut harness = headway_harness(egui::Vec2::new(1600.0, 800.0));

    harness.get_by_label("7 cards · 4 columns"); // precondition

    open_first_column_menu(&mut harness);
    harness.get_by_label("Delete column").simulate_click();

    wait_for_absent(&mut harness, "Backlog");
    // Cards survive the column removal (they reflow into the first column).
    harness.get_by_label("7 cards · 3 columns");
}

/// Drive the add-card flow: open a column's composer, type a title, commit with
/// Enter, and confirm the new card shows up in that column. Then — without
/// re-opening the composer — type a second title and commit again, exercising
/// the rapid-entry path where the composer stays open and focused after an add.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn add_card_flow() {
    let mut harness = headway_harness(egui::Vec2::new(1600.0, 800.0));

    harness.get_by_label("7 cards · 4 columns"); // precondition

    // The first "+ Add card" affordance belongs to the leftmost column (Backlog).
    harness
        .get_all_by_label("+ Add card")
        .next()
        .expect("an add-card affordance")
        .simulate_click();
    harness.run();

    // Enter (not the "Add" button) must commit the card — the composer is a
    // multiline field, which swallows Enter into a newline, so this is the path
    // that used to silently do nothing.
    harness
        // The card composer is multiline, so it has the MultilineTextInput role.
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("Write integration tests");
    harness.run();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .key_press(Key::Enter);

    wait_for_label(&mut harness, "Write integration tests");
    harness.get_by_label("8 cards · 4 columns");

    // Rapid entry: the composer is still open and focused, so a second title can
    // go straight in without clicking "+ Add card" again.
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("Ship the feature");
    harness.run();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .key_press(Key::Enter);

    wait_for_label(&mut harness, "Ship the feature");
    harness.get_by_label("9 cards · 4 columns");
}

/// Regression: when a column's cards overflow its height you must still be able
/// to scroll all the way down to the "+ Add card" button. `column_ui` sized the
/// column frame's min-height to the *pre-margin* available height, so the frame
/// plus its inner margin overflowed the viewport and clipped the bottom of the
/// card list beneath the board — worse the more the UI was zoomed in.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn add_card_reachable_when_column_overflows() {
    // Narrow enough that the columns overflow horizontally (so the board shows a
    // horizontal scrollbar along the bottom) and short enough that the Backlog
    // column's three cards overflow vertically.
    let mut harness = headway_harness(egui::Vec2::new(700.0, 300.0));

    // Hover the first (Backlog) column and wheel it to the bottom.
    let col = harness.get_by_label("Backlog").bounding_box().unwrap();
    let pos = egui::pos2(col.x0 as f32 + 20.0, col.y0 as f32 + 90.0);
    for _ in 0..40 {
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(pos));
        harness.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            modifiers: egui::Modifiers::default(),
        });
        harness.run();
    }

    // The add-card button, fully scrolled, must land inside the board's padded
    // content area with the column's own bottom margin intact below it — not
    // flush against (or past) the board edge. The board frame reserves SPACING_LG
    // (16pt) of bottom padding and each column reserves SPACING_SM (8pt); the
    // button's bottom must clear both. Before the fix the column overflowed its
    // slot and the button sat exactly on the board edge (failing this bound).
    let limit = harness.ctx.screen_rect().max.y - 16.0 - 8.0;
    let btn = harness
        .get_all_by_label("+ Add card")
        .next()
        .expect("an add-card affordance")
        .bounding_box()
        .unwrap();
    assert!(
        btn.y1 as f32 <= limit,
        "add-card button bottom {} should sit within the padded board area \
         (limit {limit}); the column is overflowing its slot",
        btn.y1,
    );
}
