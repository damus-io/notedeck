//! Regression: an agentium session chip must be *clickable* in a Dave chat
//! bubble on both surfaces — a user message and an assistant message.
//!
//! `Dave::user_chat` draws the user bubble as a filled `egui::Frame` with a
//! right-click "Copy" `context_menu`; `assistant_chat` uses a bare `ui.scope`.
//! `context_menu` forces `Sense::click` onto its response every frame, so if the
//! menu is attached to the *Frame* (whose rect fully encloses an inline chip)
//! egui's hit-test tie-break awards a chip click to the bubble, silently
//! swallowing it. Attaching the menu to an inner `scope` — whose response doesn't
//! enclose the chip the same way — keeps the chip clickable, matching the
//! assistant surface. This test clicks the chip on both surfaces and asserts an
//! app action is raised.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use enostr::{FullKeypair, Keypair};
use nostrdb::{Ndb, NoteBuilder, Transaction};
use notedeck::Notedeck;
use notedeck_dave::reference::AgentiumRefParser;
use notedeck_dave::render::AgentiumSessionRenderer;
use notedeck_dave::session_cache::AgentiumSessionCache;
use notedeck_ui::markdown::render_markdown_with_refs;

use agentium_core::session_events::AI_SESSION_STATE_KIND;

/// The two Dave chat bubble structures the chip is drawn in.
#[derive(Clone, Copy, PartialEq)]
enum Surface {
    /// Mirrors `Dave::user_chat`: a filled `Frame` bubble in a `right_to_left`
    /// layout, with the "Copy" `context_menu` attached to an inner `scope`.
    User,
    /// Mirrors `Dave::assistant_chat`: a bare `ui.scope` with the same menu.
    Assistant,
}

struct State {
    notedeck: Notedeck,
    account: FullKeypair,
    body: String,
    surface: Surface,
    /// App actions drained this frame (a chip click ⇒ ≥1).
    actions_seen: usize,
    _tmpdir: tempfile::TempDir,
    setup_done: bool,
}

/// Seed and ingest a kind-31988 session-state event authored with `secret`.
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

/// Attach the shared "Copy" context menu to `response` (as both Dave bubbles do).
fn copy_menu(response: &egui::Response) {
    notedeck_ui::context_menu::context_menu(response, |ui| {
        if ui.button("Copy").clicked() {
            ui.close_menu();
        }
    });
}

fn render(ctx: &egui::Context, state: &mut State) {
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
    let surface = state.surface;
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.add_space(16.0);
        match surface {
            Surface::User => {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    let content = egui::Frame::new()
                        .inner_margin(10.0)
                        .corner_radius(10.0)
                        .fill(ui.visuals().widgets.inactive.weak_bg_fill)
                        .show(ui, |ui| {
                            ui.scope(|ui| {
                                let mut note_ctx = app_ctx.note_context();
                                let txn = Transaction::new(note_ctx.ndb).expect("txn");
                                render_markdown_with_refs(ui, &mut note_ctx, &txn, &body);
                            })
                            .response
                        })
                        .inner;
                    copy_menu(&content);
                });
            }
            Surface::Assistant => {
                let r = ui.scope(|ui| {
                    let mut note_ctx = app_ctx.note_context();
                    let txn = Transaction::new(note_ctx.ndb).expect("txn");
                    render_markdown_with_refs(ui, &mut note_ctx, &txn, &body);
                });
                copy_menu(&r.response);
            }
        }
    });

    // Drain any imperative action the chip click raised.
    let drained = app_ctx.app_actions.take().len();
    if drained > 0 {
        state.actions_seen += drained;
    }
}

/// Primary-click the center of a labelled node via a real positional pointer
/// event (a plain `Label` exposes no accesskit action, so `.click()` is a no-op).
fn click_label(harness: &mut Harness<'static, State>, label: &str) {
    let bounds = harness.get_by_label(label).raw_bounds().expect("bounds");
    let center = egui::pos2(
        ((bounds.x0 + bounds.x1) / 2.0) as f32,
        ((bounds.y0 + bounds.y1) / 2.0) as f32,
    );
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(center));
    for pressed in [true, false] {
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    harness.run_ok();
}

/// Render `surface` with a resolved agentium chip, click it, and return how many
/// app actions the click raised.
fn actions_after_chip_click(surface: Surface) -> usize {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let mut notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    // Register the parser + renderer over one shared cache, as Dave does at startup.
    let cache = Rc::new(RefCell::new(AgentiumSessionCache::default()));
    notedeck.register_reference_parser(Box::new(AgentiumRefParser::new(cache.clone())));
    notedeck.register_kind_renderer(Box::new(AgentiumSessionRenderer::new(cache.clone())));

    let state = State {
        notedeck,
        account: FullKeypair::generate(),
        body: String::new(),
        surface,
        actions_seen: 0,
        _tmpdir: tmpdir,
        setup_done: false,
    };

    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(640.0, 380.0))
        .build_state(render, state);
    harness.run_steps(2);

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

    // Wait until the chip resolves to the session title, proving the ref folded.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if harness.query_all_by_label(title).count() >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "chip never resolved to its title"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    harness.run_steps(3);

    click_label(&mut harness, title);
    harness.run_steps(2);
    harness.state().actions_seen
}

#[test]
fn assistant_surface_chip_click_raises_action() {
    assert!(
        actions_after_chip_click(Surface::Assistant) >= 1,
        "clicking a chip in an assistant bubble should raise an app action"
    );
}

#[test]
fn user_surface_chip_click_raises_action() {
    assert!(
        actions_after_chip_click(Surface::User) >= 1,
        "clicking a chip in a user bubble should raise an app action \
         (regression: the Copy context_menu must not swallow chip clicks)"
    );
}
