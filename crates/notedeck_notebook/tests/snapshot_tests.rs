use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use enostr::{FullKeypair, Keypair, Pubkey};
use nostrdb::{Ndb, Transaction};
use notedeck::{App, Notedeck};
use notedeck_notebook::Notebook;
use notedeck_notebook::event::{
    self, EdgeEnds, Geometry, LongformInput, NodeContent, NodeKind, build_canvas, build_edge,
    build_longform, build_node, build_transform, canvas_address,
};
use notedeck_notebook::store::{
    CANVAS_ID, LongformNote, NoPublish, create_longform, ingest, list_longform, load_longform,
};

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
        ctx.style_mut(|s| {
            s.animation_time = 0.0;
            // Steady (non-blinking) text caret so a focused field — the inline
            // vault rename — renders identically regardless of how much virtual
            // time elapsed before the snapshot. The blink phase otherwise tracks
            // `input.time`, which the variable-length seed barrier makes
            // nondeterministic across machines (it flaked only on CI).
            s.visuals.text_cursor.blink = false;
        });

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
    // Mirror production: chrome runs `update` (sync poll + fan-out + seed) for
    // every opened app each frame, then `render` for the foreground one.
    state.notebook.update(&mut app_ctx, ctx);
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
        harness.run_ok();
        if harness.query_by_label(label).is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Pump frames until the whole colored demo seed has folded in (all 8 nodes and
/// both edges), or panic after a deadline. The seed's ~19 events ingest
/// asynchronously and fold across one or more polls, so waiting for a single
/// node's label (`wait_for_label`) doesn't guarantee the rest are in — a test
/// that clicks or drags a node right after would race the stragglers. Use this
/// as the setup barrier for any test that interacts with the seeded canvas.
fn wait_for_seed(harness: &mut Harness<'static, NotebookTestState>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        let canvas = harness.state().notebook.canvas();
        if canvas.get_nodes().len() >= 8 && canvas.get_edges().len() >= 2 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the demo seed never fully folded"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Pump frames until all `expected` seeded longform notes have folded into the
/// vault list, or panic after a deadline. Each seed ingests asynchronously, so
/// waiting for a single row's label doesn't guarantee the rest are in — a
/// snapshot taken too early would render a nondeterministic subset (and, with
/// them, a nondeterministic row order). Use this as the vault setup barrier.
fn wait_for_vault(harness: &mut Harness<'static, NotebookTestState>, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if harness.state().notebook.notes().len() >= expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "only {} of {expected} vault notes folded in",
            harness.state().notebook.notes().len()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Seed one longform note per title with a deterministic `d` and `created_at`,
/// so the vault renders the same rows in the same order every run. Production's
/// `create_longform` stamps wall-clock `now_secs()`, which in a slow debug build
/// spreads a seeded batch across whole-second boundaries and shuffles the
/// newest-first order — the snapshot flake. Here `created_at` is `base + i`
/// (later titles are newer), so the primary sort key is fixed run-to-run.
fn seed_vault(ndb: &Ndb, secret: &[u8; 32], titles: &[&str]) {
    for (i, title) in titles.iter().enumerate() {
        let input = LongformInput {
            title: title.to_string(),
            content: format!("# {title}\n\nbody"),
            ..Default::default()
        };
        let builder =
            build_longform(&format!("seed-{i:02}"), &input).created_at(1_700_000_000 + i as u64);
        ingest(ndb, builder, secret, &mut NoPublish).expect("seed longform");
    }
}

/// Render the colored demo canvas at a desktop viewport and snapshot it.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook() {
    let mut harness = build_harness(egui::Vec2::new(1200.0, 800.0), true, true);
    wait_for_seed(&mut harness);
    harness.run_steps(3);
    harness.snapshot("notebook_demo");
}

/// A small canvas placing each preset color (and a hex color) near the origin.
/// Verifies the JSONCanvas color field is honored for node fill/stroke and edges.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_colors() {
    let mut harness = build_harness(egui::Vec2::new(820.0, 500.0), true, true);
    wait_for_seed(&mut harness);
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

/// Seed a handful of longform notes and snapshot the canvas with its vault
/// sidebar, for eyeballing the vault's visual design.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_vault() {
    let mut harness = build_harness(egui::Vec2::new(1000.0, 700.0), false, true);

    let secret = harness.state().account.secret_key.secret_bytes();
    let ctx = harness.ctx.clone();
    {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
        seed_vault(
            app_ctx.ndb,
            &secret,
            &[
                "Meeting notes — Q3 planning",
                "Reading list",
                "nostr protocol ideas",
                "Untitled",
                "Groceries",
            ],
        );
    }

    wait_for_vault(&mut harness, 5);
    harness.run_steps(3);
    // Hover a row so the snapshot also shows the row hover highlight.
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(120.0, 120.0)));
    harness.run_ok();
    harness.snapshot("notebook_vault");
}

/// Seed vault notes and snapshot the delete-confirmation modal (opened from a
/// row's context menu) for eyeballing the destructive-action prompt.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_vault_delete() {
    let mut harness = build_harness(egui::Vec2::new(1000.0, 700.0), false, true);

    let secret = harness.state().account.secret_key.secret_bytes();
    let ctx = harness.ctx.clone();
    {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
        seed_vault(
            app_ctx.ndb,
            &secret,
            &["Meeting notes — Q3 planning", "Reading list", "Groceries"],
        );
    }

    wait_for_vault(&mut harness, 3);
    harness.run_steps(3);
    secondary_click_at(&mut harness, egui::pos2(120.0, 120.0));
    harness.get_by_label("Delete").simulate_click();
    wait_for_label(&mut harness, "Delete note?");
    harness.run_steps(2);
    harness.snapshot("notebook_vault_delete");
}

/// Seed vault notes and snapshot a row in inline-rename mode (its editable title
/// field) for eyeballing the rename affordance.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_vault_rename() {
    let mut harness = build_harness(egui::Vec2::new(1000.0, 700.0), false, true);

    let secret = harness.state().account.secret_key.secret_bytes();
    let ctx = harness.ctx.clone();
    {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
        seed_vault(
            app_ctx.ndb,
            &secret,
            &["Meeting notes — Q3 planning", "Reading list", "Groceries"],
        );
    }

    wait_for_vault(&mut harness, 3);
    harness.run_steps(3);
    secondary_click_at(&mut harness, egui::pos2(120.0, 120.0));
    harness.get_by_label("Rename").simulate_click();
    harness.run_steps(3);
    harness.snapshot("notebook_vault_rename");
}

/// Seed a note with rich markdown, open it from the vault, and snapshot the
/// full-screen editor (source on the left, live rendered preview on the right)
/// for eyeballing the editor's visual design.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_editor() {
    let mut harness = build_harness(egui::Vec2::new(1000.0, 700.0), false, true);

    let secret = harness.state().account.secret_key.secret_bytes();
    let pubkey = harness.state().account.pubkey;
    let ctx = harness.ctx.clone();
    {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
        let content = concat!(
            "# Q3 Planning\n\n",
            "Goals for the **quarter**, with a few *stretch* items.\n\n",
            "## Milestones\n\n",
            "- Ship the notebook vault\n",
            "- [x] Longform editor\n",
            "- [ ] Backlinks between notes\n\n",
            "## Notes\n\n",
            "Persist via `store::create_longform`.\n\n",
            "> Encrypt everything with PNS.\n\n",
            "```rust\nfn hello() {\n    println!(\"hi\");\n}\n```\n",
        );
        let input = LongformInput {
            title: "Q3 Planning".to_string(),
            content: content.to_string(),
            ..Default::default()
        };
        create_longform(app_ctx.ndb, &pubkey, &secret, &input, None, &mut NoPublish)
            .expect("seed longform");
    }

    // Open the note from the vault into the editor.
    wait_for_label(&mut harness, "Q3 Planning");
    harness.get_by_label("Q3 Planning").simulate_click();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if harness.state().notebook.editor_is_open() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "editor never opened from the vault"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    harness.run_steps(3);
    harness.snapshot("notebook_editor");
}

/// Drag the "Red" node and confirm its position moves; clicking a node selects
/// it and clicking empty canvas clears the selection. The scene loads with a
/// 1:1 mapping (scene_rect == viewport), so screen coords equal canvas coords.
#[test]
fn drag_and_select_nodes() {
    let mut harness = build_harness(egui::Vec2::new(820.0, 500.0), true, false);
    wait_for_seed(&mut harness);

    // Nothing selected to start.
    assert_eq!(harness.state().notebook.selected(), None);

    // Click the "Red" node to select it; capture the node's id. Use `click_at`
    // (single-frame press+release) rather than `simulate_click`: the canvas
    // requests repaints after each ingest, which stretches a multi-frame click
    // past egui's click-time threshold so it never registers. Red's rect is
    // (40,40)-(240,130); click its lower body, clear of the heading text (which,
    // being selectable, would otherwise intercept the click).
    click_at(&mut harness, egui::pos2(140.0, 115.0));
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

    // Drag the node by (+150, +80). Grab the bare lower body (clear of the
    // heading text, which would intercept the press) so the drag moves the node.
    let start = egui::pos2(140.0, 115.0);
    press(&mut harness, start);
    drag_to(&mut harness, start + egui::vec2(150.0, 80.0));
    release(&mut harness, start + egui::vec2(150.0, 80.0));

    // The move is ingested asynchronously and folds back in; wait for it.
    let target = egui::pos2(190.0, 120.0);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if let Some(p) = harness.state().notebook.node_position(&id)
            && (p - target).length() < 2.0
        {
            break;
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
    wait_for_seed(&mut harness);

    // Capture the ids of the two nodes we'll connect (clicking a node selects
    // it). Use `click_at` (single-frame press+release) rather than
    // `simulate_click`: the canvas requests repaints after each ingest, which
    // stretches a multi-frame click past egui's click-time threshold so it never
    // registers. Click the lower part of each node, clear of its heading text —
    // selectable markdown text intercepts the pointer, so only the bare body
    // falls through to the node's drag/select handle. Orange's rect is
    // (300,40)-(500,130) and Red's is (40,40)-(240,130).
    click_at(&mut harness, egui::pos2(400.0, 115.0));
    let orange = harness
        .state()
        .notebook
        .selected()
        .cloned()
        .expect("orange selected");
    click_at(&mut harness, egui::pos2(140.0, 115.0));
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
        harness.run_ok();
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

/// Regression: an edge can be drawn from a node you only *hover*, without first
/// clicking to select it. The handle's hit box straddles the node border, so the
/// pointer is already off the body as the drag begins; the gesture must survive
/// the press → drag-threshold gap (where egui hasn't yet reported a drag and the
/// node is no longer hovered) instead of being dropped.
#[test]
fn connect_from_hovered_node() {
    let mut harness = build_harness(egui::Vec2::new(820.0, 500.0), true, false);
    wait_for_seed(&mut harness);

    // Find Red and Orange by position, without selecting anything.
    let node_id_at = |h: &Harness<'static, NotebookTestState>, pos: egui::Pos2| {
        let nb = &h.state().notebook;
        nb.canvas()
            .get_nodes()
            .iter()
            .find(|(id, _)| nb.node_position(id) == Some(pos))
            .map(|(id, _)| id.clone())
            .expect("a node at the given position")
    };
    let red = node_id_at(&harness, egui::pos2(40.0, 40.0));
    let orange = node_id_at(&harness, egui::pos2(300.0, 40.0));
    let before = harness.state().notebook.canvas().get_edges().len();

    // Hover Red's body, confirm hovering alone doesn't select it, then drag from
    // its right-edge handle at (240,85) into Orange — all without a prior click.
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(140.0, 115.0)));
    harness.run_ok();
    assert_eq!(
        harness.state().notebook.selected(),
        None,
        "hovering a node must not select it"
    );

    let from = egui::pos2(240.0, 85.0);
    let into = egui::pos2(400.0, 85.0);
    press(&mut harness, from);
    drag_to(&mut harness, egui::pos2(320.0, 85.0));
    drag_to(&mut harness, into);
    release(&mut harness, into);

    // The edge is ingested asynchronously and folds back in; wait for it.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
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
            "an edge from a hovered (unselected) Red to Orange never appeared"
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
    wait_for_seed(&mut harness);

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
        harness.run_ok();
        if edge_count(&harness) < before {
            break;
        }
        assert!(Instant::now() < deadline, "edge was never deleted");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Poll frames until `load_longform` reads a note with `d` matching `pred`
/// (ingest is async and PNS-wrapped, so nostrdb has to unwrap it first). Reads
/// through the app's own ndb via a fresh `app_context`, which unwraps the
/// kind-1080 envelope transparently — proving the private note round-trips.
fn wait_for_longform(
    harness: &mut Harness<'static, NotebookTestState>,
    d: &str,
    pred: impl Fn(&LongformNote) -> bool,
) -> LongformNote {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        let pubkey = harness.state().account.pubkey;
        let ctx = harness.ctx.clone();
        let found = {
            let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
            let txn = Transaction::new(app_ctx.ndb).expect("txn");
            load_longform(app_ctx.ndb, &txn, &pubkey, d).filter(&pred)
        };
        if let Some(note) = found {
            return note;
        }
        assert!(
            Instant::now() < deadline,
            "longform note {d:?} never matched"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// End-to-end longform flow: open the editor from the toolbar, type a note, Save
/// (which signs a kind-30023, PNS-wraps it in a kind-1080, and ingests the
/// wrapper), confirm it round-trips back through nostrdb's unwrap, edit + Save
/// again and confirm the supersede, Close back to the canvas, then confirm the
/// note appears in the vault sidebar and clicking it reopens the same note.
#[test]
fn create_and_edit_longform_via_editor() {
    let mut harness = build_harness(egui::Vec2::new(1000.0, 700.0), false, false);

    // "+ New note" lives in the canvas-mode toolbar (shown even while the canvas is
    // still seeding). Opening the editor takes over the whole view.
    wait_for_label(&mut harness, "+ New note");
    assert!(!harness.state().notebook.editor_is_open());
    harness.get_by_label("+ New note").simulate_click();
    wait_for_label(&mut harness, "← Canvas");
    assert!(harness.state().notebook.editor_is_open());
    assert_eq!(harness.state().notebook.editor_saved(), None);

    // Type a title (the sole singleline field) and a markdown body (the sole
    // multiline field).
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .type_text("My first note");
    harness.run_ok();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("# Hello\n\nthis is **markdown**");
    harness.run_ok();

    // Save. create_longform runs synchronously, so the editor records its
    // (d, created_at) within a frame or two.
    harness.get_by_label("Save").simulate_click();
    let (d, created_at) = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            harness.run_ok();
            if let Some((d, ca)) = harness.state().notebook.editor_saved() {
                break (d.to_string(), ca);
            }
            assert!(Instant::now() < deadline, "the note was never saved");
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    assert!(!d.is_empty(), "a d was minted");
    assert!(created_at > 0);

    // It really persisted: wait until nostrdb has unwrapped the envelope and the
    // inner note reads back with the typed content.
    let note = wait_for_longform(&mut harness, &d, |n| n.title == "My first note");
    assert!(note.content.contains("**markdown**"));
    assert_eq!(note.created_at, created_at);

    // Edit: append text and Save again — the edit supersedes with a strictly
    // later created_at (the store stamps past the prior version, so a same-second
    // edit still wins).
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("\n\nmore");
    harness.run_ok();
    harness.get_by_label("Save").simulate_click();
    {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            harness.run_ok();
            if let Some((d2, ca2)) = harness.state().notebook.editor_saved()
                && d2 == d
                && ca2 > created_at
            {
                break;
            }
            assert!(Instant::now() < deadline, "the edit never superseded");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    let edited = wait_for_longform(&mut harness, &d, |n| n.content.contains("more"));
    assert!(edited.created_at > created_at, "the edit superseded");

    // Close returns to the canvas.
    harness.get_by_label("← Canvas").simulate_click();
    harness.run_ok();
    assert!(!harness.state().notebook.editor_is_open());

    // Back on the canvas, the saved note now shows in the vault sidebar; clicking
    // it reopens the editor bound to that same note.
    wait_for_label(&mut harness, "My first note");
    harness.get_by_label("My first note").simulate_click();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if harness.state().notebook.editor_is_open()
            && harness
                .state()
                .notebook
                .editor_saved()
                .map(|(d2, _)| d2 == d.as_str())
                == Some(true)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the note never reopened from the vault"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The count of the account's live (non-deleted) vault notes, read through the
/// app's own ndb.
fn vault_len(harness: &mut Harness<'static, NotebookTestState>) -> usize {
    let pubkey = harness.state().account.pubkey;
    let ctx = harness.ctx.clone();
    let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    list_longform(app_ctx.ndb, &txn, &pubkey).len()
}

/// Press and release a key this frame, routed to whatever egui widget has focus.
fn key_press(harness: &mut Harness<'static, NotebookTestState>, key: egui::Key) {
    for pressed in [true, false] {
        harness.input_mut().events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
    }
    harness.run_ok();
}

/// The titles of the account's live vault notes, read through the app's own ndb.
fn vault_titles(harness: &mut Harness<'static, NotebookTestState>) -> Vec<String> {
    let pubkey = harness.state().account.pubkey;
    let ctx = harness.ctx.clone();
    let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    list_longform(app_ctx.ndb, &txn, &pubkey)
        .into_iter()
        .map(|n| n.title)
        .collect()
}

/// End-to-end vault rename: seed two notes, right-click a vault row, choose
/// Rename, type into the inline field, and commit with Enter — then verify the
/// edit supersedes that note in place (the vault still holds both notes, one now
/// carrying the edited title).
#[test]
fn rename_note_via_vault_context_menu() {
    let mut harness = build_harness(egui::Vec2::new(1000.0, 700.0), false, false);

    let secret = harness.state().account.secret_key.secret_bytes();
    let pubkey = harness.state().account.pubkey;
    let ctx = harness.ctx.clone();
    {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
        for title in ["First draft", "Second draft"] {
            let input = LongformInput {
                title: title.to_string(),
                content: format!("# {title}\n\nbody"),
                ..Default::default()
            };
            create_longform(app_ctx.ndb, &pubkey, &secret, &input, None, &mut NoPublish)
                .expect("seed longform");
        }
    }

    wait_for_label(&mut harness, "First draft");
    harness.run_steps(3);
    assert!(
        vault_titles(&mut harness).iter().all(|t| !t.contains("v2")),
        "no note is renamed yet"
    );

    // Right-click a row and choose Rename, arming the inline field.
    secondary_click_at(&mut harness, egui::pos2(120.0, 120.0));
    harness.get_by_label("Rename").simulate_click();
    harness.run_ok();

    // Type into the field (appending to the seeded title) and commit with Enter.
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .type_text(" v2");
    harness.run_ok();
    key_press(&mut harness, egui::Key::Enter);

    // The rename supersedes the note in place: both notes remain, one now edited.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        let titles = vault_titles(&mut harness);
        if titles.len() == 2 && titles.iter().any(|t| t.contains("v2")) {
            break;
        }
        assert!(Instant::now() < deadline, "the note was never renamed");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// End-to-end vault delete: seed two notes, right-click a vault row to open its
/// context menu, choose Delete, confirm the modal, and verify a tombstone drops
/// that note from the vault (the list shrinks by one) while the other survives.
#[test]
fn delete_note_via_vault_context_menu() {
    let mut harness = build_harness(egui::Vec2::new(1000.0, 700.0), false, false);

    // Seed two longform notes directly (the create path is covered elsewhere).
    let secret = harness.state().account.secret_key.secret_bytes();
    let pubkey = harness.state().account.pubkey;
    let ctx = harness.ctx.clone();
    {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
        for title in ["Keep me", "Delete me"] {
            let input = LongformInput {
                title: title.to_string(),
                content: format!("# {title}\n\nbody"),
                ..Default::default()
            };
            create_longform(app_ctx.ndb, &pubkey, &secret, &input, None, &mut NoPublish)
                .expect("seed longform");
        }
    }

    // Wait until both notes have unwrapped and the vault has rendered its rows.
    wait_for_label(&mut harness, "Keep me");
    harness.run_steps(3);
    assert_eq!(vault_len(&mut harness), 2);

    // Right-click the first vault row (its rough on-screen position, same spot the
    // vault snapshot hovers) to open the context menu, then choose Delete.
    secondary_click_at(&mut harness, egui::pos2(120.0, 120.0));
    harness.get_by_label("Delete").simulate_click();
    harness.run_ok();

    // The confirmation modal appears; its Delete button fires the tombstone.
    wait_for_label(&mut harness, "Delete note?");
    harness.get_by_label("Delete").simulate_click();

    // The tombstone ingests + unwraps asynchronously; poll until the vault drops
    // to a single note.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if vault_len(&mut harness) == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "the note was never deleted");
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
    harness.run_ok();
}

/// A secondary (right) click delivered as press+release in one frame, to open a
/// widget's context menu. Same single-frame reasoning as [`click_at`].
fn secondary_click_at(harness: &mut Harness<'static, NotebookTestState>, pos: egui::Pos2) {
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    harness.run_ok();
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
    harness.run_ok();
}

fn drag_to(harness: &mut Harness<'static, NotebookTestState>, pos: egui::Pos2) {
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    harness.run_ok();
}

fn release(harness: &mut Harness<'static, NotebookTestState>, pos: egui::Pos2) {
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    });
    harness.run_ok();
}
