//! The cross-app demo for the agentium inline-reference epic: an
//! `agentium:<word-id>` reference resolves to a live session chip — a status bar
//! plus the session's *current* title, folded from the shared session cache —
//! rendered inline in both a plain note and a Dave-style chat message.
//!
//! Both surfaces render through `render_markdown_with_refs`, the exact path
//! `NoteOptions::InlineReferences` drives in notes and Dave chat, so the snapshot
//! exercises the real parser + renderer registry rather than calling the renderer
//! directly. The parser + renderer are registered over one shared
//! `Rc<RefCell<AgentiumSessionCache>>` — the same wiring `Dave::reference_parsers`
//! / `Dave::kind_renderers` do — and the cache seeds + folds off the seeded
//! kind-31988 session-state event as the surfaces render.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use nostrdb::{Ndb, NoteBuilder, Transaction};
use nostrdb_net::{FullKeypair, Keypair, Pubkey};
use notedeck::Notedeck;
use notedeck_dave::reference::AgentiumRefParser;
use notedeck_dave::render::AgentiumSessionRenderer;
use notedeck_dave::session_cache::AgentiumSessionCache;
use notedeck_ui::markdown::render_markdown_with_refs;

use agentium_core::session_events::AI_SESSION_STATE_KIND;
use notedeck_testing::fixtures::test_config;

struct RefChipState {
    notedeck: Notedeck,
    account: FullKeypair,
    /// The inline body (holding an `agentium:<word-id>`) rendered on both surfaces.
    body: String,
    _tmpdir: tempfile::TempDir,
    setup_done: bool,
}

/// Seed a kind-31988 session-state event authored by `author` and ingest it
/// locally, returning nothing — the caller derives the word-id from the session id.
///
/// Stamped with the current time, so the session is fresh when the chip draws
/// it and its status bar shows the status's own color rather than the faded
/// one a stale session gets (which a chip measures against the wall clock).
fn seed_session(ndb: &Ndb, secret: &[u8; 32], session_id: &str, title: &str, status: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_secs();
    let note = NoteBuilder::new()
        .kind(AI_SESSION_STATE_KIND)
        .content("")
        .created_at(now)
        .start_tag()
        .tag_str("d")
        .tag_str(session_id)
        .start_tag()
        .tag_str("title")
        .tag_str(title)
        .start_tag()
        .tag_str("status")
        .tag_str(status)
        .start_tag()
        .tag_str("cwd")
        .tag_str("~/dev/notedeck")
        .sign(secret)
        .build()
        .unwrap();
    let frame = nostrdb_net::ClientMessage::event(&note)
        .unwrap()
        .to_json()
        .unwrap();
    ndb.process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
        .unwrap();
}

fn render_ref_chip(ui: &mut egui::Ui, state: &mut RefChipState) {
    notedeck::test_harness::full_window(ui, |ui| {
        let ctx = &ui.ctx().clone();
        if !state.setup_done {
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
            state.setup_done = true;
            return;
        }

        let mut app_ctx = state.notedeck.app_context();
        let body = state.body.clone();
        egui::CentralPanel::default().show(ui, |ui| {
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
                        render_markdown_with_refs(ui, &mut note_ctx, &txn, &body);
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
                        render_markdown_with_refs(ui, &mut note_ctx, &txn, &body);
                    });
            });
        });
    });
}

fn build_harness() -> Harness<'static, RefChipState> {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let mut notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    // Register the agentium parser + session renderer over one shared cache —
    // exactly what `Dave::reference_parsers` / `Dave::kind_renderers` do at startup
    // (chrome registers both into the host). Sharing the Rc means the chip resolves
    // and folds off the same session state.
    let cache = Rc::new(RefCell::new(AgentiumSessionCache::default()));
    notedeck.register_reference_parser(Box::new(AgentiumRefParser::new(cache.clone())));
    notedeck.register_kind_renderer(Box::new(AgentiumSessionRenderer::new(cache.clone())));

    let state = RefChipState {
        notedeck,
        account: FullKeypair::generate(),
        body: String::new(),
        _tmpdir: tmpdir,
        setup_done: false,
    };

    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(640.0, 380.0))
        .renderer(notedeck::software_renderer())
        .build_ui_state(render_ref_chip, state);
    // First frame installs fonts + injects the account.
    harness.run_steps(2);
    harness
}

/// Seed a session, reference it by `agentium:<word-id>` inline in a note and a
/// Dave-style message, and snapshot both surfaces — the cross-app demo for the
/// agentium inline-reference epic. Each surface renders through
/// [`render_markdown_with_refs`], the same path `NoteOptions::InlineReferences`
/// drives, and the chip resolves + folds its title from the shared session cache.
/// Waiting on the session's *title* (not the raw ref text) asserts the reference
/// actually resolved before the snapshot is taken.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_agentium_reference_chip() {
    let mut harness = build_harness();

    let secret = harness.state().account.secret_key.secret_bytes();
    let session_id = "claude-session-q3-planning";
    let title = "Refactor the session parser";
    {
        let app_ctx = harness.state_mut().notedeck.app_context();
        seed_session(app_ctx.ndb, &secret, session_id, title, "working");
    }
    let session_ref = agentium_core::wordid::session_ref(session_id);
    harness.state_mut().body = format!("Kicked off {session_ref} — watching it run.");

    // Wait until the chip renders the resolved session title (not the raw ref) in
    // *both* surfaces, proving the parser + renderer folded it from the shared
    // cache. The title is shown twice — once per surface.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if harness.query_all_by_label(title).count() >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the session reference never resolved to its title chip in both surfaces"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    harness.run_steps(3);
    harness.snapshot("agentium_reference_chip");
}

/// An `agentium:` ref inside a GitHub-flavored markdown **table cell** resolves to
/// the same live session chip it renders in a paragraph — the regression guard for
/// headway:notedeck/trend-jacket-young, where `render_table` drew every cell span
/// as raw text and bypassed the reference-aware inline seam. The body is a
/// two-column table whose data cell holds the ref; the resolved session *title*
/// (not the raw word-id) must appear on both surfaces (note + Dave message),
/// proving the cell now flows through the ref seam.
///
/// Asserts on the accesskit tree rather than a pixel snapshot, so it needs no
/// committed baseline png and stays deterministic across mesa versions — but the
/// egui_kittest harness still needs a lavapipe adapter to build, so it runs in the
/// `snapshot-test` job (via `scripts/snapshot-test`), not the default suite. Before
/// the fix the cell rendered the raw `agentium:<word-id>` text and this title query
/// found nothing.
#[test]
#[ignore] // needs lavapipe to build the harness — run via scripts/snapshot-test
fn agentium_reference_chip_renders_in_table_cell() {
    let mut harness = build_harness();

    let secret = harness.state().account.secret_key.secret_bytes();
    let session_id = "claude-session-table-cell";
    let title = "Thread RefCtx into tables";
    {
        let app_ctx = harness.state_mut().notedeck.app_context();
        seed_session(app_ctx.ndb, &secret, session_id, title, "working");
    }
    let session_ref = agentium_core::wordid::session_ref(session_id);
    harness.state_mut().body =
        format!("| Session | Status |\n| --- | --- |\n| {session_ref} | running |\n");

    // Wait until the chip renders the resolved title inside the cell on both
    // surfaces, proving the cell went through the ref-aware seam (raw text would
    // show the word-id, not the title).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if harness.query_all_by_label(title).count() >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the session reference never resolved to its title chip inside the table cell"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A *tombstoned* session still resolves to a chip — drawn with a "resume"
/// triangle after its status bar — so a soft-deleted `agentium:` ref reads as click-to-resume rather than vanishing. The chip folds
/// its title from the note snapshot (the projection drops tombstones), proving the
/// deleted ref resolved before the snapshot.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_agentium_deleted_chip() {
    let mut harness = build_harness();

    let secret = harness.state().account.secret_key.secret_bytes();
    let session_id = "claude-session-deleted-demo";
    let title = "Resume the parser refactor";
    {
        let app_ctx = harness.state_mut().notedeck.app_context();
        seed_session(app_ctx.ndb, &secret, session_id, title, "deleted");
    }
    let session_ref = agentium_core::wordid::session_ref(session_id);
    harness.state_mut().body = format!("Closed {session_ref} — click to resume.");

    // Wait until the deleted session's chip renders its title in both surfaces.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if harness.query_all_by_label(title).count() >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the deleted session reference never resolved to its title chip"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    harness.run_steps(3);
    harness.snapshot("agentium_deleted_chip");
}

/// The word-id in a resolved `agentium:` ref round-trips through the parser's
/// resolution the same way `session_ref` renders it — a fast, renderer-free guard
/// that the seed + scheme wiring line up (the snapshot above needs lavapipe, so
/// this keeps a headless check in the default suite).
#[test]
fn seeded_session_resolves_through_the_shared_cache() {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ndb = Ndb::new(tmpdir.path().to_str().unwrap(), &test_config()).unwrap();
    let kp = FullKeypair::generate();
    let session_id = "claude-session-headless";

    let sub = ndb
        .subscribe(&[notedeck_dave::session_cache::session_state_filter(
            &kp.pubkey,
        )])
        .unwrap();
    seed_session(
        &ndb,
        &kp.secret_key.secret_bytes(),
        session_id,
        "Headless",
        "idle",
    );

    // Wait for the async ingest to commit on a side subscription.
    let deadline = Instant::now() + Duration::from_secs(5);
    while ndb.poll_for_notes(sub, 64).is_empty() {
        assert!(Instant::now() < deadline, "seed never committed");
        std::thread::sleep(Duration::from_millis(10));
    }

    let cache = Rc::new(RefCell::new(AgentiumSessionCache::default()));
    let parser = AgentiumRefParser::new(cache.clone());
    let words = agentium_core::wordid::session_ref(session_id);
    let author: Pubkey = kp.pubkey;

    // In the app the parser shares the cache Dave's `update` already drives, so it
    // reads an already-seeded one. Here it owns a fresh cache, so drive the two
    // advances that seed it — the first opens the subscription, the next folds the
    // history from a snapshot taken after it (see `notedeck::RealtimeCache`).
    for _ in 0..2 {
        let txn = Transaction::new(&ndb).unwrap();
        cache.borrow_mut().poll(&ndb, &txn, &kp.pubkey);
    }

    let txn = Transaction::new(&ndb).unwrap();
    let ctx = notedeck::ReferenceResolveCtx {
        ndb: &ndb,
        txn: &txn,
        selected_account: Some(author),
    };
    use notedeck::ReferenceParser;
    assert!(
        parser.resolve(&words, &ctx).is_some(),
        "a seeded session must resolve through the shared cache"
    );
}

/// Stands in for headway's `headway:` reference parser, which Dave's tests
/// don't load: each ref it knows resolves to its seeded kind-1621 issue note,
/// drawn by [`StubCardRenderer`].
struct StubCardParser {
    cards: Rc<RefCell<Vec<(String, nostrdb_net::NoteId)>>>,
}

impl notedeck::ReferenceParser for StubCardParser {
    fn id(&self) -> &'static str {
        "headway"
    }

    fn find(&self, text: &str) -> Option<std::ops::Range<usize>> {
        let start = text.find("headway:")?;
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
        self.cards
            .borrow()
            .iter()
            .find(|(card, _)| card == matched)
            .map(|(_, note)| notedeck::ResolvedRef::note(*note))
    }
}

/// Stands in for headway's card chip: the issue's content as an
/// [`inline_chip`](notedeck_ui::inline_chip) behind a ring, the way
/// `card_chip_ui` draws a backlog card.
struct StubCardRenderer;

impl notedeck::KindRenderer for StubCardRenderer {
    fn id(&self) -> &'static str {
        "test.card"
    }

    fn name(&self) -> &'static str {
        "Test card"
    }

    fn kinds(&self) -> &'static [u32] {
        &[1621]
    }

    fn render(
        &self,
        ui: &mut egui::Ui,
        _note_context: &mut notedeck::NoteContext,
        req: &notedeck::KindRenderRequest,
    ) -> notedeck::KindRenderResponse {
        let theme = notedeck::ColorTheme::current(ui.ctx());
        let response =
            notedeck_ui::inline_chip(ui, &theme, req.note.content(), req.chrome, |ui, size| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
                ui.painter().circle_stroke(
                    rect.center(),
                    size * 0.35,
                    egui::Stroke::new(1.5, theme.text_muted),
                );
            });
        notedeck::open_on_click(ui, response, req)
    }
}

/// Seed a kind-1621 card note titled `title`, returning its id.
fn seed_card(ndb: &Ndb, secret: &[u8; 32], title: &str) -> nostrdb_net::NoteId {
    let note = NoteBuilder::new()
        .kind(1621)
        .content(title)
        .created_at(1_700_000_000)
        .sign(secret)
        .build()
        .unwrap();
    let id = nostrdb_net::NoteId::new(*note.id());
    let frame = nostrdb_net::ClientMessage::event(&note)
        .unwrap()
        .to_json()
        .unwrap();
    ndb.process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
        .unwrap();
    id
}

/// A card a session is titled with: its ref, and the title its chip shows.
struct TitleCard {
    card_ref: &'static str,
    title: &'static str,
}

/// A card titled shorter than its ref.
const SHORT_CARD: TitleCard = TitleCard {
    card_ref: "headway:dave/fringe-cup-pigeon",
    title: "Fix it",
};

/// A card titled longer than its ref.
const LONG_CARD: TitleCard = TitleCard {
    card_ref: "headway:dave/able-badge-brick",
    title: "Session list title chip steals the row click from the session",
};

/// A harness with the agentium parser and renderer plus the stub card ones
/// registered, and the cards the stub parser resolves, for a test to seed.
struct RefTitleHarness {
    harness: Harness<'static, RefChipState>,
    cards: Rc<RefCell<Vec<(String, nostrdb_net::NoteId)>>>,
}

/// Build a [`RefTitleHarness`]. `render` builds it with lavapipe, for a
/// snapshot; without it the harness lays out and builds its accesskit tree,
/// which is all a layout check reads.
fn ref_title_harness(render: bool) -> RefTitleHarness {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let mut notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);
    let cache = Rc::new(RefCell::new(AgentiumSessionCache::default()));
    let cards = Rc::new(RefCell::new(Vec::new()));
    notedeck.register_reference_parser(Box::new(AgentiumRefParser::new(cache.clone())));
    notedeck.register_reference_parser(Box::new(StubCardParser {
        cards: cards.clone(),
    }));
    notedeck.register_kind_renderer(Box::new(AgentiumSessionRenderer::new(cache)));
    notedeck.register_kind_renderer(Box::new(StubCardRenderer));

    let state = RefChipState {
        notedeck,
        account: FullKeypair::generate(),
        body: String::new(),
        _tmpdir: tmpdir,
        setup_done: false,
    };
    let builder = Harness::builder().with_size(egui::Vec2::new(640.0, 380.0));
    let builder = if render {
        builder.renderer(notedeck::software_renderer())
    } else {
        builder
    };
    let mut harness = builder.build_ui_state(render_ref_chip, state);
    harness.run_steps(2);
    RefTitleHarness { harness, cards }
}

/// The harness for a session titled with a card's ref: two sessions titled
/// [`SHORT_CARD`] and [`LONG_CARD`], and a body that puts each session's chip
/// near the end of a line — the short one with room to spare, the long one
/// without.
fn build_title_card_harness(render: bool) -> Harness<'static, RefChipState> {
    let RefTitleHarness { mut harness, cards } = ref_title_harness(render);
    let secret = harness.state().account.secret_key.secret_bytes();
    {
        let app_ctx = harness.state_mut().notedeck.app_context();
        for (session, card) in [
            ("claude-session-short", SHORT_CARD),
            ("claude-session-long", LONG_CARD),
        ] {
            let note = seed_card(app_ctx.ndb, &secret, card.title);
            cards.borrow_mut().push((card.card_ref.to_string(), note));
            seed_session(app_ctx.ndb, &secret, session, card.card_ref, "working");
        }
    }
    let short = agentium_core::wordid::session_ref("claude-session-short");
    let long = agentium_core::wordid::session_ref("claude-session-long");
    harness.state_mut().body = format!(
        "This filler runs most of the way across the line, then {short}\n\n\
         Kicked off a couple of runs; next {long}"
    );

    // Both chips show their card's title on both surfaces once the sessions
    // fold and their refs resolve.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        let resolved = harness.query_all_by_label(SHORT_CARD.title).count()
            + harness.query_all_by_label(LONG_CARD.title).count();
        if resolved >= 4 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the sessions' card-ref titles never resolved to card chips"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // Let the chips settle on their remembered widths.
    harness.run_steps(3);
    harness
}

/// The bounds of the first accesskit node labelled `label`.
fn bounds(harness: &Harness<'_, RefChipState>, label: &str) -> egui::Rect {
    let b = harness
        .get_all_by_label(label)
        .next()
        .expect("a labelled node")
        .accesskit_node()
        .raw_bounds()
        .expect("bounds");
    egui::Rect::from_min_max(
        egui::pos2(b.x0 as f32, b.y0 as f32),
        egui::pos2(b.x1 as f32, b.y1 as f32),
    )
}

/// The bounds of the first accesskit node whose label contains `text`.
fn bounds_containing(harness: &Harness<'_, RefChipState>, text: &str) -> egui::Rect {
    let b = harness
        .get_all_by_label_contains(text)
        .next()
        .expect("a node containing the text")
        .accesskit_node()
        .raw_bounds()
        .expect("bounds");
    egui::Rect::from_min_max(
        egui::pos2(b.x0 as f32, b.y0 as f32),
        egui::pos2(b.x1 as f32, b.y1 as f32),
    )
}

/// A session titled with a card's ref draws the card's chip in its own, and
/// the chip breaks the row on the width of what it drew, not of the raw ref:
/// the short card's chip stays on the line it fits on, and the long card's
/// moves to a row of its own instead of being cut short at the line's end.
///
/// Headless (no lavapipe): it reads the layout off the accesskit tree.
#[test]
fn a_card_ref_session_title_draws_the_cards_chip_and_breaks_on_its_width() {
    let harness = build_title_card_harness(false);

    assert!(
        harness.query_by_label(SHORT_CARD.card_ref).is_none()
            && harness.query_by_label(LONG_CARD.card_ref).is_none(),
        "a resolved card-ref title shows the card's chip, not the raw ref"
    );

    let filler = bounds_containing(&harness, "This filler runs");
    let short = bounds(&harness, SHORT_CARD.title);
    assert!(
        (short.center().y - filler.center().y).abs() < 4.0,
        "the short card's chip fits after the filler, so it stays on its line: \
         filler {filler:?}, chip {short:?}"
    );

    let lead = bounds_containing(&harness, "Kicked off");
    let long = bounds(&harness, LONG_CARD.title);
    assert!(
        long.min.y > lead.max.y - 1.0,
        "the long card's chip doesn't fit after the lead-in, so it breaks to \
         the next row: lead-in {lead:?}, chip {long:?}"
    );
}

/// Snapshot of [`a_card_ref_session_title_draws_the_cards_chip_and_breaks_on_its_width`]'s
/// layout: each session chip draws its card's chip, pill-less, inside its own.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_agentium_chip_with_a_card_ref_title() {
    let mut harness = build_title_card_harness(true);
    harness.snapshot("agentium_chip_card_ref_title");
}

/// A session titled with its own `agentium:` ref draws its chip inside its
/// chip once, then the inner one's title as text, rather than nesting until
/// the stack runs out. (Two sessions titled with each other's refs take the
/// same path.)
#[test]
fn a_session_titled_with_its_own_ref_nests_once() {
    let RefTitleHarness { mut harness, .. } = ref_title_harness(false);
    let secret = harness.state().account.secret_key.secret_bytes();
    let session = "claude-session-ouroboros";
    let session_ref = agentium_core::wordid::session_ref(session);
    {
        let app_ctx = harness.state_mut().notedeck.app_context();
        seed_session(app_ctx.ndb, &secret, session, &session_ref, "working");
    }
    harness.state_mut().body = format!("Watching {session_ref} chase itself.");

    // The inner chip's title, drawn as text, is the raw ref: once per surface.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if harness.query_all_by_label(&session_ref).count() >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the self-titled session never drew its nested chip"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
