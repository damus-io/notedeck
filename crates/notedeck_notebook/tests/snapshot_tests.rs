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
    CANVAS_ID, LongformNote, NoPublish, create_longform, ingest, list_canvases, list_longform,
    load_canvas, load_longform,
};
use notedeck_notebook::wordid;
use notedeck_ui::markdown::render_markdown_with_refs;

struct NotebookTestState {
    notedeck: Notedeck,
    notebook: Notebook,
    /// Signing account injected on the first frame so the notebook can seed and
    /// edit its event-backed canvas.
    account: FullKeypair,
    /// Whether to seed the colored demo canvas on the injection frame.
    seed_colors: bool,
    /// When set, `render_notebook` draws note/Dave surfaces rendering this body
    /// (which holds an inline `notebook:<word-id>` reference) instead of the
    /// canvas — the inline-reference chip snapshot. The notebook app still
    /// `update`s each frame, so the shared cache the chip folds through stays
    /// current; only the drawn surface changes.
    ref_surface: Option<String>,
    /// Canvases to seed on the injection frame, *before* the app auto-seeds one, so
    /// a multi-canvas test gets deterministic ids/titles/order (the auto-seed uses
    /// wall-clock `created_at`, which shuffles ordering run-to-run). The earliest to
    /// fold suppresses the auto-seed, exactly as `seed_colors` does with `CANVAS_ID`.
    seed_canvases: Vec<SeedCanvas>,
    _tmpdir: tempfile::TempDir,
    setup_done: bool,
}

/// A canvas to seed deterministically on the injection frame: a fixed `d`, title
/// and `created_at` (which fixes its sort position in the vault list), plus any
/// text nodes to place on it so an open-swap test can tell one canvas from another.
struct SeedCanvas {
    d: String,
    title: String,
    created_at: u64,
    nodes: Vec<SeedNode>,
}

/// One text node to seed onto a [`SeedCanvas`], at canvas position `(x, y)`.
struct SeedNode {
    text: String,
    x: i64,
    y: i64,
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

        let secret = state.account.secret_key.secret_bytes();
        let mut seeded_canvas_ids: Vec<String> = Vec::new();
        if state.seed_colors {
            seed_colored_canvas(app_ctx.ndb, &pubkey, &secret);
            seeded_canvas_ids.push(CANVAS_ID.to_string());
        }
        for canvas in &state.seed_canvases {
            seed_canvas_with_nodes(app_ctx.ndb, &pubkey, &secret, canvas);
            seeded_canvas_ids.push(canvas.d.clone());
        }
        // Block until the seeded canvas docs commit, so the app's *first* history
        // fold (next frame's `update`) already sees them and doesn't spuriously
        // auto-seed a second empty canvas before they land — which would pop the
        // vault sidebar open (≥2 canvases) and offset these canvas fixtures. This
        // is the test-side stand-in for the deferred sync-caught-up seed gate
        // (headway:notebook/social-genuine-crane).
        wait_canvases_committed(app_ctx.ndb, &pubkey, &seeded_canvas_ids);

        state.setup_done = true;
        return;
    }

    let mut app_ctx = state.notedeck.app_context(ctx);
    // Mirror production: chrome runs `update` (sync poll + fan-out + seed) for
    // every opened app each frame, then `render` for the foreground one.
    state.notebook.update(&mut app_ctx, ctx);

    // Reference-chip mode: draw note/Dave surfaces holding an inline
    // `notebook:<word-id>` instead of the canvas. `update` above already folded
    // this frame, so the chip resolves against the same live cache the canvas
    // would.
    if let Some(body) = &state.ref_surface {
        render_ref_surfaces(ctx, &mut app_ctx, body);
        return;
    }

    egui::CentralPanel::default().show(ctx, |ui| {
        state.notebook.render(&mut app_ctx, ui);
    });
}

/// Draw two ref-aware surfaces — a plain note and a Dave-style chat bubble — each
/// rendering `body` through [`render_markdown_with_refs`], the very path notes and
/// Dave messages use for `NoteOptions::InlineReferences`. A `notebook:<word-id>` in
/// `body` resolves via the registered parser and draws as a live node chip folded
/// from the shared cache — the cross-app demo the card asks for, in one frame.
fn render_ref_surfaces(ctx: &egui::Context, app_ctx: &mut notedeck::AppContext, body: &str) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(560.0);

            ui.label(egui::RichText::new("In a note").weak());
            ui.add_space(4.0);
            egui::Frame::group(ui.style())
                .inner_margin(12.0)
                .show(ui, |ui| {
                    let txn = Transaction::new(app_ctx.ndb).expect("txn");
                    let mut note_ctx = app_ctx.note_context();
                    render_markdown_with_refs(ui, &mut note_ctx, &txn, body);
                });

            ui.add_space(24.0);

            ui.label(egui::RichText::new("In a Dave message").weak());
            ui.add_space(4.0);
            egui::Frame::group(ui.style())
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(12.0)
                .show(ui, |ui| {
                    let txn = Transaction::new(app_ctx.ndb).expect("txn");
                    let mut note_ctx = app_ctx.note_context();
                    render_markdown_with_refs(ui, &mut note_ctx, &txn, body);
                });
        });
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

/// Seed a canvas with a fixed `d`/title/`created_at` and its text nodes as nostr
/// events — the deterministic multi-canvas counterpart to the app's wall-clock
/// auto-seed. Mirrors [`seed_colored_canvas`]'s node+transform ingest per node.
fn seed_canvas_with_nodes(ndb: &Ndb, author: &Pubkey, secret: &[u8; 32], canvas: &SeedCanvas) {
    let addr = canvas_address(author, &canvas.d);
    let mut publisher = NoPublish;
    ingest(
        ndb,
        build_canvas(&canvas.d, &canvas.title, &[], false).created_at(canvas.created_at),
        secret,
        &mut publisher,
    );
    let mut last = String::new();
    for node in &canvas.nodes {
        let geo = Geometry {
            x: node.x,
            y: node.y,
            w: 200,
            h: 90,
        };
        let content = NodeContent {
            text: node.text.clone(),
            ..Default::default()
        };
        let id = ingest(
            ndb,
            build_node(&addr, NodeKind::Text, &geo, &content),
            secret,
            &mut publisher,
        )
        .expect("seed node ingested");
        let z = event::rank_between((!last.is_empty()).then_some(last.as_str()), None);
        ingest(
            ndb,
            build_transform(&canvas.d, &addr, &id, &geo, &z, None),
            secret,
            &mut publisher,
        );
        last = z;
    }
}

/// Block until every canvas in `ids` has committed and folds under a fresh read
/// txn (ingest is async on a writer thread), or panic after a deadline. Used by
/// the setup frame so the app's first history fold sees the seeded canvases and
/// skips the auto-seed.
fn wait_canvases_committed(ndb: &Ndb, author: &Pubkey, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let all_present = {
            let txn = Transaction::new(ndb).expect("txn");
            ids.iter()
                .all(|d| load_canvas(ndb, &txn, author, d).is_some())
        };
        if all_present {
            return;
        }
        assert!(Instant::now() < deadline, "seeded canvases never committed");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn build_harness(
    size: egui::Vec2,
    seed_colors: bool,
    renderer: bool,
) -> Harness<'static, NotebookTestState> {
    build_harness_inner(size, seed_colors, renderer, vec![])
}

/// Build a harness that seeds a fixed set of `canvases` on the injection frame
/// (suppressing the wall-clock auto-seed) — for the deterministic multi-canvas
/// vault/open/rename/delete tests.
fn build_harness_canvases(
    size: egui::Vec2,
    renderer: bool,
    canvases: Vec<SeedCanvas>,
) -> Harness<'static, NotebookTestState> {
    build_harness_inner(size, false, renderer, canvases)
}

fn build_harness_inner(
    size: egui::Vec2,
    seed_colors: bool,
    renderer: bool,
    seed_canvases: Vec<SeedCanvas>,
) -> Harness<'static, NotebookTestState> {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let mut notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let notebook = Notebook::new();
    // Mirror chrome: register the app's kind renderers into the host so a
    // `nostr:` reference (inline in a text node, or a note-embed node) resolves
    // to its kind widget instead of falling back to raw text.
    for renderer in notebook.kind_renderers() {
        notedeck.register_kind_renderer(renderer);
    }
    // ...and its reference parsers, so a `notebook:<word-id>` in a run of text
    // resolves to its node before the renderer above draws it (chrome does both
    // at startup; see `chrome.rs`).
    for parser in notebook.reference_parsers() {
        notedeck.register_reference_parser(parser);
    }

    let state = NotebookTestState {
        notedeck,
        notebook,
        account: FullKeypair::generate(),
        seed_colors,
        ref_surface: None,
        seed_canvases,
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
/// full-screen editor for eyeballing its visual design. A note opened from the
/// vault lands on the Preview face (`notebook_editor`); clicking the header's
/// Write toggle flips to the raw markdown source (`notebook_editor_write`).
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

    // Flip to the Write face and snapshot the raw markdown source.
    harness.get_by_label("Write").simulate_click();
    harness.run_steps(3);
    harness.snapshot("notebook_editor_write");
}

/// Seed one longform note with a title, summary and body under a deterministic
/// `d`, for a note-embed to reference. A real NIP-23 note keeps its title in the
/// tag (not repeated as a body heading), so the seed sets one; the `summary` is
/// seeded too so a block-embed preview would differ visibly from the full-body
/// render a note-embed node draws.
fn seed_embed_note(ndb: &Ndb, secret: &[u8; 32], d: &str, title: &str, summary: &str, body: &str) {
    let input = LongformInput {
        title: title.to_string(),
        summary: Some(summary.to_string()),
        content: body.to_string(),
        ..Default::default()
    };
    let builder = build_longform(d, &input).created_at(1_700_000_000);
    ingest(ndb, builder, secret, &mut NoPublish).expect("seed embed longform");
}

/// Seed a note-embed (Link) node whose url is `reference` (a `nostr:naddr…`) onto
/// canvas `canvas_id`. Pass the app's active canvas so the embed lands on the
/// foreground surface (the app mints its own canvas `d` on first run, so a
/// hard-coded id would drop the node on a canvas the app isn't showing). Placed
/// clear of the vault sidebar so the whole embed is visible.
fn seed_embed_canvas(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    canvas_id: &str,
    reference: &str,
) {
    let addr = canvas_address(author, canvas_id);
    let mut publisher = NoPublish;
    ingest(
        ndb,
        build_canvas(canvas_id, "Embed", &[], false),
        secret,
        &mut publisher,
    );
    let geo = Geometry {
        x: 280,
        y: 40,
        w: 380,
        h: 220,
    };
    let content = NodeContent {
        url: Some(reference.to_string()),
        ..Default::default()
    };
    let id = ingest(
        ndb,
        build_node(&addr, NodeKind::Link, &geo, &content),
        secret,
        &mut publisher,
    )
    .expect("embed node ingested");
    let z = event::rank_between(None, None);
    ingest(
        ndb,
        build_transform(canvas_id, &addr, &id, &geo, &z, None),
        secret,
        &mut publisher,
    );
}

/// Seed a longform note and a canvas holding a single note-embed node that
/// references it by naddr, then snapshot the rendered embed — the note's title
/// and *full* markdown body drawn full-node via the longform kind renderer
/// (`RenderContext::Full`), not the summary/head preview a block embed shows.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_note_embed() {
    let mut harness = build_harness(egui::Vec2::new(700.0, 380.0), false, true);

    let secret = harness.state().account.secret_key.secret_bytes();
    let author = harness.state().account.pubkey;
    let ctx = harness.ctx.clone();
    let reference = event::longform_naddr(&author, "embed-00").expect("naddr");
    // Seed the embed onto the app's active (auto-seeded) canvas, so it lands on the
    // foreground surface rather than a hard-coded id the app isn't showing.
    let canvas_id = harness
        .state()
        .notebook
        .active_canvas()
        .expect("the app auto-seeded a canvas during warmup")
        .to_string();
    {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
        seed_embed_note(
            app_ctx.ndb,
            &secret,
            "embed-00",
            "Q3 planning notes",
            "Quarterly goals, milestones, and a few stretch items to revisit at the mid-point review.",
            "# Milestones\n\nShip the notebook vault and the longform editor.\n\n\
             ## Stretch goals\n\n\
             - Cross-device longform sync\n\
             - Note templates and daily notes\n\n\
             Revisit these at the **mid-point review**.",
        );
        seed_embed_canvas(app_ctx.ndb, &author, &secret, &canvas_id, &reference);
    }

    // The longform note must fold in before the embed can resolve it.
    wait_for_vault(&mut harness, 1);
    // And the embed node must fold into the canvas.
    let deadline = Instant::now() + Duration::from_secs(5);
    while harness.state().notebook.canvas().get_nodes().is_empty() {
        harness.run_ok();
        assert!(Instant::now() < deadline, "embed node never folded");
        std::thread::sleep(Duration::from_millis(25));
    }
    harness.run_steps(3);
    harness.snapshot("notebook_note_embed");
}

/// Simulate the vault → canvas drag-drop end to end: seed a longform note, drag
/// its vault row onto the canvas, and snapshot the note-embed node the drop
/// creates — the note's title and full markdown body drawn full-node by the
/// longform renderer, the same shape [`snapshot_notebook_note_embed`] seeds by hand.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_note_embed_drag() {
    let mut harness = build_harness(egui::Vec2::new(900.0, 560.0), false, true);

    let secret = harness.state().account.secret_key.secret_bytes();
    let ctx = harness.ctx.clone();
    {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
        seed_embed_note(
            app_ctx.ndb,
            &secret,
            "drag-00",
            "Q3 planning notes",
            "Quarterly goals, milestones, and a few stretch items to revisit at the mid-point review.",
            "# Milestones\n\nShip the notebook vault and the longform editor.\n\n\
             ## Stretch goals\n\n\
             - Cross-device longform sync\n\
             - Note templates and daily notes\n\n\
             Revisit these at the **mid-point review**.",
        );
    }

    // The row must render (and the note fold in, so the embed resolves) before we
    // drag it.
    wait_for_vault(&mut harness, 1);
    wait_for_label(&mut harness, "Q3 planning notes");
    harness.run_steps(3);

    drag_first_vault_row(&mut harness, egui::pos2(560.0, 250.0));

    // The dropped embed node folds in asynchronously; wait for it before snapshotting.
    let deadline = Instant::now() + Duration::from_secs(5);
    while harness.state().notebook.canvas().get_nodes().is_empty() {
        harness.run_ok();
        assert!(
            Instant::now() < deadline,
            "the dropped embed node never folded"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    harness.run_steps(3);
    harness.snapshot("notebook_note_embed_drag");
}

/// Seed a canvas with a single text node titled `title` near the origin,
/// returning the node's creation event id — the 32-byte identity a
/// `notebook:<word-id>` reference encodes (see [`wordid::node_ref`]).
fn seed_ref_node(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    canvas_id: &str,
    title: &str,
) -> notedeck::enostr::NoteId {
    seed_ref_node_at(ndb, author, secret, canvas_id, title, 40, 40)
}

/// Like [`seed_ref_node`] but places the node at canvas position `(x, y)`, so a
/// test can seed a node deliberately outside the initial viewport. The node is
/// attached to canvas `canvas_id` — pass the app's active canvas so it lands on
/// the foreground surface (the app mints its own canvas `d` on first run).
fn seed_ref_node_at(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    canvas_id: &str,
    title: &str,
    x: i64,
    y: i64,
) -> notedeck::enostr::NoteId {
    let addr = canvas_address(author, canvas_id);
    let mut publisher = NoPublish;
    ingest(
        ndb,
        build_canvas(canvas_id, "Planning", &[], false),
        secret,
        &mut publisher,
    );
    let geo = Geometry {
        x,
        y,
        w: 240,
        h: 100,
    };
    let content = NodeContent {
        text: title.to_string(),
        ..Default::default()
    };
    let id = ingest(
        ndb,
        build_node(&addr, NodeKind::Text, &geo, &content),
        secret,
        &mut publisher,
    )
    .expect("ref node ingested");
    let z = event::rank_between(None, None);
    ingest(
        ndb,
        build_transform(canvas_id, &addr, &id, &geo, &z, None),
        secret,
        &mut publisher,
    );
    id
}

/// Seed a node, reference it by `notebook:<word-id>` inline in a note and a
/// Dave-style message, and snapshot both surfaces — the cross-app demo for the
/// inline-reference epic. Each surface renders through
/// [`render_markdown_with_refs`], the same path `NoteOptions::InlineReferences`
/// drives, and the chip resolves + folds its title from the shared cache the
/// notebook app maintains. Waiting on the node's *title* (not the raw ref text)
/// asserts the reference actually resolved before the snapshot is taken.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_reference_chip() {
    let mut harness = build_harness(egui::Vec2::new(720.0, 460.0), false, true);

    let secret = harness.state().account.secret_key.secret_bytes();
    let author = harness.state().account.pubkey;
    let ctx = harness.ctx.clone();
    let title = "Q3 planning canvas node";
    let canvas_id = harness
        .state()
        .notebook
        .active_canvas()
        .expect("the app auto-seeded a canvas during warmup")
        .to_string();
    let node_ref = {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
        let id = seed_ref_node(app_ctx.ndb, &author, &secret, &canvas_id, title);
        wordid::node_ref(id.bytes())
    };
    harness.state_mut().ref_surface = Some(format!(
        "Captured in {node_ref} — worth a look before Friday."
    ));

    // Wait until the chip renders the resolved node title (not the raw ref) in
    // *both* surfaces, proving the parser + renderer folded it from the shared
    // cache. `query_all_by_label` (not `query_by_label`) because the title is
    // deliberately shown twice — once per surface.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if harness.query_all_by_label(title).count() >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the node reference never resolved to its title chip in both surfaces"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    harness.run_steps(3);
    harness.snapshot("notebook_reference_chip");
}

/// Opening a node whose reference was clicked elsewhere must not just select it
/// but pan the canvas to it — otherwise a node far from the current viewport stays
/// offscreen and the user sees nothing. Seed a node well outside the initial view,
/// drive `Notebook::open` (as chrome does for a clicked chip), and assert the node
/// moves from *outside* `scene_rect` to *inside* it, at the same zoom.
#[test]
fn open_pans_the_canvas_to_an_offscreen_node() {
    let mut harness = build_harness(egui::Vec2::new(820.0, 500.0), false, false);

    let secret = harness.state().account.secret_key.secret_bytes();
    let author = harness.state().account.pubkey;
    let ctx = harness.ctx.clone();

    // Seed a node far outside the initial viewport (which loads at the origin,
    // ~820x500) onto the app's active canvas — the one it auto-seeded during
    // warmup — so the node lands on the foreground surface rather than a sibling
    // canvas the app isn't showing. `open` takes the kind-1606 note id the seed
    // returns.
    let canvas_id = harness
        .state()
        .notebook
        .active_canvas()
        .expect("the app auto-seeded a canvas during warmup")
        .to_string();
    let node_note = {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
        seed_ref_node_at(
            app_ctx.ndb,
            &author,
            &secret,
            &canvas_id,
            "Far away node",
            4000,
            4000,
        )
    };
    // The canvas keys nodes by the hex of their creation event id.
    let jc_id: jsoncanvas::NodeId = node_note.hex().parse().expect("node id");

    // Wait until the node folds into the canvas, then let `notebook_ui` lay out
    // the scene (`scene_rect`) at least once.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if harness
            .state()
            .notebook
            .canvas()
            .get_nodes()
            .contains_key(&jc_id)
        {
            break;
        }
        assert!(Instant::now() < deadline, "the far node never folded");
        std::thread::sleep(Duration::from_millis(25));
    }
    harness.run_steps(3);

    // Precondition: nothing selected, and the node starts offscreen.
    assert_eq!(harness.state().notebook.selected(), None);
    let before = harness.state().notebook.scene_rect();
    let node_pos = harness
        .state()
        .notebook
        .node_position(&jc_id)
        .expect("node position");
    assert!(
        !before.contains(node_pos),
        "test setup: node should start offscreen (scene {before:?} vs node {node_pos:?})"
    );

    // Open it, exactly as chrome does when its inline chip is clicked.
    harness.state_mut().notebook.open(node_note);

    // The reveal selects immediately but pans over several frames (it's animated),
    // so pump until the pan settles and the node is actually in view. Terminal
    // state, not a fixed step count, so the animation length can't flake it.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        let selected = harness.state().notebook.selected() == Some(&jc_id);
        let scene = harness.state().notebook.scene_rect();
        let pos = harness
            .state()
            .notebook
            .node_position(&jc_id)
            .expect("node position");
        if selected && scene.contains(pos) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "open never revealed the node in the viewport"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // The canvas panned to reveal the node: it's now within the viewport...
    let after = harness.state().notebook.scene_rect();
    let node_pos = harness
        .state()
        .notebook
        .node_position(&jc_id)
        .expect("node position");
    assert!(
        after.contains(node_pos),
        "node should be revealed in the viewport after open (scene {after:?} vs node {node_pos:?})"
    );
    // ...by a pan only — the viewport size (zoom) is unchanged.
    assert_eq!(after.size(), before.size());
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

/// Dragging a vault row onto the canvas drops a note-embed Link node referencing
/// that note by naddr — the drag-drop counterpart to pasting a lone `nostr:naddr`.
/// The scene loads 1:1 (screen coords == canvas coords).
#[test]
fn drag_vault_note_creates_embed_node() {
    let mut harness = build_harness(egui::Vec2::new(1000.0, 700.0), false, false);

    let secret = harness.state().account.secret_key.secret_bytes();
    let author = harness.state().account.pubkey;
    let ctx = harness.ctx.clone();
    {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
        seed_embed_note(
            app_ctx.ndb,
            &secret,
            "drag-00",
            "Draggable note",
            "A note to drag onto the canvas.",
            "# Body\n\ncontent",
        );
    }

    // The row must render before it can be dragged.
    wait_for_vault(&mut harness, 1);
    wait_for_label(&mut harness, "Draggable note");
    harness.run_steps(3);
    let before = harness.state().notebook.canvas().get_nodes().len();

    drag_first_vault_row(&mut harness, egui::pos2(600.0, 320.0));

    // The drop ingests a note-embed Link node (async) referencing the note by naddr.
    let reference = event::longform_naddr(&author, "drag-00").expect("naddr");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        let canvas = harness.state().notebook.canvas();
        let embedded = canvas
            .get_nodes()
            .values()
            .any(|n| matches!(n, jsoncanvas::Node::Link(link) if link.url().as_str() == reference));
        if canvas.get_nodes().len() > before && embedded {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the dropped note-embed node never appeared"
        );
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

/// The titles of the account's live (non-deleted) canvases, read through the
/// app's own ndb via the vault projection.
fn canvas_titles(harness: &mut Harness<'static, NotebookTestState>) -> Vec<String> {
    let pubkey = harness.state().account.pubkey;
    let ctx = harness.ctx.clone();
    let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    list_canvases(app_ctx.ndb, &txn, &pubkey)
        .into_iter()
        .map(|c| c.title)
        .collect()
}

/// The title of the canvas keyed by `d`, read through the app's own ndb, or `None`
/// if it's gone (deleted / never folded).
fn canvas_title_of(harness: &mut Harness<'static, NotebookTestState>, d: &str) -> Option<String> {
    let pubkey = harness.state().account.pubkey;
    let ctx = harness.ctx.clone();
    let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    load_canvas(app_ctx.ndb, &txn, &pubkey, d).map(|c| c.title)
}

/// Two deterministically-seeded canvases (fixed ids/titles, "Ideas" the newer so
/// the app adopts it as the active surface) each carrying one distinctly-labelled
/// node — the fixture the multi-canvas open/rename/delete tests share.
fn two_seed_canvases() -> Vec<SeedCanvas> {
    vec![
        SeedCanvas {
            d: "cv-ideas".to_string(),
            title: "Ideas".to_string(),
            created_at: 1_700_000_050,
            nodes: vec![SeedNode {
                text: "Idea one".to_string(),
                x: 320,
                y: 80,
            }],
        },
        SeedCanvas {
            d: "cv-roadmap".to_string(),
            title: "Roadmap".to_string(),
            created_at: 1_700_000_030,
            nodes: vec![SeedNode {
                text: "Ship v1".to_string(),
                x: 320,
                y: 80,
            }],
        },
    ]
}

/// Pump frames until the vault sidebar has listed at least `expected` documents
/// (notes + canvases), or panic after a deadline. The canvas half seeds
/// asynchronously like the notes, so a mixed-vault snapshot taken too early would
/// render a partial (nondeterministic) list.
fn wait_for_vault_docs(harness: &mut Harness<'static, NotebookTestState>, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        let docs = harness.state().notebook.notes().len() + canvas_titles(harness).len();
        if docs >= expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "only {docs} of {expected} vault docs folded in"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Clicking a canvas row in the vault must swap the *background surface* to that
/// canvas — not open the editor. Seed two canvases (each with a distinct node),
/// let the app adopt the newer ("Ideas") as active, click the other ("Roadmap")
/// row, and assert the active canvas swapped and the rendered surface now shows
/// Roadmap's node — with the editor still closed.
#[test]
fn open_canvas_swaps_active_surface() {
    let mut harness =
        build_harness_canvases(egui::Vec2::new(1000.0, 700.0), false, two_seed_canvases());

    // Both canvases fold; the newer one ("Ideas") is the adopted active surface.
    wait_for_vault_docs(&mut harness, 2);
    wait_for_label(&mut harness, "Roadmap");
    let deadline = Instant::now() + Duration::from_secs(5);
    while harness.state().notebook.active_canvas() != Some("cv-ideas") {
        harness.run_ok();
        assert!(
            Instant::now() < deadline,
            "the app never adopted the newer canvas as active (got {:?})",
            harness.state().notebook.active_canvas()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!harness.state().notebook.editor_is_open());

    // Click the *other* canvas's vault row (by its title label — the same way the
    // note tests open a note). It must swap the surface, not open the editor.
    harness.get_by_label("Roadmap").simulate_click();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        let nb = &harness.state().notebook;
        let swapped = nb.active_canvas() == Some("cv-roadmap")
            && nb
                .canvas()
                .get_nodes()
                .values()
                .any(|n| matches!(n, jsoncanvas::Node::Text(t) if t.text() == "Ship v1"));
        if swapped {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the canvas surface never swapped to Roadmap"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !harness.state().notebook.editor_is_open(),
        "opening a canvas must not open the longform editor"
    );
}

/// End-to-end canvas rename from the sidebar: seed two canvases, right-click the
/// top row (the newer "Ideas"), choose Rename, type into the inline field, commit
/// with Enter, and verify the canvas document is superseded with the new title in
/// place (the other canvas untouched).
#[test]
fn rename_canvas_via_vault_context_menu() {
    let mut harness =
        build_harness_canvases(egui::Vec2::new(1000.0, 700.0), false, two_seed_canvases());

    // Both canvases folded into the list; render the sidebar. (Don't wait on the
    // "Ideas" label — the toolbar shows the active canvas's title too, so it isn't
    // unique.)
    wait_for_vault_docs(&mut harness, 2);
    harness.run_steps(3);

    // Right-click a canvas row (both seeded docs are canvases) and choose Rename.
    // Which row `(120,120)` lands on isn't pinned — like the note-rename test we
    // don't depend on it, only that the clicked canvas is renamed in place.
    secondary_click_at(&mut harness, egui::pos2(120.0, 120.0));
    harness.get_by_label("Rename").simulate_click();
    harness.run_ok();

    // Append " v2" to the seeded title and commit with Enter.
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .type_text(" v2");
    harness.run_ok();
    key_press(&mut harness, egui::Key::Enter);

    // The rename supersedes that canvas doc in place: one canvas now ends " v2",
    // both canvases still exist (the other untouched).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        let titles = canvas_titles(&mut harness);
        if titles.len() == 2 && titles.iter().any(|t| t.ends_with(" v2")) {
            break;
        }
        assert!(Instant::now() < deadline, "the canvas was never renamed");
        std::thread::sleep(Duration::from_millis(25));
    }
    // Exactly one seeded canvas was renamed; the other kept its title.
    let ideas = canvas_title_of(&mut harness, "cv-ideas").expect("Ideas canvas");
    let roadmap = canvas_title_of(&mut harness, "cv-roadmap").expect("Roadmap canvas");
    assert!(
        (ideas == "Ideas v2" && roadmap == "Roadmap")
            || (ideas == "Ideas" && roadmap == "Roadmap v2"),
        "exactly one canvas was renamed in place (Ideas={ideas:?}, Roadmap={roadmap:?})"
    );
}

/// End-to-end canvas delete from the sidebar: seed two canvases, right-click the
/// top row ("Ideas"), choose Delete, confirm the modal, and verify a tombstone
/// drops that canvas from the vault while the sibling survives.
#[test]
fn delete_canvas_via_vault_context_menu() {
    let mut harness =
        build_harness_canvases(egui::Vec2::new(1000.0, 700.0), false, two_seed_canvases());

    // Both canvases folded into the list; render the sidebar. (The toolbar echoes
    // the active canvas's title, so "Ideas" isn't a unique label to wait on.)
    wait_for_vault_docs(&mut harness, 2);
    harness.run_steps(3);
    assert_eq!(canvas_titles(&mut harness).len(), 2);

    // Right-click a canvas row (both seeded docs are canvases; row position isn't
    // pinned) and choose Delete, then confirm the modal — worded "Delete canvas?".
    secondary_click_at(&mut harness, egui::pos2(120.0, 120.0));
    harness.get_by_label("Delete").simulate_click();
    harness.run_ok();
    wait_for_label(&mut harness, "Delete canvas?");
    harness.get_by_label("Delete").simulate_click();

    // The tombstone folds in and drops the clicked canvas; exactly one survives,
    // and it's one of the two seeded (not a re-seeded replacement).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if canvas_titles(&mut harness).len() == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the canvas was never deleted (canvases: {:?})",
            canvas_titles(&mut harness)
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let survivor = canvas_titles(&mut harness);
    assert!(
        survivor == ["Ideas"] || survivor == ["Roadmap"],
        "one seeded canvas survives the delete (got {survivor:?})"
    );
}

/// Seed notes and canvases and snapshot the mixed vault sidebar — note rows and
/// canvas rows in one newest-edited list, each with its typed leading icon — for
/// eyeballing the unified vault's visual design.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_vault_mixed() {
    let mut harness =
        build_harness_canvases(egui::Vec2::new(1000.0, 700.0), true, two_seed_canvases());

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

    // Two canvases + three notes.
    wait_for_vault_docs(&mut harness, 5);
    // The seeded canvases must have suppressed the wall-clock auto-seed.
    assert_eq!(harness.state().notebook.active_canvas(), Some("cv-ideas"));
    harness.run_steps(3);
    harness.snapshot("notebook_vault_mixed");
}

/// Snapshot a canvas surface swap driven from the sidebar: seed two canvases, click
/// the non-active "Roadmap" row, and snapshot the swapped-in surface (its node) with
/// the sidebar still listing both canvases.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_canvas_open() {
    let mut harness =
        build_harness_canvases(egui::Vec2::new(1000.0, 700.0), true, two_seed_canvases());

    wait_for_vault_docs(&mut harness, 2);
    wait_for_label(&mut harness, "Roadmap");
    let deadline = Instant::now() + Duration::from_secs(5);
    while harness.state().notebook.active_canvas() != Some("cv-ideas") {
        harness.run_ok();
        assert!(Instant::now() < deadline, "active canvas never settled");
        std::thread::sleep(Duration::from_millis(25));
    }

    harness.get_by_label("Roadmap").simulate_click();
    let deadline = Instant::now() + Duration::from_secs(5);
    while harness.state().notebook.active_canvas() != Some("cv-roadmap") {
        harness.run_ok();
        assert!(
            Instant::now() < deadline,
            "surface never swapped to Roadmap"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    harness.run_steps(3);
    harness.snapshot("notebook_canvas_open");
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

/// Drag the first vault row (its on-screen spot, ~(120,120) — the same row the
/// vault tests target) onto the canvas and release at `onto`, so the drop lands a
/// note-embed node there. Mirrors the node-drag tests' press → drag → drag →
/// release, with intermediate moves to cross egui's drag threshold.
fn drag_first_vault_row(harness: &mut Harness<'static, NotebookTestState>, onto: egui::Pos2) {
    let from = egui::pos2(120.0, 120.0);
    press(harness, from);
    drag_to(harness, egui::pos2(300.0, 200.0));
    drag_to(harness, onto);
    release(harness, onto);
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
