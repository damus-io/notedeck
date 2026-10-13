//! A chat's turn headers and footers format nothing per frame.
//!
//! Their text is worked out when the chat's rows, turns, day or locale change
//! (`ChatTimes::refresh`) and handed to egui by `Arc`, so a steady-state frame
//! pays only what egui charges to lay out and draw the labels themselves. This
//! draws one chat through the real `DaveUi` with and without its times, over a
//! real `Notedeck`, and holds the difference to a measured figure: a `format!`
//! or a reseed slipped into the per-frame path costs at least one more
//! allocation per header or footer, which the figure has no room for.

use agentium_core::session_loader::TurnSpan;
use egui_kittest::Harness;
use notedeck::Notedeck;
use notedeck_dave::{
    AiMode, AssistantMessage, ChatSession, DaveUi, Message, SessionId, UserMessage,
};
use notedeck_testing::alloc::{measure, CountingAllocator};

/// Process-wide and one per binary, so it lives in this test rather than in
/// `notedeck_testing`.
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Frames run before measuring, so fonts, galleys, ids and the times are all
/// warm.
const WARM_UP: usize = 30;

/// What drawing [`chat`]'s three headers and three footers adds to a
/// steady-state frame, measured on the UI thread. Attributed with
/// `notedeck_testing::alloc::attribute` when it was written down, every one is
/// egui's: mostly the accesskit text nodes the harness builds for each label
/// (about two thirds), then `Label` cloning its layout job, and the child `Ui`
/// that centres a header. None is in `chat_times`. Measured, not chosen; if it
/// moves, measure it again and write the new figure down.
const MEASURED_TIMES_ALLOCS: u64 = 87;

/// Unix seconds for 2026-10-`day` `h`:`m` UTC.
fn at(day: u32, h: u32, m: u32) -> u64 {
    chrono::NaiveDate::from_ymd_opt(2026, 10, day)
        .unwrap()
        .and_hms_opt(h, m, 0)
        .unwrap()
        .and_utc()
        .timestamp() as u64
}

/// "Now" for the measured frames: under an hour after the newest turn, so its
/// footer reads relative, the case that moves with the clock.
fn now() -> u64 {
    at(13, 10, 1)
}

/// A chat and the turns folded onto it.
struct Chat {
    rows: Vec<Message>,
    turns: Vec<TurnSpan>,
}

/// Three turns over two days, each opening on a header (a new day, or an
/// hour's quiet) and closing on a footer.
fn chat() -> Chat {
    let user = |t| {
        Message::User(UserMessage {
            sent_at: Some(t),
            ..UserMessage::from("go")
        })
    };
    let reply =
        |t| Message::Assistant(AssistantMessage::from_text("done".to_owned()).with_sent_at(t));
    let span = |last_row, start, end| TurnSpan {
        last_row,
        start,
        end,
    };
    let rows = vec![
        user(at(12, 16, 12)),
        reply(at(12, 16, 20)),
        user(at(13, 8, 0)),
        reply(at(13, 8, 30)),
        user(at(13, 9, 41)),
        reply(at(13, 9, 53)),
    ];
    let turns = vec![
        span(1, at(12, 16, 12), at(12, 16, 20)),
        span(3, at(13, 8, 0), at(13, 8, 30)),
        span(5, at(13, 9, 41), at(13, 9, 53)),
    ];
    Chat { rows, turns }
}

struct ChatState {
    notedeck: Notedeck,
    session: ChatSession,
    /// Draw the headers and footers.
    timed: bool,
    set_up: bool,
    _tmpdir: tempfile::TempDir,
}

fn render(ui: &mut egui::Ui, state: &mut ChatState) {
    notedeck::test_harness::full_window(ui, |ui| {
        if !state.set_up {
            state.notedeck.setup(ui.ctx());
            state.set_up = true;
        }
        let mut app_ctx = state.notedeck.app_context();
        let session = &mut state.session;
        let mut dave_ui = DaveUi::new(
            false,
            session.id,
            &session.chat,
            &mut session.input,
            &mut session.focus_requested,
            &mut session.block_nav,
            AiMode::Agentic,
        )
        .now_unix(now())
        .time_zone(chrono::FixedOffset::east_opt(0).unwrap());
        if state.timed {
            dave_ui = dave_ui.chat_times(&mut session.chat_times, session.turns.spans());
        }
        dave_ui.ui(&mut app_ctx, ui);
    });
}

/// Allocations on the UI thread for one steady-state frame of [`chat`], with
/// its headers and footers drawn if `timed`.
fn frame_allocs(timed: bool) -> u64 {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&egui::Context::default(), tmpdir.path(), &args);
    let mut session = ChatSession::new(
        1 as SessionId,
        "/tmp/chat-times".into(),
        AiMode::Agentic,
        notedeck_dave::backend::BackendType::Claude,
    );
    let Chat { rows, turns } = chat();
    session.replace_chat(rows);
    session.set_turns(turns);
    let state = ChatState {
        notedeck,
        session,
        timed,
        set_up: false,
        _tmpdir: tmpdir,
    };
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(760.0, 900.0))
        .build_ui_state(render, state);
    for _ in 0..WARM_UP {
        harness.step();
    }
    let counts = measure(|| harness.step()).counts;
    println!("timed {timed}: {counts}");
    counts.thread.allocs
}

/// The headers and footers add exactly what egui charges for their labels.
#[test]
fn chat_times_format_nothing_per_frame() {
    let plain = frame_allocs(false);
    let timed = frame_allocs(true);
    let added = timed.saturating_sub(plain);
    assert_eq!(
        added, MEASURED_TIMES_ALLOCS,
        "drawing the turn headers and footers costs {added} allocations a frame \
         ({timed} timed, {plain} plain), measured at {MEASURED_TIMES_ALLOCS}: \
         a frame must not format or reseed them"
    );
}
