use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::{Key, Node, Queryable};
use enostr::{FullKeypair, Keypair, NoteId, Pubkey};
use nostrdb::{Filter, IngestMetadata, Ndb, NoteBuilder, Subscription, Transaction};
use notedeck::{App, AppContext, Notedeck};
use notedeck_headway::{Headway, event, store};

/// The two-party suite's SNS helpers; here we reuse only its gift-wrap key-share
/// builder so the shared-board flows below don't hand-roll a kind-1059 envelope.
mod common;

struct HeadwayTestState {
    notedeck: Notedeck,
    headway: Headway,
    /// Signing account injected on first frame so Headway can seed + edit its
    /// event-backed board.
    account: FullKeypair,
    _tmpdir: tempfile::TempDir,
    fonts_installed: bool,
    /// When set, the harness renders this note through `NoteView` instead of the
    /// Headway app — how the note-renderer inline-reference snapshot views a
    /// kind-1 note that mentions a card. Headway's `update` still runs each frame
    /// so its board cache (which the reference parser + issue renderer resolve
    /// against) stays live.
    ref_note: Option<NoteId>,
    /// When set, the harness renders this chrome global-history entry via
    /// [`App::render_nav`] (with this exact route token) instead of the plain
    /// [`App::render`] root — mirroring how the chrome draws the top nav entry, so
    /// a test can exercise the board↔card port's seeding both ways (a `Card` token
    /// shows the detail, a `Board`/`()` token returns to the grid).
    nav_token: Option<std::rc::Rc<dyn std::any::Any>>,
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

    // The inline-reference snapshot views a kind-1 note through NoteView rather
    // than the Headway app; the board sync above keeps its cache live either way.
    if let Some(note_id) = state.ref_note {
        render_ref_note(ctx, &mut app_ctx, note_id);
        return;
    }

    egui::CentralPanel::default().show(ctx, |ui| {
        // Mirror the chrome: when a global-history entry is set, draw it through
        // `render_nav` with its route token (the chrome always reaches an app this
        // way); otherwise the plain `render` root.
        match &state.nav_token {
            Some(token) => {
                state.headway.render_nav(&mut app_ctx, ui, token);
            }
            None => {
                state.headway.render(&mut app_ctx, ui);
            }
        }
    });
}

/// Render `note_id` (a kind-1 note) through `NoteView`, the surface the
/// note-renderer inline-reference feature lights up: a `headway:board/word-word-word` in
/// the note's content resolves to the card's live status chip via the registered
/// reference parser + issue renderer, with no note→headway dependency.
fn render_ref_note(ctx: &egui::Context, app_ctx: &mut AppContext, note_id: NoteId) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.add_space(16.0);
        let mut note_context = app_ctx.note_context();
        let txn = Transaction::new(note_context.ndb).expect("txn");
        // `ndb` is a shared `&` on NoteContext, so the note borrow doesn't pin the
        // context we hand to NoteView.
        let Ok(note) = note_context.ndb.get_note_by_id(&txn, note_id.bytes()) else {
            ui.label("(waiting for note to ingest)");
            return;
        };
        notedeck_ui::NoteView::new(
            &mut note_context,
            &note,
            notedeck_ui::NoteOptions::default(),
        )
        .show(ui);
    });
}

/// Sign and ingest a kind-1 note, returning its id and a subscription that fires
/// once the async writer has committed it (subscribe *before* ingesting, then
/// `wait_for_notes` — never a sleep). Pins `created_at` to the frozen clock so the
/// note's id is stable across runs.
fn ingest_kind1(ndb: &Ndb, content: &str, secret: &[u8; 32]) -> (NoteId, Subscription) {
    let note = NoteBuilder::new()
        .content(content)
        .kind(1)
        .created_at(SEED_AT)
        .sign(secret)
        .build()
        .expect("note builds");
    let id = NoteId::new(*note.id());
    let sub = ndb
        .subscribe(&[Filter::new().ids([id.bytes()]).build()])
        .expect("subscribe");
    let json = enostr::ClientMessage::event(&note)
        .expect("client msg")
        .to_json()
        .expect("json");
    ndb.process_event_with(&json, IngestMetadata::new().client(true))
        .expect("ingest");
    (id, sub)
}

/// The instant the demo board is seeded at, and what the frozen clock reads.
/// Pinning both (plus the signing key) makes every seeded event — and so the
/// word-ids and relative times the UI renders — identical run to run.
const SEED_AT: u64 = 1_700_000_000;

/// A fixed signing key, so seeded event ids don't vary with a random keypair.
fn test_keypair() -> FullKeypair {
    fixed_keypair(7)
}

/// A deterministic keypair from a single fill byte — the account plus every
/// stand-in co-member (see the shared-board flows) use fixed keys so coordinates,
/// switcher ordering, and snapshots reproduce run to run.
fn fixed_keypair(fill: u8) -> FullKeypair {
    let secret = enostr::SecretKey::from_slice(&[fill; 32]).expect("valid test secret");
    let kp = Keypair::from_secret(secret);
    FullKeypair::new(kp.pubkey, kp.secret_key.expect("has secret"))
}

/// Build a fully-initialised [`HeadwayTestState`] (fonts pending, account keyed,
/// demo board seeded on the first frame). Shared by both harness constructors —
/// the pixel-snapshot [`headway_harness`] adds the wgpu software renderer, the
/// behavioural [`behavioral_harness`] omits it so it needs no GPU.
fn headway_state() -> HeadwayTestState {
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
    // `headway:board/word-word-word` reference in a card description resolves.
    let headway = Headway::new();
    for renderer in headway.kind_renderers() {
        notedeck.register_kind_renderer(renderer);
    }
    for parser in headway.reference_parsers() {
        notedeck.register_reference_parser(parser);
    }

    HeadwayTestState {
        notedeck,
        headway,
        account: test_keypair(),
        _tmpdir: tmpdir,
        fonts_installed: false,
        ref_note: None,
        nav_token: None,
    }
}

/// The shared [`Harness::builder`] both constructors use, minus the renderer:
/// `size` and a raised `max_steps`. `wake()` schedules an 8-frame
/// `request_repaint_after` burst to poll for async ndb ingests; the harness's
/// simulated clock elapses each delay immediately, so a single `run()` can take
/// ~8 steps. Lift the default cap of 4 above that burst so the wait loops don't
/// spuriously panic.
fn harness_builder(size: egui::Vec2) -> egui_kittest::HarnessBuilder<HeadwayTestState> {
    Harness::builder().with_size(size).with_max_steps(16)
}

/// Build a harness at `size` with fonts installed, a signing account injected,
/// and the default board seeded + materialised. Renders through the wgpu software
/// renderer, so `snapshot()` works but a CPU Vulkan adapter (lavapipe) is
/// required — these tests are `#[ignore]`d and run via `scripts/snapshot-test`.
fn headway_harness(size: egui::Vec2) -> Harness<'static, HeadwayTestState> {
    let mut harness = harness_builder(size)
        .renderer(notedeck::software_renderer())
        .build_state(render_headway, headway_state());

    wait_for_board(&mut harness);
    harness
}

/// Like [`headway_harness`] but with **no** wgpu renderer, so it builds and drives
/// `update()`/`render()` under plain `cargo test` — no CPU Vulkan adapter
/// (lavapipe) needed. Use it for behavioural flows that only assert on the
/// accesskit tree (via [`wait_for_label`]); it cannot `snapshot()` (that rasterises
/// through the renderer, which is exactly what needs lavapipe).
fn behavioral_harness(size: egui::Vec2) -> Harness<'static, HeadwayTestState> {
    let mut harness = harness_builder(size).build_state(render_headway, headway_state());

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
        harness.run_ok();
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
        harness.run_ok();
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
    harness.run_ok();

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

/// A blocked card's detail shows its dependency edges: the demo seed blocks the
/// sync card on the event-model card (open) and the scaffold card (in Done, so a
/// *cleared*, struck-through blocker), so its detail carries a "Blocked by" list
/// spanning both states plus a "Blocks" list (it holds back the card-detail
/// card). The board listing behind it marks blocked cards with a dim ⊘.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_blocked_detail() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));

    harness
        .get_by_label("Sync cards across relays")
        .simulate_click();
    harness.run_steps(3);
    harness.snapshot("headway_blocked_detail");
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

/// The canonical `headway:<board>/<word-word-word>` reference for `card` — the
/// same string the detail pane shows in its topbar and the CLI prints.
fn card_ref(card: NoteId) -> String {
    headway::wordid::card_ref(store::BOARD_ID, card.bytes())
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
            &store::Signer::new(&secret, None),
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

/// A card's detail pane must fold in edits that arrive *while it stays open* — a
/// `headway` CLI move or a relay peer's edit. Its title and description render
/// from edit buffers that historically seeded only once on open, so a remote
/// edit went stale until the pane was closed and reopened (comments/labels/status
/// updated live because they render straight from the fresh board). Open the card
/// first, then edit it through the store the way an external writer would, and
/// assert the new text shows without leaving the pane.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn detail_pane_live_updates_open_card() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));

    // Open the card first: this seeds the title/description edit buffers from the
    // board as it stands now.
    harness
        .get_by_label("Define nostr event model for boards")
        .simulate_click();
    harness.run_ok();

    // Now edit the open card the way a `headway` CLI run or a relay peer would —
    // straight into the store, not through the pane's own editor. Scoped so the
    // `AppContext` (and its harness borrow) drops before we pump frames again.
    let new_title = "Define nostr event model for boards (edited live)";
    let new_desc = "Edited while the detail pane was open.";
    {
        let egui_ctx = harness.ctx.clone();
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let secret = state.account.secret_key.secret_bytes();
        let app_ctx = &mut state.notedeck.app_context(&egui_ctx);
        let card = demo_card_id(app_ctx.ndb, &author, "Define nostr event model for boards");
        let signer = store::Signer::new(&secret, None);

        // Re-fold before each edit so the second action builds on the first.
        for action in [
            store::BoardAction::EditTitle {
                card,
                title: new_title.to_string(),
            },
            store::BoardAction::EditDescription {
                card,
                description: new_desc.to_string(),
            },
        ] {
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
                &signer,
                action,
                &mut store::NoPublish,
            );
        }
    }

    // Without closing the pane, the new title and description must appear as the
    // async ingest lands — the regression rendered the pre-edit text until reopen.
    wait_for_label(&mut harness, new_title);
    wait_for_label(&mut harness, new_desc);
}

/// A plain kind-1 nostr note that mentions a card by its `headway:board/word-word-word`
/// id renders that reference as the card's live status chip in NoteView — the
/// note-renderer counterpart to the detail-pane test above. Proves the browser's
/// reference parser fires on note *content* (nostrdb shatters `#`-anchored ids
/// across Text/Hashtag blocks, so the note renderer reconstructs the run before
/// scanning) with no notedeck_ui→headway dependency.
#[tokio::test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
async fn snapshot_note_inline_ref() {
    let mut harness = headway_harness(egui::Vec2::new(560.0, 160.0));

    // Post a note referencing a seeded card, then flip the harness to view it
    // through NoteView. Scoped so the `AppContext` borrow is dropped before we
    // pump frames; `ctx` is cloned out first because `state_mut` borrows the
    // harness.
    let card_title = "Sync cards across relays";
    let (ndb, sub) = {
        let egui_ctx = harness.ctx.clone();
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let secret = state.account.secret_key.secret_bytes();
        let app_ctx = &mut state.notedeck.app_context(&egui_ctx);

        let target = card_ref(demo_card_id(app_ctx.ndb, &author, card_title));
        let content = format!("picking up {target} next week");
        let (note_id, sub) = ingest_kind1(app_ctx.ndb, &content, &secret);
        state.ref_note = Some(note_id);
        (app_ctx.ndb.clone(), sub)
    };

    // Await the async ndb write, then let the UI settle. `wait_for_label` on the
    // referenced card's title proves the reference resolved to its live chip
    // (drawn as a `Label`), not just that the note ingested.
    ndb.wait_for_notes(sub, 1).await.expect("note ingested");
    wait_for_label(&mut harness, card_title);
    harness.run_steps(3);
    harness.snapshot("note_inline_ref");
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
    harness.run_ok();

    // The fixture rollup: one of the two children sits in Done.
    harness.get_by_label("1/2");

    // The composer is collapsed behind "+ Add sub-issue" (Linear-style);
    // opening it focuses the field, so typing can start immediately.
    harness.get_by_label("+ Add sub-issue").simulate_click();
    harness.run_ok();
    focused_text_input(&harness).type_text("Write a relay conformance suite");
    harness.run_ok();
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
        blocked_by: vec![],
        blocks: vec![],
        related: vec![],
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

    harness.run_ok();
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

    harness.run_ok();
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

    harness.run_ok();
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
    harness.run_ok();
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
    harness.run_ok();

    // Type into the (auto-focused) composer field, then commit via "Add". The
    // field has no label, so target it by focus.
    focused_text_input(&harness).type_text("Ideas");
    harness.run_ok();
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
    harness.run_ok();

    // The header is now an inline field seeded with "Backlog". Select all
    // (Command+A maps to egui's select-all), replace it, and commit with Enter.
    focused_text_input(&harness).key_combination(&[Key::Command, Key::A]);
    harness.run_ok();
    focused_text_input(&harness).type_text("Inbox");
    harness.run_ok();
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
        harness.run_ok();
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
    harness.run_ok();

    // Enter (not the "Add" button) must commit the card — the composer is a
    // multiline field, which swallows Enter into a newline, so this is the path
    // that used to silently do nothing.
    harness
        // The card composer is multiline, so it has the MultilineTextInput role.
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("Write integration tests");
    harness.run_ok();
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
    harness.run_ok();
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
        harness.run_ok();
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

// ---------------------------------------------------------------------------
// Coordinate-aware board addressing, end to end (headway:headway/visa-water-sniff)
//
// These drive the *wired* experience the parent card's unit/integration tests
// couldn't reach: the real egui render loop routing an owner's own shared board
// through the multi-writer fold, the switcher keeping same-slug boards distinct,
// and the saved selection surviving a restart by coordinate.
// ---------------------------------------------------------------------------

/// The active board's switcher button label — its title plus the dropdown caret,
/// exactly as `board_switcher` composes it (`"{title}  ⏷"`, two spaces). The demo
/// board is titled "Headway".
const SWITCHER_LABEL: &str = "Headway  ⏷";

/// Poll the shared board at `board_addr` (async ingest) until its sealed
/// definition has folded in, returning the folded view. Sleeps between reads
/// rather than pumping frames — the caller holds an `AppContext` borrow — so it
/// waits on the ndb writer thread, not the render loop. Panics past a deadline.
fn wait_shared_board(ndb: &Ndb, board_addr: &str, team_pubkey: &Pubkey) -> event::BoardView {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        {
            let txn = Transaction::new(ndb).expect("txn");
            if let Some(view) =
                event::load_shared_board(ndb, &txn, board_addr, std::slice::from_ref(team_pubkey))
            {
                return view;
            }
        }
        assert!(
            Instant::now() < deadline,
            "shared board {board_addr:?} definition never folded"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Poll `author`'s own board `slug` (async ingest) until it has folded in,
/// returning the folded view. The own-board analogue of [`wait_shared_board`].
fn wait_own_board(ndb: &Ndb, author: &Pubkey, slug: &str) -> event::BoardView {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        {
            let txn = Transaction::new(ndb).expect("txn");
            if let Some(view) = event::load_board(ndb, &txn, author, slug) {
                return view;
            }
        }
        assert!(Instant::now() < deadline, "own board {slug:?} never folded");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Pump frames until `author`'s persisted board preference (kind-30623, read back
/// through the app's own nostrdb) resolves to `slug`, or panic past a deadline.
/// Used to fence a restart against racing an un-committed preference save.
fn wait_for_saved_slug(
    harness: &mut Harness<'static, HeadwayTestState>,
    author: &Pubkey,
    slug: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        let saved = {
            let egui_ctx = harness.ctx.clone();
            let state = harness.state_mut();
            let app_ctx = &mut state.notedeck.app_context(&egui_ctx);
            event::load_board_pref(app_ctx.ndb, author)
        };
        if saved.as_ref().map(|c| c.slug.as_str()) == Some(slug) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "board preference never persisted {slug:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Ingest a self-addressed kind-1082 key-share naming `board_addr`, derived from a
/// fresh deterministic root (`root_fill`) — the db half of the account joining
/// that board's channel. The app's live key-share poll then reconstructs the
/// roster and lists the board in the switcher; a shared board with no sealed
/// definition simply falls back to its slug for a title.
fn ingest_keyshare(
    ndb: &Ndb,
    sender: &FullKeypair,
    recipient: &Pubkey,
    root_fill: u8,
    board_addr: &str,
) {
    let root = [root_fill; 32];
    let giftwrap = common::gift_wrapped_keyshare(sender, recipient, &root, Some(board_addr), None);
    ndb.process_event(&format!("[\"EVENT\",\"kg\",{giftwrap}]"))
        .expect("ingest keyshare giftwrap");
}

/// Deliverable 1 (behavioural, no lavapipe): a board you OWN and shared routes
/// through the multi-writer shared fold in the real render loop, so a co-member's
/// card — authored by a *second* keypair, sealed under the team key, anchored at
/// your coordinate — shows in YOUR UI. Before the coordinate-addressing fix the
/// owner's own shared board folded author-scoped, so this card was invisible.
///
/// Mirrors `shared_board_folds_via_cache`'s SNS setup but proves it through
/// `update()` + `render()` rather than the cache in isolation.
#[test]
fn own_shared_board_folds_teammate_card_in_render() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let account = test_keypair();

    // A distinct second member authors the card that must surface in the owner's
    // fold — the whole point of the fix.
    let teammate = fixed_keypair(0x2b);
    const TEAMMATE_CARD: &str = "co-member sealed card";

    // A fixed team_root (distinctive bytes) → deterministic channel keys.
    let mut root = [0u8; 32];
    root[0] = 0x53;
    root[31] = 0x11;
    let channel = store::SnsChannel {
        keys: enostr::sns::derive_sns_keys(&root).expect("derive sns keys"),
    };
    let team_pubkey = channel.keys.team_keypair.pubkey;

    // The active board defaults to the account's own `headway` coordinate; the
    // self-share names exactly that coordinate, so once the roster reconstructs it
    // the active board routes through the shared fold with no explicit switch.
    let board_addr = event::board_address(&account.pubkey, store::BOARD_ID);

    {
        let egui_ctx = harness.ctx.clone();
        let state = harness.state_mut();
        let app_ctx = &mut state.notedeck.app_context(&egui_ctx);
        let ndb: &Ndb = app_ctx.ndb;

        // Register the channel root so nostrdb unwraps its kind-1081 envelopes on
        // ingest (the app re-registers idempotently once it sees the 1082).
        assert!(ndb.add_team_root(&root), "team root registers");

        // Self-share: gift-wrap a kind-1082 key-share to the account naming the
        // coordinate, so `teams_from_ndb` reconstructs the roster (a team-of-one)
        // and the app's live key-share poll flips the active board to shared. This
        // share must name our fixed `root` (the channel we seal into below), so
        // build it explicitly rather than through `ingest_keyshare`'s fresh root.
        let giftwrap = common::gift_wrapped_keyshare(
            &account,
            &account.pubkey,
            &root,
            Some(&board_addr),
            None,
        );
        ndb.process_event(&format!("[\"EVENT\",\"kg\",{giftwrap}]"))
            .expect("ingest keyshare");

        // Seal the board definition into the channel (owner-authored). A shared
        // board has no plaintext leg, so the definition itself must travel sealed
        // for `fold_shared_board` to resolve the board at all.
        let cols = vec![
            event::ColumnDef::new("backlog", "Backlog"),
            event::ColumnDef::new("todo", "Todo"),
            event::ColumnDef::new("done", "Done"),
        ];
        store::ingest_signed(
            ndb,
            event::build_board(store::BOARD_ID, "Shared board", "", &cols),
            &store::Signer::shared(&account.secret_key.secret_bytes(), &channel),
            &mut store::NoPublish,
        );

        // Wait for the sealed definition to fold in (async ingest), then seal a
        // card AUTHORED BY THE TEAMMATE off that shared view — `store::apply`
        // anchors it at the OWNER's coordinate (carried by `view.author`), the
        // coordinate the owner's own fold now gathers.
        let view = wait_shared_board(ndb, &board_addr, &team_pubkey);
        store::apply(
            ndb,
            store::BOARD_ID,
            &view,
            &teammate.pubkey,
            &store::Signer::shared(&teammate.secret_key.secret_bytes(), &channel),
            store::BoardAction::AddCard {
                col: 0,
                title: TEAMMATE_CARD.to_string(),
                description: String::new(),
                labels: vec![],
                parent: None,
            },
            &mut store::NoPublish,
        );
    }

    // Drive update()+render() until the teammate's card appears in the owner's UI:
    // proof the *active own* board routed through the shared fold and gathered a
    // co-member's coordinate-anchored, team-sealed card.
    wait_for_label(&mut harness, TEAMMATE_CARD);
}

/// Runtime click-through (behavioural, no lavapipe): drilling from the board into
/// a card enqueues exactly one chrome global-history push carrying that card's
/// route token, so the browser back chevron returns to the board. Drives a real
/// pointer click through `render()` and inspects the [`Navigator`] queue the
/// chrome would drain — the app-side half of the board↔card global-nav port.
#[test]
fn opening_a_card_pushes_a_global_nav_entry() {
    use notedeck::NavRequest;
    use notedeck_headway::HeadwayRoute;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));

    const CARD: &str = "Define nostr event model for boards";
    // A card title is a non-interactive label; a real pointer click lands on the
    // drag-source card surface beneath it and opens the detail (the same trick the
    // detail snapshots use). Then wait for the detail's "← Back" back affordance
    // so the board↔card diff has run and enqueued the push.
    harness.get_by_label(CARD).simulate_click();
    wait_for_label(&mut harness, "← Back");

    let egui_ctx = harness.ctx.clone();
    let state = harness.state_mut();
    let author = state.account.pubkey;
    let app_ctx = state.notedeck.app_context(&egui_ctx);
    let card = demo_card_id(app_ctx.ndb, &author, CARD);

    // Nothing drains the Navigator in this chrome-less harness, so every request
    // the app enqueued since boot is still here: opening one card must be exactly
    // one self-owned push carrying a `Card` route for the clicked card, its title
    // snapshotted for the history dropdown.
    let requests = app_ctx.navigator.take();
    let pushed: Vec<&HeadwayRoute> = requests
        .iter()
        .filter_map(|req| match req {
            NavRequest::PushToActive(entry) => entry.token.downcast_ref::<HeadwayRoute>(),
            _ => None,
        })
        .collect();

    assert_eq!(
        pushed.len(),
        1,
        "opening a card pushes exactly one global-nav entry"
    );
    assert_eq!(
        pushed[0].card_id(),
        Some(card),
        "push targets the clicked card"
    );
    assert_eq!(
        pushed[0].title(),
        Some(CARD),
        "the entry snapshots the card title for the history dropdown"
    );
}

/// Runtime round-trip (behavioural, no lavapipe): the chrome draws a global-history
/// entry via `render_nav` with its route token, so a `Card` token opens the detail
/// and — the browser back result — a `Board` token returns to the grid, regardless
/// of any stale selection. Proves `render_nav` seeds the open card from the nav
/// stack rather than lingering view-state, the app half of chrome-back.
#[test]
fn render_nav_seeds_board_and_card_from_the_route_token() {
    use notedeck_headway::HeadwayRoute;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));

    const CARD: &str = "Define nostr event model for boards";
    let card = {
        let egui_ctx = harness.ctx.clone();
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context(&egui_ctx);
        demo_card_id(app_ctx.ndb, &author, CARD)
    };

    // Chrome pushes a Card entry: render_nav seeds that card, so its detail opens
    // (the "← Back" back affordance is only in the detail top bar).
    harness.state_mut().nav_token = Some(std::rc::Rc::new(HeadwayRoute::card(
        card,
        Some(CARD.into()),
    )));
    wait_for_label(&mut harness, "← Back");

    // Chrome-back pops to the board's root entry (a `Board` token): render_nav
    // reseeds no open card, so the grid returns even though the detail was just up.
    harness.state_mut().nav_token = Some(std::rc::Rc::new(HeadwayRoute::Board));
    wait_for_absent(&mut harness, "← Back");
    wait_for_label(&mut harness, "7 cards · 5 columns");
}

/// Full chrome round-trip (behavioural, no lavapipe): replicate the chrome's global
/// nav loop — render the stack's top entry via `render_nav`, then drain the app's
/// `Navigator` requests into a real `NavStack<ChromeNavEntry>` exactly like
/// `Chrome::apply_nav_requests` — and prove a real card click grows the stack and a
/// back step returns to the board. This closes the gap the two tests above leave
/// (they drive `render_nav`/the push in isolation); here the push actually lands in
/// the chrome stack and back actually pops it.
#[test]
fn chrome_nav_loop_card_open_then_back_returns_to_board() {
    use notedeck::{AppId, ChromeNavEntry, NavRequest, NavStack};
    use notedeck_headway::HeadwayRoute;
    use std::rc::Rc;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));

    // The chrome seeds one entry per app-switch with a `()` token; Headway's home
    // (board root) entry is that. AppId is arbitrary here (single app under test).
    let mut stack: NavStack<ChromeNavEntry> =
        NavStack::new(vec![ChromeNavEntry::new(AppId(0), Rc::new(()))]);

    // One chrome frame: draw the stack top through `render_nav` (its token), pump
    // the harness, then drain the app's queued nav requests into the stack the way
    // `Chrome::apply_nav_requests` does — `PushToActive`/`Back` are the only kinds
    // Headway raises. A self-push inherits the active (top) app's slot.
    fn chrome_frame(
        harness: &mut Harness<'static, HeadwayTestState>,
        stack: &mut NavStack<ChromeNavEntry>,
    ) {
        harness.state_mut().nav_token = Some(stack.top().token.clone());
        harness.run_ok();

        let egui_ctx = harness.ctx.clone();
        let state = harness.state_mut();
        let app_ctx = state.notedeck.app_context(&egui_ctx);
        let active = stack.top().app;
        for request in app_ctx.navigator.take() {
            match request {
                NavRequest::PushToActive(entry) => stack.route_to(entry.tag(active)),
                // A back step from the chevron is instant here (go_to_route), so the
                // app never raises one; handle it for completeness.
                NavRequest::Back => {
                    stack.go_back();
                }
                _ => panic!("unexpected nav request kind from Headway"),
            }
        }
    }

    // The board root shows the grid; no card entry yet.
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 1, "board root only until a card is opened");
    assert!(
        harness.query_by_label("7 cards · 5 columns").is_some(),
        "the board grid renders at the root"
    );

    // Click a card. The click lands on the next pump inside chrome_frame, which then
    // drains the resulting self-push into the stack.
    const CARD: &str = "Define nostr event model for boards";
    let card = {
        let egui_ctx = harness.ctx.clone();
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context(&egui_ctx);
        demo_card_id(app_ctx.ndb, &author, CARD)
    };
    harness.get_by_label(CARD).simulate_click();
    chrome_frame(&mut harness, &mut stack);

    // The click pushed a Card entry onto the global stack (so the back chevron is now
    // live), carrying the clicked card's route.
    assert_eq!(stack.len(), 2, "opening a card grows the global stack");
    assert_eq!(
        stack
            .top()
            .token
            .downcast_ref::<HeadwayRoute>()
            .and_then(|r| r.card_id()),
        Some(card),
        "the pushed entry is the clicked card"
    );

    // Render the new top (the Card entry): its detail opens.
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "← Back");

    // Drill from the card into one of its subissues. This is the case that was
    // broken: a card→card jump used to `replace` the top, which collapses the whole
    // history (`ReplacementType::All`) — dropping the board root so a global-back had
    // nowhere to return. It must PUSH instead, growing the stack to three.
    const SUBISSUE: &str = "Sync cards across relays";
    let subissue = {
        let egui_ctx = harness.ctx.clone();
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context(&egui_ctx);
        demo_card_id(app_ctx.ndb, &author, SUBISSUE)
    };
    // The subissue title shows up in more than one place in the parent detail (the
    // checklist and a dependency list); either row navigates to it via `OpenCard`,
    // so click the first match.
    harness
        .get_all_by_label(SUBISSUE)
        .next()
        .expect("subissue row present in the parent detail")
        .simulate_click();
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(
        stack.len(),
        3,
        "drilling into a subissue pushes (does not collapse the stack)"
    );
    assert_eq!(
        stack
            .top()
            .token
            .downcast_ref::<HeadwayRoute>()
            .and_then(|r| r.card_id()),
        Some(subissue),
        "the pushed entry is the subissue"
    );
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "← Back");

    // Browser back from the subissue returns to its parent card (not straight to the
    // board), and a second back returns to the board grid — a walkable trail.
    stack.go_to_route(stack.len() - 2);
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(
        stack
            .top()
            .token
            .downcast_ref::<HeadwayRoute>()
            .and_then(|r| r.card_id()),
        Some(card),
        "back from the subissue returns to the parent card"
    );

    stack.go_to_route(stack.len() - 2);
    assert_eq!(stack.len(), 1, "a second back pops to the board root");
    chrome_frame(&mut harness, &mut stack);
    wait_for_absent(&mut harness, "← Back");
    assert!(
        harness.query_by_label("7 cards · 5 columns").is_some(),
        "back returns to the board grid"
    );
}

/// Deliverable 3 (behavioural, no lavapipe): the selected board survives a restart
/// keyed by COORDINATE. Switch to a second own board through the real switcher,
/// drop + rebuild `Headway` against the same ndb + account (a cold boot), and it
/// reopens on the saved coordinate — the kind-30623 preference round-trips —
/// instead of falling back to the default `headway` board.
#[test]
fn restart_reopens_saved_board_coordinate() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let account = test_keypair();
    const ROADMAP_CARD: &str = "roadmap-only card";

    // Seed a distinct second own board carrying a card only it has, so which board
    // is active after the restart is unambiguous from what renders.
    {
        let egui_ctx = harness.ctx.clone();
        let state = harness.state_mut();
        let app_ctx = &mut state.notedeck.app_context(&egui_ctx);
        let ndb: &Ndb = app_ctx.ndb;
        let secret = account.secret_key.secret_bytes();
        store::seed_board(
            ndb,
            &account.pubkey,
            &secret,
            "roadmap",
            "Roadmap",
            &mut store::NoPublish,
        );
        let view = wait_own_board(ndb, &account.pubkey, "roadmap");
        store::apply(
            ndb,
            "roadmap",
            &view,
            &account.pubkey,
            &store::Signer::plain(&secret),
            store::BoardAction::AddCard {
                col: 0,
                title: ROADMAP_CARD.to_string(),
                description: String::new(),
                labels: vec![],
                parent: None,
            },
            &mut store::NoPublish,
        );
    }

    // Switch to the roadmap board through the real switcher menu.
    harness.get_by_label(SWITCHER_LABEL).simulate_click();
    // The entry appears once the roadmap board folds into the switcher list.
    wait_for_label(&mut harness, "Roadmap");
    harness.get_by_label("Roadmap").simulate_click();

    // The switch persists a kind-30623 preference. Wait until both the roadmap-only
    // card renders (active board is now roadmap) and the preference has committed,
    // so the restart below can't race an un-saved preference.
    wait_for_label(&mut harness, ROADMAP_CARD);
    wait_for_saved_slug(&mut harness, &account.pubkey, "roadmap");

    // Simulate a restart: rebuild `Headway` against the SAME ndb + account. A cold
    // boot doesn't know the selection until the first `update` reloads the pref.
    {
        let state = harness.state_mut();
        state.headway = Headway::new();
    }

    // It reopens on the coordinate-keyed selection — the roadmap-only card is back
    // with no manual switch — and did NOT fall back to the default demo board.
    wait_for_label(&mut harness, ROADMAP_CARD);
    assert!(
        harness.query_by_label("7 cards · 5 columns").is_none(),
        "restart fell back to the default board instead of the saved coordinate"
    );
}

/// Deliverable 2 (pixel snapshot, needs lavapipe): the switcher keeps two joined
/// boards that share a slug but differ in owner as two distinct entries (breakage
/// #3), and lists a board you own AND shared only once (coordinate dedup). Run via
/// `scripts/snapshot-test`; `sharefile` the PNG for review.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_switcher_same_slug_boards() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));
    let account = test_keypair();

    // Two co-members whose boards collide on the slug `notes` but not on owner.
    let alice = fixed_keypair(0xa1);
    let bob = fixed_keypair(0xb0);

    {
        let egui_ctx = harness.ctx.clone();
        let state = harness.state_mut();
        let app_ctx = &mut state.notedeck.app_context(&egui_ctx);
        let ndb: &Ndb = app_ctx.ndb;
        let secret = account.secret_key.secret_bytes();

        // An own board `roadmap` the account ALSO shares (self-share): it must
        // appear once, not twice, despite being both an own and a joined board.
        store::seed_board(
            ndb,
            &account.pubkey,
            &secret,
            "roadmap",
            "Roadmap",
            &mut store::NoPublish,
        );
        ingest_keyshare(
            ndb,
            &account,
            &account.pubkey,
            0x30,
            &event::board_address(&account.pubkey, "roadmap"),
        );

        // Two joined boards, same slug `notes`, different owners → two entries.
        ingest_keyshare(
            ndb,
            &alice,
            &account.pubkey,
            0xa5,
            &event::board_address(&alice.pubkey, "notes"),
        );
        ingest_keyshare(
            ndb,
            &bob,
            &account.pubkey,
            0xb5,
            &event::board_address(&bob.pubkey, "notes"),
        );
    }

    // Open the switcher and wait until every joined board has been picked up and
    // listed: the own+shared `roadmap` once, and both same-slug `notes` boards.
    harness.get_by_label(SWITCHER_LABEL).simulate_click();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        // `query_all_*` (unlike `get_all_*`) yields an empty iterator instead of
        // panicking while the shares are still being picked up.
        let both_notes = harness.query_all_by_label("notes").count() >= 2;
        if both_notes && harness.query_by_label("Roadmap").is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "switcher never listed both joined same-slug boards"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    harness.run_steps(3);
    harness.snapshot("headway_switcher_same_slug");
}
