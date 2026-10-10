//! A chat's turn headers and footers, the session header's walltime, and a
//! picked message range's pill, format nothing per frame.
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
use notedeck_dave::ui::HeaderWalltimeSource;
use notedeck_dave::{
    AiMode, AssistantMessage, ChatSession, DaveUi, Message, SessionId, UserMessage,
};
use notedeck_testing::alloc::{measure, CountingAllocator};
use notedeck_testing::TestHost;

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

/// What drawing the session header's walltime ("· 23m", while a turn runs)
/// adds to a steady-state frame that already draws the header. The label is
/// built once per minute (`WalltimeLabel::refresh`) and handed to egui by
/// `Arc`, so these are egui's, by attribution: the label's accesskit node and
/// text, and the row and scope that keep it beside the truncated subtitle.
/// Through `spacing_mut` the row cost two more (a `Style` clone). Measured,
/// not chosen; if it moves, measure it again and write the new figure down.
const MEASURED_WALLTIME_ALLOCS: u64 = 12;

/// What a picked message range over the last two turns adds to a frame,
/// its wash and its pill. The pill's labels are worked out when the range
/// changes (`ChatRange::refresh`) and handed to egui by `Arc`, so by
/// attribution these are egui's: the pill's frame and row, its three labels
/// (two of them buttons) with their layout jobs and accesskit nodes. None
/// is in `chat_range`. Measured, not chosen; if it moves, measure it again
/// and write the new figure down.
const MEASURED_RANGE_ALLOCS: u64 = 39;

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

/// What a measured frame draws.
#[derive(Clone, Copy)]
struct Draw {
    /// The chat's turn headers and footers.
    times: bool,
    /// The session header.
    header: bool,
    /// The header's walltime.
    walltime: bool,
    /// Let a click pick a message range (a frame with none picked).
    range: bool,
    /// And pick one, from the third row to the last.
    picked: bool,
}

struct ChatState {
    host: TestHost,
    session: ChatSession,
    draw: Draw,
}

fn render(ui: &mut egui::Ui, state: &mut ChatState) {
    notedeck::test_harness::full_window(ui, |ui| {
        // The warm-up frames cover the one that installs the fonts.
        state.host.setup_once(ui.ctx());
        let mut app_ctx = state.host.app_context();
        let session = &mut state.session;
        let walltime = session.walltime();
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
        if state.draw.times {
            dave_ui = dave_ui.chat_times(&mut session.chat_times, session.turns.spans());
        }
        if state.draw.header {
            dave_ui = dave_ui.details(&session.details);
        }
        if state.draw.range {
            dave_ui = dave_ui.chat_range(&mut session.chat_range);
        }
        if state.draw.walltime {
            dave_ui = dave_ui.walltime(HeaderWalltimeSource {
                label: &mut session.walltime_label,
                walltime,
                finished_turns: session.turns.spans().len(),
            });
        }
        dave_ui.ui(&mut app_ctx, ui);
    });
}

/// Allocations on the UI thread for one steady-state frame of [`chat`],
/// drawing what `draw` says, with a fourth turn running since 9:58.
fn frame_allocs(draw: Draw) -> u64 {
    let mut session = ChatSession::new(
        1 as SessionId,
        "/tmp/chat-times".into(),
        AiMode::Agentic,
        notedeck_dave::backend::BackendType::Claude,
    );
    let Chat { rows, turns } = chat();
    session.replace_chat(rows);
    session.set_turns(turns);
    session.turn_started_at = Some(at(13, 9, 58));
    if draw.picked {
        session.chat_range.click(2);
        session.chat_range.extend_to(5, None);
    }
    let state = ChatState {
        host: TestHost::new(),
        session,
        draw,
    };
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(760.0, 900.0))
        .build_ui_state(render, state);
    for _ in 0..WARM_UP {
        harness.step();
    }
    let counts = measure(|| harness.step()).counts;
    println!(
        "times {} header {} walltime {} range {} picked {}: {counts}",
        draw.times, draw.header, draw.walltime, draw.range, draw.picked
    );
    counts.thread.allocs
}

/// The headers and footers add exactly what egui charges for their labels.
#[test]
fn chat_times_format_nothing_per_frame() {
    let draw = Draw {
        times: false,
        header: false,
        walltime: false,
        range: false,
        picked: false,
    };
    let plain = frame_allocs(draw);
    let timed = frame_allocs(Draw {
        times: true,
        ..draw
    });
    let added = timed.saturating_sub(plain);
    assert_eq!(
        added, MEASURED_TIMES_ALLOCS,
        "drawing the turn headers and footers costs {added} allocations a frame \
         ({timed} timed, {plain} plain), measured at {MEASURED_TIMES_ALLOCS}: \
         a frame must not format or reseed them"
    );
}

/// The walltime adds exactly what egui charges for its label, though the
/// running turn moves it every second.
#[test]
fn the_header_walltime_formats_nothing_per_frame() {
    let header = Draw {
        times: false,
        header: true,
        walltime: false,
        range: false,
        picked: false,
    };
    let plain = frame_allocs(header);
    let with = frame_allocs(Draw {
        walltime: true,
        ..header
    });
    let added = with.saturating_sub(plain);
    assert_eq!(
        added, MEASURED_WALLTIME_ALLOCS,
        "drawing the header's walltime costs {added} allocations a frame \
         ({with} with, {plain} without), measured at {MEASURED_WALLTIME_ALLOCS}: \
         a frame must not format it"
    );
}

/// Letting a click pick a range costs a frame nothing while none is picked,
/// and a picked one adds exactly what egui charges for its wash and pill.
#[test]
fn a_message_range_formats_nothing_per_frame() {
    let draw = Draw {
        times: true,
        header: false,
        walltime: false,
        range: false,
        picked: false,
    };
    let plain = frame_allocs(draw);
    let idle = frame_allocs(Draw {
        range: true,
        ..draw
    });
    assert_eq!(
        idle, plain,
        "a chat with no range picked must cost what one that can't pick does"
    );
    let picked = frame_allocs(Draw {
        range: true,
        picked: true,
        ..draw
    });
    let added = picked.saturating_sub(plain);
    assert_eq!(
        added, MEASURED_RANGE_ALLOCS,
        "drawing a picked range costs {added} allocations a frame \
         ({picked} picked, {plain} plain), measured at {MEASURED_RANGE_ALLOCS}: \
         a frame must not format its pill"
    );
}
