use notedeck::test_harness::PressKey;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::Node;
use egui_kittest::kittest::{NodeT, Queryable};
use nostrdb::{Filter, IngestMetadata, Ndb, NoteBuilder, Subscription, Transaction};
use nostrdb_net::{FullKeypair, Keypair, NoteId, Pubkey};
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
    /// When set, the harness plays the chrome's whole global-nav loop on this
    /// stack instead, slides included (the entry beneath the top draws in the
    /// same pass as the top while one runs), and Headway's nav requests land
    /// on it as the chrome's `apply_nav_requests` lands them. See
    /// [`chrome_nav_pass`] and [`slide_harness`].
    chrome_nav: Option<ChromeNav>,
}

fn render_headway(ui: &mut egui::Ui, state: &mut HeadwayTestState) {
    notedeck::test_harness::full_window(ui, |ui| {
        let ctx = &ui.ctx().clone();
        // Fonts/styles must be installed before the first real frame; do it once,
        // and take the same first frame to inject a signing account.
        if !state.fonts_installed {
            state.notedeck.setup(ctx);
            ctx.global_style_mut(|s| s.animation_time = 0.0);

            let secret = state.account.secret_key.clone();
            let pubkey = state.account.pubkey;
            let app_ctx = &mut state.notedeck.app_context();
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

        let mut app_ctx = state.notedeck.app_context();
        // Mirror production: chrome runs `update` (sync poll + fan-out + seed) for
        // every opened app each frame, then `render` for the foreground one.
        state.headway.update(&mut app_ctx);

        // The inline-reference snapshot views a kind-1 note through NoteView rather
        // than the Headway app; the board sync above keeps its cache live either way.
        if let Some(note_id) = state.ref_note {
            render_ref_note(ui, &mut app_ctx, note_id);
            return;
        }

        egui::CentralPanel::default().show(ui, |ui| {
            // Mirror the chrome, which zeroes the horizontal item gap for every app
            // (`notedeck_chrome/src/chrome/frame.rs`, `Chrome::show`): a Headway that
            // stops owning its spacing then shows up glued here, as it would live.
            ui.spacing_mut().item_spacing.x = 0.0;
            if let Some(nav) = &mut state.chrome_nav {
                chrome_nav_pass(ui, &mut app_ctx, &mut state.headway, nav);
                return;
            }
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
    });
}

/// How many passes a [`ChromeNav`] slide draws two entries for before it
/// lands. egui_nav's spring takes a few dozen; the bug a slide can hide shows
/// on its first.
const SLIDE_PASSES: u8 = 3;

/// The chrome's global stack and the slide running on it, for a harness that
/// plays the chrome's whole nav loop (see [`chrome_nav_pass`]).
struct ChromeNav {
    stack: notedeck::NavStack<notedeck::ChromeNavEntry>,
    /// Passes the running slide has drawn.
    slid: u8,
}

/// One pass of the chrome's global nav, as `Chrome::show` runs it for an app
/// (`notedeck_chrome/src/chrome/frame.rs`): every entry draws through
/// [`App::render_nav`] with its own token, and the nav requests the pass
/// raised land on the stack as `Chrome::apply_nav_requests` lands them — a
/// push or a back starts a slide, and a back's slide pops when it ends,
/// handing the popped entry to [`App::cleanup_nav`].
///
/// While a slide runs, the entry beneath the top draws first and the top
/// after it, in one pass, as egui_nav's `show_internal` draws them. This
/// doesn't call egui_nav itself: its `render_bg` and `render_fg` both build a
/// `Ui` with the nav's own id, on different layers, which egui
/// debug-asserts against, so a debug test can't run a real slide.
fn chrome_nav_pass(
    ui: &mut egui::Ui,
    app_ctx: &mut AppContext,
    headway: &mut Headway,
    nav: &mut ChromeNav,
) {
    use notedeck::NavRequest;

    let stack = &mut nav.stack;
    let area = ui.available_rect_before_wrap();
    let sliding = stack.navigating() || stack.returning();
    if sliding && let Some(under) = stack.prev() {
        let token = under.token.clone();
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(area).id_salt("slide-under"),
            |ui| headway.render_nav(app_ctx, ui, &token),
        );
    }
    let token = stack.top().token.clone();
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(area).id_salt("slide-top"),
        |ui| headway.render_nav(app_ctx, ui, &token),
    );

    if sliding {
        nav.slid += 1;
        ui.ctx().request_repaint();
        if nav.slid >= SLIDE_PASSES {
            nav.slid = 0;
            if stack.returning() {
                if let Some(popped) = stack.pop() {
                    headway.cleanup_nav(app_ctx, &popped.token);
                }
            } else {
                stack.navigating_mut(false);
            }
        }
    }

    let active = stack.top().app;
    for request in app_ctx.navigator.take() {
        match request {
            NavRequest::PushToActive(entry) => stack.route_to(entry.tag(active)),
            NavRequest::Back => {
                stack.go_back();
            }
            _ => panic!("unexpected nav request kind from Headway"),
        }
    }
}

/// Render `note_id` (a kind-1 note) through `NoteView`, the surface the
/// note-renderer inline-reference feature lights up: a `headway:board/word-word-word` in
/// the note's content resolves to the card's live status chip via the registered
/// reference parser + issue renderer, with no note→headway dependency.
fn render_ref_note(ui: &mut egui::Ui, app_ctx: &mut AppContext, note_id: NoteId) {
    egui::CentralPanel::default().show(ui, |ui| {
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
    let json = nostrdb_net::ClientMessage::event(&note)
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
    let secret = nostrdb_net::SecretKey::from_slice(&[fill; 32]).expect("valid test secret");
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
        chrome_nav: None,
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
        .build_ui_state(render_headway, headway_state());

    wait_for_board(&mut harness);
    harness
}

/// Like [`headway_harness`] but with **no** wgpu renderer, so it builds and drives
/// `update()`/`render()` under plain `cargo test` — no CPU Vulkan adapter
/// (lavapipe) needed. Use it for behavioural flows that only assert on the
/// accesskit tree (via [`wait_for_label`]); it cannot `snapshot()` (that rasterises
/// through the renderer, which is exactly what needs lavapipe).
fn behavioral_harness(size: egui::Vec2) -> Harness<'static, HeadwayTestState> {
    let mut harness = harness_builder(size).build_ui_state(render_headway, headway_state());

    wait_for_board(&mut harness);
    harness
}

/// The board is seeded by ingesting events into nostrdb, which lands on an async
/// writer thread, and each card folds in across several events. Wait for the
/// header's full-count summary rather than just the first column, so every test
/// starts from a fully-materialised board instead of a half-ingested one.
fn wait_for_board(harness: &mut Harness<'static, HeadwayTestState>) {
    const SUMMARY: &str = "7 cards · 5 columns";
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        harness.run_ok();
        if harness.query_by_label(SUMMARY).is_some() {
            return;
        }
        if Instant::now() >= deadline {
            // Report the summary the header actually rendered: that one string
            // separates the two candidate failures — no summary at all means the
            // board never materialised, while "0 cards · 5 columns" means it
            // folded and the seed went missing (which is what
            // headway:notedeck/awesome-purpose-fossil turned out to be).
            //
            // Probed one exact label at a time rather than by substring, because
            // `query_all_by_label_contains` does not see these nodes: on a run
            // where `query_by_label(SUMMARY)` succeeds, a sibling
            // `query_all_by_label_contains(" columns")` still comes back empty,
            // and `query_all_by_role(Role::Label)` returns nothing even though
            // the summary node's own role *is* `Label`. So the substring probe
            // this replaces reported "no board summary rendered at all"
            // unconditionally — including when the header was plainly there —
            // and cost two sessions chasing a board that had in fact folded.
            let seen = (0..=DEMO_CARDS)
                .map(|n| {
                    format!(
                        "{n} card{} · {DEMO_COLUMNS} columns",
                        if n == 1 { "" } else { "s" }
                    )
                })
                .find(|label| harness.query_by_label(label).is_some());
            let seen = match seen {
                Some(label) => format!("header showed {label:?}"),
                None => format!(
                    "no board summary rendered at all (board switcher {}present)",
                    if harness.query_by_label(SWITCHER_LABEL).is_some() {
                        ""
                    } else {
                        "not "
                    }
                ),
            };
            panic!("timed out waiting for {SUMMARY:?}: {seen}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Seed the populated demo board for the snapshot/flow tests to render against,
/// and **block until it is visible to the app**.
///
/// The production seed is card-less; the fixture lives in
/// [`store::seed_demo_board`]. Writing it only queues the events on nostrdb's
/// ingest thread, so without this barrier the app's very next `update` can run
/// its `has_board` check against a snapshot that predates the commit. That check
/// is the only thing stopping the app from auto-seeding a default board of its
/// own — and the one it creates is a *sealed, self-shared team-of-one* at the
/// same coordinate, which puts the coordinate in the roster. From then on the
/// board folds the shared way, and the shared fold takes only team-sealed rumors
/// (`event::fold_shared_board`), so these plaintext demo cards are invisible to
/// it for the life of the process: the board renders its five default columns
/// with nothing in them, and `wait_for_board` burns its whole timeout
/// (headway:notedeck/awesome-purpose-fossil).
///
/// Losing that race needs the ingest to lag a frame, which is why it only ever
/// showed up on loaded CI runners.
fn seed_demo(ndb: &Ndb, pubkey: &Pubkey, secret: &[u8; 32]) {
    // Subscribed before the first write, so the drain below cannot miss an
    // event that commits while the seed is still running.
    let sub = ndb
        .subscribe(&[Filter::new().authors([pubkey.bytes()]).build()])
        .expect("subscribe");

    let expected = store::seed_demo_board(
        ndb,
        pubkey,
        secret,
        store::BOARD_ID,
        SEED_AT,
        &mut store::NoPublish,
    );

    // Wait for *every* event the seed wrote, which is what `seed_demo_board`
    // returns a count of — not merely for enough of them to make the board look
    // right. The two differ: a card's issue event carries the title it was
    // created with, and the seed then amends some of them (the event-model card
    // is born "Nostr event model" and renamed to "Define nostr event model for
    // boards"), so a barrier that stops at "seven cards have folded" can hand
    // back a board whose cards still answer to their pre-amendment titles — and
    // the tests address cards by their final title (`demo_card_id`,
    // `get_by_label`). Counting the seed's own events covers the amendments,
    // the placements and the relations without this barrier having to know what
    // any of them are.
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    let mut seen = 0usize;
    while seen < expected {
        seen += ndb.poll_for_notes(sub, 256).len();
        assert!(
            Instant::now() < deadline,
            "demo seed never committed: {seen} of {expected} events ingested"
        );
        if seen < expected {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    // (the subscription is left to the harness's temporary ndb, which is
    // dropped with the test — `unsubscribe` needs `&mut Ndb` and the fixture
    // only ever holds `&Ndb`.)

    // Every seeded event is committed, so one fold now sees the finished board.
    let txn = Transaction::new(ndb).expect("txn");
    let view = event::load_board(ndb, &txn, pubkey, store::BOARD_ID).expect("demo board folds");
    let cards: usize = view.columns.iter().map(|c| c.cards.len()).sum();
    assert_eq!(
        cards, DEMO_CARDS,
        "demo board folded {cards} cards from a fully-ingested seed"
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

/// Ceiling for the frame-pumping barriers below.
///
/// These wait on asynchronous nostrdb ingest, so the bound has to cover a loaded
/// CI runner rather than a quiet laptop: four of these tests timed out together
/// on one Linux run at the old five seconds, all on the same seed barrier, while
/// passing everywhere else. Matches `common::CONVERGE_TIMEOUT`, whose comment
/// already settles the tradeoff for this crate — long enough for a slow runner,
/// short enough that a genuinely stuck fold still fails rather than hangs.
///
/// Raising it costs nothing when things are healthy: every loop returns as soon
/// as its condition holds, so only a run that was going to fail waits longer.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How many cards [`store::seed_demo_board`] puts on the demo board. The seed
/// barrier in [`seed_demo`] waits for exactly this many, and it's the count
/// `wait_for_board`'s summary asserts.
const DEMO_CARDS: usize = 7;

/// How many columns the demo board has — the other half of the summary
/// [`wait_for_board`] waits for, and the fixed column count its timeout probes
/// card counts against.
const DEMO_COLUMNS: usize = 5;

/// Pump frames (with small sleeps, since ndb ingest is async) until a widget
/// with `label` appears, or panic after a deadline.
fn wait_for_label(harness: &mut Harness<'static, HeadwayTestState>, label: &str) {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
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
    let deadline = Instant::now() + SETTLE_TIMEOUT;
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

/// The header's narrowed state: hiding sub-issues via the "View" menu drops the
/// two sub-issue cards from the grid, tints the "View" trigger, and swaps the
/// muted size summary for the prominent "Filtered" pill.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_filtered() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));

    harness.get_by_label("☰ View").click();
    harness.run_ok();
    harness.get_by_label("Hide sub-issues").click();
    harness.run_steps(3);
    harness.snapshot("headway_filtered");
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
        .click();
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

    harness.get_by_label("Sync cards across relays").click();
    harness.run_steps(3);
    harness.snapshot("headway_subissue_detail");
}

/// The dependency-graph view with a node hovered: the hovered card's incident
/// edges brighten to the accent colour while the rest recede, and the node itself
/// takes its hover border. Doubles as a rasterising regression guard for the
/// graph's edge rendering (an open bezier fill panics the tessellator — see the
/// shared `draw_edge` fix in `notedeck_ui`), which the non-rendering behavioural
/// tests can't catch.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_graph_hover() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));

    // Open the epic's detail, then its dependency graph from the detail action.
    harness
        .get_by_label("Define nostr event model for boards")
        .click();
    harness.run_steps(3);
    harness.get_by_label("☍ View dependency graph").click();
    harness.run_steps(3);

    // Hover the sync card's node so its incident edges highlight. The node lives
    // inside an `egui::Scene`, so accesskit reports its box in scene-local space;
    // map the box centre to global through the scene layer's `to_global` transform
    // before moving the pointer there (the edge highlight is a geometric hit-test in
    // scene space, so this positions it deterministically).
    let bb = harness
        .get_by_label("Sync cards across relays")
        .accesskit_node()
        .bounding_box()
        .expect("the graph node has an on-screen box");
    let local = egui::pos2((bb.x0 + bb.x1) as f32 / 2.0, (bb.y0 + bb.y1) as f32 / 2.0);
    let to_global = harness
        .ctx
        .memory(|m| {
            m.to_global
                .values()
                .find(|t| **t != egui::emath::TSTransform::IDENTITY)
                .copied()
        })
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(to_global * local));
    harness.run_steps(3);
    harness.snapshot("headway_graph_hover");
}

/// The dependency-graph view in its resting state: the demo epic's laid-out
/// nodes and every blocking arrow between them, with nothing hovered. Locks in
/// the layered auto-layout ([`notedeck_ui::graph::layout::layered_layout`]) and
/// the node/edge drawing — the epic ("Define nostr event model for boards") owns
/// the sync and scaffold sub-issues, and the seed's dependency chain pulls the
/// epic itself in as an upstream *ghost* blocker of the sync card plus the
/// card-detail card as a downstream ghost, so the frame exercises primary and
/// ghost nodes, cleared (done) and open edges, and multiple ranks at once. The
/// companion [`snapshot_headway_graph_hover`] captures the hover state (incident-
/// edge highlight + connection handles) on top of this baseline.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_graph() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));

    // Open the epic's detail, then its dependency graph from the detail action —
    // the same entry point the hover snapshot uses, minus the pointer move.
    harness
        .get_by_label("Define nostr event model for boards")
        .click();
    harness.run_steps(3);
    harness.get_by_label("☍ View dependency graph").click();
    harness.run_steps(3);
    harness.snapshot("headway_graph");
}

/// The dependency-graph node variants in one column: a plain in-progress node, a
/// blocked node (leading ⊘), a done node, an off-board ghost, a *collapsed* node
/// that stands in for a whole sub-tree — showing the right-aligned done/total
/// progress pill that marks it expandable (a click drills the graph into it) —
/// and a collapsed node whose sub-tree is *fully* done. Locks the node chrome
/// the collapse feature added without needing a nested-epic board fixture.
///
/// It also pins the finished-work fade, which is the whole reason three of these
/// six are here: a done node and a cleared branch sink their box (fill, border
/// and content) toward the pane, a ghost keeps a card's weight on the recessed
/// secondary surface, and the two unfinished nodes are the only bright boxes
/// left. The cleared branch fades on its progress alone — its own column is the
/// middle one, same as the expandable node above it.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_graph_nodes() {
    use notedeck_headway::{GRAPH_NODE_SIZE, GraphNodeView, graph_node_ui};

    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let three = |idx: usize| {
        Some(headway::event::ColumnPos {
            index: idx,
            count: 3,
        })
    };
    let nodes = [
        GraphNodeView {
            title: "Plain in-progress node",
            column: three(1),
            blocked: false,
            ghost: false,
            progress: None,
        },
        GraphNodeView {
            title: "Blocked node",
            column: three(0),
            blocked: true,
            ghost: false,
            progress: None,
        },
        GraphNodeView {
            title: "Done node recedes",
            column: three(2),
            blocked: false,
            ghost: false,
            progress: None,
        },
        GraphNodeView {
            title: "Off-board ghost node",
            column: None,
            blocked: false,
            ghost: true,
            progress: None,
        },
        GraphNodeView {
            title: "Collapsed branch (expandable)",
            column: three(1),
            blocked: false,
            ghost: false,
            progress: Some(headway::graph::SubtreeProgress { done: 2, total: 5 }),
        },
        GraphNodeView {
            title: "Cleared branch recedes",
            column: three(1),
            blocked: false,
            ghost: false,
            progress: Some(headway::graph::SubtreeProgress { done: 4, total: 4 }),
        },
    ];

    let pad = 12.0;
    let width = GRAPH_NODE_SIZE.x + pad * 2.0;
    let height = pad + (GRAPH_NODE_SIZE.y + pad) * nodes.len() as f32;

    let mut installed = false;
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(width, height))
        .renderer(notedeck::software_renderer())
        .build_ui(move |ui| {
            if !installed {
                notedeck.setup(ui.ctx());
                ui.ctx().global_style_mut(|s| s.animation_time = 0.0);
                installed = true;
            }
            let theme = notedeck::ColorTheme::current(ui.ctx());
            let origin = ui.min_rect().min;
            for (i, node) in nodes.iter().enumerate() {
                let min = egui::pos2(
                    origin.x + pad,
                    origin.y + pad + (GRAPH_NODE_SIZE.y + pad) * i as f32,
                );
                graph_node_ui(
                    ui,
                    &theme,
                    egui::Rect::from_min_size(min, GRAPH_NODE_SIZE),
                    node,
                );
            }
        });

    harness.run_ok();
    harness.snapshot("headway_graph_nodes");
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

    harness.get_by_label("Sync cards across relays").click();
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
    // again.
    let description = {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let secret = state.account.secret_key.secret_bytes();
        let app_ctx = &mut state.notedeck.app_context();

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
        .click();
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
        .click();
    harness.run_ok();

    // Now edit the open card the way a `headway` CLI run or a relay peer would —
    // straight into the store, not through the pane's own editor. Scoped so the
    // `AppContext` (and its harness borrow) drops before we pump frames again.
    let new_title = "Define nostr event model for boards (edited live)";
    let new_desc = "Edited while the detail pane was open.";
    {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let secret = state.account.secret_key.secret_bytes();
        let app_ctx = &mut state.notedeck.app_context();
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
    // pump frames.
    let card_title = "Sync cards across relays";
    let (ndb, sub) = {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let secret = state.account.secret_key.secret_bytes();
        let app_ctx = &mut state.notedeck.app_context();

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
        .click();
    harness.run_ok();

    // The fixture rollup: one of the two children sits in Done.
    harness.get_by_label("1/2");

    // The composer is collapsed behind "+ Add sub-issue" (Linear-style);
    // opening it focuses the field, so typing can start immediately.
    harness.get_by_label("+ Add sub-issue").click();
    harness.run_ok();
    focused_text_input(&harness).focus();
    focused_text_input(&harness).type_text("Write a relay conformance suite");
    harness.run_ok();
    focused_text_input(&harness).focus();
    harness.key_press(egui::Key::Enter);

    // The new child lands in the checklist (in Backlog, so not done).
    wait_for_label(&mut harness, "1/3");
    wait_for_label(&mut harness, "Write a relay conformance suite");
}

/// The header's "View" menu hides sub-issue cards from the grid, and the
/// "Filtered" pill then reports the narrowed count — so a board narrowed by a
/// view option (or a search) never passes for the whole board, the affordance
/// gap the card `injury-enlist-swarm` flagged. Behavioural (accesskit only), so
/// it runs without lavapipe.
#[test]
fn hide_subissues_view_option() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));

    // Precondition: the demo board's two sub-issue cards sit on the grid and the
    // header states the full, unnarrowed size.
    harness.get_by_label("Sync cards across relays");
    harness.get_by_label("Scaffold the Headway app crate");
    harness.get_by_label("7 cards · 5 columns");

    // Open the View menu and toggle "Hide sub-issues".
    harness.get_by_label("☰ View").click();
    harness.run_ok();
    harness.get_by_label("Hide sub-issues").click();
    harness.run_ok();

    // Both sub-issue cards leave the grid, and the muted size summary gives way
    // to the prominent "Filtered" pill reporting 5 of the 7 cards showing.
    wait_for_absent(&mut harness, "Sync cards across relays");
    wait_for_absent(&mut harness, "Scaffold the Headway app crate");
    harness.get_by_label("Filtered · 5 of 7 shown");
}

/// The inline card widget must render its content left-aligned even though the
/// notebook lays node content out centered (egui's `Ui::put` →
/// `centered_and_justified`). Reproduce that centered context and snapshot the
/// card, guarding against a regression to centered content.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_inline_card() {
    inline_card_snapshot(
        "inline_card",
        "Update headway-cli to use negentropy for sync",
        "headway",
    );
}

/// A card whose title and label carry colour emoji, drawn from notedeck's
/// bundled Noto COLRv1 font rather than as monochrome outlines.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_inline_card_colour_emoji() {
    inline_card_snapshot(
        "inline_card_colour_emoji",
        "🚀 Ship colour emoji 🎉 in notes",
        "🐛 bug",
    );
}

/// Render one inline card with `title` and a single `label`, centered the way
/// the notebook lays out node content, and snapshot it as `name`.
fn inline_card_snapshot(name: &str, title: &str, label: &str) {
    use notedeck_headway::{card_inline_ui, event::CardView};

    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let card = CardView {
        id: nostrdb_net::NoteId::new([1u8; 32]),
        author: [0u8; 32],
        title: title.to_string(),
        description: String::new(),
        labels: vec![label.to_string()],
        priority: headway::event::Priority::None,
        rank: String::new(),
        placed_at: 0,
        created_at: 0,
        updated_at: 0,
        comments: vec![],
        reviews: vec![],
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
                ui.ctx().global_style_mut(|s| s.animation_time = 0.0);
                installed = true;
            }
            let theme = notedeck::ColorTheme::current(ui.ctx());
            // Mimic the notebook's centered node layout.
            let rect = ui.available_rect_before_wrap();
            ui.put(rect, |ui: &mut egui::Ui| card_inline_ui(ui, &theme, &card));
        });

    harness.run_ok();
    harness.snapshot(name);
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
                ui.ctx().global_style_mut(|s| s.animation_time = 0.0);
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
                ui.ctx().global_style_mut(|s| s.animation_time = 0.0);
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
        .click();
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
    harness.get_by_label("+ Add column").click();
    harness.run_ok();

    // Type into the (auto-focused) composer field, then commit via "Add". The
    // field has no label, so target it by focus.
    focused_text_input(&harness).focus();
    focused_text_input(&harness).type_text("Ideas");
    harness.run_ok();
    harness.get_by_label("Add").click();

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
    harness.get_by_label("Rename").click();
    harness.run_ok();

    // The header is now an inline field seeded with "Backlog". Select all
    // (Command+A maps to egui's select-all), replace it, and commit with Enter.
    focused_text_input(&harness).focus();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run_ok();
    focused_text_input(&harness).focus();
    focused_text_input(&harness).type_text("Inbox");
    harness.run_ok();
    focused_text_input(&harness).focus();
    harness.key_press(egui::Key::Enter);

    wait_for_label(&mut harness, "Inbox");
    wait_for_absent(&mut harness, "Backlog");
}

/// Reorder a column via its "⋯" menu: Move right shifts Backlog past Todo.
/// Asserted by comparing the columns' on-screen x positions.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn reorder_column_flow() {
    let mut harness = headway_harness(egui::Vec2::new(1600.0, 800.0));

    let backlog_x = harness
        .get_by_label("Backlog")
        .accesskit_node()
        .bounding_box()
        .unwrap()
        .x0;
    let todo_x = harness
        .get_by_label("Todo")
        .accesskit_node()
        .bounding_box()
        .unwrap()
        .x0;
    assert!(backlog_x < todo_x, "precondition: Backlog is left of Todo");

    open_first_column_menu(&mut harness);
    harness.get_by_label("Move right").click();

    // Wait for the reordered board to materialise (Backlog moves right of Todo).
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        harness.run_ok();
        let backlog_x = harness
            .get_by_label("Backlog")
            .accesskit_node()
            .bounding_box()
            .unwrap()
            .x0;
        let todo_x = harness
            .get_by_label("Todo")
            .accesskit_node()
            .bounding_box()
            .unwrap()
            .x0;
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
    harness.get_by_label("Delete column").click();

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
        .click();
    harness.run_ok();

    // Enter (not the "Add" button) must commit the card — the composer is a
    // multiline field, which swallows Enter into a newline, so this is the path
    // that used to silently do nothing.
    harness
        // The card composer is multiline, so it has the MultilineTextInput role.
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .focus();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("Write integration tests");
    harness.run_ok();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .focus();
    harness.key_press(egui::Key::Enter);

    wait_for_label(&mut harness, "Write integration tests");
    harness.get_by_label("8 cards · 5 columns");

    // Rapid entry: the composer is still open and focused, so a second title can
    // go straight in without clicking "+ Add card" again.
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .focus();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("Ship the feature");
    harness.run_ok();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .focus();
    harness.key_press(egui::Key::Enter);

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
    let col = harness
        .get_by_label("Backlog")
        .accesskit_node()
        .bounding_box()
        .unwrap();
    let pos = egui::pos2(col.x0 as f32 + 20.0, col.y0 as f32 + 90.0);
    for _ in 0..40 {
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(pos));
        harness.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            // A discrete wheel notch, as egui-winit reports a mouse wheel.
            phase: egui::TouchPhase::Move,
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
    let limit = harness.ctx.content_rect().max.y - 16.0 - 8.0;
    let btn = harness
        .get_all_by_label("+ Add card")
        .next()
        .expect("an add-card affordance")
        .accesskit_node()
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
/// exactly as `board_switcher` composes it (`"{title}  ▾"`, two spaces). The demo
/// board is titled "Headway".
const SWITCHER_LABEL: &str = "Headway  ▾";

/// Poll the shared board at `board_addr` (async ingest) until its sealed
/// definition has folded in, returning the folded view. Sleeps between reads
/// rather than pumping frames — the caller holds an `AppContext` borrow — so it
/// waits on the ndb writer thread, not the render loop. Panics past a deadline.
fn wait_shared_board(ndb: &Ndb, board_addr: &str, team_pubkey: &Pubkey) -> event::BoardView {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
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
    let deadline = Instant::now() + SETTLE_TIMEOUT;
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
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        harness.run_ok();
        let saved = {
            let state = harness.state_mut();
            let app_ctx = &mut state.notedeck.app_context();
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
    // TEMPORARY, with the phase markers in `snapshot_switcher_same_slug_boards`
    // (headway:notedeck/man-eight-damp). Those narrowed the SIGILL to this
    // function; these two split it again, because the crypto here runs in two
    // different places. `gift_wrapped_keyshare` seals in Rust on this thread;
    // `process_event` hands the wrap to nostrdb's ingester thread, which
    // unwraps it in C. Which marker is last says which. Remove with them.
    eprintln!("PHASE keyshare {root_fill:#x}: sealing");
    let giftwrap = common::gift_wrapped_keyshare(sender, recipient, &root, Some(board_addr), None);
    eprintln!("PHASE keyshare {root_fill:#x}: sealed, submitting");
    ndb.process_event(&format!("[\"EVENT\",\"kg\",{giftwrap}]"))
        .expect("ingest keyshare giftwrap");
    eprintln!("PHASE keyshare {root_fill:#x}: submitted");
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
        keys: nostrdb_net::sns::derive_sns_keys(&root).expect("derive sns keys"),
    };
    let team_pubkey = channel.keys.team_keypair.pubkey;

    // The active board defaults to the account's own `headway` coordinate; the
    // self-share names exactly that coordinate, so once the roster reconstructs it
    // the active board routes through the shared fold with no explicit switch.
    let board_addr = event::board_address(&account.pubkey, store::BOARD_ID);

    {
        let state = harness.state_mut();
        let app_ctx = &mut state.notedeck.app_context();
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
    harness.get_by_label(CARD).click();
    wait_for_label(&mut harness, "← Back");

    let state = harness.state_mut();
    let author = state.account.pubkey;
    let app_ctx = state.notedeck.app_context();
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
        pushed[0].selected_card(),
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
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context();
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

/// Run `git -C <dir> <args>` for the review fixture repo, panicking with git's
/// stderr on failure; returns stdout, trimmed.
fn fixture_git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Pump frames until at least one widget carries `label`. Unlike
/// [`wait_for_label`] this tolerates several (a diff's file path is both a
/// summary row and a file header).
fn wait_for_any_label(harness: &mut Harness<'static, HeadwayTestState>, label: &str) {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        harness.run_ok();
        if harness.query_all_by_label(label).next().is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Behavioural (no lavapipe): a card carrying a review record shows it in the
/// detail's Review section, and "Review diff" opens the review pane, which
/// resolves the commit off the UI thread and draws its diff. The record points
/// at a fixture repo on this host, so resolution is local — no fetch — and the
/// test waits on the diff's file path, the pane's terminal state. Opening the
/// pane also pushes exactly one `Review` global-nav entry for the card.
#[test]
fn review_diff_opens_the_review_pane_with_the_commit() {
    use notedeck::NavRequest;
    use notedeck_headway::HeadwayRoute;

    const CARD: &str = "Define nostr event model for boards";
    const FILE: &str = "src/reviewed.rs";

    // A one-commit repo touching FILE, on a named branch.
    let repo = tempfile::tempdir().expect("repo dir");
    let dir = repo.path();
    fixture_git(dir, &["init", "-q", "-b", "review-branch"]);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join(FILE), "fn reviewed() {}\n").unwrap();
    fixture_git(dir, &["add", "."]);
    fixture_git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "reviewed: add the reviewed fn",
        ],
    );
    let sha = fixture_git(dir, &["rev-parse", "HEAD"]);

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let card = {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let secret = state.account.secret_key.secret_bytes();
        let app_ctx = &mut state.notedeck.app_context();
        let card = demo_card_id(app_ctx.ndb, &author, CARD);

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
            store::BoardAction::AddReview {
                card,
                review: event::ReviewFields {
                    commit: Some(sha.clone()),
                    title: Some("reviewed: add the reviewed fn".to_string()),
                    branch: Some("review-branch".to_string()),
                    // Recorded on this host, so the resolver uses `path` itself.
                    host: headway::git::host_name(),
                    path: Some(dir.to_string_lossy().into_owned()),
                    ..Default::default()
                },
            },
            &mut store::NoPublish,
        );
        card
    };

    harness.get_by_label(CARD).click();
    // The record's short sha is the detail row's own button, once it folds in
    // (and the sidebar's too, as the newest record).
    wait_for_any_label(&mut harness, &sha[..12]);
    harness.get_by_label("± Review diff").click_accesskit();
    wait_for_any_label(&mut harness, FILE);

    let requests = harness.state_mut().notedeck.app_context().navigator.take();
    let reviews: Vec<&HeadwayRoute> = requests
        .iter()
        .filter_map(|req| match req {
            NavRequest::PushToActive(entry) => entry.token.downcast_ref::<HeadwayRoute>(),
            _ => None,
        })
        .filter(|route| route.review_card().is_some())
        .collect();
    assert_eq!(reviews.len(), 1, "opening the review pushes one entry");
    assert_eq!(reviews[0].review_card(), Some(card));
    assert_eq!(reviews[0].selected_card(), Some(card));
    // "Review diff" opens on the newest record, which the route names as `None`.
    assert_eq!(reviews[0].review_target().map(|t| t.record), Some(None));

    // Escape closes the pane back to the card's detail.
    harness.press_key(egui::Key::Escape);
    wait_for_label(&mut harness, "± Review diff");
}

/// Commit `file` (containing `body`) in the fixture repo `dir` with subject
/// `subject`, returning the new commit's sha.
fn fixture_commit(dir: &std::path::Path, file: &str, body: &str, subject: &str) -> String {
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().expect("file has a parent")).unwrap();
    std::fs::write(path, body).unwrap();
    fixture_git(dir, &["add", "."]);
    fixture_git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            subject,
        ],
    );
    fixture_git(dir, &["rev-parse", "HEAD"])
}

/// Behavioural (no lavapipe): a `Review` route carries which record the pane
/// shows. With two records on a card, an entry naming the one that isn't the
/// head opens the pane on it (its subject in the record summary, its commit's
/// file in the diff), and an entry naming no record opens the head — so
/// back/forward onto a review reproduces what it showed rather than reusing the
/// pane's last pick.
///
/// Both records land in the same second, so which is the head (newest first,
/// the id breaking the tie) is read back off the fold rather than assumed.
#[test]
fn review_route_opens_the_record_it_names() {
    use notedeck_headway::HeadwayRoute;

    const CARD: &str = "Define nostr event model for boards";
    // (file, subject) per fixture commit.
    const COMMITS: [(&str, &str); 2] = [
        ("src/first.rs", "first: one reviewed commit"),
        ("src/second.rs", "second: another reviewed commit"),
    ];

    let repo = tempfile::tempdir().expect("repo dir");
    let dir = repo.path();
    fixture_git(dir, &["init", "-q", "-b", "review-branch"]);
    let shas: Vec<String> = COMMITS
        .iter()
        .map(|(file, subject)| fixture_commit(dir, file, "fn reviewed() {}\n", subject))
        .collect();

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let (card, author) = {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let secret = state.account.secret_key.secret_bytes();
        let app_ctx = &mut state.notedeck.app_context();
        let card = demo_card_id(app_ctx.ndb, &author, CARD);

        for (sha, (_, subject)) in shas.iter().zip(COMMITS) {
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
                store::BoardAction::AddReview {
                    card,
                    review: event::ReviewFields {
                        commit: Some(sha.clone()),
                        title: Some(subject.to_string()),
                        branch: Some("review-branch".to_string()),
                        host: headway::git::host_name(),
                        path: Some(dir.to_string_lossy().into_owned()),
                        ..Default::default()
                    },
                },
                &mut store::NoPublish,
            );
        }
        (card, author)
    };

    // Both records fold in: each is a row in the card detail's Review section.
    harness.get_by_label(CARD).click();
    // (The newest is in the sidebar's Review block too.)
    for sha in &shas {
        wait_for_any_label(&mut harness, &sha[..12]);
    }
    // The head's commit, and the other record's id and commit.
    let (head, (other, other_commit)) = {
        let state = harness.state_mut();
        let app_ctx = state.notedeck.app_context();
        let txn = Transaction::new(app_ctx.ndb).expect("txn");
        let boards = headway::event::fold_board(app_ctx.ndb, &txn, &author)
            .expect("folded")
            .finalize();
        let view =
            headway::event::find_board(&boards, &author, store::BOARD_ID).expect("demo board");
        let reviews = &view.card(card).expect("card").reviews;
        assert_eq!(reviews.len(), 2, "both records folded");
        let commit_of = |i: usize| {
            let sha = reviews[i].fields.commit.as_deref();
            shas.iter()
                .position(|s| Some(s.as_str()) == sha)
                .expect("a fixture commit")
        };
        (commit_of(0), (reviews[1].id, commit_of(1)))
    };
    let (head_file, head_subject) = COMMITS[head];
    let (other_file, other_subject) = COMMITS[other_commit];

    // An entry naming the non-head record opens the pane on it.
    harness.state_mut().nav_token = Some(std::rc::Rc::new(HeadwayRoute::review(
        card,
        Some(other),
        Some(CARD.to_string()),
    )));
    wait_for_any_label(&mut harness, other_file);
    wait_for_label(&mut harness, other_subject);
    assert!(
        harness.query_by_label(head_subject).is_none(),
        "only the named record is summarised"
    );

    // Landing on the card's detail and then an entry naming no record opens
    // the head, not the record the pane last showed.
    harness.state_mut().nav_token = Some(std::rc::Rc::new(HeadwayRoute::card(card, None)));
    wait_for_label(&mut harness, "± Review diff");
    harness.state_mut().nav_token = Some(std::rc::Rc::new(HeadwayRoute::review(
        card,
        None,
        Some(CARD.to_string()),
    )));
    wait_for_any_label(&mut harness, head_file);
    wait_for_label(&mut harness, head_subject);
    assert!(harness.query_by_label(other_subject).is_none());
}

/// Fold the demo board fresh off the harness's db and apply `action` to it,
/// signed by the harness account.
fn apply_demo_action(harness: &mut Harness<'static, HeadwayTestState>, action: store::BoardAction) {
    let state = harness.state_mut();
    let author = state.account.pubkey;
    let secret = state.account.secret_key.secret_bytes();
    let app_ctx = state.notedeck.app_context();
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    let boards = headway::event::fold_board(app_ctx.ndb, &txn, &author)
        .expect("folded")
        .finalize();
    let view = headway::event::find_board(&boards, &author, store::BOARD_ID).expect("demo board");
    store::apply(
        app_ctx.ndb,
        store::BOARD_ID,
        view,
        &author,
        &store::Signer::new(&secret, None),
        action,
        &mut store::NoPublish,
    );
}

/// Move the demo card `card` to the end of the In Review column, and wait for
/// the move to fold in so the next one ranks after it.
fn move_to_in_review(harness: &mut Harness<'static, HeadwayTestState>, card: NoteId) {
    let to_row = {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context();
        let txn = Transaction::new(app_ctx.ndb).expect("txn");
        let boards = headway::event::fold_board(app_ctx.ndb, &txn, &author)
            .expect("folded")
            .finalize();
        let view =
            headway::event::find_board(&boards, &author, store::BOARD_ID).expect("demo board");
        view.columns[IN_REVIEW_COL].cards.len()
    };
    apply_demo_action(
        harness,
        store::BoardAction::MoveCard {
            card,
            to_col: IN_REVIEW_COL,
            to_row,
        },
    );
    wait_for_card_column(harness, card, "In Review");
}

/// The demo board's In Review column, by index.
const IN_REVIEW_COL: usize = 3;

/// Every [`HeadwayRoute::ReviewQueue`] push the app has enqueued since boot
/// (nothing drains the Navigator in these harnesses), and whether the last
/// request of all was a back.
fn queue_pushes_and_last_back(harness: &mut Harness<'static, HeadwayTestState>) -> (usize, bool) {
    use notedeck::NavRequest;
    use notedeck_headway::HeadwayRoute;

    let requests = harness.state_mut().notedeck.app_context().navigator.take();
    let pushes = requests
        .iter()
        .filter_map(|req| match req {
            NavRequest::PushToActive(entry) => entry.token.downcast_ref::<HeadwayRoute>(),
            _ => None,
        })
        .filter(|route| route.is_review_queue())
        .count();
    let last_back = matches!(requests.last(), Some(NavRequest::Back));
    (pushes, last_back)
}

/// Move the demo cards titled `titles` into In Review, in order, each with a
/// review record naming a fresh commit touching the matching `files` entry in
/// a fixture repo at `dir` (on this host, so the pane resolves it with no
/// fetch). Returns the cards' ids.
fn seed_in_review(
    harness: &mut Harness<'static, HeadwayTestState>,
    dir: &std::path::Path,
    titles: &[&str],
    files: &[&str],
) -> Vec<NoteId> {
    seed_in_review_with(harness, dir, titles, files, |_| {
        "fn queued() {}\n".to_string()
    })
}

/// [`seed_in_review`], with card `n`'s commit writing `body(n)` to its file.
fn seed_in_review_with(
    harness: &mut Harness<'static, HeadwayTestState>,
    dir: &std::path::Path,
    titles: &[&str],
    files: &[&str],
    body: impl Fn(usize) -> String,
) -> Vec<NoteId> {
    fixture_git(dir, &["init", "-q", "-b", "review-branch"]);
    let ids: Vec<NoteId> = titles
        .iter()
        .map(|title| harness_card_id(harness, title))
        .collect();
    for (n, ((&card, title), file)) in ids.iter().zip(titles).zip(files).enumerate() {
        move_to_in_review(harness, card);
        let subject = format!("queue: {title}");
        let sha = fixture_commit(dir, file, &body(n), &subject);
        apply_demo_action(
            harness,
            store::BoardAction::AddReview {
                card,
                review: event::ReviewFields {
                    commit: Some(sha),
                    title: Some(subject),
                    branch: Some("review-branch".to_string()),
                    host: headway::git::host_name(),
                    path: Some(dir.to_string_lossy().into_owned()),
                    ..Default::default()
                },
            },
        );
    }
    ids
}

/// Behavioural (no lavapipe): `R` walks the board's In Review cards through the
/// review pane in column order. `n` steps forward (the position and the card's
/// title follow, and its record's commit diff loads), `p` steps back, and `q`
/// leaves for the grid with the cursor on the card last shown. The whole walk
/// is one global-nav entry, left with one back.
#[test]
fn review_queue_walks_the_in_review_column() {
    const CARDS: [&str; 3] = [
        "Inline card creation",
        "Column reordering",
        "Drag-and-drop between columns",
    ];
    const FILES: [&str; 3] = ["src/queue_one.rs", "src/queue_two.rs", "src/queue_three.rs"];

    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let ids = seed_in_review(&mut harness, repo.path(), &CARDS, &FILES);

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    wait_for_label(&mut harness, "1 / 3");
    wait_for_label(&mut harness, CARDS[0]);
    wait_for_any_label(&mut harness, FILES[0]);
    assert!(
        harness.query_by_label(CARDS[1]).is_none(),
        "the next card is named on the ↓'s hover, not in the header"
    );

    press_board_keys(&mut harness, &[egui::Key::N, egui::Key::N]);
    wait_for_label(&mut harness, "3 / 3");
    wait_for_label(&mut harness, CARDS[2]);
    wait_for_any_label(&mut harness, FILES[2]);
    assert!(
        harness.query_by_label(CARDS[0]).is_none(),
        "the first card is behind us, not peeked"
    );

    press_board_keys(&mut harness, &[egui::Key::P]);
    wait_for_label(&mut harness, "2 / 3");
    wait_for_label(&mut harness, CARDS[1]);

    press_board_keys(&mut harness, &[egui::Key::Q]);
    wait_for_label(&mut harness, "7 cards · 5 columns");
    assert!(harness.query_by_label("← Back").is_none());
    assert_eq!(harness.state().headway.cursor(), Some(ids[1]));

    let (pushes, last_back) = queue_pushes_and_last_back(&mut harness);
    assert_eq!(pushes, 1, "stepping the queue pushes nothing more");
    assert!(last_back, "leaving the queue is one back");
}

/// Behavioural (no lavapipe): each card's diff in the queue scrolls on its own.
/// Page down the first card's long diff and `D` it: the next card's diff opens
/// at its top, not at the offset the last one was left at (every card's diff
/// draws in the same place, so one shared scroll id carried it over). `p` back
/// to the first card returns to where it was left.
#[test]
fn review_queue_opens_each_diff_at_the_top() {
    const CARDS: [&str; 2] = ["Inline card creation", "Column reordering"];
    const FILES: [&str; 2] = ["src/queue_one.rs", "src/queue_two.rs"];
    const NAMES: [&str; 2] = ["one", "two"];
    // A diff line's label: its content, as the diff draws it.
    let line = |n: usize, i: usize| format!("fn {}_{i}() {{}}", NAMES[n]);

    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let ids = seed_in_review_with(&mut harness, repo.path(), &CARDS, &FILES, |n| {
        (1..=300).map(|i| line(n, i) + "\n").collect()
    });

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    wait_for_label(&mut harness, "1 / 2");
    wait_for_label(&mut harness, &line(0, 1));

    press_board_keys(&mut harness, &[egui::Key::Space; 3]);
    wait_for_absent(&mut harness, &line(0, 1));

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::D);
    wait_for_card_column(&mut harness, ids[0], "Done");
    wait_for_label(&mut harness, "2 / 2");
    wait_for_any_label(&mut harness, FILES[1]);
    wait_for_label(&mut harness, &line(1, 1));

    press_board_keys(&mut harness, &[egui::Key::P]);
    wait_for_label(&mut harness, "1 / 2");
    wait_for_any_label(&mut harness, FILES[0]);
    harness.run_steps(2);
    assert!(
        harness.query_by_label(&line(0, 1)).is_none(),
        "back on the first card, its diff is still paged down"
    );
}

/// Every part of the review header's breadcrumb bar sits on one centre line,
/// left side and right: ← Back, the status, the card ref, the position pill and
/// ↓/↑. egui centres a row's item in the row's height as it stands when the
/// item is placed, so a row that grows mid-layout (the ↓/↑ are its tallest
/// parts, and draw after ← Back) walks each later part down a step.
#[test]
fn review_breadcrumb_parts_share_a_centre_line() {
    const CARDS: [&str; 2] = ["Inline card creation", "Column reordering"];
    const FILES: [&str; 2] = ["src/queue_one.rs", "src/queue_two.rs"];

    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let ids = seed_in_review(&mut harness, repo.path(), &CARDS, &FILES);
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    wait_for_label(&mut harness, "1 / 2");
    wait_for_any_label(&mut harness, FILES[0]);
    harness.run_steps(2);

    let card_ref = headway::wordid::card_ref(store::BOARD_ID, ids[0].bytes());
    let parts = [
        "← Back",
        "In Review",
        card_ref.as_str(),
        "1 / 2",
        "Next card",
        "Previous card",
    ];
    let centre = |label: &str| {
        let bb = label_box(&harness, label);
        (bb.y0 + bb.y1) / 2.0
    };
    let back = centre(parts[0]);
    for part in parts {
        let y = centre(part);
        assert!(
            (y - back).abs() < 0.5,
            "{part:?} is centred at y={y}, ← Back at y={back}"
        );
    }
}

/// Behavioural (no lavapipe): the review header's breadcrumb bar shows the
/// current card's column, live rather than the queue's snapshot, gapped from
/// the card ref after it and above the title row. The queue opens on an In
/// Review card; `D` moves it to Done and steps on, and `p` back to it reads
/// Done, so a card already ruled on says so.
#[test]
fn review_header_shows_the_card_status() {
    const CARDS: [&str; 2] = ["Inline card creation", "Column reordering"];
    const FILES: [&str; 2] = ["src/queue_one.rs", "src/queue_two.rs"];

    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let ids = seed_in_review(&mut harness, repo.path(), &CARDS, &FILES);

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    wait_for_label(&mut harness, "1 / 2");
    wait_for_label(&mut harness, "In Review");
    let card_ref = headway::wordid::card_ref(store::BOARD_ID, ids[0].bytes());
    assert_labels_gapped(&harness, "In Review", &card_ref);
    let bottom = |label: &str| {
        harness
            .get_by_label(label)
            .accesskit_node()
            .bounding_box()
            .expect("box")
            .y1
    };
    let top = |label: &str| {
        harness
            .get_by_label(label)
            .accesskit_node()
            .bounding_box()
            .expect("box")
            .y0
    };
    assert!(
        bottom("In Review") <= top(CARDS[0]),
        "the status sits in the bar above the title"
    );
    assert!(harness.query_by_label("Done").is_none());

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::D);
    wait_for_card_column(&mut harness, ids[0], "Done");
    wait_for_label(&mut harness, "2 / 2");
    wait_for_label(&mut harness, "In Review");

    press_board_keys(&mut harness, &[egui::Key::P]);
    wait_for_label(&mut harness, "1 / 2");
    wait_for_label(&mut harness, CARDS[0]);
    wait_for_label(&mut harness, "Done");
    assert!(harness.query_by_label("In Review").is_none());
}

/// Assert `right` starts a real gap after `left` ends on the same row. The
/// harness hands Headway the chrome's zero item gap (see [`render_headway`]),
/// so this fails if Headway stops owning its own spacing and the two labels
/// glue together as "In Reviewheadway:headway/…".
fn assert_labels_gapped(harness: &Harness<'static, HeadwayTestState>, left: &str, right: &str) {
    let left_box = harness
        .get_by_label(left)
        .accesskit_node()
        .bounding_box()
        .expect("left bounds");
    let right_box = harness
        .get_by_label(right)
        .accesskit_node()
        .bounding_box()
        .expect("right bounds");
    let gap = right_box.x0 - left_box.x1;
    assert!(
        gap >= f64::from(notedeck::tokens::SPACING_XS),
        "{left:?} and {right:?} are glued together (gap {gap})"
    );
}

/// Behavioural (no lavapipe): with nothing in In Review, `R` says so in the
/// header and opens nothing.
#[test]
fn review_queue_with_nothing_in_review_does_not_open() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    wait_for_label(&mut harness, "Nothing in review");
    assert!(harness.query_by_label("← Back").is_none());
    assert_eq!(queue_pushes_and_last_back(&mut harness).0, 0);
}

/// Where the review-queue snapshot's fixture repo lives. A fixed path, not a
/// tempdir, because the pane prints it (the source's hover, and the detail's
/// record rows) and a random path would change the pixels every run. Spelled `/tmp` rather than
/// `temp_dir()` for the same reason: nix shells point `TMPDIR` elsewhere.
/// Windows has no `/tmp`, and snapshots only render on Linux, so its
/// behavioural run takes the temp dir.
fn review_fixture_dir() -> String {
    if cfg!(unix) {
        "/tmp/headway-review-fixture/notedeck".to_string()
    } else {
        std::env::temp_dir()
            .join("headway-review-fixture")
            .join("notedeck")
            .to_string_lossy()
            .into_owned()
    }
}

/// The host the queue's records say they were made on: not this one, so the
/// pane has to find the commit in a checkout of the same repo, as it would a
/// commit made on another machine. Fixed, since the pane prints it.
const REVIEW_HOST: &str = "jex0";

/// The review queue fixture: where its repo is and the commit each In Review
/// card records.
struct ReviewFixture {
    /// The repo, at [`review_fixture_dir`].
    dir: String,
    /// Modifies `src/queue.rs` and adds `src/keys.rs` — the snapshot's diff.
    queue: String,
    /// Touches `src/keys.rs` again, for the queue's second card.
    verdicts: String,
    /// The root commit: the repo identity the records carry.
    root: String,
}

/// Commit everything in `dir` as `subject`, dated `at` (unix seconds) for
/// author and committer, by a fixed author, and return the sha. Pinning all of
/// it makes the sha, and the byline the pane prints, the same on every machine.
///
/// The offset is deliberately not UTC: newer gits print a UTC strict-ISO date
/// as `Z` and older ones as `+00:00`, which would move the byline's pixels
/// with the git version.
fn dated_commit(dir: &std::path::Path, subject: &str, at: u64) -> String {
    let date = format!("@{at} -0700");
    fixture_git(dir, &["add", "."]);
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Headway Tester",
            "-c",
            "user.email=tester@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            subject,
        ])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git commit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    fixture_git(dir, &["rev-parse", "HEAD"])
}

/// Build the review fixture's three commits in a fresh repo at `dir`.
fn build_review_fixture(dir: &std::path::Path) -> ReviewFixture {
    let write = |file: &str, body: &str| {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().expect("file has a parent")).unwrap();
        std::fs::write(path, body).unwrap();
    };
    fixture_git(dir, &["init", "-q", "-b", "headway"]);

    write(
        "src/queue.rs",
        "/// The cards a review walks, in column order.\n\
         pub struct ReviewQueue {\n    \
             cards: Vec<NoteId>,\n    \
             index: usize,\n\
         }\n",
    );
    let root = dated_commit(dir, "headway: review queue skeleton", SEED_AT - 7200);

    write(
        "src/queue.rs",
        "/// The cards a review walks, in column order.\n\
         pub struct ReviewQueue {\n    \
             cards: Vec<NoteId>,\n    \
             index: usize,\n\
         }\n\
         \n\
         impl ReviewQueue {\n    \
             /// The card on show, if the queue has any.\n    \
             pub fn current(&self) -> Option<NoteId> {\n        \
                 self.cards.get(self.index).copied()\n    \
             }\n\
         \n    \
             /// Step one card forward, stopping at the last.\n    \
             pub fn next(&mut self) {\n        \
                 self.index = (self.index + 1).min(self.cards.len().saturating_sub(1));\n    \
             }\n\
         }\n",
    );
    write(
        "src/keys.rs",
        "/// What a key does in the review queue.\n\
         pub enum QueueKey {\n    \
             Next,\n    \
             Prev,\n    \
             Leave,\n\
         }\n\
         \n\
         /// Map a bare key to its queue action.\n\
         pub fn queue_key(key: char) -> Option<QueueKey> {\n    \
             match key {\n        \
                 'n' | ']' => Some(QueueKey::Next),\n        \
                 'p' | '[' => Some(QueueKey::Prev),\n        \
                 'q' => Some(QueueKey::Leave),\n        \
                 _ => None,\n    \
             }\n\
         }\n",
    );
    let queue = dated_commit(
        dir,
        "headway: review queue over In Review cards (R, n/p)",
        SEED_AT - 3600,
    );

    write(
        "src/keys.rs",
        "/// What a key does in the review queue.\n\
         pub enum QueueKey {\n    \
             Next,\n    \
             Prev,\n    \
             Done,\n    \
             SendBack,\n    \
             Leave,\n\
         }\n",
    );
    let verdicts = dated_commit(
        dir,
        "headway: D and X verdicts in the review queue",
        SEED_AT,
    );

    ReviewFixture {
        dir: dir.to_string_lossy().into_owned(),
        queue,
        verdicts,
        root,
    }
}

/// The review fixture repo at [`review_fixture_dir`], built if it isn't there.
///
/// Every commit is fully pinned, so any build produces the same shas. That
/// lets concurrent runs (sibling worktrees share `/tmp`) settle on one repo
/// without a lock: each builds its own copy in a tempdir and renames it into
/// place, and a run that loses the rename keeps the winner's identical repo.
/// A repo left there by an older version of this fixture has a different head
/// and is replaced.
fn review_fixture() -> ReviewFixture {
    let dir = review_fixture_dir();
    let fixed = std::path::Path::new(&dir);
    let parent = fixed.parent().expect("fixture has a parent");
    std::fs::create_dir_all(parent).unwrap();
    let build = tempfile::tempdir_in(parent).expect("build dir");
    let built = build.path().join("notedeck");
    std::fs::create_dir(&built).unwrap();
    let fixture = ReviewFixture {
        dir: dir.clone(),
        ..build_review_fixture(&built)
    };

    let head = |dir: &std::path::Path| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    if head(fixed).as_deref() == Some(fixture.verdicts.as_str()) {
        return fixture;
    }
    if fixed.exists() {
        std::fs::remove_dir_all(fixed).expect("remove a stale review fixture");
    }
    if std::fs::rename(&built, fixed).is_err() {
        // Another run renamed its copy in first; it's the same repo.
        assert_eq!(
            head(fixed).as_deref(),
            Some(fixture.verdicts.as_str()),
            "a concurrent run left a different review fixture at {dir}"
        );
    }
    fixture
}

/// The review queue's two cards, in queue order.
const QUEUE_CARDS: [&str; 2] = ["Inline card creation", "Column reordering"];

/// The explainer the first queue card's record links.
const QUEUE_EXPLAINER: &str = "https://claude.ai/artifact/review-queue-explainer";

/// The agentic session the first queue card's record names.
const QUEUE_SESSION: &str = "agentium:power-baby-metal";

/// Seed the review queue snapshot's board: [`QUEUE_CARDS`] moved into In
/// Review, each with a record from [`REVIEW_HOST`] naming a fixture commit
/// (the first with an explainer and an agentium session, as an `autowork`
/// done step records), plus a record on the Done card saying this host has a
/// checkout of the repo at [`review_fixture_dir`] — which is how the pane finds
/// the other host's commits here without a fetch.
fn seed_review_queue(harness: &mut Harness<'static, HeadwayTestState>, fixture: &ReviewFixture) {
    let local = headway::git::host_name().expect("this host has a name");
    let done = harness_card_id(harness, "Scaffold the Headway app crate");
    apply_demo_action(
        harness,
        store::BoardAction::AddReview {
            card: done,
            review: event::ReviewFields {
                commit: Some(fixture.root.clone()),
                title: Some("headway: review queue skeleton".to_string()),
                branch: Some("headway".to_string()),
                host: Some(local),
                path: Some(fixture.dir.clone()),
                repo: Some(fixture.root.clone()),
                ..Default::default()
            },
        },
    );

    let records = [
        (
            &fixture.queue,
            "headway: review queue over In Review cards (R, n/p)",
            Some(QUEUE_SESSION),
            Some(QUEUE_EXPLAINER),
        ),
        (
            &fixture.verdicts,
            "headway: D and X verdicts in the review queue",
            None,
            None,
        ),
    ];
    for (title, (sha, subject, session, explainer)) in QUEUE_CARDS.iter().zip(records) {
        let card = harness_card_id(harness, title);
        move_to_in_review(harness, card);
        apply_demo_action(
            harness,
            store::BoardAction::AddReview {
                card,
                review: event::ReviewFields {
                    commit: Some(sha.clone()),
                    title: Some(subject.to_string()),
                    branch: Some("headway".to_string()),
                    host: Some(REVIEW_HOST.to_string()),
                    path: Some("/home/jb55/dev/notedeck".to_string()),
                    repo: Some(fixture.root.clone()),
                    agentium: session.map(str::to_string),
                    explainer: explainer.map(str::to_string),
                    ..Default::default()
                },
            },
        );
    }
}

/// Open the review queue on the seeded board and wait for its first card's
/// diff: both files, found in the local checkout rather than fetched.
fn open_review_queue(harness: &mut Harness<'static, HeadwayTestState>) {
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    wait_for_label(harness, "1 / 2");
    wait_for_label(harness, QUEUE_CARDS[0]);
    wait_for_any_label(harness, "src/queue.rs");
    wait_for_any_label(harness, "src/keys.rs");
    wait_for_label(harness, "local checkout");
}

/// Behavioural twin of [`snapshot_headway_review_queue`] (no lavapipe): the
/// same fixture, checked for what the snapshot shows. The queue opens on the
/// first In Review card with its record's commit diff (a modified and an added
/// file), found in this host's checkout though the record came from another
/// host; the title row carries the record's actions as icons (copy the ref,
/// open the session, review in it, the explainer), the byline its session
/// chip, and the breadcrumb bar ↓/↑ for the queue; and `?` pins the queue's
/// key strip.
#[test]
fn review_queue_shows_the_recorded_commit_diff() {
    let fixture = review_fixture();
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_review_queue(&mut harness, &fixture);
    open_review_queue(&mut harness);

    for icon in HEADER_ICONS {
        wait_for_label(&mut harness, icon);
    }
    wait_for_label(&mut harness, QUEUE_SESSION);
    wait_for_label(
        &mut harness,
        &format!("{REVIEW_HOST}:/home/jb55/dev/notedeck"),
    );
    wait_for_label(&mut harness, "Next card");
    wait_for_label(&mut harness, "Previous card");
    wait_for_label(&mut harness, "Headway Tester · 1h ago");
    assert!(harness.query_by_label("send back").is_none());

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::Questionmark);
    wait_for_label(&mut harness, "send back");
    wait_for_label(&mut harness, "next/prev file");
    assert_queue_hints_fit(&harness, 1200.0);
}

/// The review header's title-row icons' accessible names, left to right, for
/// a record with a session and an explainer.
const HEADER_ICONS: [&str; 4] = [
    "Copy card ref",
    "Open session",
    "Review in session",
    "Explainer",
];

/// A real-length title for the current queue card, about as long as an
/// `autowork` card's (~70 chars): the length the one-row header used to elide.
const LONG_QUEUE_TITLE: &str =
    "headway: review header — title first, peek takes the leftover of the row";

/// [`seed_review_queue`], then retitle its first card to `title` and open the
/// queue on it.
fn open_retitled_queue(harness: &mut Harness<'static, HeadwayTestState>, title: &str) {
    let fixture = review_fixture();
    seed_review_queue(harness, &fixture);
    let card = harness_card_id(harness, QUEUE_CARDS[0]);
    apply_demo_action(
        harness,
        store::BoardAction::EditTitle {
            card,
            title: title.to_string(),
        },
    );
    wait_for_label(harness, title);
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    wait_for_label(harness, "1 / 2");
    wait_for_any_label(harness, "src/queue.rs");
    wait_for_label(harness, "local checkout");
    harness.run_steps(2);
}

/// The width `text` lays out at on one line in `style`'s font, as the review
/// header draws it.
fn text_width(
    harness: &Harness<'static, HeadwayTestState>,
    text: &str,
    style: egui::TextStyle,
) -> f64 {
    let font = style.resolve(&harness.ctx.global_style());
    let width = harness.ctx.fonts_mut(|f| {
        f.layout_no_wrap(text.to_owned(), font, egui::Color32::WHITE)
            .size()
            .x
    });
    f64::from(width)
}

/// The box of the node labelled `label`.
fn label_box(harness: &Harness<'static, HeadwayTestState>, label: &str) -> egui::accesskit::Rect {
    harness
        .get_by_label(label)
        .accesskit_node()
        .bounding_box()
        .unwrap_or_else(|| panic!("{label:?} has a box"))
}

/// Assert the header's icons all sit inside a `width`-wide screen, right of
/// `title`'s box and within its first line's height, so none overlaps it.
fn assert_icons_beside(
    harness: &Harness<'static, HeadwayTestState>,
    title: egui::accesskit::Rect,
    width: f64,
) {
    for icon in HEADER_ICONS {
        let bb = label_box(harness, icon);
        assert!(bb.x1 <= width, "{icon:?} ends at {} past {width}px", bb.x1);
        assert!(
            bb.x0 >= title.x1,
            "{icon:?} at x={} overlaps the title ending at {}",
            bb.x0,
            title.x1
        );
        assert!(
            bb.y0 >= title.y0 - 1.0 && bb.y0 < title.y1,
            "{icon:?} is on the title's row"
        );
    }
}

/// The title row gives a real-length title its whole natural width at 1200px,
/// with no ellipsis, and wraps it onto more lines at 700px rather than eliding
/// it. Either way the icons sit right of it, inside the screen.
#[test]
fn review_header_title_wraps_beside_its_icons() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    open_retitled_queue(&mut harness, LONG_QUEUE_TITLE);
    let natural = text_width(&harness, LONG_QUEUE_TITLE, egui::TextStyle::Heading);
    let title = label_box(&harness, LONG_QUEUE_TITLE);
    assert!(
        title.width() + 1.0 >= natural,
        "the title drew {}px of its {natural}px",
        title.width()
    );
    let line = title.height();
    assert_icons_beside(&harness, title, 1200.0);

    harness.set_size(egui::Vec2::new(700.0, 800.0));
    harness.run_steps(3);
    let title = label_box(&harness, LONG_QUEUE_TITLE);
    assert!(
        title.height() > 1.5 * line,
        "at 700px the title wraps ({}px tall, one line is {line}px)",
        title.height()
    );
    assert!(
        title.x1 <= 700.0,
        "the wrapped title stays inside the screen"
    );
    assert_icons_beside(&harness, title, 700.0);
}

/// The breadcrumb bar's ↓ and ↑ step the queue as `n` and `p` do.
#[test]
fn review_header_arrows_step_the_queue() {
    let fixture = review_fixture();
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_review_queue(&mut harness, &fixture);
    open_review_queue(&mut harness);

    harness.get_by_label("Next card").click_accesskit();
    wait_for_label(&mut harness, "2 / 2");
    wait_for_label(&mut harness, QUEUE_CARDS[1]);
    wait_for_any_label(&mut harness, "src/keys.rs");

    harness.get_by_label("Previous card").click_accesskit();
    wait_for_label(&mut harness, "1 / 2");
    wait_for_label(&mut harness, QUEUE_CARDS[0]);
}

/// The URL the app was asked to open in the next few frames, if any.
fn opened_url(harness: &mut Harness<'static, HeadwayTestState>) -> Option<String> {
    for _ in 0..4 {
        harness.step();
        let opened = harness
            .output()
            .platform_output
            .commands
            .iter()
            .find_map(|c| match c {
                egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
                _ => None,
            });
        if opened.is_some() {
            return opened;
        }
    }
    None
}

/// The title row's session and explainer icons raise what `s` and `e` do: the
/// same `AppAction::Open` of the record's session, and the same explainer URL.
/// (`S` and the review-in-session icon are
/// [`shift_s_in_the_queue_opens_the_session_asking_for_a_review`]'s.)
#[test]
fn review_header_icons_are_their_keys() {
    let fixture = review_fixture();
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_review_queue(&mut harness, &fixture);
    open_review_queue(&mut harness);
    wait_for_label(&mut harness, "Open session");
    raised_opens(&mut harness);

    harness.press_key(egui::Key::S);
    harness.run_ok();
    let keyed = raised_opens(&mut harness);
    assert_eq!(keyed, vec![notedeck::OpenUri::new(QUEUE_SESSION)]);
    harness.get_by_label("Open session").click_accesskit();
    harness.run_ok();
    assert_eq!(raised_opens(&mut harness), keyed, "the icon is s");

    harness.press_key(egui::Key::E);
    let keyed = opened_url(&mut harness);
    assert_eq!(keyed.as_deref(), Some(QUEUE_EXPLAINER));
    harness.get_by_label("Explainer").click_accesskit();
    assert_eq!(opened_url(&mut harness), keyed, "the icon is e");
}

/// The queue's key-strip labels, in strip order: what [`assert_queue_hints_fit`]
/// checks for.
const QUEUE_HINT_LABELS: [&str; 15] = [
    "open",
    "explainer",
    "session/review",
    "review diff",
    "archive",
    "done",
    "send back",
    "next/prev card",
    "scroll",
    "half page",
    "top/bottom",
    "next/prev file",
    "comment on picked lines",
    "send comments",
    "leave",
];

/// Assert every queue hint label ends inside a `width`-wide screen, so no
/// group ran past the right edge into the clip, and return how many rows the
/// strip took.
fn assert_queue_hints_fit(harness: &Harness<'static, HeadwayTestState>, width: f64) -> usize {
    let mut rows: Vec<f64> = Vec::new();
    for label in QUEUE_HINT_LABELS {
        let bb = harness
            .get_by_label(label)
            .accesskit_node()
            .bounding_box()
            .expect("a hint label has a box");
        assert!(
            bb.x1 <= width,
            "hint {label:?} ends at x={} past the {width}px screen",
            bb.x1
        );
        if !rows.iter().any(|y| (y - bb.y0).abs() < 1.0) {
            rows.push(bb.y0);
        }
    }
    rows.len()
}

/// On a 600px screen the queue's key strip wraps whole groups onto several
/// rows: every label, through `leave`, ends inside the screen instead of
/// running off it.
#[test]
fn review_queue_key_hints_wrap_on_a_narrow_pane() {
    let fixture = review_fixture();
    let mut harness = behavioral_harness(egui::Vec2::new(600.0, 800.0));
    seed_review_queue(&mut harness, &fixture);
    open_review_queue(&mut harness);
    wait_for_label(&mut harness, "Explainer");

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::Questionmark);
    wait_for_label(&mut harness, "leave");
    let rows = assert_queue_hints_fit(&harness, 600.0);
    assert!(rows >= 2, "the strip wrapped onto {rows} row(s)");
}

/// The top of the highest node labelled `label`, or `None` when none is laid
/// out (scrolled out of a `show_rows` area, say).
fn label_top(harness: &Harness<'static, HeadwayTestState>, label: &str) -> Option<f64> {
    harness
        .get_all_by_label(label)
        .filter_map(|node| node.accesskit_node().bounding_box())
        .map(|bb| bb.y0)
        .reduce(f64::min)
}

/// The agentium sessions the app was asked to open since the last call.
fn raised_opens(harness: &mut Harness<'static, HeadwayTestState>) -> Vec<notedeck::OpenUri> {
    let app_ctx = harness.state_mut().notedeck.app_context();
    app_ctx
        .app_actions
        .take()
        .into_iter()
        .filter_map(|action| match action {
            notedeck::AppAction::Open(open) => Some(open),
            _ => None,
        })
        .collect()
}

/// The comment posted on the queue card's record before the pane opens, in
/// [`review_comments_draft_then_send_to_the_session`] and its snapshot.
const POSTED_COMMENT: &str = "current() and next() could share an index guard";

/// Post [`POSTED_COMMENT`] on new lines 8-10 of `src/queue.rs` (the
/// `current` fn) in the first queue card's record, as `C` would have.
fn post_queue_comment(harness: &mut Harness<'static, HeadwayTestState>, fixture: &ReviewFixture) {
    let card = harness_card_id(harness, QUEUE_CARDS[0]);
    // The seeded record lands on the async writer thread like any edit, so
    // wait for it to fold rather than reading the board once.
    let record = std::cell::Cell::new(None);
    wait_for_demo(harness, "the queue card's review record", |view| {
        record.set(
            view.card(card)
                .and_then(|c| c.reviews.first())
                .map(|r| r.id),
        );
        record.get().is_some()
    });
    let record = record.get().expect("folded record");
    apply_demo_action(
        harness,
        store::BoardAction::AddReviewComments {
            card,
            record,
            comments: vec![store::NewReviewComment {
                location: event::ReviewLocation {
                    path: "src/queue.rs".to_string(),
                    commit: fixture.queue.clone(),
                    start: 8,
                    end: 10,
                    side: event::LineSide::New,
                },
                body: POSTED_COMMENT.to_string(),
            }],
        },
    );
}

/// The `src/keys.rs` hunk header's button in the queue card's diff (a new
/// 16-line file), which picks the whole hunk.
fn keys_hunk<'h>(harness: &'h Harness<'static, HeadwayTestState>) -> Node<'h> {
    harness.get(
        egui_kittest::kittest::By::new()
            .role(egui::accesskit::Role::Button)
            .label("@@ -0,0 +1,16 @@"),
    )
}

/// Pick the keys.rs hunk, write `body` in the composer (`c` gives it the
/// keyboard) and file it with Ctrl+Enter.
fn draft_on_keys_hunk(harness: &mut Harness<'static, HeadwayTestState>, body: &str) {
    keys_hunk(harness).click_accesskit();
    wait_for_label(harness, "src/keys.rs:1-16");
    harness.press_key(egui::Key::C);
    harness.run_ok();
    harness
        .input_mut()
        .events
        .push(egui::Event::Text(body.to_string()));
    harness.run_ok();
    harness.press_key_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    wait_for_label(harness, "Send 1 comment");
}

/// Inline review comments, as a reviewer writes them in the queue: a comment
/// already posted on the record shows under its lines; `c` with nothing picked
/// says how to pick; a click on a hunk's header picks it and opens the
/// composer, where `c` puts the keyboard; Ctrl+Enter files a draft, drawn under
/// its lines and counted on the header's "Send 1 comment"; a click on a line's
/// numbers then a shift-click picks a run; and `C` posts the draft on the
/// record and sends it to the record's session in one message quoting its
/// lines, after which it comes back from the fold as a posted comment.
#[test]
fn review_comments_draft_then_send_to_the_session() {
    let fixture = review_fixture();
    // Tall enough that both files' lines are all laid out.
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 1400.0));
    seed_review_queue(&mut harness, &fixture);
    post_queue_comment(&mut harness, &fixture);
    open_review_queue(&mut harness);
    wait_for_label(&mut harness, POSTED_COMMENT);
    raised_opens(&mut harness);

    harness.press_key(egui::Key::C);
    wait_for_label(
        &mut harness,
        "Click a line number to pick lines to comment on",
    );

    draft_on_keys_hunk(&mut harness, "keys want a test");
    wait_for_label(&mut harness, "Draft: keys want a test");
    assert!(
        harness.query_by_label("src/keys.rs:1-16").is_some(),
        "draft row"
    );

    // New line 17 is only in queue.rs; the shift-click on its context line 5
    // stretches the pick up over the added block.
    harness.get_by_label("       17").click_accesskit();
    wait_for_label(&mut harness, "src/queue.rs:17");
    // Modifiers are input state, set by events since egui 0.36.
    harness
        .input_mut()
        .events
        .push(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    harness.get_by_label("   5    5").click_accesskit();
    harness.run_ok();
    harness
        .input_mut()
        .events
        .push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    wait_for_label(&mut harness, "src/queue.rs:5-17");

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::C);
    wait_for_absent(&mut harness, "Send 1 comment");
    let opens = raised_opens(&mut harness);
    assert_eq!(opens.len(), 1, "one open for the batch: {opens:?}");
    assert_eq!(opens[0].reference, QUEUE_SESSION);
    let msg = opens[0].msg.as_deref().expect("a message");
    assert!(msg.starts_with("Review comments on commit "), "{msg}");
    assert!(msg.contains("src/keys.rs:1-16\n```diff\n+/// What a key does in the review queue."));
    assert!(msg.ends_with("```\nkeys want a test"), "{msg}");

    // Posted: it folds back onto the record and draws as a posted comment,
    // by the note renderer, so it says who wrote it (this account has no
    // profile, so the renderer's placeholder name).
    wait_for_label(&mut harness, "keys want a test");
    assert!(harness.query_by_label("Draft: keys want a test").is_none());
    assert_eq!(
        harness.query_all_by_label("nostrich").count(),
        2,
        "both posted comments carry their author"
    );
}

/// Snapshot: the review queue's diff with a comment posted on `src/queue.rs`
/// drawn under its lines, a draft on the whole `src/keys.rs` hunk marked the
/// same way in the warning colour (and listed above the diff, counted on the
/// header's "Send 1 comment"), and the keys.rs hunk picked again with the
/// composer open over it.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_review_comments() {
    let fixture = review_fixture();
    // Tall enough for both files, so the posted comment shows too.
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 1400.0));
    seed_review_queue(&mut harness, &fixture);
    post_queue_comment(&mut harness, &fixture);
    open_review_queue(&mut harness);
    wait_for_label(&mut harness, POSTED_COMMENT);
    draft_on_keys_hunk(&mut harness, "keys want a test");
    keys_hunk(&harness).click_accesskit();
    // The composer, over the pick (its place reads as the draft's does).
    wait_for_label(&mut harness, "Comment on");
    harness.run_steps(3);
    harness.snapshot("headway_review_comments");
}

/// A review pane opened from a card's detail ("Review diff"), not the queue,
/// reads with the queue's keys: `G` scrolls the diff, `?` pins a strip that
/// says `q` goes back to the card, `n` steps to the next In Review card's
/// review, and `q` backs out to that card's detail.
#[test]
fn a_plain_review_pane_reads_with_the_queue_keys() {
    let fixture = review_fixture();
    // Short, so the two-file diff overflows and has somewhere to scroll.
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 420.0));
    seed_review_queue(&mut harness, &fixture);
    harness.get_by_label(QUEUE_CARDS[0]).click();
    wait_for_label(&mut harness, "± Review diff");
    harness.get_by_label("± Review diff").click_accesskit();
    wait_for_any_label(&mut harness, "src/queue.rs");
    wait_for_label(&mut harness, "local checkout");
    harness.run_steps(3);

    let before = label_top(&harness, "src/queue.rs");
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::G);
    harness.run_steps(3);
    let after = label_top(&harness, "src/queue.rs");
    assert!(
        after.is_none_or(|y| Some(y) < before),
        "G scrolls the diff: src/queue.rs went from {before:?} to {after:?}"
    );

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::Questionmark);
    wait_for_label(&mut harness, "back to card");
    wait_for_label(&mut harness, "next/prev file");

    press_board_keys(&mut harness, &[egui::Key::N]);
    wait_for_label(&mut harness, QUEUE_CARDS[1]);
    wait_for_any_label(&mut harness, "src/keys.rs");

    press_board_keys(&mut harness, &[egui::Key::Q]);
    wait_for_label(&mut harness, "± Review diff");
    wait_for_label(&mut harness, QUEUE_CARDS[1]);
}

/// The card detail takes the card actions: `s` opens its record's agentium
/// session, `n` steps to the next card in its column, `D` moves that card to
/// Done. An `s` typed into the comment composer stays text.
#[test]
fn the_detail_takes_the_card_actions() {
    const FIRST: &str = "Define nostr event model for boards";
    const SECOND: &str = "Sync cards across relays";
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let first = harness_card_id(&mut harness, FIRST);
    let second = harness_card_id(&mut harness, SECOND);
    let sha = "4e5f60718293a4b5c6d7e8f9012345678ab9c0d1";
    apply_demo_action(
        &mut harness,
        store::BoardAction::AddReview {
            card: first,
            review: event::ReviewFields {
                commit: Some(sha.to_string()),
                agentium: Some(QUEUE_SESSION.to_string()),
                ..Default::default()
            },
        },
    );
    harness.get_by_label(FIRST).click();
    wait_for_any_label(&mut harness, &sha[..12]);
    raised_opens(&mut harness);

    press_board_keys(&mut harness, &[egui::Key::S]);
    assert_eq!(
        raised_opens(&mut harness),
        vec![notedeck::OpenUri::new(QUEUE_SESSION)]
    );

    press_board_keys(&mut harness, &[egui::Key::N]);
    wait_for_label(&mut harness, SECOND);

    // `D` lands on the card `n` stepped to, not the one it stepped from.
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::D);
    wait_for_card_column(&mut harness, second, "Done");

    // Focused through AccessKit rather than clicked: the move's fold can still
    // add rows above the composer after the column changes, and a click (a
    // press frame then a release frame since egui 0.36) aimed at where the
    // field was then lands on whatever moved into its place.
    harness
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .last()
        .expect("the comment composer")
        .focus();
    harness.run_ok();
    type_key(&mut harness, egui::Key::S, "s");
    assert_eq!(raised_opens(&mut harness), vec![], "s typed, not a session");
    let composer = harness
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .find(|n| n.is_focused())
        .expect("the focused comment composer");
    assert_eq!(composer.value().as_deref(), Some("s"));
}

/// An `X` composer closes with the detail it was opened in. Before, it
/// stayed open over the next card's detail and took the Enter typed into that
/// card's comment box, posting the reason on the first card and sending it
/// back. Now the Enter is the comment box's newline, and the first card keeps
/// its column and its thread.
#[test]
fn the_reason_composer_does_not_follow_you_to_another_card() {
    const FIRST: &str = "Define nostr event model for boards";
    const SECOND: &str = "Sync cards across relays";
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let first = harness_card_id(&mut harness, FIRST);
    let (column, comments) = {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context();
        (
            demo_card_column(app_ctx.ndb, &author, first),
            demo_card_comments(app_ctx.ndb, &author, first),
        )
    };

    harness.get_by_label(FIRST).click();
    wait_for_label(&mut harness, "← Back");
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::X);
    wait_for_label(&mut harness, "Send back");
    harness
        .input_mut()
        .events
        .push(egui::Event::Text("flaky".to_string()));
    harness.run_ok();

    harness.get_by_label("← Back").click_accesskit();
    harness.run_ok();
    harness.get_by_label(SECOND).click();
    wait_for_label(&mut harness, "← Back");
    harness
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .last()
        .expect("the comment composer")
        .click();
    harness.run_ok();
    type_key(&mut harness, egui::Key::H, "hi");
    harness.press_key(egui::Key::Enter);
    harness.run_ok();

    let composer = harness
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .find(|n| n.is_focused())
        .expect("the focused comment composer");
    assert_eq!(
        composer.value().as_deref(),
        Some("hi\n"),
        "Enter is its own"
    );
    assert!(
        harness.query_by_label("Send back").is_none(),
        "composer closed"
    );
    // A send-back lands through the async writer: give it the time it would
    // take before checking it never came.
    let settle = Instant::now() + Duration::from_millis(500);
    while Instant::now() < settle {
        harness.run_ok();
        std::thread::sleep(Duration::from_millis(25));
    }
    let state = harness.state_mut();
    let author = state.account.pubkey;
    let app_ctx = state.notedeck.app_context();
    assert_eq!(
        demo_card_column(app_ctx.ndb, &author, first),
        column,
        "not sent back"
    );
    assert_eq!(
        demo_card_comments(app_ctx.ndb, &author, first),
        comments,
        "no review: comment"
    );
}

/// A grid `X` shows its composer on the frame of the press, focused and
/// without the X typed into it: the grid's keys run before the bar draws.
#[test]
fn a_grid_x_shows_its_composer_at_once() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    // One pass a frame, so a discard's second pass can't draw the bar late
    // and hide the order this checks.
    harness
        .ctx
        .options_mut(|o| o.max_passes = std::num::NonZeroUsize::MIN);
    press_board_keys(&mut harness, &[egui::Key::J]);
    // The key-down and its character straight into the input: `press_key*`
    // queues a release too, and `step` would run a frame for each.
    harness.input_mut().events.extend([
        egui::Event::Key {
            key: egui::Key::X,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::SHIFT,
        },
        egui::Event::Text("X".to_string()),
    ]);
    harness.step();
    assert!(
        harness.query_by_label("Send back").is_some(),
        "the bar draws on the press's frame"
    );
    harness.run_ok();
    assert_eq!(focused_text_input(&harness).value().as_deref(), Some(""));
}

/// The detail's sidebar keeps the newest review in reach under the thread:
/// its sha, its session and "Explainer ↗", with no "Review in session" link
/// (that stays the review pane's). "All 4 records ›" opens the review pane,
/// as `r` does.
#[test]
fn the_detail_sidebar_review_block_opens_the_pane() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 900.0));
    seed_detail_reviews(&mut harness);
    harness.get_by_label(DETAIL_REVIEW_CARD).click();
    let newest = DETAIL_RECORDS[3].0;
    wait_for_label(&mut harness, "All 4 records ›");
    // The newest record, its session and its explainer are in the body's
    // Review section and the sidebar; one older shown row has an explainer.
    assert_eq!(harness.get_all_by_label(&newest[..12]).count(), 2);
    assert_eq!(harness.get_all_by_label(QUEUE_SESSION).count(), 2);
    assert_eq!(harness.get_all_by_label("Explainer ↗").count(), 3);
    assert!(harness.query_by_label("Review in session").is_none());

    harness.get_by_label("All 4 records ›").click_accesskit();
    wait_for_absent(&mut harness, "± Review diff");
    assert!(harness.query_by_label("All 4 records ›").is_none());
}

/// Snapshot: the review queue open on the first of two In Review cards — the
/// breadcrumb bar with the card's status, its position and ↓/↑, the title row
/// with the record's icons, where the commit was found with its agentium
/// session in the byline, and its two-file diff. Then the same with `?`
/// pinning the queue's key strip.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_review_queue() {
    let fixture = review_fixture();
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));
    seed_review_queue(&mut harness, &fixture);
    open_review_queue(&mut harness);
    harness.run_steps(3);
    harness.snapshot("headway_review_queue");

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::Questionmark);
    wait_for_label(&mut harness, "send back");
    harness.run_steps(3);
    harness.snapshot("headway_review_queue_key_hints");
}

/// Snapshot: the review queue with a real-length current title (see
/// [`review_header_title_wraps_beside_its_icons`]): at 1200px it draws in
/// full on the title row, no ellipsis, beside the record's icons.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_review_queue_long_titles() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));
    open_retitled_queue(&mut harness, LONG_QUEUE_TITLE);
    harness.run_steps(3);
    harness.snapshot("headway_review_queue_long_titles");
}

/// Snapshot: an epic's review queue. Both queue cards are made subissues of
/// the demo epic, whose detail then offers "Review 2" in its Sub-issues
/// header; clicking it opens the queue over them, its header naming the epic
/// (`in <word-id>`) beside the position.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_review_queue_epic() {
    let fixture = review_fixture();
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));
    seed_review_queue(&mut harness, &fixture);
    let epic = parent_under_demo_epic(&mut harness, &QUEUE_CARDS);
    harness.get_by_label(DEMO_EPIC).click();
    wait_for_label(&mut harness, "Review 2");
    harness.run_steps(3);
    harness.snapshot("headway_detail_epic_review");

    harness.get_by_label("Review 2").click_accesskit();
    wait_for_label(&mut harness, "1 / 2");
    wait_for_label(
        &mut harness,
        &format!("in {}", headway::wordid::encode(epic.bytes())),
    );
    wait_for_any_label(&mut harness, "src/queue.rs");
    wait_for_label(&mut harness, "local checkout");
    harness.run_steps(3);
    harness.snapshot("headway_review_queue_epic");
}

/// Snapshot: a plain review pane (opened from a card's "Review diff") with `?`
/// pinning its key strip: the queue's strip, bar `q` going back to the card.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_review_pane_key_hints() {
    let fixture = review_fixture();
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));
    seed_review_queue(&mut harness, &fixture);
    harness.get_by_label(QUEUE_CARDS[0]).click();
    wait_for_label(&mut harness, "± Review diff");
    harness.get_by_label("± Review diff").click_accesskit();
    wait_for_any_label(&mut harness, "src/queue.rs");
    wait_for_label(&mut harness, "local checkout");
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::Questionmark);
    wait_for_label(&mut harness, "back to card");
    harness.run_steps(3);
    harness.snapshot("headway_review_pane_key_hints");
}

/// Snapshot: a card's detail with `?` pinning its key strip: the card actions
/// and the detail's scrolling.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_detail_key_hints() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));
    harness
        .get_by_label("Define nostr event model for boards")
        .click();
    wait_for_label(&mut harness, "← Back");
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::Questionmark);
    wait_for_label(&mut harness, "session/review");
    harness.run_steps(3);
    harness.snapshot("headway_detail_key_hints");
}

/// Snapshot: the review queue on a phone-width screen, where the breadcrumb
/// bar drops the card ref and the column's name, and the title wraps beside
/// its icons.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_review_queue_narrow() {
    let fixture = review_fixture();
    let mut harness = headway_harness(egui::Vec2::new(420.0, 800.0));
    seed_review_queue(&mut harness, &fixture);
    open_review_queue(&mut harness);
    harness.run_steps(3);
    harness.snapshot("headway_review_queue_narrow");
}

/// The card the detail Review section tests seed their records on.
const DETAIL_REVIEW_CARD: &str = "Column reordering";

/// The detail Review section's four records, oldest first: `(sha, subject,
/// session, explainer)`. Not resolved (the section never loads a diff), so the
/// shas needn't exist; fixed, so the snapshot is too. The newest names a
/// session and an explainer, as an `autowork` done step records.
const DETAIL_RECORDS: [(&str, &str, Option<&str>, Option<&str>); 4] = [
    (
        "0a1b2c3d4e5f60718293a4b5c6d7e8f901234567",
        "headway: drag columns by their header",
        None,
        None,
    ),
    (
        "1b2c3d4e5f60718293a4b5c6d7e8f9012345678a",
        "headway: persist the column order as ranks",
        None,
        None,
    ),
    (
        "2c3d4e5f60718293a4b5c6d7e8f9012345678ab9",
        "headway: animate a column sliding into its new slot",
        None,
        Some(DETAIL_OLDER_EXPLAINER),
    ),
    (
        "3d4e5f60718293a4b5c6d7e8f9012345678ab9c0",
        "headway: keyboard column reordering with a long subject that has to elide",
        Some(QUEUE_SESSION),
        Some(QUEUE_EXPLAINER),
    ),
];

/// The explainer of [`DETAIL_RECORDS`]' second-newest record: not the
/// newest's, so a click on it can be told from `e`, which opens the newest's.
const DETAIL_OLDER_EXPLAINER: &str = "https://claude.ai/artifact/column-slide-explainer";

/// Record [`DETAIL_RECORDS`] on [`DETAIL_REVIEW_CARD`] ([`seed_records`]).
fn seed_detail_reviews(harness: &mut Harness<'static, HeadwayTestState>) {
    seed_records(harness, &DETAIL_RECORDS);
}

/// Record `records` (shaped as [`DETAIL_RECORDS`]) on [`DETAIL_REVIEW_CARD`],
/// oldest first, each from [`REVIEW_HOST`] with a deep checkout path. Waits
/// for each to fold before adding the next, so `AddReview` stamps it past the
/// last and the order is the listed one.
fn seed_records(
    harness: &mut Harness<'static, HeadwayTestState>,
    records: &[(&str, &str, Option<&str>, Option<&str>)],
) {
    let card = harness_card_id(harness, DETAIL_REVIEW_CARD);
    for (i, (sha, subject, session, explainer)) in records.iter().enumerate() {
        apply_demo_action(
            harness,
            store::BoardAction::AddReview {
                card,
                review: event::ReviewFields {
                    commit: Some(sha.to_string()),
                    title: Some(subject.to_string()),
                    branch: Some("headway".to_string()),
                    host: Some(REVIEW_HOST.to_string()),
                    path: Some("/home/jb55/dev/github/damus-io/notedeck-headway".to_string()),
                    agentium: session.map(str::to_string),
                    explainer: explainer.map(str::to_string),
                    ..Default::default()
                },
            },
        );
        let deadline = Instant::now() + SETTLE_TIMEOUT;
        loop {
            let folded = {
                let state = harness.state_mut();
                let author = state.account.pubkey;
                let app_ctx = state.notedeck.app_context();
                let txn = Transaction::new(app_ctx.ndb).expect("txn");
                let boards = headway::event::fold_board(app_ctx.ndb, &txn, &author)
                    .expect("folded")
                    .finalize();
                headway::event::find_board(&boards, &author, store::BOARD_ID)
                    .and_then(|view| view.card(card))
                    .map_or(0, |c| c.reviews.len())
            };
            if folded > i {
                break;
            }
            assert!(Instant::now() < deadline, "record {i} never folded");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// Behavioural (no lavapipe): the detail's Review section lists a card's
/// records newest first, but only the newest three until "Show all 4" is
/// clicked; "Show fewer" folds it back.
#[test]
fn detail_review_section_shows_the_newest_three_until_show_all() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_detail_reviews(&mut harness);
    harness.get_by_label(DETAIL_REVIEW_CARD).click();

    let short = |i: usize| &DETAIL_RECORDS[i].0[..12];
    // The newest is in the sidebar's Review block too.
    for i in 1..4 {
        wait_for_any_label(&mut harness, short(i));
    }
    assert!(
        harness.query_by_label(short(0)).is_none(),
        "the oldest record waits behind Show all"
    );

    harness.get_by_label("Show all 4").click_accesskit();
    wait_for_label(&mut harness, short(0));
    harness.get_by_label("Show fewer").click_accesskit();
    wait_for_absent(&mut harness, short(0));
}

/// Behavioural (no lavapipe): a row in the detail's Review section acts on its
/// own record, not the newest `r` and `e` act on. Clicking the second row's
/// sha pushes a `Review` entry naming that record, and clicking its explainer
/// opens that record's explainer — both through the card-action path the keys
/// and the sidebar take.
#[test]
fn a_detail_review_row_click_acts_on_its_own_record() {
    use notedeck::NavRequest;
    use notedeck_headway::HeadwayRoute;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 900.0));
    seed_detail_reviews(&mut harness);
    let card = harness_card_id(&mut harness, DETAIL_REVIEW_CARD);
    // The second row's record, newest first.
    let second = {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context();
        let txn = Transaction::new(app_ctx.ndb).expect("txn");
        let boards = headway::event::fold_board(app_ctx.ndb, &txn, &author)
            .expect("folded")
            .finalize();
        let view =
            headway::event::find_board(&boards, &author, store::BOARD_ID).expect("demo board");
        let record = &view.card(card).expect("card").reviews[1];
        assert_eq!(record.fields.commit.as_deref(), Some(DETAIL_RECORDS[2].0));
        record.id
    };
    harness.get_by_label(DETAIL_REVIEW_CARD).click();
    let second_sha = &DETAIL_RECORDS[2].0[..12];
    wait_for_label(&mut harness, second_sha);
    wait_for_label(&mut harness, "All 4 records ›");

    // Its explainer: the body's link on the line under its sha (the sidebar's
    // is right of the body, in the column of "All 4 records ›").
    let sha_top = label_top(&harness, second_sha).expect("second row's sha");
    let sidebar_left = harness
        .get_by_label("All 4 records ›")
        .accesskit_node()
        .bounding_box()
        .expect("sidebar records line")
        .x0;
    harness
        .get_all_by_label("Explainer ↗")
        .filter(|node| {
            node.accesskit_node()
                .bounding_box()
                .is_some_and(|bb| bb.x1 < sidebar_left && bb.y0 > sha_top)
        })
        .min_by(|a, b| {
            let top = |n: &Node<'_>| {
                n.accesskit_node()
                    .bounding_box()
                    .map_or(f64::MAX, |bb| bb.y0)
            };
            top(a).total_cmp(&top(b))
        })
        .expect("the second row's explainer")
        .click_accesskit();
    let mut opened = None;
    for _ in 0..4 {
        harness.step();
        opened = harness
            .output()
            .platform_output
            .commands
            .iter()
            .find_map(|c| match c {
                egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
                _ => None,
            });
        if opened.is_some() {
            break;
        }
    }
    assert_eq!(opened.as_deref(), Some(DETAIL_OLDER_EXPLAINER));

    harness.state_mut().notedeck.app_context().navigator.take();
    harness.get_by_label(second_sha).click_accesskit();
    wait_for_absent(&mut harness, "± Review diff");
    let requests = harness.state_mut().notedeck.app_context().navigator.take();
    let reviews: Vec<&HeadwayRoute> = requests
        .iter()
        .filter_map(|req| match req {
            NavRequest::PushToActive(entry) => entry.token.downcast_ref::<HeadwayRoute>(),
            _ => None,
        })
        .filter(|route| route.review_card().is_some())
        .collect();
    assert_eq!(reviews.len(), 1, "the click pushes one review entry");
    assert_eq!(reviews[0].review_card(), Some(card));
    assert_eq!(
        reviews[0].review_target().map(|t| t.record),
        Some(Some(second)),
        "the pane opens on the clicked record, not the newest"
    );
}

/// Stands in for Dave's `agentium:` reference parser, which the harness
/// doesn't load: each session it knows resolves to its kind-1 note, drawn by
/// [`StubSessionRenderer`].
struct StubSessionParser {
    sessions: Vec<(&'static str, NoteId)>,
}

impl notedeck::ReferenceParser for StubSessionParser {
    fn id(&self) -> &'static str {
        "agentium"
    }

    fn find(&self, text: &str) -> Option<std::ops::Range<usize>> {
        let start = text.find("agentium:")?;
        let len = text[start..]
            .find(char::is_whitespace)
            .unwrap_or(text.len() - start);
        Some(start..start + len)
    }

    fn resolve(
        &self,
        matched: &str,
        _ctx: &notedeck::ReferenceResolveCtx,
    ) -> Option<notedeck::ResolvedRef> {
        self.sessions
            .iter()
            .find(|(session, _)| *session == matched)
            .map(|(_, note)| notedeck::ResolvedRef::note(*note))
    }
}

/// Stands in for Dave's session chip: the note's content as a label that, on
/// a click, raises what Dave's does ([`notedeck::open_on_click`]).
struct StubSessionRenderer;

impl notedeck::KindRenderer for StubSessionRenderer {
    fn id(&self) -> &'static str {
        "test.session"
    }

    fn name(&self) -> &'static str {
        "Test session"
    }

    fn kinds(&self) -> &'static [u32] {
        &[1]
    }

    fn render(
        &self,
        ui: &mut egui::Ui,
        _note_context: &mut notedeck::NoteContext,
        req: &notedeck::KindRenderRequest,
    ) -> notedeck::KindRenderResponse {
        let response = ui.label(req.note.content());
        notedeck::open_on_click(ui, response, req.note)
    }
}

/// Behavioural (no lavapipe): a session chip in the detail's Review section
/// opens its own record's session the way `s` does, as one
/// `AppAction::Open` of it and not also the chip renderer's own open. The
/// second row's chip opens the older record's session, not the newest's
/// that `s` would; the sidebar's opens the newest's.
#[test]
fn a_detail_session_chip_click_opens_its_records_session() {
    const OLDER_SESSION: &str = "agentium:older-session-chip";
    const OLDER_CHIP: &str = "Older session chip";
    const NEWEST_CHIP: &str = "Newest session chip";
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 900.0));
    {
        let state = harness.state_mut();
        let secret = state.account.secret_key.secret_bytes();
        let ndb = state.notedeck.app_context().ndb.clone();
        let (older, _) = ingest_kind1(&ndb, OLDER_CHIP, &secret);
        let (newest, _) = ingest_kind1(&ndb, NEWEST_CHIP, &secret);
        state
            .notedeck
            .register_kind_renderer(Box::new(StubSessionRenderer));
        state
            .notedeck
            .register_reference_parser(Box::new(StubSessionParser {
                sessions: vec![(OLDER_SESSION, older), (QUEUE_SESSION, newest)],
            }));
    }
    let mut records = DETAIL_RECORDS;
    records[2].2 = Some(OLDER_SESSION);
    seed_records(&mut harness, &records);

    harness.get_by_label(DETAIL_REVIEW_CARD).click();
    wait_for_label(&mut harness, OLDER_CHIP);
    wait_for_label(&mut harness, "All 4 records ›");
    harness
        .state_mut()
        .notedeck
        .app_context()
        .app_actions
        .take();

    let raised = |harness: &mut Harness<'static, HeadwayTestState>| {
        harness.run_ok();
        let actions = harness
            .state_mut()
            .notedeck
            .app_context()
            .app_actions
            .take();
        actions
            .into_iter()
            .map(|action| match action {
                notedeck::AppAction::Open(open) => Ok(open),
                notedeck::AppAction::Note(_) => Err("AppAction::Note"),
                notedeck::AppAction::ToggleChrome => Err("AppAction::ToggleChrome"),
            })
            .collect::<Vec<_>>()
    };
    harness.get_by_label(OLDER_CHIP).click_accesskit();
    assert_eq!(
        raised(&mut harness),
        vec![Ok(notedeck::OpenUri::new(OLDER_SESSION))],
        "the row's chip opens its record's session, once"
    );

    let sidebar_left = harness
        .get_by_label("All 4 records ›")
        .accesskit_node()
        .bounding_box()
        .expect("sidebar records line")
        .x0;
    harness
        .get_all_by_label(NEWEST_CHIP)
        .find(|node| {
            node.accesskit_node()
                .bounding_box()
                .is_some_and(|bb| bb.x0 >= sidebar_left)
        })
        .expect("the sidebar's chip")
        .click_accesskit();
    assert_eq!(
        raised(&mut harness),
        vec![Ok(notedeck::OpenUri::new(QUEUE_SESSION))],
        "the sidebar's chip opens the newest record's session, once"
    );
}

/// Behavioural (no lavapipe): a description edit survives a sidebar Review
/// click in the frame that commits it. egui drops the editor's focus on the
/// press and fires the click on the release, so a quick click (here, press and
/// release in one frame) makes one detail pass carry both the edit and the
/// sidebar's `CardAction::Review`, which returns no board edit and used to
/// overwrite the edit with nothing.
///
/// The sidebar, not a body Review row: the body's rows sit under the editor,
/// whose collapse renumbers their widget ids in that frame, so egui drops a
/// row's click before the detail ever sees it.
#[test]
fn a_detail_review_click_keeps_a_same_frame_description_edit() {
    const EDITED: &str = "Drag a column header to reorder.";
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 900.0));
    seed_detail_reviews(&mut harness);
    let card = harness_card_id(&mut harness, DETAIL_REVIEW_CARD);
    harness.get_by_label(DETAIL_REVIEW_CARD).click();
    wait_for_label(&mut harness, "All 4 records ›");

    harness.get_by_label("Add description…").click_accesskit();
    harness.run_ok();
    harness
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .find(|n| n.is_focused())
        .expect("the focused description editor")
        .type_text(EDITED);
    harness.run_ok();

    // One frame takes the press and the release: the editor commits on the
    // press and the sidebar's click fires on the release. Raw input, since
    // kittest's own click runs a frame per event.
    let link = harness
        .get_by_label("All 4 records ›")
        .accesskit_node()
        .bounding_box()
        .expect("the sidebar records line");
    let pos = egui::pos2(
        ((link.x0 + link.x1) / 2.0) as f32,
        ((link.y0 + link.y1) / 2.0) as f32,
    );
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    harness.step();
    for pressed in [true, false] {
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    harness.run_ok();

    // The click still took: the review pane replaced the detail.
    assert!(harness.query_by_label("± Review diff").is_none());
    assert!(harness.query_by_label("All 4 records ›").is_none());

    // And the edit was published: the folded card carries it.
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        let description = {
            let state = harness.state_mut();
            let author = state.account.pubkey;
            let app_ctx = state.notedeck.app_context();
            let txn = Transaction::new(app_ctx.ndb).expect("txn");
            let boards = headway::event::fold_board(app_ctx.ndb, &txn, &author)
                .expect("folded")
                .finalize();
            headway::event::find_board(&boards, &author, store::BOARD_ID)
                .and_then(|view| view.card(card))
                .map(|c| c.description.clone())
        };
        if description.as_deref() == Some(EDITED) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the description edit was dropped: {description:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Snapshot: a card detail's Review section with four records — the newest
/// three as two-line rows (sha pill and subject; then where it was made, the
/// session and the explainer) and the "Show all 4" toggle — above the
/// "Review diff" button. Then on a phone-width screen, where each record's
/// second line wraps under its subject.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_detail_review() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 900.0));
    seed_detail_reviews(&mut harness);
    harness.get_by_label(DETAIL_REVIEW_CARD).click();
    // The newest sha shows twice (the section and the sidebar); this is once.
    wait_for_label(&mut harness, "All 4 records ›");
    for &(name, w, h) in &[
        ("headway_detail_review", 1200.0, 900.0),
        ("headway_detail_review_mobile", 400.0, 900.0),
    ] {
        harness.set_size(egui::Vec2::new(w, h));
        harness.run_steps(3);
        harness.snapshot(name);
    }
}

/// Snapshot: a card detail with four review records and a long comment
/// thread scrolled to its end, where the fixed sidebar still shows the newest
/// review under Properties: the sha pill and subject, the session chip, "Review
/// in session", "Explainer ↗" and "All 4 records ›".
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_detail_review_sidebar() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 700.0));
    seed_detail_reviews(&mut harness);
    let card = harness_card_id(&mut harness, DETAIL_REVIEW_CARD);
    // One comment, since comments added in the same second would order by id
    // and so differ from run to run. It's stamped in the oldest record's
    // second, not by `AddComment`: the records are stamped one past another
    // (T, T+1, …, running ahead of the clock), so a wall-clock comment lands
    // between whichever two records the seeding time put it. A same-second
    // tie draws the activity row first, so the thread is always the oldest
    // record, the comment, then the other three. The comment's "now" is
    // still wall-clock (the note renderer isn't on the frozen clock), so it
    // holds only while seeding through render stays under ~2s.
    {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let secret = state.account.secret_key.secret_bytes();
        let app_ctx = state.notedeck.app_context();
        let txn = Transaction::new(app_ctx.ndb).expect("txn");
        let boards = headway::event::fold_board(app_ctx.ndb, &txn, &author)
            .expect("folded")
            .finalize();
        let c = headway::event::find_board(&boards, &author, store::BOARD_ID)
            .and_then(|view| view.card(card))
            .expect("the review card");
        let oldest = c.reviews.last().expect("the seeded records").created_at;
        store::ingest_signed(
            app_ctx.ndb,
            event::build_comment(&card, &Pubkey::new(c.author), None, SIDEBAR_THREAD)
                .created_at(oldest),
            &store::Signer::new(&secret, None),
            &mut store::NoPublish,
        );
    }
    wait_for_card_comments(&mut harness, card, 1);
    harness.get_by_label(DETAIL_REVIEW_CARD).click();
    wait_for_label(&mut harness, "All 4 records ›");
    harness.run_steps(3);
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::G);
    harness.run_steps(3);
    harness.snapshot("headway_detail_review_sidebar");
}

/// Behavioural (no lavapipe): the detail's scroll keys move what's drawn, not
/// just the request they leave. On a short window a long comment thread
/// pushes the comment composer below the fold; `G` brings it up into view and
/// `gg` puts it back where it was.
#[test]
fn g_and_gg_scroll_the_detail() {
    const CARD: &str = "Define nostr event model for boards";
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 400.0));
    let card = harness_card_id(&mut harness, CARD);
    apply_demo_action(
        &mut harness,
        store::BoardAction::AddComment {
            card,
            body: SIDEBAR_THREAD.to_string(),
            reply_to: None,
        },
    );
    wait_for_card_comments(&mut harness, card, 1);
    harness.get_by_label(CARD).click();
    wait_for_label(&mut harness, "← Back");
    harness.run_steps(3);

    let composer_top = |harness: &Harness<'static, HeadwayTestState>| {
        harness
            .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
            .last()
            .and_then(|node| node.accesskit_node().bounding_box())
            .map(|bb| bb.y0)
            .expect("the comment composer")
    };
    let window = 400.0;
    let before = composer_top(&harness);
    assert!(
        before > window,
        "the composer starts below the fold: {before}"
    );

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::G);
    harness.run_steps(3);
    let bottom = composer_top(&harness);
    assert!(
        bottom < window,
        "G scrolls the composer into view: {before} -> {bottom}"
    );

    press_board_keys(&mut harness, &[egui::Key::G, egui::Key::G]);
    harness.run_steps(3);
    let top = composer_top(&harness);
    assert!(
        (top - before).abs() < 1.0,
        "gg scrolls back to the top: {before} -> {bottom} -> {top}"
    );
}

/// Behavioural (no lavapipe): Esc in the detail's title editor only leaves
/// the editor, which commits the edit; the detail stays open, and the next
/// Esc backs out to the grid.
#[test]
fn esc_in_the_title_editor_commits_and_keeps_the_detail() {
    const CARD: &str = "Define nostr event model for boards";
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let card = harness_card_id(&mut harness, CARD);
    harness.get_by_label(CARD).click();
    wait_for_label(&mut harness, "← Back");

    harness.get_by_label(CARD).click();
    harness.run_ok();
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .type_text(" v2");
    harness.run_ok();
    press_board_keys(&mut harness, &[egui::Key::Escape]);

    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        harness.run_ok();
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let title = demo_card_title(state.notedeck.app_context().ndb, &author, card);
        if title != CARD {
            assert!(title.contains("v2"), "the typed edit landed: {title:?}");
            break;
        }
        assert!(Instant::now() < deadline, "the title edit never committed");
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        harness.query_by_label("← Back").is_some(),
        "the first Esc kept the detail open"
    );

    press_board_keys(&mut harness, &[egui::Key::Escape]);
    wait_for_label(&mut harness, "7 cards · 5 columns");
}

/// A comment long enough to scroll the detail's main column past its Review
/// section, for [`snapshot_headway_detail_review_sidebar`] and
/// [`g_and_gg_scroll_the_detail`].
const SIDEBAR_THREAD: &str = "Picked this up: the reorder keys go on the column header.

First pass drags fine but loses the order on reload.

Ranks now persist; the column slides into its slot.

The slide overshoots on a narrow window, looking at it.

Fixed the overshoot, and the keys wrap at the ends now.

The header hit area was a bit small on touch.

Grew the hit area to the whole header row.

Ready for another look.";

/// Behavioural (no lavapipe): clicking a card also puts the board's keyboard
/// cursor on it, and the cursor survives the detail closing, so backing out
/// lands with the ring on the card you came from. The cursor is deliberately
/// separate from the open detail, which closing clears.
#[test]
fn clicking_a_card_leaves_the_cursor_on_it_after_back() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    assert_eq!(harness.state().headway.cursor(), None, "no cursor at boot");

    const CARD: &str = "Define nostr event model for boards";
    let card = {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context();
        demo_card_id(app_ctx.ndb, &author, CARD)
    };

    harness.get_by_label(CARD).click();
    wait_for_label(&mut harness, "← Back");
    harness.get_by_label("← Back").click_accesskit();
    wait_for_absent(&mut harness, "← Back");

    assert_eq!(harness.state().headway.cursor(), Some(card));
}

/// Press each of `keys` bare on the board, settling after each, the way
/// someone types them.
fn press_board_keys(harness: &mut Harness<'static, HeadwayTestState>, keys: &[egui::Key]) {
    for &key in keys {
        harness.press_key(key);
        harness.run_ok();
    }
}

/// Type `key` the way a real keyboard delivers it: the key press together with
/// the character it types. The board keymap has to swallow the character when it
/// acts on the key, or a field it focuses would receive it.
fn type_key(harness: &mut Harness<'static, HeadwayTestState>, key: egui::Key, text: &str) {
    harness
        .input_mut()
        .events
        .push(egui::Event::Text(text.to_owned()));
    harness.press_key(key);
    harness.run_ok();
}

/// Drive `keys` then Enter from a freshly booted board, wait for the detail to
/// open, and return the one card the app pushed a global-nav entry for.
fn open_card_by_keys(keys: &[egui::Key]) -> (Harness<'static, HeadwayTestState>, NoteId) {
    use notedeck::NavRequest;
    use notedeck_headway::HeadwayRoute;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    press_board_keys(&mut harness, keys);
    press_board_keys(&mut harness, &[egui::Key::Enter]);
    wait_for_label(&mut harness, "← Back");

    // As in `opening_a_card_pushes_a_global_nav_entry`: nothing drains the
    // Navigator here, so this is every request since boot.
    let requests = harness.state_mut().notedeck.app_context().navigator.take();
    let pushed: Vec<NoteId> = requests
        .iter()
        .filter_map(|req| match req {
            NavRequest::PushToActive(entry) => entry.token.downcast_ref::<HeadwayRoute>(),
            _ => None,
        })
        .filter_map(HeadwayRoute::selected_card)
        .collect();
    assert_eq!(
        pushed.len(),
        1,
        "opening a card by key pushes exactly one global-nav entry"
    );
    (harness, pushed[0])
}

/// The id of the demo card titled `title`, from the harness's own store.
fn harness_card_id(harness: &mut Harness<'static, HeadwayTestState>, title: &str) -> NoteId {
    let state = harness.state_mut();
    let author = state.account.pubkey;
    demo_card_id(state.notedeck.app_context().ndb, &author, title)
}

/// Behavioural (no lavapipe): `j` puts the cursor on the first card of the first
/// column and Enter opens it, through the same single global-nav push a click
/// makes.
#[test]
fn j_then_enter_opens_the_first_card() {
    let (mut harness, opened) = open_card_by_keys(&[egui::Key::J]);
    let first = harness_card_id(&mut harness, "Define nostr event model for boards");
    assert_eq!(opened, first);
}

/// Behavioural (no lavapipe): `j j` walks down Backlog to its second card, and
/// `k` walks back up.
#[test]
fn j_and_k_walk_the_first_column() {
    use egui::Key::{J, K};

    let (mut harness, opened) = open_card_by_keys(&[J, J]);
    let second = harness_card_id(&mut harness, "Sync cards across relays");
    assert_eq!(opened, second, "j j lands on the second card");

    let (mut harness, opened) = open_card_by_keys(&[J, J, K]);
    let first = harness_card_id(&mut harness, "Define nostr event model for boards");
    assert_eq!(opened, first, "j j k lands back on the first");
}

/// The name of the column `card` sits in on the demo board, folded fresh off
/// the db (so it reflects ingested moves, not the rendered frame).
fn demo_card_column(ndb: &Ndb, author: &Pubkey, card: NoteId) -> String {
    let txn = Transaction::new(ndb).expect("txn");
    let reducer = headway::event::fold_board(ndb, &txn, author).expect("demo board folded");
    let boards = reducer.finalize();
    let view = headway::event::find_board(&boards, author, store::BOARD_ID).expect("demo board");
    view.columns
        .iter()
        .find(|c| c.cards.iter().any(|c| c.id == card))
        .unwrap_or_else(|| panic!("card {card:?} is on no column"))
        .name
        .clone()
}

/// How many comments the demo board's `card` has.
fn demo_card_comments(ndb: &Ndb, author: &Pubkey, card: NoteId) -> usize {
    let txn = Transaction::new(ndb).expect("txn");
    let reducer = headway::event::fold_board(ndb, &txn, author).expect("demo board folded");
    let boards = reducer.finalize();
    let view = headway::event::find_board(&boards, author, store::BOARD_ID).expect("demo board");
    view.card(card).expect("the card").comments.len()
}

/// The demo board's `card`, folded fresh off the db.
fn demo_card_title(ndb: &Ndb, author: &Pubkey, card: NoteId) -> String {
    let txn = Transaction::new(ndb).expect("txn");
    let reducer = headway::event::fold_board(ndb, &txn, author).expect("demo board folded");
    let boards = reducer.finalize();
    let view = headway::event::find_board(&boards, author, store::BOARD_ID).expect("demo board");
    view.card(card).expect("the card").title.clone()
}

/// Pump frames until the demo board's `card` has folded in `count` comments,
/// or panic after a deadline. Comments land on the async writer thread.
fn wait_for_card_comments(
    harness: &mut Harness<'static, HeadwayTestState>,
    card: NoteId,
    count: usize,
) {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        harness.run_ok();
        let state = harness.state_mut();
        let author = state.account.pubkey;
        if demo_card_comments(state.notedeck.app_context().ndb, &author, card) >= count {
            return;
        }
        assert!(Instant::now() < deadline, "the comments never folded");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Pump frames until `card` has been ingested into the column named `column`,
/// or panic after a deadline. Card moves land on the async writer thread.
fn wait_for_card_column(
    harness: &mut Harness<'static, HeadwayTestState>,
    card: NoteId,
    column: &str,
) {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        harness.run_ok();
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let now_in = demo_card_column(state.notedeck.app_context().ndb, &author, card);
        if now_in == column {
            // The db has the move; the app's view folds it in on its next
            // pass. Take that pass, so a click aimed off the layout that
            // follows hits the card where it now is, not where it was.
            harness.run_ok();
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the card to reach {column:?}; it's in {now_in:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Behavioural (no lavapipe): Shift+L moves the cursor card into the next
/// column and Shift+H brings it back. The cursor follows the card by id, so the
/// second key acts on it in its new column.
#[test]
fn shift_l_and_shift_h_move_the_cursor_card_across_columns() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let card = harness_card_id(&mut harness, "Define nostr event model for boards");

    press_board_keys(&mut harness, &[egui::Key::J]);
    assert_eq!(harness.state().headway.cursor(), Some(card));

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::L);
    wait_for_card_column(&mut harness, card, "Todo");
    assert_eq!(
        harness.state().headway.cursor(),
        Some(card),
        "cursor follows"
    );

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::H);
    wait_for_card_column(&mut harness, card, "Backlog");
    assert_eq!(
        harness.state().headway.cursor(),
        Some(card),
        "cursor follows"
    );
}

/// Behavioural (no lavapipe): `/` focuses the filter field without typing the
/// slash into it.
#[test]
fn slash_focuses_an_empty_filter() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    type_key(&mut harness, egui::Key::Slash, "/");
    harness.run_ok();

    let field = focused_text_input(&harness);
    assert_eq!(field.value().as_deref(), Some(""), "the / was not typed");
}

/// Behavioural (no lavapipe): `c` opens the add-card composer, focused and
/// empty — the `c` itself isn't typed into it.
#[test]
fn c_opens_an_empty_add_card_composer() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    type_key(&mut harness, egui::Key::C, "c");
    harness.run_ok();

    // The card composer is multiline, so it's the one MultilineTextInput.
    let composer = harness
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .find(|n| n.is_focused())
        .expect("a focused add-card composer");
    assert_eq!(composer.value().as_deref(), Some(""), "the c was not typed");
}

/// Behavioural (no lavapipe): keys typed into the filter field stay text — `j`
/// types a j rather than moving the cursor, and Enter doesn't open a card.
#[test]
fn typing_in_the_filter_never_navigates() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .click();
    harness.run_ok();

    type_key(&mut harness, egui::Key::J, "j");
    assert_eq!(
        focused_text_input(&harness).value().as_deref(),
        Some("j"),
        "j typed into the filter"
    );
    assert_eq!(harness.state().headway.cursor(), None, "j didn't move");

    press_board_keys(&mut harness, &[egui::Key::Enter]);
    for _ in 0..3 {
        harness.run_ok();
    }
    assert!(
        harness.query_by_label("← Back").is_none(),
        "Enter in the filter doesn't open a card"
    );
}

/// Snapshot: the board after `j l`, the cursor ring on Todo's first card.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_board_cursor() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));
    press_board_keys(&mut harness, &[egui::Key::J, egui::Key::L]);
    harness.run_steps(3);
    harness.snapshot("headway_board_cursor");
}

/// `?` pins the which-key strip of board keys under the columns and a second
/// `?` puts it away; a pending `g` shows just its continuation meanwhile.
#[test]
fn question_mark_toggles_the_key_hint_strip() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    assert!(harness.query_by_label("move card").is_none());

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::Questionmark);
    wait_for_label(&mut harness, "move card");

    press_board_keys(&mut harness, &[egui::Key::G]);
    wait_for_label(&mut harness, "first card");
    assert!(harness.query_by_label("move card").is_none());
    press_board_keys(&mut harness, &[egui::Key::G]);
    wait_for_label(&mut harness, "move card");

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::Questionmark);
    wait_for_absent(&mut harness, "move card");
}

/// Snapshot: the board with the which-key strip pinned open under the columns,
/// the cursor on Backlog's first card.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_board_key_hints() {
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));
    press_board_keys(&mut harness, &[egui::Key::J]);
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::Questionmark);
    harness.run_steps(3);
    harness.snapshot("headway_board_key_hints");
}

/// Regression (behavioural, no lavapipe): a `Card` route whose card isn't on the
/// board *yet* must not back out of its own history entry.
///
/// `board_ui` used to drop any selection `find_card` missed, so the post-render
/// nav diff read Card→Board and emitted a `Back` that pops a real global-history
/// entry. On a cross-app deep link — where the chrome pushes the routed entry
/// before Headway has necessarily folded that card in — the card would open and
/// immediately snap back to the board. The selection is now held until the detail
/// has actually rendered it once (`detail_for`), which is what separates a card
/// that *went away* from one that hasn't arrived.
#[test]
fn a_card_that_has_not_folded_in_keeps_its_route_entry() {
    use notedeck::NavRequest;
    use notedeck_headway::HeadwayRoute;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));

    // Nothing drains the Navigator in this chrome-less harness, so clear out
    // whatever boot and the board's first frames enqueued; what we read back
    // below is then only what the unresolved route produced.
    harness.state_mut().notedeck.app_context().navigator.take();

    // A card id on no board here — the shape a deep link takes while its card is
    // still in flight (or, today, a remote card that lands late).
    let absent = NoteId::new([0xab; 32]);
    harness.state_mut().nav_token = Some(std::rc::Rc::new(HeadwayRoute::card(absent, None)));

    // The diff runs once per render pass; a single frame reproduces the bug, and
    // the rest prove the entry isn't backed out of on some later frame either.
    for _ in 0..5 {
        harness.run_ok();
    }

    let backs = harness
        .state_mut()
        .notedeck
        .app_context()
        .navigator
        .take()
        .iter()
        .filter(|req| matches!(req, NavRequest::Back))
        .count();
    assert_eq!(
        backs, 0,
        "a route whose card hasn't folded in must keep its own history entry"
    );
}

/// The over-suppression guard for the test above: holding an unresolved selection
/// must not swallow the *real* dismissal. With a card's detail genuinely open,
/// deleting it still steps the global history back to the board — `resolve_detail_outcome`
/// clears the selection itself in the same frame, so the Card→Board diff still fires.
#[test]
fn deleting_the_open_card_still_backs_out_to_the_board() {
    use notedeck::NavRequest;
    use notedeck_headway::HeadwayRoute;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));

    const CARD: &str = "Define nostr event model for boards";
    let card = {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context();
        demo_card_id(app_ctx.ndb, &author, CARD)
    };

    harness.state_mut().nav_token = Some(std::rc::Rc::new(HeadwayRoute::card(
        card,
        Some(CARD.into()),
    )));
    wait_for_label(&mut harness, "← Back");
    // Drop the push the open itself enqueued, so the only requests left are the
    // delete's.
    harness.state_mut().notedeck.app_context().navigator.take();

    harness.get_by_label("Delete card").click_accesskit();
    // The frame that takes the click. Not `run_ok`: once the delete folds in,
    // the card is gone and the detail asks for Back again every frame until
    // the chrome lands the first one, which this harness never does.
    harness.step();

    let backs = harness
        .state_mut()
        .notedeck
        .app_context()
        .navigator
        .take()
        .iter()
        .filter(|req| matches!(req, NavRequest::Back))
        .count();
    assert_eq!(
        backs, 1,
        "deleting the open card backs the global history out to the board"
    );
}

/// One chrome frame of the global nav loop: draw the stack top through
/// `render_nav` (its token), pump the harness, then drain the app's queued nav
/// requests into the stack the way `Chrome::apply_nav_requests` does —
/// `PushToActive`/`Back` are the only kinds Headway raises. A self-push inherits
/// the active (top) app's slot. A back lands at once, as the chrome's does
/// when its slide ends (the frames in between redraw the outgoing entry).
/// Shared by the `chrome_nav_loop_*` tests.
fn chrome_frame(
    harness: &mut Harness<'static, HeadwayTestState>,
    stack: &mut notedeck::NavStack<notedeck::ChromeNavEntry>,
) {
    use notedeck::NavRequest;

    harness.state_mut().nav_token = Some(stack.top().token.clone());
    harness.run_ok();

    let state = harness.state_mut();
    let app_ctx = state.notedeck.app_context();
    let active = stack.top().app;
    for request in app_ctx.navigator.take() {
        match request {
            NavRequest::PushToActive(entry) => stack.route_to(entry.tag(active)),
            // `go_back` only flags the slide; `pop` is what its end does.
            NavRequest::Back => {
                stack.go_back();
                if stack.returning() {
                    stack.pop();
                }
            }
            _ => panic!("unexpected nav request kind from Headway"),
        }
    }
}

/// A [`behavioral_harness`] that plays the chrome's global nav with its
/// slides (see [`HeadwayTestState::chrome_nav`]), rooted on the app-switch
/// entry the chrome seeds.
fn slide_harness() -> Harness<'static, HeadwayTestState> {
    use notedeck::{AppId, ChromeNavEntry, NavStack};

    let mut state = headway_state();
    state.chrome_nav = Some(ChromeNav {
        stack: NavStack::new(vec![ChromeNavEntry::new(AppId(0), std::rc::Rc::new(()))]),
        slid: 0,
    });
    let mut harness =
        harness_builder(egui::Vec2::new(1200.0, 800.0)).build_ui_state(render_headway, state);
    wait_for_board(&mut harness);
    harness
}

/// The [`slide_harness`]'s global stack.
fn chrome_stack<'h>(
    harness: &'h Harness<'static, HeadwayTestState>,
) -> &'h notedeck::NavStack<notedeck::ChromeNavEntry> {
    &harness
        .state()
        .chrome_nav
        .as_ref()
        .expect("a slide harness")
        .stack
}

/// Pump frames until the [`slide_harness`]'s stack has no slide running and
/// has `len` entries, or panic after a deadline.
fn settle_slides(harness: &mut Harness<'static, HeadwayTestState>, len: usize) {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        harness.run_ok();
        let stack = chrome_stack(harness);
        if !stack.navigating() && !stack.returning() && stack.len() == len {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for a still stack of {len}; it has {}",
            stack.len()
        );
    }
}

/// Behavioural (no lavapipe), under the chrome's slides: `D` on the board
/// queue's last card says "Review queue done" on the grid the back lands on.
/// Before, the back's slide drew the outgoing queue entry in the same pass,
/// which reseeded the queue open, and the notice was taken down as left
/// before it ever showed.
#[test]
fn a_finished_queue_says_so_after_the_back_slide() {
    const CARD: &str = "Inline card creation";
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = slide_harness();
    seed_in_review(&mut harness, repo.path(), &[CARD], &["src/done.rs"]);

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    settle_slides(&mut harness, 2);
    wait_for_label(&mut harness, "1 / 1");

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::D);
    settle_slides(&mut harness, 1);
    wait_for_label(&mut harness, "7 cards · 5 columns");
    assert!(
        harness.query_by_label("Review queue done").is_some(),
        "the grid says the queue is done"
    );
}

/// As [`a_finished_queue_says_so_after_the_back_slide`], for an epic's queue
/// (`R` from the epic's detail): the notice shows on the epic's detail the
/// back lands on.
#[test]
fn a_finished_epic_queue_says_so_after_the_back_slide() {
    const EPIC: &str = "Define nostr event model for boards";
    const SUBISSUE: &str = "Sync cards across relays";
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = slide_harness();
    seed_in_review(&mut harness, repo.path(), &[SUBISSUE], &["src/sync.rs"]);

    harness.get_by_label(EPIC).click();
    settle_slides(&mut harness, 2);
    wait_for_label(&mut harness, "← Back");

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    settle_slides(&mut harness, 3);
    wait_for_label(&mut harness, "1 / 1");

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::D);
    settle_slides(&mut harness, 2);
    wait_for_label(&mut harness, "← Back");
    assert!(
        harness.query_by_label("1 / 1").is_none(),
        "the queue has gone"
    );
    assert!(
        harness.query_by_label("Review queue done").is_some(),
        "the epic's detail says its queue is done"
    );
}

/// As [`a_finished_epic_queue_says_so_after_the_back_slide`], with the epic
/// archived while its queue is open: `D` on the last card closes the queue
/// onto the grid, by way of the epic's detail entry the back lands on first,
/// and the grid still says the queue is done once it settles. Before, that
/// entry's pass drew the grid only after the pass had been checked for the
/// notice's view (the gone epic's selection drops as the pane draws), so the
/// next pass took the notice down.
#[test]
fn a_gone_epics_finished_queue_says_so_on_the_grid() {
    const SUBISSUE: &str = "Sync cards across relays";
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = slide_harness();
    seed_in_review(&mut harness, repo.path(), &[SUBISSUE], &["src/sync.rs"]);
    let epic = harness_card_id(&mut harness, DEMO_EPIC);

    harness.get_by_label(DEMO_EPIC).click();
    settle_slides(&mut harness, 2);
    wait_for_label(&mut harness, "← Back");
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    settle_slides(&mut harness, 3);
    wait_for_label(&mut harness, "1 / 1");

    archive_demo_cards(&mut harness, &[epic]);

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::D);
    settle_slides(&mut harness, 1);
    wait_for_label(&mut harness, "6 cards · 5 columns");
    assert!(
        harness.query_by_label("Review queue done").is_some(),
        "the grid says the queue is done"
    );
}

/// Pump frames until the demo board, folded fresh off the db, passes `done`,
/// or panic after a deadline naming `what`. Board edits land on the async
/// writer thread, so a helper that applies one waits here before returning:
/// a caller then waits on its UI only when that's what it's testing.
fn wait_for_demo(
    harness: &mut Harness<'static, HeadwayTestState>,
    what: &str,
    done: impl Fn(&headway::event::BoardView) -> bool,
) {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        harness.run_ok();
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context();
        let txn = Transaction::new(app_ctx.ndb).expect("txn");
        let boards = headway::event::fold_board(app_ctx.ndb, &txn, &author)
            .expect("folded")
            .finalize();
        let view =
            headway::event::find_board(&boards, &author, store::BOARD_ID).expect("demo board");
        if done(view) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Behavioural (no lavapipe), under the chrome's slides: a notice is about
/// the view it went up in. `s` on a sessionless record in a plain review pane
/// says so there, and it's gone from the detail Esc backs out to, and from the
/// grid the next Esc reaches.
#[test]
fn a_pane_notice_stays_with_its_pane_through_the_slides() {
    const CARD: &str = "Inline card creation";
    const NO_SESSION: &str = "No agentium session on this record";
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = slide_harness();
    seed_in_review(&mut harness, repo.path(), &[CARD], &["src/pane.rs"]);

    harness.get_by_label(CARD).click();
    settle_slides(&mut harness, 2);
    wait_for_label(&mut harness, "± Review diff");
    press_board_keys(&mut harness, &[egui::Key::R]);
    settle_slides(&mut harness, 3);
    wait_for_any_label(&mut harness, "src/pane.rs");

    press_board_keys(&mut harness, &[egui::Key::S]);
    wait_for_label(&mut harness, NO_SESSION);

    press_board_keys(&mut harness, &[egui::Key::Escape]);
    settle_slides(&mut harness, 2);
    wait_for_label(&mut harness, "± Review diff");
    assert!(
        harness.query_by_label(NO_SESSION).is_none(),
        "the pane's notice didn't follow it to the detail"
    );

    press_board_keys(&mut harness, &[egui::Key::Escape]);
    settle_slides(&mut harness, 1);
    wait_for_label(&mut harness, "7 cards · 5 columns");
    assert!(
        harness.query_by_label(NO_SESSION).is_none(),
        "nor on to the grid"
    );
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
    use notedeck::{AppId, ChromeNavEntry, NavStack};
    use notedeck_headway::HeadwayRoute;
    use std::rc::Rc;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));

    // The chrome seeds one entry per app-switch with a `()` token; Headway's home
    // (board root) entry is that. AppId is arbitrary here (single app under test).
    let mut stack: NavStack<ChromeNavEntry> =
        NavStack::new(vec![ChromeNavEntry::new(AppId(0), Rc::new(()))]);

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
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context();
        demo_card_id(app_ctx.ndb, &author, CARD)
    };
    harness.get_by_label(CARD).click();
    chrome_frame(&mut harness, &mut stack);

    // The click pushed a Card entry onto the global stack (so the back chevron is now
    // live), carrying the clicked card's route.
    assert_eq!(stack.len(), 2, "opening a card grows the global stack");
    assert_eq!(
        stack
            .top()
            .token
            .downcast_ref::<HeadwayRoute>()
            .and_then(|r| r.selected_card()),
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
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context();
        demo_card_id(app_ctx.ndb, &author, SUBISSUE)
    };
    // The subissue title shows up in more than one place in the parent detail (the
    // checklist and a dependency list); either row navigates to it via `OpenCard`,
    // so click the first match.
    harness
        .get_all_by_label(SUBISSUE)
        .next()
        .expect("subissue row present in the parent detail")
        .click();
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
            .and_then(|r| r.selected_card()),
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
            .and_then(|r| r.selected_card()),
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

/// Full chrome round-trip for the graph view (behavioural, no lavapipe): the same
/// stack loop as [`chrome_nav_loop_card_open_then_back_returns_to_board`], but
/// exercising the graph leg — opening an epic's card, then its dependency graph
/// from the detail's "View dependency graph" action, pushes a `Graph` entry one
/// level deeper than the card, and a global-back off the graph returns to the
/// epic's detail (not straight to the board). This is the app-side proof that the
/// graph joins the chrome global-nav stack as its own entry.
#[test]
fn chrome_nav_loop_graph_open_then_back_returns_to_epic_detail() {
    use notedeck::{AppId, ChromeNavEntry, NavStack};
    use notedeck_headway::HeadwayRoute;
    use std::rc::Rc;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));

    let mut stack: NavStack<ChromeNavEntry> =
        NavStack::new(vec![ChromeNavEntry::new(AppId(0), Rc::new(()))]);

    // Open the epic's card — it carries sub-issues, so its detail offers the graph.
    const EPIC: &str = "Define nostr event model for boards";
    let epic = {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context();
        demo_card_id(app_ctx.ndb, &author, EPIC)
    };
    chrome_frame(&mut harness, &mut stack);
    harness.get_by_label(EPIC).click();
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 2, "opening the epic card grows the stack");
    // Render the card detail and wait for its graph entry point.
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "☍ View dependency graph");

    // Click the graph entry point. It sets local graph mode, which the app diffs
    // into a pushed `Graph` route — the stack grows to three, one deeper than the
    // card, carrying the epic id.
    harness.get_by_label("☍ View dependency graph").click();
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(
        stack.len(),
        3,
        "opening the graph pushes an entry deeper than the epic card"
    );
    assert_eq!(
        stack
            .top()
            .token
            .downcast_ref::<HeadwayRoute>()
            .and_then(|r| r.graph_epic()),
        Some(epic),
        "the pushed entry is the epic's graph"
    );

    // Render the graph top: the graph view's breadcrumb ("· dependency graph")
    // only exists in the graph top bar, so its presence proves the graph is up.
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "· dependency graph");

    // Browser back from the graph returns to the epic's card (not the board): the
    // popped entry is the `Card` route the graph was entered from.
    stack.go_to_route(stack.len() - 2);
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(
        stack
            .top()
            .token
            .downcast_ref::<HeadwayRoute>()
            .and_then(|r| r.selected_card()),
        Some(epic),
        "back from the graph returns to the epic card"
    );
    // The graph is gone and the epic's detail (with its graph entry point) is back.
    wait_for_absent(&mut harness, "· dependency graph");
    wait_for_label(&mut harness, "☍ View dependency graph");

    // A second back pops to the board root.
    stack.go_to_route(stack.len() - 2);
    assert_eq!(stack.len(), 1, "a second back pops to the board root");
    chrome_frame(&mut harness, &mut stack);
    wait_for_absent(&mut harness, "← Back");
    assert!(
        harness.query_by_label("7 cards · 5 columns").is_some(),
        "back returns to the board grid"
    );
}

/// Full chrome round-trip for the review queue: `R` pushes one entry, stepping
/// through the queue pushes none, a chrome back returns to the board grid, and
/// a forward onto the queue's entry reopens it on the card it was left at.
#[test]
fn chrome_nav_loop_review_queue_is_one_entry() {
    use notedeck::{AppId, ChromeNavEntry, NavStack};
    use notedeck_headway::HeadwayRoute;
    use std::rc::Rc;

    const CARDS: [&str; 2] = ["Inline card creation", "Column reordering"];
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_in_review(
        &mut harness,
        repo.path(),
        &CARDS,
        &["src/queue_one.rs", "src/queue_two.rs"],
    );

    let mut stack: NavStack<ChromeNavEntry> =
        NavStack::new(vec![ChromeNavEntry::new(AppId(0), Rc::new(()))]);
    chrome_frame(&mut harness, &mut stack);

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 2, "opening the queue pushes one entry");
    assert!(
        stack
            .top()
            .token
            .downcast_ref::<HeadwayRoute>()
            .is_some_and(HeadwayRoute::is_review_queue)
    );
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "1 / 2");

    harness.press_key(egui::Key::N);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "2 / 2");
    wait_for_label(&mut harness, CARDS[1]);
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 2, "stepping the queue pushes nothing");

    stack.go_to_route(0);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "7 cards · 5 columns");
    assert!(harness.query_by_label("← Back").is_none());

    assert!(stack.go_forward());
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "2 / 2");
    wait_for_label(&mut harness, CARDS[1]);
    assert_eq!(stack.len(), 2, "reopening the queue pushes nothing");
}

/// Drive [`chrome_frame`] until `done` holds of the stack, or panic naming
/// `what` after a deadline. For a change that lands through async ndb ingest,
/// such as an archive folding in.
fn chrome_frames_until(
    harness: &mut Harness<'static, HeadwayTestState>,
    stack: &mut notedeck::NavStack<notedeck::ChromeNavEntry>,
    what: &str,
    done: impl Fn(&notedeck::NavStack<notedeck::ChromeNavEntry>) -> bool,
) {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        chrome_frame(harness, stack);
        if done(stack) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The Headway route on top of the stack, if it is one.
fn top_route(
    stack: &notedeck::NavStack<notedeck::ChromeNavEntry>,
) -> Option<&notedeck_headway::HeadwayRoute> {
    stack
        .top()
        .token
        .downcast_ref::<notedeck_headway::HeadwayRoute>()
}

/// Whether the stack's top is `card`'s detail (not a pane over it).
fn top_is_detail(stack: &notedeck::NavStack<notedeck::ChromeNavEntry>, card: NoteId) -> bool {
    top_route(stack).is_some_and(|r| {
        r.selected_card() == Some(card)
            && r.review_card().is_none()
            && r.graph_epic().is_none()
            && !r.is_review_queue()
    })
}

/// A chrome global-nav stack at the board root, with the harness drawn
/// through it once.
fn chrome_stack_at_board(
    harness: &mut Harness<'static, HeadwayTestState>,
) -> notedeck::NavStack<notedeck::ChromeNavEntry> {
    use notedeck::{AppId, ChromeNavEntry, NavStack};
    let mut stack = NavStack::new(vec![ChromeNavEntry::new(AppId(0), std::rc::Rc::new(()))]);
    chrome_frame(harness, &mut stack);
    stack
}

/// Put the grid's cursor on the first In Review card by walking the review
/// queue there and leaving it, which leaves the cursor on the card last shown.
fn cursor_on_first_in_review(
    harness: &mut Harness<'static, HeadwayTestState>,
    stack: &mut notedeck::NavStack<notedeck::ChromeNavEntry>,
) {
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    chrome_frame(harness, stack);
    chrome_frame(harness, stack);
    wait_for_label(harness, "1 / 2");
    harness.press_key(egui::Key::Q);
    chrome_frame(harness, stack);
    assert_eq!(stack.len(), 1, "leaving the queue backs out to the board");
}

/// Grid `r` opens the cursor card's review over its detail, which the pane
/// never came from: under the chrome's stack the detail goes in underneath,
/// so the pane's `q` lands on the card's detail, as its strip says, and one
/// more back lands on the board. Before, the pane sat straight on the board
/// and `q` skipped the detail.
#[test]
fn chrome_nav_loop_grid_review_backs_out_to_the_detail() {
    const CARDS: [&str; 2] = ["Inline card creation", "Column reordering"];
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let ids = seed_in_review(&mut harness, repo.path(), &CARDS, &["src/a.rs", "src/b.rs"]);
    let mut stack = chrome_stack_at_board(&mut harness);
    cursor_on_first_in_review(&mut harness, &mut stack);

    harness.press_key(egui::Key::R);
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 3, "the detail, then the review over it");
    assert_eq!(
        top_route(&stack).and_then(|r| r.review_card()),
        Some(ids[0])
    );
    assert!(top_is_detail_below(&stack, ids[0]));
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "local checkout");

    harness.press_key(egui::Key::Q);
    chrome_frame(&mut harness, &mut stack);
    assert!(
        top_is_detail(&stack, ids[0]),
        "q lands on the card's detail"
    );
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "± Review diff");
    assert!(harness.query_by_label("7 cards · 5 columns").is_none());

    harness.press_key(egui::Key::Q);
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 1, "one more back lands on the board");
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "7 cards · 5 columns");
}

/// Whether the entry under the stack's top is `card`'s detail.
fn top_is_detail_below(stack: &notedeck::NavStack<notedeck::ChromeNavEntry>, card: NoteId) -> bool {
    let routes = stack.routes();
    routes.len() >= 2
        && routes[routes.len() - 2]
            .token
            .downcast_ref::<notedeck_headway::HeadwayRoute>()
            .is_some_and(|r| r.selected_card() == Some(card) && r.review_card().is_none())
}

/// A pane's `n` opens the next card's review over that card's detail, so the
/// pane's `q` lands on the next card's detail. Before, `n` pushed the review
/// straight onto the first card's review, and `q` went back to it after a
/// frame of the second card's detail.
#[test]
fn chrome_nav_loop_pane_step_backs_out_to_the_new_cards_detail() {
    const CARDS: [&str; 2] = ["Inline card creation", "Column reordering"];
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let ids = seed_in_review(&mut harness, repo.path(), &CARDS, &["src/a.rs", "src/b.rs"]);
    let mut stack = chrome_stack_at_board(&mut harness);

    harness.get_by_label(CARDS[0]).click();
    chrome_frame(&mut harness, &mut stack);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "± Review diff");
    harness.get_by_label("± Review diff").click_accesskit();
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 3, "the review over the detail it came from");
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "local checkout");

    harness.press_key(egui::Key::N);
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 5, "the next card's detail, then its review");
    assert_eq!(
        top_route(&stack).and_then(|r| r.review_card()),
        Some(ids[1])
    );
    assert!(top_is_detail_below(&stack, ids[1]));
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, CARDS[1]);

    harness.press_key(egui::Key::Q);
    chrome_frame(&mut harness, &mut stack);
    assert!(
        top_is_detail(&stack, ids[1]),
        "q lands on the next card's detail"
    );
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "± Review diff");
    wait_for_label(&mut harness, CARDS[1]);
}

/// A pane's `a` archives its card and backs out to its detail; the detail
/// then leaves for the board once the archive folds in. Opened from the grid
/// with `r`, the detail under the pane never drew the card, and if the
/// archive folds in during the back's slide it never will: that is the case
/// that used to strand the grid under a stale detail entry for good. The
/// test holds the slide open (the chrome keeps drawing the outgoing pane's
/// entry, and ignores backs, until it ends) until the archive has folded.
#[test]
fn chrome_nav_loop_pane_archive_ends_on_the_board() {
    use notedeck::NavRequest;

    const CARDS: [&str; 2] = ["Inline card creation", "Column reordering"];
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_in_review(&mut harness, repo.path(), &CARDS, &["src/a.rs", "src/b.rs"]);
    let mut stack = chrome_stack_at_board(&mut harness);
    cursor_on_first_in_review(&mut harness, &mut stack);

    harness.press_key(egui::Key::R);
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 3);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "local checkout");

    harness.press_key(egui::Key::A);
    // The frame that reads the key. Not `run_ok`: once the archive folds in
    // the pane's card has left the board, and it asks for Back again every
    // frame until the chrome lands the first one, which this harness only
    // does in `chrome_frame`.
    harness.step();
    let requests = harness.state_mut().notedeck.app_context().navigator.take();
    assert!(
        matches!(requests[..], [NavRequest::Back]),
        "a is one back, to the detail"
    );
    // The slide: the pane's entry is still the top, drawn until it ends.
    wait_for_label(&mut harness, "6 cards · 5 columns");
    harness.state_mut().notedeck.app_context().navigator.take();
    stack.go_back();
    stack.pop();
    assert_eq!(stack.len(), 2, "the slide lands on the card's detail");

    chrome_frames_until(&mut harness, &mut stack, "the board root", |s| s.len() == 1);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "6 cards · 5 columns");
    assert!(harness.query_by_label("← Back").is_none());
}

/// The detail's `n` opens the next card as a drill, so its `q` (one back)
/// returns to the card `n` left, as the strip's "back" says, not the grid.
#[test]
fn chrome_nav_loop_detail_step_backs_to_the_previous_card() {
    const CARDS: [&str; 2] = ["Inline card creation", "Column reordering"];
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let ids = seed_in_review(&mut harness, repo.path(), &CARDS, &["src/a.rs", "src/b.rs"]);
    let mut stack = chrome_stack_at_board(&mut harness);

    harness.get_by_label(CARDS[0]).click();
    chrome_frame(&mut harness, &mut stack);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "± Review diff");

    harness.press_key(egui::Key::N);
    chrome_frame(&mut harness, &mut stack);
    assert!(top_is_detail(&stack, ids[1]));
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, CARDS[1]);

    harness.press_key(egui::Key::Q);
    chrome_frame(&mut harness, &mut stack);
    assert!(top_is_detail(&stack, ids[0]), "back to the card n left");
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "← Back");
    wait_for_label(&mut harness, CARDS[0]);
}

/// The demo board's epic: "Sync cards across relays" and "Scaffold the
/// Headway app crate" are its subissues.
const DEMO_EPIC: &str = "Define nostr event model for boards";

/// Make each of the demo cards titled `children` a subissue of [`DEMO_EPIC`],
/// and wait for every relation to fold in. Returns the epic's id.
fn parent_under_demo_epic(
    harness: &mut Harness<'static, HeadwayTestState>,
    children: &[&str],
) -> NoteId {
    let epic = harness_card_id(harness, DEMO_EPIC);
    let cards: Vec<NoteId> = children
        .iter()
        .map(|child| harness_card_id(harness, child))
        .collect();
    for &card in &cards {
        apply_demo_action(
            harness,
            store::BoardAction::SetParent {
                card,
                parent: Some(epic),
            },
        );
    }
    wait_for_demo(harness, "the subissues to fold under the epic", |view| {
        cards
            .iter()
            .all(|&card| view.card(card).is_some_and(|c| c.parent == Some(epic)))
    });
    epic
}

/// Full chrome round-trip for an epic's review queue (behavioural, no
/// lavapipe): `R` on the epic's detail pushes one queue entry that seeds the
/// epic underneath and walks only its In Review descendants ("Inline card
/// creation", also In Review, is outside it, so the queue is `1 / 2`, not
/// `1 / 3`), naming the epic in its header. A chrome back lands on the epic's
/// detail, a forward reopens the same epic's queue where it was left, and `q`
/// leaves it for the epic's detail as one back.
#[test]
fn chrome_nav_loop_epic_review_queue() {
    use notedeck::{AppId, ChromeNavEntry, NavStack};
    use notedeck_headway::HeadwayRoute;
    use std::rc::Rc;

    const CARDS: [&str; 3] = [
        "Inline card creation",
        "Sync cards across relays",
        "Column reordering",
    ];
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_in_review(
        &mut harness,
        repo.path(),
        &CARDS,
        &["src/one.rs", "src/two.rs", "src/three.rs"],
    );
    let epic = parent_under_demo_epic(&mut harness, &[CARDS[2]]);
    let scope_label = format!("in {}", headway::wordid::encode(epic.bytes()));

    let mut stack: NavStack<ChromeNavEntry> =
        NavStack::new(vec![ChromeNavEntry::new(AppId(0), Rc::new(()))]);
    chrome_frame(&mut harness, &mut stack);
    harness.get_by_label(DEMO_EPIC).click();
    chrome_frame(&mut harness, &mut stack);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "Review 2");
    assert_eq!(stack.len(), 2);

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 3, "opening the epic's queue pushes one entry");
    let route = stack.top().token.downcast_ref::<HeadwayRoute>();
    assert!(route.is_some_and(HeadwayRoute::is_review_queue));
    assert_eq!(route.and_then(|r| r.selected_card()), Some(epic));
    assert_eq!(
        route.and_then(|r| r.title()),
        Some(format!("Review queue: {DEMO_EPIC}").as_str())
    );
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "1 / 2");
    wait_for_label(&mut harness, &scope_label);

    harness.press_key(egui::Key::N);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "2 / 2");
    harness.press_key(egui::Key::N);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "2 / 2");
    assert!(
        harness.query_by_label(CARDS[0]).is_none(),
        "the In Review card outside the epic isn't in its queue"
    );
    assert_eq!(stack.len(), 3, "stepping the queue pushes nothing");

    // Back lands on the epic's detail, not the grid.
    stack.go_to_route(1);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "Review 2");
    wait_for_absent(&mut harness, "2 / 2");
    assert!(harness.query_by_label("7 cards · 5 columns").is_none());

    // Forward reopens the epic's queue where it was left.
    assert!(stack.go_forward());
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "2 / 2");
    wait_for_label(&mut harness, &scope_label);
    assert_eq!(stack.len(), 3, "reopening the queue pushes nothing");

    // `q` leaves it for the epic's detail, as one back. (The chrome's back
    // animates, so the stack's top only changes once it lands; drive the
    // frame by hand and read the request instead.)
    harness.press_key(egui::Key::Q);
    harness.run_ok();
    assert_eq!(
        queue_pushes_and_last_back(&mut harness),
        (0, true),
        "q is one back, no new entry"
    );
    assert!(
        top_is_detail_below(&stack, epic),
        "the back lands on the epic's detail"
    );
    stack.go_to_route(1);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "Review 2");
}

/// Archive the demo cards `cards` off the board, and wait for every archive
/// to fold in.
fn archive_demo_cards(harness: &mut Harness<'static, HeadwayTestState>, cards: &[NoteId]) {
    for &card in cards {
        apply_demo_action(harness, store::BoardAction::ArchiveCard { card });
    }
    wait_for_demo(harness, "the archived cards to leave the board", |view| {
        cards.iter().all(|&card| view.card(card).is_none())
    });
}

/// A back onto a queue entry of another scope than the one the queue holds
/// retakes its snapshot from the board (the board's queue after an epic's,
/// and the reverse), and one with nothing left in review backs off onto the
/// grid saying so. Before, the notice went up and was wiped in the same
/// frame, so the entry backed off with no message.
#[test]
fn chrome_nav_loop_queue_back_off_keeps_its_notice() {
    use notedeck::{AppId, ChromeNavEntry};
    use notedeck_headway::HeadwayRoute;
    use std::rc::Rc;

    const CARDS: [&str; 3] = [
        "Inline card creation",
        "Sync cards across relays",
        "Column reordering",
    ];
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let ids = seed_in_review(
        &mut harness,
        repo.path(),
        &CARDS,
        &["src/one.rs", "src/two.rs", "src/three.rs"],
    );
    let epic = parent_under_demo_epic(&mut harness, &[CARDS[2]]);
    let mut stack = chrome_stack_at_board(&mut harness);

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    chrome_frame(&mut harness, &mut stack);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "1 / 3");

    // The epic's detail and its queue over the board's queue, as a history
    // walk leaves them. The demo epic already holds "Sync cards across
    // relays", so its queue walks two.
    for route in [
        HeadwayRoute::card(epic, None),
        HeadwayRoute::review_queue(Some(epic), None),
    ] {
        stack.route_to(ChromeNavEntry::new(AppId(0), Rc::new(route)));
    }
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "1 / 2");

    stack.go_to_route(1);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "1 / 3");
    assert!(stack.go_forward() && stack.go_forward());
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "1 / 2");

    archive_demo_cards(&mut harness, &ids);
    wait_for_label(&mut harness, "This card is no longer on the board.");

    stack.go_to_route(1);
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 1, "the empty queue backs off onto the board");
    // The notice first: it's up for NOTICE_SECS of egui time, which the
    // harness's steps advance, so waiting on anything else first could let
    // it lapse. The archives have folded, so the grid's count is already
    // drawn beside it.
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "Nothing in review");
    assert!(
        harness.query_by_label("4 cards · 5 columns").is_some(),
        "the notice is on the grid"
    );
}

/// An epic archived while its review queue is open: `q` can't land on the
/// epic's detail, so it ends on the board, one back off the queue and one
/// off the epic's detail entry. Before, `q` selected the gone epic, the same
/// frame dropped it and forgot it had shown, and the grid drew under the
/// epic's detail entry for good. The queue is opened with the detail's
/// "Review 1" button, the click `R` stands in for elsewhere.
#[test]
fn chrome_nav_loop_epic_queue_whose_epic_left_ends_on_the_board() {
    const CARDS: [&str; 1] = ["Column reordering"];
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let ids = seed_in_review(&mut harness, repo.path(), &CARDS, &["src/one.rs"]);
    let epic = parent_under_demo_epic(&mut harness, &CARDS);
    let mut stack = chrome_stack_at_board(&mut harness);

    harness.get_by_label(DEMO_EPIC).click();
    chrome_frame(&mut harness, &mut stack);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "Review 1");
    harness.get_by_label("Review 1").click_accesskit();
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 3, "the button opens the epic's queue");
    // The epic's, not the board's: both would read "1 / 1" here.
    assert!(top_route(&stack).is_some_and(|r| r.is_review_queue()));
    assert_eq!(
        top_route(&stack).and_then(|r| r.selected_card()),
        Some(epic),
        "the queue is scoped to the epic"
    );
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "1 / 1");

    // The queue's card goes too, so the pane says it has gone.
    archive_demo_cards(&mut harness, &[epic, ids[0]]);
    wait_for_label(&mut harness, "This card is no longer on the board.");

    harness.press_key(egui::Key::Q);
    chrome_frames_until(&mut harness, &mut stack, "the board root", |s| s.len() == 1);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "5 cards · 5 columns");
    assert!(harness.query_by_label("← Back").is_none());
}

/// As [`chrome_nav_loop_epic_queue_whose_epic_left_ends_on_the_board`], with
/// the epic's queue reached by a history walk, so the epic's detail never
/// drew and nothing but `close_queue`'s archived marker says the epic has
/// left. The queue's card stays, so `q` is what closes it. Without the
/// marker, the epic's detail entry the back lands on holds the selection as
/// a card not folded in yet, and never backs on to the board.
#[test]
fn chrome_nav_loop_gone_epics_queue_backs_off_its_undrawn_detail() {
    use notedeck::{AppId, ChromeNavEntry};
    use notedeck_headway::HeadwayRoute;
    use std::rc::Rc;

    const CARDS: [&str; 1] = ["Column reordering"];
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_in_review(&mut harness, repo.path(), &CARDS, &["src/one.rs"]);
    let epic = parent_under_demo_epic(&mut harness, &CARDS);
    let mut stack = chrome_stack_at_board(&mut harness);

    for route in [
        HeadwayRoute::card(epic, None),
        HeadwayRoute::review_queue(Some(epic), None),
    ] {
        stack.route_to(ChromeNavEntry::new(AppId(0), Rc::new(route)));
    }
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "1 / 1");
    wait_for_label(&mut harness, CARDS[0]);

    archive_demo_cards(&mut harness, &[epic]);

    harness.press_key(egui::Key::Q);
    chrome_frames_until(&mut harness, &mut stack, "the board root", |s| s.len() == 1);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "6 cards · 5 columns");
    assert!(harness.query_by_label("← Back").is_none());
}

/// Behavioural (no lavapipe): `R` on a card's detail with nothing in review
/// under it says so and opens nothing, even with In Review cards elsewhere.
#[test]
fn epic_review_queue_with_nothing_under_the_card_does_not_open() {
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_in_review(
        &mut harness,
        repo.path(),
        &["Inline card creation"],
        &["src/one.rs"],
    );
    harness.get_by_label(DEMO_EPIC).click();
    wait_for_label(&mut harness, "← Back");
    assert!(harness.query_by_label("Review 1").is_none());
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    wait_for_label(&mut harness, "Nothing in review under this card");
    assert_eq!(queue_pushes_and_last_back(&mut harness).0, 0);
}

/// `S` in the review queue leaves for the record's agentium session: the app
/// raises exactly one `AppAction::Open` naming the session, with a
/// `/code-review` message that names the commit and the card, and Headway
/// itself pushes no history entry, so the chrome's switch to Dave is the only
/// one and back returns to the queue. (The chrome's leg — Dave takes the
/// open as one history entry, and back returns — is notedeck_chrome's
/// `open_lands_an_agentium_session_in_dave_and_back_returns`.) The header's
/// "Review in session" icon raises the same open.
#[test]
fn shift_s_in_the_queue_opens_the_session_asking_for_a_review() {
    use notedeck::{AppAction, AppId, ChromeNavEntry, NavStack};
    use std::rc::Rc;

    let fixture = review_fixture();
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_review_queue(&mut harness, &fixture);

    let mut stack: NavStack<ChromeNavEntry> =
        NavStack::new(vec![ChromeNavEntry::new(AppId(0), Rc::new(()))]);
    chrome_frame(&mut harness, &mut stack);
    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    chrome_frame(&mut harness, &mut stack);
    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "1 / 2");
    wait_for_label(&mut harness, QUEUE_SESSION);
    wait_for_label(&mut harness, "Review in session");
    assert_eq!(stack.len(), 2, "the queue is one entry");

    let opens = |harness: &mut Harness<'static, HeadwayTestState>| -> Vec<notedeck::OpenUri> {
        let app_ctx = harness.state_mut().notedeck.app_context();
        app_ctx
            .app_actions
            .take()
            .into_iter()
            .filter_map(|action| match action {
                AppAction::Open(open) => Some(open),
                _ => None,
            })
            .collect()
    };
    // Nothing the harness drains yet (a chip click, say) may count below.
    opens(&mut harness);

    harness.press_key_modifiers(egui::Modifiers::SHIFT, egui::Key::S);
    chrome_frame(&mut harness, &mut stack);
    let raised = opens(&mut harness);
    assert_eq!(raised.len(), 1, "one open: {raised:?}");
    let open = &raised[0];
    assert_eq!(open.reference, QUEUE_SESSION);
    let msg = open.msg.as_deref().expect("S sends a message");
    assert!(msg.starts_with("launch a /code-review"), "{msg}");
    assert!(
        msg.contains(&format!("commit {}", &fixture.queue[..12])),
        "{msg}"
    );
    assert!(msg.contains("card headway:"), "{msg}");

    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 2, "Headway pushes nothing for the open");
    wait_for_label(&mut harness, "1 / 2");

    // The header icon does the same.
    harness.get_by_label("Review in session").click_accesskit();
    chrome_frame(&mut harness, &mut stack);
    let clicked = opens(&mut harness);
    assert_eq!(clicked, raised, "the icon is S");

    // `s` opens the session with no message.
    harness.press_key(egui::Key::S);
    chrome_frame(&mut harness, &mut stack);
    assert_eq!(
        opens(&mut harness),
        vec![notedeck::OpenUri::new(QUEUE_SESSION)]
    );
}

/// Title of the one card [`seed_roadmap_board`] puts on its board. It exists on no
/// other board, so seeing it rendered proves the `roadmap` board is the active one.
const ROADMAP_CARD: &str = "roadmap-only card";

/// Seed a second own board, `roadmap`, carrying a single [`ROADMAP_CARD`], and wait
/// for that card to fold in, returning its id. The demo board stays active: this
/// only writes events, so the tests that use it get a board the app is *not* on.
fn seed_roadmap_board(harness: &mut Harness<'static, HeadwayTestState>) -> NoteId {
    let state = harness.state_mut();
    let author = state.account.pubkey;
    let secret = state.account.secret_key.secret_bytes();
    let app_ctx = &mut state.notedeck.app_context();
    let ndb: &Ndb = app_ctx.ndb;
    store::seed_board(
        ndb,
        &author,
        &secret,
        "roadmap",
        "Roadmap",
        &mut store::NoPublish,
    );
    let view = wait_own_board(ndb, &author, "roadmap");
    store::apply(
        ndb,
        "roadmap",
        &view,
        &author,
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

    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        let card = {
            let txn = Transaction::new(ndb).expect("txn");
            event::load_board(ndb, &txn, &author, "roadmap").and_then(|view| {
                view.columns
                    .iter()
                    .flat_map(|c| c.cards.iter())
                    .find(|c| c.title == ROADMAP_CARD)
                    .map(|c| c.id)
            })
        };
        if let Some(card) = card {
            return card;
        }
        assert!(
            Instant::now() < deadline,
            "{ROADMAP_CARD:?} never folded onto the roadmap board"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Right-click the node labelled `label`, as a mouse does: move onto its
/// centre, then press and release the secondary button there.
fn secondary_click(harness: &mut Harness<'static, HeadwayTestState>, label: &str) {
    let bounds = harness
        .get_by_label(label)
        .accesskit_node()
        .bounding_box()
        .expect("the node's bounds");
    let pos = egui::pos2(
        ((bounds.x0 + bounds.x1) / 2.0) as f32,
        ((bounds.y0 + bounds.y1) / 2.0) as f32,
    );
    let events = &mut harness.input_mut().events;
    events.push(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    harness.run_ok();
}

/// Behavioural (no lavapipe): a card's context-menu "Move to board" lands.
/// The grid raises it as a `BoardEffect::CardMove` that only the app's
/// `drain_effects` acts on, and the keys harness drops it, so this is
/// the one test that drives the whole way: the card leaves the demo board and
/// folds onto the one picked.
#[test]
fn move_to_board_from_the_card_menu_lands() {
    const CARD: &str = "Inline card creation";
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_roadmap_board(&mut harness);
    let card = harness_card_id(&mut harness, CARD);

    secondary_click(&mut harness, CARD);
    harness.get_by_label("Move to board").hover();
    harness.run_ok();
    harness.get_by_label("Roadmap").click_accesskit();
    harness.run_ok();

    wait_for_label(&mut harness, "6 cards · 5 columns");
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        harness.run_ok();
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let app_ctx = state.notedeck.app_context();
        let txn = Transaction::new(app_ctx.ndb).expect("txn");
        let on_roadmap = event::load_board(app_ctx.ndb, &txn, &author, "roadmap")
            .is_some_and(|view| view.card(card).is_some());
        if on_roadmap {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{CARD:?} never folded onto the roadmap board"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The note id of `author`'s own board definition (kind 30619) for `slug` — what
/// an inline board reference in another app carries, so what a cross-app open of
/// the board itself is handed.
fn own_board_note_id(ndb: &Ndb, author: &Pubkey, slug: &str) -> NoteId {
    let txn = Transaction::new(ndb).expect("txn");
    let filter = Filter::new()
        .kinds([event::KIND_BOARD as u64])
        .authors([author.bytes()])
        .build();
    ndb.query(&txn, &[filter], 64)
        .expect("query boards")
        .into_iter()
        .find(|r| matches!(event::parse(&r.note), Some(event::HeadwayEvent::Board(b)) if b.id == slug))
        .map(|r| NoteId::new(*r.note.id()))
        .unwrap_or_else(|| panic!("no board note for {slug:?}"))
}

/// The headline cross-app deep-link invariant (behavioural, no lavapipe): opening a
/// Headway card from *another* app lands exactly **one** global-history entry, so a
/// single back returns to the app the click came from.
///
/// Stands in for the chrome's `AppAction::Note` path: the stack starts on a foreign
/// app's entry (`AppId(1)`, playing Dave), `open_note_route` mints the route, and the
/// token is pushed tagged with Headway's own slot (`AppId(0)`) — the one push the
/// chrome makes. The card lives on a board that is *not* active, so its detail can
/// only render once the open has switched boards, and its title can only come off
/// the issue event: the route is minted before that board's view has folded.
///
/// Before the hook existed the chrome pushed an untyped app-switch entry and the
/// app's own post-render diff pushed the card on top of it — two entries, and the
/// first back landed on the Headway board rather than the source app.
#[test]
fn chrome_nav_loop_cross_app_open_pushes_one_entry() {
    use notedeck::{AppId, ChromeNavEntry, NavStack};
    use notedeck_headway::HeadwayRoute;
    use std::rc::Rc;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let card = seed_roadmap_board(&mut harness);

    // The foreign root: the app the inline reference was clicked in.
    const SOURCE: AppId = AppId(1);
    const HEADWAY: AppId = AppId(0);
    let mut stack: NavStack<ChromeNavEntry> =
        NavStack::new(vec![ChromeNavEntry::new(SOURCE, Rc::new(()))]);

    let token = {
        let state = harness.state_mut();
        let mut app_ctx = state.notedeck.app_context();
        state
            .headway
            .open_note_route(&mut app_ctx, card)
            .expect("a card note mints a route")
    };
    stack.route_to(ChromeNavEntry::new(HEADWAY, token));

    chrome_frame(&mut harness, &mut stack);
    assert_eq!(stack.len(), 2, "a cross-app open is one history entry");
    let top = stack.top();
    assert_eq!(top.app, HEADWAY, "the entry is filed under Headway's slot");
    let route = top
        .token
        .downcast_ref::<HeadwayRoute>()
        .expect("the entry carries a Headway route");
    assert_eq!(route.selected_card(), Some(card), "the route is the card");
    assert_eq!(
        route.title(),
        Some(ROADMAP_CARD),
        "the title comes off the issue event, no folded view needed"
    );

    // The detail comes up once the roadmap board is active and folded. The card is
    // on no other board, so this also proves the open switched boards.
    wait_for_label(&mut harness, "← Back");

    // Keep driving the loop: the pending-open retry and the fold landing must not
    // push a second entry (reconcile sees Card→same Card) or pop this one (the
    // not-yet-folded selection is held, not dropped).
    for _ in 0..5 {
        chrome_frame(&mut harness, &mut stack);
    }
    assert_eq!(
        stack.len(),
        2,
        "no spurious push or pop once the card has opened"
    );
    assert!(harness.query_by_label("← Back").is_some());

    // One back returns to the source app.
    stack.go_to_route(0);
    assert_eq!(stack.len(), 1);
    assert_eq!(
        stack.top().app,
        SOURCE,
        "one back returns to the app the click came from"
    );
}

/// An open *by reference string* — `AppAction::Open` with a
/// `headway:<board>/<word-id>?msg=…` [`notedeck::OpenUri`] — lands exactly where a
/// click on the card's inline chip does: one global-history entry for the card
/// (behavioural, no lavapipe).
///
/// Mirrors the chrome's `AppAction::Open` arm: parse the URI, resolve its reference
/// through the *registered* parsers (`ReferenceParserRegistry::resolve_exact`, which
/// the chrome's `resolve_reference` wraps) relative to the selected account, then
/// open the note the way the click path does (`open_note_route` + one push). The
/// `msg` is carried but not consumed, so it must not change where the open lands.
#[test]
fn open_uri_for_a_card_ref_lands_one_history_entry() {
    use notedeck::{AppId, ChromeNavEntry, NavStack, OpenUri, ReferenceResolveCtx};
    use notedeck_headway::HeadwayRoute;
    use std::rc::Rc;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let card = seed_roadmap_board(&mut harness);

    const SOURCE: AppId = AppId(1);
    const HEADWAY: AppId = AppId(0);
    let mut stack: NavStack<ChromeNavEntry> =
        NavStack::new(vec![ChromeNavEntry::new(SOURCE, Rc::new(()))]);

    let uri = format!(
        "{}?msg=%22review%20this%22",
        headway::wordid::card_ref("roadmap", card.bytes())
    );
    let open = OpenUri::parse(&uri).expect("a card ref parses");
    assert_eq!(open.msg.as_deref(), Some("review this"));

    let token = {
        let state = harness.state_mut();
        let mut app_ctx = state.notedeck.app_context();
        let resolved = {
            let txn = Transaction::new(app_ctx.ndb).expect("txn");
            let resolve_ctx = ReferenceResolveCtx {
                ndb: app_ctx.ndb,
                txn: &txn,
                selected_account: Some(*app_ctx.accounts.selected_account_pubkey()),
            };
            app_ctx
                .registries
                .reference_parsers
                .resolve_exact(&open.reference, &resolve_ctx)
                .expect("the registered headway parser resolves the card ref")
        };
        assert_eq!(
            resolved.note_id, card,
            "the ref resolves to the card's note"
        );
        state
            .headway
            .open_note_route(&mut app_ctx, resolved.note_id)
            .expect("a card note mints a route")
    };
    stack.route_to(ChromeNavEntry::new(HEADWAY, token));

    chrome_frame(&mut harness, &mut stack);
    wait_for_label(&mut harness, "← Back");
    for _ in 0..5 {
        chrome_frame(&mut harness, &mut stack);
    }
    assert_eq!(stack.len(), 2, "an open by URI is one history entry");
    let route = stack
        .top()
        .token
        .downcast_ref::<HeadwayRoute>()
        .expect("the entry carries a Headway route");
    assert_eq!(route.selected_card(), Some(card), "the route is the card");

    // Prose around the reference is not a reference: nothing resolves.
    let state = harness.state_mut();
    let app_ctx = state.notedeck.app_context();
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    let resolve_ctx = ReferenceResolveCtx {
        ndb: app_ctx.ndb,
        txn: &txn,
        selected_account: Some(*app_ctx.accounts.selected_account_pubkey()),
    };
    let prose = format!("see {}", open.reference);
    assert!(
        app_ctx
            .registries
            .reference_parsers
            .resolve_exact(&prose, &resolve_ctx)
            .is_none()
    );
}

/// A cross-app open of a *board* reference mints the board-root route and makes
/// that board the active one — the switch is the whole job, so no card is selected
/// (behavioural, no lavapipe).
#[test]
fn open_note_route_for_a_board_note_returns_the_board_route() {
    use notedeck_headway::HeadwayRoute;

    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    seed_roadmap_board(&mut harness);
    assert!(
        harness.query_by_label(ROADMAP_CARD).is_none(),
        "the demo board is active to start with"
    );

    let token = {
        let state = harness.state_mut();
        let author = state.account.pubkey;
        let mut app_ctx = state.notedeck.app_context();
        let board = own_board_note_id(app_ctx.ndb, &author, "roadmap");
        state
            .headway
            .open_note_route(&mut app_ctx, board)
            .expect("a board note mints a route")
    };
    let route = token
        .downcast_ref::<HeadwayRoute>()
        .expect("the token is a Headway route");
    assert!(
        matches!(route, HeadwayRoute::Board),
        "a board note routes to the board root"
    );
    assert_eq!(route.selected_card(), None, "no card is selected");

    // Draw the minted route: it's the roadmap board's grid, not the demo board's.
    harness.state_mut().nav_token = Some(token);
    wait_for_label(&mut harness, ROADMAP_CARD);
    assert!(
        harness.query_by_label("← Back").is_none(),
        "the board root shows the grid, not a detail"
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

    // Seed a distinct second own board carrying a card only it has, so which board
    // is active after the restart is unambiguous from what renders.
    seed_roadmap_board(&mut harness);

    // Switch to the roadmap board through the real switcher menu.
    harness.get_by_label(SWITCHER_LABEL).click();
    // The entry appears once the roadmap board folds into the switcher list.
    wait_for_label(&mut harness, "Roadmap");
    harness.get_by_label("Roadmap").click();

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

/// A joined shared board whose sealed definition never arrives must not strand the
/// app. Join a co-member's board, deliver no definition for it, switch onto it
/// through the real switcher, and the board still draws its chrome: it says it's
/// loading, but the switcher comes with it, so switching back off works.
///
/// Regression for the full-pane "Loading shared board…" dead-end, which replaced
/// the whole view — switcher included — and so left no way off a board whose fold
/// stayed empty. Seeding a board into a channel the app can't resolve made Headway
/// unusable until the selection was edited out of band.
#[test]
fn unfolded_shared_board_keeps_the_switcher_reachable() {
    let mut harness = behavioral_harness(egui::Vec2::new(1200.0, 800.0));
    let account = test_keypair();
    let alice = fixed_keypair(0xa1);

    // Join alice's `ghost` board — a key-share and nothing else, so the shared fold
    // has no definition to resolve and stays empty for the rest of the test.
    {
        let state = harness.state_mut();
        let app_ctx = &mut state.notedeck.app_context();
        ingest_keyshare(
            app_ctx.ndb,
            &alice,
            &account.pubkey,
            0xa5,
            &event::board_address(&alice.pubkey, "ghost"),
        );
    }

    // Switch onto it through the real switcher. With no definition to title it, the
    // roster lists it (and the switcher button then names it) by slug.
    harness.get_by_label(SWITCHER_LABEL).click();
    wait_for_label(&mut harness, "ghost");
    harness.get_by_label("ghost").click();
    wait_for_label(&mut harness, "Loading shared board…");

    // The escape hatch: the switcher is still on screen, so the demo board is one
    // switch away — the whole point of not dead-ending on a full-pane message.
    harness.get_by_label("ghost  ▾").click();
    wait_for_label(&mut harness, "Headway");
    harness.get_by_label("Headway").click();
    wait_for_board(&mut harness);
}

/// Deliverable 2 (pixel snapshot, needs lavapipe): the switcher keeps two joined
/// boards that share a slug but differ in owner as two distinct entries (breakage
/// #3), and lists a board you own AND shared only once (coordinate dedup). Run via
/// `scripts/snapshot-test`; `sharefile` the PNG for review.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_switcher_same_slug_boards() {
    // TEMPORARY phase markers for headway:notedeck/man-eight-damp. This test is
    // the one the snapshot suite dies in with SIGILL on CI's x86_64 lavapipe,
    // established by serialising the suite (ebef5dcf4f06): 18 tests pass and
    // this one prints its name and never returns. It never reproduces locally
    // (100+ runs on aarch64 lavapipe), so the phase has to be read off CI.
    //
    // Unbuffered via eprintln! — libtest block-buffers stdout into cargo's pipe,
    // so a SIGILL loses whatever is still sitting in it, which is how the crash
    // stayed unattributed for so long. Remove once the phase is known.
    macro_rules! phase {
        ($p:expr) => {
            eprintln!("PHASE switcher: {}", $p)
        };
    }
    phase!("start");
    let mut harness = headway_harness(egui::Vec2::new(1200.0, 800.0));
    phase!("harness built");
    let account = test_keypair();

    // Two co-members whose boards collide on the slug `notes` but not on owner.
    let alice = fixed_keypair(0xa1);
    let bob = fixed_keypair(0xb0);

    {
        let state = harness.state_mut();
        let app_ctx = &mut state.notedeck.app_context();
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
        phase!("own board seeded");
        ingest_keyshare(
            ndb,
            &account,
            &account.pubkey,
            0x30,
            &event::board_address(&account.pubkey, "roadmap"),
        );
        phase!("self-share ingested");

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
        phase!("both joined boards ingested");
    }

    // Open the switcher and wait until every joined board has been picked up and
    // listed: the own+shared `roadmap` once, and both same-slug `notes` boards.
    phase!("switcher clicked");
    harness.get_by_label(SWITCHER_LABEL).click();
    let deadline = Instant::now() + SETTLE_TIMEOUT;
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

    phase!("boards listed");
    harness.run_steps(3);
    phase!("stepped, about to rasterise");
    harness.snapshot("headway_switcher_same_slug");
    phase!("done");
}
