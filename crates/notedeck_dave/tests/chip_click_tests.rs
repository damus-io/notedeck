//! A Dave chat message, drawn by the functions the chat calls for one —
//! [`user_message_ui`] and [`assistant_message_ui`] — must keep both its
//! clicks: a primary click on an inline agentium chip reaches the chip, and a
//! right-click anywhere on the message opens its menu, with when it was sent
//! and "Copy".
//!
//! The two pulled against each other once. egui 0.31's `context_menu` gave its
//! response `Sense::click`, so a menu on the user bubble's `Frame` won the
//! hit-test over a chip inside it. egui 0.36's opens only on the response's own
//! secondary click, which a container never senses, so the menu stopped
//! opening at all. The chat now uses a container menu, which opens over any
//! child without sensing clicks itself.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use nostrdb::{Ndb, NoteBuilder, Transaction};
use nostrdb_net::{FullKeypair, Keypair};
use notedeck::Notedeck;
use notedeck_dave::reference::AgentiumRefParser;
use notedeck_dave::render::AgentiumSessionRenderer;
use notedeck_dave::session_cache::AgentiumSessionCache;
use notedeck_dave::ui::{assistant_message_ui, user_message_ui};
use notedeck_dave::{AssistantMessage, UserMessage};

use agentium_core::session_events::AI_SESSION_STATE_KIND;

/// The two kinds of chat message a person sees text in.
#[derive(Clone, Copy, PartialEq)]
enum Surface {
    /// A message the person typed, drawn by [`user_message_ui`].
    User,
    /// A reply, drawn by [`assistant_message_ui`].
    Assistant,
}

/// When every message here was sent: 2023-11-14 22:13:20 UTC, which is in
/// 2023 in every time zone.
const SENT_AT: u64 = 1_700_000_000;

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
    let frame = nostrdb_net::ClientMessage::event(&note)
        .unwrap()
        .to_json()
        .unwrap();
    ndb.process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
        .unwrap();
}

fn render(ui: &mut egui::Ui, state: &mut State) {
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
        let body = state.body.as_str();
        let surface = state.surface;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(16.0);
            match surface {
                Surface::User => {
                    let msg = UserMessage {
                        sent_at: Some(SENT_AT),
                        ..UserMessage::from(body)
                    };
                    user_message_ui(&msg, false, &mut app_ctx, ui);
                }
                Surface::Assistant => {
                    let msg = AssistantMessage::from_text(body.to_owned()).with_sent_at(SENT_AT);
                    assistant_message_ui(&msg, &mut app_ctx, ui);
                }
            }
        });

        // Drain any imperative action the chip click raised.
        let drained = app_ctx.app_actions.take().len();
        if drained > 0 {
            state.actions_seen += drained;
        }
    });
}

/// Click the center of the first node whose label contains `label` with
/// `button`, via a real positional pointer event (a plain `Label` exposes no
/// accesskit action, so `.click()` is a no-op).
fn click_label(harness: &mut Harness<'static, State>, label: &str, button: egui::PointerButton) {
    let bounds = harness
        .get_by_label_contains(label)
        .accesskit_node()
        .raw_bounds()
        .expect("bounds");
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
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    harness.run_ok();
}

/// A harness drawing a `surface` message, its body still empty, past the
/// first frame that installs fonts and the account. `renderer` builds it with
/// lavapipe, for a snapshot; without it the harness only lays out.
fn build_harness(surface: Surface, renderer: bool) -> Harness<'static, State> {
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

    let builder = Harness::builder().with_size(egui::Vec2::new(640.0, 380.0));
    let builder = if renderer {
        builder.renderer(notedeck::software_renderer())
    } else {
        builder
    };
    let mut harness = builder.build_ui_state(render, state);
    harness.run_steps(2);
    harness
}

/// Render `surface` with a resolved agentium chip, click it, and return how many
/// app actions the click raised.
fn actions_after_chip_click(surface: Surface) -> usize {
    let mut harness = build_harness(surface, false);
    let secret = harness.state().account.secret_key.secret_bytes();
    let session_id = "claude-session-q3-planning";
    let title = "Refactor the session parser";
    {
        let app_ctx = harness.state_mut().notedeck.app_context();
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

    click_label(&mut harness, title, egui::PointerButton::Primary);
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

/// Right-click a `surface` message's text, and return the harness with
/// whatever menu that opened (see [`build_harness`] for `renderer`).
fn right_click_message(surface: Surface, renderer: bool) -> Harness<'static, State> {
    let mut harness = build_harness(surface, renderer);
    harness.state_mut().body = "Hello from the chat".to_owned();
    harness.run_steps(2);
    click_label(&mut harness, "Hello from", egui::PointerButton::Secondary);
    harness.run_steps(2);
    harness
}

/// The menu a right-click opens: when the message was sent, then Copy.
fn assert_message_menu(harness: &Harness<'static, State>, surface: &str) {
    assert!(
        harness.query_by_label("Copy").is_some(),
        "right-clicking a {surface} message should open its menu with Copy"
    );
    assert!(
        harness.query_by_label_contains("2023").is_some(),
        "the {surface} message's menu should say when it was sent"
    );
}

#[test]
fn user_message_right_click_opens_copy_menu() {
    assert_message_menu(&right_click_message(Surface::User, false), "user");
}

#[test]
fn assistant_message_right_click_opens_copy_menu() {
    assert_message_menu(&right_click_message(Surface::Assistant, false), "assistant");
}

/// The menu a right-click on a user message opens: when it was sent, then Copy.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_user_message_menu() {
    let mut harness = right_click_message(Surface::User, true);
    harness.snapshot("user_message_menu");
}

/// The same menu on an assistant reply.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_assistant_message_menu() {
    let mut harness = right_click_message(Surface::Assistant, true);
    harness.snapshot("assistant_message_menu");
}
