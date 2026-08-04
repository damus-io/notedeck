use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::{Key, Node, Queryable};
use enostr::{FullKeypair, Keypair, NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};
use notedeck::{App, Notedeck};
use notedeck_headway::{Headway, store};

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

        // The production seed is card-less; seed demo cards here (before the app
        // auto-seeds) so the snapshots and flows have content to render against.
        seed_demo(
            app_ctx.ndb,
            &pubkey,
            &state.account.secret_key.secret_bytes(),
        );

        state.fonts_installed = true;
        return;
    }

    let mut app_ctx = state.notedeck.app_context(ctx);
    // Mirror production: chrome runs `update` (sync poll + fan-out + seed) for
    // every opened app each frame, then `render` for the foreground one.
    state.headway.update(&mut app_ctx, ctx);
    egui::CentralPanel::default().show(ctx, |ui| {
        state.headway.render(&mut app_ctx, ui);
    });
}

/// The instant the demo board is seeded at, and what the frozen clock reads.
/// Pinning both (plus the signing key) makes every seeded event — and so the
/// word-ids and relative times the UI renders — identical run to run.
const SEED_AT: u64 = 1_700_000_000;

/// A fixed signing key, so seeded event ids don't vary with a random keypair.
fn test_keypair() -> FullKeypair {
    let secret = enostr::SecretKey::from_slice(&[7u8; 32]).expect("valid test secret");
    let kp = Keypair::from_secret(secret);
    FullKeypair::new(kp.pubkey, kp.secret_key.expect("has secret"))
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
    let mut notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    // The harness renders on this same thread, so the frozen clock covers
    // every relative time the app draws (e.g. the card detail's "created").
    headway::fmt::freeze_now(SEED_AT);

    // Chrome registers every app's inline-reference contributions at startup
    // (`setup_app_registries`); mirror that here, off *this* Headway instance,
    // so the parser and renderers share its board cache and a
    // `board#word-word-word` reference in a card description resolves.
    let headway = Headway::new();
    for renderer in headway.kind_renderers() {
        notedeck.register_kind_renderer(renderer);
    }
    for parser in headway.reference_parsers() {
        notedeck.register_reference_parser(parser);
    }

    let state = HeadwayTestState {
        notedeck,
        headway,
        account: test_keypair(),
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
    wait_for_label(harness, "7 cards · 5 columns");
}

/// Seed the populated demo board for the snapshot/flow tests to render against.
/// The production seed is card-less; the fixture lives in [`store::seed_demo_board`].
fn seed_demo(ndb: &Ndb, pubkey: &Pubkey, secret: &[u8; 32]) {
    store::seed_demo_board(
        ndb,
        pubkey,
        secret,
        store::BOARD_ID,
        SEED_AT,
        &mut store::NoPublish,
    );
}

/// The focused text input — the field a just-opened composer or rename editor
/// grabs. A bare `TextInput` role query is ambiguous now that the board header
/// carries an always-visible filter field; the flows only ever type into the
/// input that took focus, so focus picks the right one.
fn focused_text_input<'h>(harness: &'h Harness<'static, HeadwayTestState>) -> Node<'h> {
    harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.is_focused())
        .expect("a focused text input")
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
/// viewport to exercise the full-pane detail screen (which replaces the board
/// while a card is open).
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

/// A subissue's detail carries a "↳ subissue of" breadcrumb above the title —
/// the demo board parents the sync card under the event-model card.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_subissue_detail() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));

    harness
        .get_by_label("Sync cards across relays")
        .simulate_click();
    harness.run_steps(3);
    harness.snapshot("headway_subissue_detail");
}

/// The id of the demo card titled `title`. Folded fresh off the db rather than
/// hard-coded: the ids are stable (fixed key + frozen clock), but deriving them
/// keeps the test honest if the demo fixture changes.
fn demo_card_id(ndb: &Ndb, author: &Pubkey, title: &str) -> NoteId {
    let txn = Transaction::new(ndb).expect("txn");
    let reducer = headway::event::fold_board(ndb, &txn, author).expect("demo board folded");
    let boards = reducer.finalize();
    let view = headway::event::find_board(&boards, author, store::BOARD_ID).expect("demo board");
    view.columns
        .iter()
        .flat_map(|c| c.cards.iter())
        .find(|c| c.title == title)
        .unwrap_or_else(|| panic!("no demo card titled {title:?}"))
        .id
}

/// The canonical `board#word-word-word` reference for `card` — the same string
/// the detail pane shows in its topbar and the CLI prints.
fn card_ref(card: NoteId) -> String {
    format!(
        "{}#{}",
        store::BOARD_ID,
        headway::wordid::encode(card.bytes())
    )
}

/// A card description that mentions another card by word-id renders it as a live
/// status chip, in headway's own detail pane — the surface where cross-references
/// are densest. Guards both the wiring (the description goes through the ref-aware
/// markdown path) and the re-entrancy: the parser and the issue renderer both
/// borrow the board cache the app is rendering out of.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_detail_inline_ref() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));

    // Point the event-model card's description at the sync card. Scoped so the
    // `AppContext` (and its harness borrow) is dropped before we pump frames
    // again; `ctx` is cloned out first because `state_mut` borrows the harness.
    let description = {
        let egui_ctx = harness.ctx.clone();
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let secret = state.account.secret_key.secret_bytes();
        let app_ctx = &mut state.notedeck.app_context(&egui_ctx);

        let target_ref = card_ref(demo_card_id(
            app_ctx.ndb,
            &author,
            "Sync cards across relays",
        ));
        let host = demo_card_id(app_ctx.ndb, &author, "Define nostr event model for boards");
        let description = format!("Blocked on {target_ref} until the model lands.");

        let txn = Transaction::new(app_ctx.ndb).expect("txn");
        let reducer = headway::event::fold_board(app_ctx.ndb, &txn, &author).expect("folded");
        let boards = reducer.finalize();
        let view =
            headway::event::find_board(&boards, &author, store::BOARD_ID).expect("demo board");
        store::apply(
            app_ctx.ndb,
            store::BOARD_ID,
            view,
            &author,
            &secret,
            store::BoardAction::EditDescription {
                card: host,
                description: description.clone(),
            },
            &mut store::NoPublish,
        );
        description
    };

    // The edit lands through an async ndb ingest, and the detail pane seeds its
    // edit buffer *once* when the card opens — so wait for the board card (which
    // renders the raw description under its title) to carry the new text before
    // opening it, or the pane renders the pre-edit description all run.
    wait_for_label(&mut harness, &description);
    harness
        .get_by_label("Define nostr event model for boards")
        .simulate_click();
    harness.run_steps(3);
    harness.snapshot("headway_detail_inline_ref");
}

/// The detail pane's subissue checklist and its inline composer: the demo
/// board's event-model card starts at 1/2 done (the scaffold child is in Done),
/// and adding a subissue creates a card in Backlog already parented to it.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn add_subissue_flow() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));

    harness
        .get_by_label("Define nostr event model for boards")
        .simulate_click();
    harness.run();

    // The fixture rollup: one of the two children sits in Done.
    harness.get_by_label("1/2");

    // The composer is collapsed behind "+ Add sub-issue" (Linear-style);
    // opening it focuses the field, so typing can start immediately.
    harness.get_by_label("+ Add sub-issue").simulate_click();
    harness.run();
    focused_text_input(&harness).type_text("Write a relay conformance suite");
    harness.run();
    focused_text_input(&harness).key_press(Key::Enter);

    // The new child lands in the checklist (in Backlog, so not done).
    wait_for_label(&mut harness, "1/3");
    wait_for_label(&mut harness, "Write a relay conformance suite");
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
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let card = CardView {
        id: enostr::NoteId::new([1u8; 32]),
        author: [0u8; 32],
        title: "Update headway-cli to use negentropy for sync".to_string(),
        description: String::new(),
        labels: vec!["headway".to_string()],
        priority: headway::event::Priority::None,
        rank: String::new(),
        placed_at: 0,
        created_at: 0,
        updated_at: 0,
        comments: vec![],
        activity: vec![],
        parent: None,
        subissues: vec![],
        due: None,
        estimate: None,
        seq: None,
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

/// How a chip behaves when it meets the edge of a row — the three cases, top to
/// bottom in one 560px column:
///
/// 1. **Fits.** Drawn inline after the text, title in full.
/// 2. **Doesn't fit.** Breaks to a fresh row and draws in full there, the way a
///    word too long for the line would. egui does that for text on its own, but a
///    widget is placed at the cursor and has to fit into what remains, so the
///    chip measures itself first ([`notedeck_ui::inline_chip`]). Without it the
///    pill ellipsized away to nothing against the right edge with an empty row
///    waiting below.
/// 3. **Wider than a whole row.** Breaking can't help, so it truncates — one
///    line, never a second.
///
/// The whole pass runs under `style.wrap_mode = Some(Wrap)`, which is what Dave
/// used to force over its chat. A style override reaches inside every descendant
/// widget, so that folded a chip's title into a tall column of text; the label
/// now names its own wrap mode and can't be overridden from outside.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_inline_chip_row_wrapping() {
    use headway::event::ColumnPos;
    use notedeck_headway::card_chip_ui;

    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let short = "cache the parse";
    let long = "notedeck_ui: cache per-frame markdown parse + reference scan";
    let column = Some(ColumnPos { index: 1, count: 5 });

    let mut installed = false;
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(560.0, 260.0))
        .renderer(notedeck::software_renderer())
        .build_ui(move |ui| {
            if !installed {
                notedeck.setup(ui.ctx());
                ui.ctx().style_mut(|s| s.animation_time = 0.0);
                installed = true;
            }
            let theme = notedeck::ColorTheme::current(ui.ctx());
            // What Dave used to do to its whole chat pass.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);

            let paragraph = |ui: &mut egui::Ui, lead: &str, title: &str| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(lead);
                    card_chip_ui(ui, &theme, title, column);
                    ui.label("last.");
                });
                ui.add_space(12.0);
            };

            // 1. Room on the row: inline, in full.
            paragraph(ui, "Flagged by", short);
            // 2. Not enough room: breaks to its own row, still in full.
            paragraph(ui, "Scanning every block each frame is the cost flagged by", long);
            // 3. Longer than any row: truncates rather than growing a second line.
            paragraph(
                ui,
                "Blocked on",
                "notedeck_ui: resolve inline references in note content blocks, gated behind a NoteOptions bit",
            );
        });

    harness.run();
    harness.snapshot("inline_chip_row_wrapping");
}

/// The compact chip shape ([`notedeck::RenderContext::Inline`]) previewed within
/// a run of flowing text — the Linear/GitHub target look, with the different
/// column-derived status icons.
///
/// NOTE: this lays the chips into a `horizontal_wrapped` paragraph directly to
/// show the *intended* within-line flow. The current `render_markdown_with_refs`
/// splice path still places a reference on its own row *between* markdown blocks
/// (md-stream wraps elements in `ui.vertical`), so wiring the chip to flow truly
/// inline — as a ref inline-element inside a paragraph — is separate scanner /
/// md-stream work (the epic's whisper-crop-merge + Dave demo steps).
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_inline_chip() {
    use headway::event::ColumnPos;
    use notedeck_headway::card_chip_ui;

    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let mut installed = false;
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(560.0, 200.0))
        .renderer(notedeck::software_renderer())
        .build_ui(move |ui| {
            if !installed {
                notedeck.setup(ui.ctx());
                ui.ctx().style_mut(|s| s.animation_time = 0.0);
                installed = true;
            }
            let theme = notedeck::ColorTheme::current(ui.ctx());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label("Landed the fix in");
                // A board of 5 columns: index 2 → a "started" icon.
                card_chip_ui(
                    ui,
                    &theme,
                    "Render inline references as chips",
                    Some(ColumnPos { index: 2, count: 5 }),
                );
                ui.label("which unblocks");
                // index 0 → the backlog icon.
                card_chip_ui(
                    ui,
                    &theme,
                    "Thread AppContext through Dave",
                    Some(ColumnPos { index: 0, count: 5 }),
                );
                ui.label("and closes out the epic — superseding");
                // Archived (no live column) → muted fallback icon.
                card_chip_ui(ui, &theme, "The earlier sketch", None);
                ui.label("entirely.");
            });
        });

    harness.run();
    harness.snapshot("inline_chip");
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
    // Wide enough that all five seeded columns plus the "+ Add column"
    // affordance are on-screen, so the simulated clicks land on them. Five
    // 280px columns plus the add-column frame and gaps overflow 1600px, which
    // would clip the affordance off the scroll area and make the click miss.
    let mut harness = headway_harness(egui::Vec2::new(2000.0, 800.0));

    // Precondition: the seeded board has five columns.
    harness.get_by_label("7 cards · 5 columns");

    // Open the add-column composer.
    harness.get_by_label("+ Add column").simulate_click();
    harness.run();

    // Type into the (auto-focused) composer field, then commit via "Add". The
    // field has no label, so target it by focus.
    focused_text_input(&harness).type_text("Ideas");
    harness.run();
    harness.get_by_label("Add").simulate_click();

    // A sixth column now exists (asserted via the always-visible board summary,
    // since the new column itself renders off-screen to the right). The ingest
    // is async, so wait for the reload.
    wait_for_label(&mut harness, "7 cards · 6 columns");
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
    focused_text_input(&harness).key_combination(&[Key::Command, Key::A]);
    harness.run();
    focused_text_input(&harness).type_text("Inbox");
    harness.run();
    focused_text_input(&harness).key_press(Key::Enter);

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
/// the first column, so the board keeps all seven cards but drops to four
/// columns.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn delete_column_flow() {
    let mut harness = headway_harness(egui::Vec2::new(1600.0, 800.0));

    harness.get_by_label("7 cards · 5 columns"); // precondition

    open_first_column_menu(&mut harness);
    harness.get_by_label("Delete column").simulate_click();

    wait_for_absent(&mut harness, "Backlog");
    // Cards survive the column removal (they reflow into the first column).
    harness.get_by_label("7 cards · 4 columns");
}

/// Drive the add-card flow: open a column's composer, type a title, commit with
/// Enter, and confirm the new card shows up in that column. Then — without
/// re-opening the composer — type a second title and commit again, exercising
/// the rapid-entry path where the composer stays open and focused after an add.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn add_card_flow() {
    let mut harness = headway_harness(egui::Vec2::new(1600.0, 800.0));

    harness.get_by_label("7 cards · 5 columns"); // precondition

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
    harness.get_by_label("8 cards · 5 columns");

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
    harness.get_by_label("9 cards · 5 columns");
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
