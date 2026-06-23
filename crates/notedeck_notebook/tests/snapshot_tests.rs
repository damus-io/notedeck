use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use enostr::{FullKeypair, Keypair, Pubkey};
use nostrdb::{Ndb, Transaction};
use notedeck::{App, Notedeck};
use notedeck_notebook::Notebook;
use notedeck_notebook::event::{
    self, EdgeEnds, Geometry, NodeContent, NodeKind, build_canvas, build_edge, build_node,
    build_transform, canvas_address,
};
use notedeck_notebook::store::{CANVAS_ID, NoPublish, ingest};

struct NotebookTestState {
    notedeck: Notedeck,
    notebook: Notebook,
    /// Signing account injected on the first frame so the notebook can seed and
    /// edit its event-backed canvas.
    account: FullKeypair,
    /// Whether to seed the colored demo canvas on the injection frame.
    seed_colors: bool,
    _tmpdir: tempfile::TempDir,
    setup_done: bool,
}

fn render_notebook(ctx: &egui::Context, state: &mut NotebookTestState) {
    // Fonts/styles must be installed before the first real frame; do it once,
    // and take the same first frame to inject a signing account (and optionally
    // seed a canvas).
    if !state.setup_done {
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

        if state.seed_colors {
            seed_colored_canvas(
                app_ctx.ndb,
                &pubkey,
                &state.account.secret_key.secret_bytes(),
            );
        }

        state.setup_done = true;
        return;
    }

    let mut app_ctx = state.notedeck.app_context(ctx);
    egui::CentralPanel::default().show(ctx, |ui| {
        state.notebook.render(&mut app_ctx, ui);
    });
}

/// The colored demo canvas as nostr events: each preset color, a hex color, a
/// plain node, and two colored edges, all near the origin so they fall in the
/// initial viewport. Mirrors the old in-memory `colors_canvas` fixture.
fn seed_colored_canvas(ndb: &Ndb, author: &Pubkey, secret: &[u8; 32]) {
    let addr = canvas_address(author, CANVAS_ID);
    let mut publisher = NoPublish;
    ingest(
        ndb,
        build_canvas(CANVAS_ID, "Notebook", &[], false),
        secret,
        &mut publisher,
    );

    // (text, x, y, color)
    let specs: [(&str, i64, i64, Option<&str>); 8] = [
        ("# Red", 40, 40, Some("1")),
        ("# Orange", 300, 40, Some("2")),
        ("# Yellow", 560, 40, Some("3")),
        ("# Green", 40, 200, Some("4")),
        ("# Cyan", 300, 200, Some("5")),
        ("# Purple", 560, 200, Some("6")),
        ("# Hex #3b82f6", 300, 360, Some("#3b82f6")),
        ("plain node", 40, 360, None),
    ];

    let mut ids = std::collections::HashMap::new();
    let mut last = String::new();
    for (text, x, y, color) in specs {
        let content = NodeContent {
            text: text.to_string(),
            ..Default::default()
        };
        let geo = Geometry {
            x,
            y,
            w: 200,
            h: 90,
        };
        let id = ingest(
            ndb,
            build_node(&addr, NodeKind::Text, &geo, &content),
            secret,
            &mut publisher,
        )
        .expect("node ingested");
        let z = event::rank_between((!last.is_empty()).then_some(last.as_str()), None);
        ingest(
            ndb,
            build_transform(CANVAS_ID, &addr, &id, &geo, &z, color),
            secret,
            &mut publisher,
        );
        last = z;
        ids.insert(text, id);
    }

    let edge = |color: &str| EdgeEnds {
        from_side: Some("bottom".to_string()),
        to_side: Some("top".to_string()),
        color: Some(color.to_string()),
        ..Default::default()
    };
    ingest(
        ndb,
        build_edge(
            CANVAS_ID,
            &addr,
            "e1",
            &ids["# Red"],
            &ids["# Green"],
            &edge("1"),
        ),
        secret,
        &mut publisher,
    );
    ingest(
        ndb,
        build_edge(
            CANVAS_ID,
            &addr,
            "e2",
            &ids["# Cyan"],
            &ids["# Hex #3b82f6"],
            &edge("5"),
        ),
        secret,
        &mut publisher,
    );
}

fn build_harness(
    size: egui::Vec2,
    seed_colors: bool,
    renderer: bool,
) -> Harness<'static, NotebookTestState> {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let state = NotebookTestState {
        notedeck,
        notebook: Notebook::new(),
        account: FullKeypair::generate(),
        seed_colors,
        _tmpdir: tmpdir,
        setup_done: false,
    };

    let mut builder = Harness::builder().with_size(size).with_max_steps(16);
    if renderer {
        builder = builder.renderer(notedeck::software_renderer());
    }
    let mut harness = builder.build_state(render_notebook, state);

    // First frame installs fonts + injects the account; pump more so the canvas
    // folds and the scene lays out.
    harness.run_steps(4);
    harness
}

/// Pump frames (ndb ingest is async) until a widget with `label` appears, or
/// panic after a deadline.
fn wait_for_label(harness: &mut Harness<'static, NotebookTestState>, label: &str) {
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

/// Render the colored demo canvas at a desktop viewport and snapshot it.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook() {
    let mut harness = build_harness(egui::Vec2::new(1200.0, 800.0), true, true);
    wait_for_label(&mut harness, "Red");
    harness.run_steps(3);
    harness.snapshot("notebook_demo");
}

/// A small canvas placing each preset color (and a hex color) near the origin.
/// Verifies the JSONCanvas color field is honored for node fill/stroke and edges.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_colors() {
    let mut harness = build_harness(egui::Vec2::new(820.0, 500.0), true, true);
    wait_for_label(&mut harness, "Red");
    harness.run_steps(3);
    harness.snapshot("notebook_colors");
}

/// Select a node (click its heading) and snapshot the selection highlight.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_selected() {
    let mut harness = build_harness(egui::Vec2::new(820.0, 500.0), true, true);
    wait_for_label(&mut harness, "Cyan");
    harness.get_by_label("Cyan").simulate_click();
    harness.run_steps(3);
    harness.snapshot("notebook_selected");
}

/// Drag the "Red" node and confirm its position moves; clicking a node selects
/// it and clicking empty canvas clears the selection. The scene loads with a
/// 1:1 mapping (scene_rect == viewport), so screen coords equal canvas coords.
#[test]
fn drag_and_select_nodes() {
    let mut harness = build_harness(egui::Vec2::new(820.0, 500.0), true, false);
    wait_for_label(&mut harness, "Red");

    // Nothing selected to start.
    assert_eq!(harness.state().notebook.selected(), None);

    // Click the "Red" heading to select its node; capture the node's id.
    harness.get_by_label("Red").simulate_click();
    harness.run();
    let id = harness
        .state()
        .notebook
        .selected()
        .cloned()
        .expect("a node is selected after clicking it");

    // It sits at its declared position.
    assert_eq!(
        harness.state().notebook.node_position(&id),
        Some(egui::pos2(40.0, 40.0))
    );

    // Drag the node by (+150, +80).
    let start = egui::pos2(80.0, 70.0);
    press(&mut harness, start);
    drag_to(&mut harness, start + egui::vec2(150.0, 80.0));
    release(&mut harness, start + egui::vec2(150.0, 80.0));

    // The move is ingested asynchronously and folds back in; wait for it.
    let target = egui::pos2(190.0, 120.0);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run();
        if let Some(p) = harness.state().notebook.node_position(&id) {
            if (p - target).length() < 2.0 {
                break;
            }
        }
        assert!(Instant::now() < deadline, "n1 never moved to ~{target:?}");
        std::thread::sleep(Duration::from_millis(25));
    }

    // Click an empty gap (clear of the moved node) to clear the selection.
    click_at(&mut harness, egui::pos2(700.0, 430.0));
    assert_eq!(harness.state().notebook.selected(), None);
}

/// Dragging from a node's side handle onto another node creates an edge between
/// them. The scene loads 1:1 (screen coords == canvas coords), so the handle sits
/// at the node's right-edge midpoint.
#[test]
fn connect_nodes_with_edge() {
    let mut harness = build_harness(egui::Vec2::new(820.0, 500.0), true, false);
    wait_for_label(&mut harness, "Red");

    // Capture the ids of the two nodes we'll connect (clicking selects a node).
    harness.get_by_label("Orange").simulate_click();
    harness.run();
    let orange = harness
        .state()
        .notebook
        .selected()
        .cloned()
        .expect("orange selected");
    harness.get_by_label("Red").simulate_click();
    harness.run();
    let red = harness
        .state()
        .notebook
        .selected()
        .cloned()
        .expect("red selected");

    let before = harness.state().notebook.canvas().get_edges().len();

    // Drag from Red's right-edge handle (its rect is (40,40)-(240,130)) into the
    // Orange node beside it (its rect is (300,40)-(500,130)).
    let from = egui::pos2(240.0, 85.0);
    let into = egui::pos2(400.0, 85.0);
    press(&mut harness, from);
    drag_to(&mut harness, egui::pos2(320.0, 85.0));
    drag_to(&mut harness, into);
    release(&mut harness, into);

    // The edge is ingested asynchronously and folds back in; wait for it.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run();
        let canvas = harness.state().notebook.canvas();
        let connected = canvas.get_edges().len() > before
            && canvas.get_edges().values().any(|e| {
                e.from_node().as_str() == red.as_str() && e.to_node().as_str() == orange.as_str()
            });
        if connected {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "an edge from Red to Orange never appeared"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Clicking an edge's midpoint delete handle removes the edge. The colored demo
/// seeds edge "e1" from Red (40,40)-(240,130) down to Green (40,200)-(240,290),
/// anchored bottom→top, so the curve — and its midpoint handle — sits around
/// x≈140, between the two nodes.
#[test]
fn delete_edge_via_handle() {
    let mut harness = build_harness(egui::Vec2::new(820.0, 500.0), true, false);
    wait_for_label(&mut harness, "Red");

    // The demo seeds two edges; e1 connects Red -> Green.
    let edge_count =
        |h: &Harness<'static, NotebookTestState>| h.state().notebook.canvas().get_edges().len();
    assert!(edge_count(&harness) >= 1, "the demo seeds edges");
    let before = edge_count(&harness);

    // Red's bottom is y=130, Green's top is y=200, both centered at x=140, so the
    // edge's midpoint handle lands near (140, 165).
    click_at(&mut harness, egui::pos2(140.0, 165.0));

    // The delete is ingested asynchronously and folds back in; wait for it.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run();
        if edge_count(&harness) < before {
            break;
        }
        assert!(Instant::now() < deadline, "edge was never deleted");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A click delivered as press+release within a single frame, so it registers
/// even though the canvas keeps requesting repaints (which would otherwise
/// stretch a held button past egui's click-time threshold across `run()`).
fn click_at(harness: &mut Harness<'static, NotebookTestState>, pos: egui::Pos2) {
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::default(),
    });
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    });
    harness.run();
}

fn press(harness: &mut Harness<'static, NotebookTestState>, pos: egui::Pos2) {
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::default(),
    });
    harness.run();
}

fn drag_to(harness: &mut Harness<'static, NotebookTestState>, pos: egui::Pos2) {
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    harness.run();
}

fn release(harness: &mut Harness<'static, NotebookTestState>, pos: egui::Pos2) {
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    });
    harness.run();
}
