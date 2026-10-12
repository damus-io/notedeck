//! The card a chat draws for a `user` message another session sent
//! (`agentium send` from inside a session): a violet-outlined card headed by
//! "from" and the sender's live `agentium:` chip, which shows the sender's
//! current title rather than its raw ref.
//!
//! Rendered through [`from_session_message_ui`], the function Dave's chat calls
//! for such a row, over a real `Notedeck` with the agentium parser and renderer
//! registered on one shared cache, as `Dave::reference_parsers` /
//! `Dave::kind_renderers` wire them.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use nostrdb::{Ndb, NoteBuilder, Transaction};
use nostrdb_net::{FullKeypair, Keypair};
use notedeck::Notedeck;
use notedeck_dave::reference::AgentiumRefParser;
use notedeck_dave::render::AgentiumSessionRenderer;
use notedeck_dave::session_cache::AgentiumSessionCache;
use notedeck_dave::ui::from_session_message_ui;
use notedeck_dave::UserMessage;

use agentium_core::session_events::AI_SESSION_STATE_KIND;

/// The sending session's id, which its `agentium:` ref derives from.
const SENDER_ID: &str = "claude-session-from-card-sender";
/// The sending session's title, which its chip shows once it resolves.
const SENDER_TITLE: &str = "Report the child's result back";
/// The message the sender sent.
const BODY: &str = "UT7b landed as `8be21d`. The vendor window reads GIVERS rows now.";

struct FromCardState {
    notedeck: Notedeck,
    account: FullKeypair,
    /// The message drawn, its `from_session` the sender's ref.
    message: UserMessage,
    _tmpdir: tempfile::TempDir,
    setup_done: bool,
}

/// Seed the sender's kind-31988 session state, stamped now so its chip draws
/// the status's own colour rather than the faded one of a stale session.
fn seed_sender(ndb: &Ndb, secret: &[u8; 32]) {
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
        .tag_str(SENDER_ID)
        .start_tag()
        .tag_str("title")
        .tag_str(SENDER_TITLE)
        .start_tag()
        .tag_str("status")
        .tag_str("working")
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

fn render_from_card(ui: &mut egui::Ui, state: &mut FromCardState) {
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
        let message = &state.message;
        let from = message.from_session.as_deref().expect("a sent message");
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(16.0);
            from_session_message_ui(message, from, false, &mut app_ctx, ui);
        });
    });
}

/// A harness drawing a message [`SENDER_ID`] sent, with the sender's state
/// seeded. `render` builds it with lavapipe, for a snapshot; without it the
/// harness only lays out and builds its accesskit tree.
fn build_harness(render: bool) -> Harness<'static, FromCardState> {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let mut notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);
    let cache = Rc::new(RefCell::new(AgentiumSessionCache::default()));
    notedeck.register_reference_parser(Box::new(AgentiumRefParser::new(cache.clone())));
    notedeck.register_kind_renderer(Box::new(AgentiumSessionRenderer::new(cache)));

    let state = FromCardState {
        notedeck,
        account: FullKeypair::generate(),
        message: UserMessage {
            from_session: Some(agentium_core::wordid::session_ref(SENDER_ID)),
            ..UserMessage::from(BODY)
        },
        _tmpdir: tmpdir,
        setup_done: false,
    };
    let builder = Harness::builder().with_size(egui::Vec2::new(560.0, 140.0));
    let builder = if render {
        builder.renderer(notedeck::software_renderer())
    } else {
        builder
    };
    let mut harness = builder.build_ui_state(render_from_card, state);
    // The first frame installs fonts and injects the account.
    harness.run_steps(2);
    let secret = harness.state().account.secret_key.secret_bytes();
    seed_sender(harness.state_mut().notedeck.app_context().ndb, &secret);
    harness
}

/// Run frames until the sender's chip shows its title, proving the header's
/// ref resolved rather than falling back to the bare ref.
fn wait_for_sender_chip(harness: &mut Harness<'_, FromCardState>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_ok();
        if harness.query_by_label(SENDER_TITLE).is_some() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the sender's ref never resolved to its chip"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The card heads the message with "from" and the sender's chip, showing the
/// sender's title, above the message itself.
#[test]
fn a_message_from_a_session_is_headed_by_the_senders_chip() {
    let mut harness = build_harness(false);
    wait_for_sender_chip(&mut harness);
    harness.get_by_label("from");
    let header = harness.get_by_label(SENDER_TITLE).rect();
    let body = harness
        .query_all_by_label_contains("GIVERS")
        .next()
        .expect("the message's text")
        .rect();
    assert!(
        header.bottom() <= body.top(),
        "the sender heads the message: header {header:?}, body {body:?}"
    );
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_from_session_card() {
    let mut harness = build_harness(true);
    wait_for_sender_chip(&mut harness);
    harness.run_steps(3);
    harness.snapshot("from_session_card");
}
