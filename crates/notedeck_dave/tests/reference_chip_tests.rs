//! The cross-app demo for the agentium inline-reference epic: an
//! `agentium:<word-id>` reference resolves to a live session chip — a status dot
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

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use enostr::{FullKeypair, Keypair, Pubkey};
use nostrdb::{Ndb, NoteBuilder, Transaction};
use notedeck::Notedeck;
use notedeck_dave::reference::AgentiumRefParser;
use notedeck_dave::render::AgentiumSessionRenderer;
use notedeck_dave::session_cache::AgentiumSessionCache;
use notedeck_ui::markdown::render_markdown_with_refs;

use agentium_core::session_events::AI_SESSION_STATE_KIND;

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
fn seed_session(ndb: &Ndb, secret: &[u8; 32], session_id: &str, title: &str, status: &str) {
    let note = NoteBuilder::new()
        .kind(AI_SESSION_STATE_KIND)
        .content("")
        .created_at(1_700_000_000)
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
    let frame = enostr::ClientMessage::event(&note)
        .unwrap()
        .to_json()
        .unwrap();
    ndb.process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
        .unwrap();
}

fn render_ref_chip(ctx: &egui::Context, state: &mut RefChipState) {
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
        state.setup_done = true;
        return;
    }

    let mut app_ctx = state.notedeck.app_context(ctx);
    let body = state.body.clone();
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
        .build_state(render_ref_chip, state);
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
    let ctx = harness.ctx.clone();
    let session_id = "claude-session-q3-planning";
    let title = "Refactor the session parser";
    {
        let app_ctx = harness.state_mut().notedeck.app_context(&ctx);
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

/// The word-id in a resolved `agentium:` ref round-trips through the parser's
/// resolution the same way `session_ref` renders it — a fast, renderer-free guard
/// that the seed + scheme wiring line up (the snapshot above needs lavapipe, so
/// this keeps a headless check in the default suite).
#[test]
fn seeded_session_resolves_through_the_shared_cache() {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ndb = Ndb::new(tmpdir.path().to_str().unwrap(), &nostrdb::Config::new()).unwrap();
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
