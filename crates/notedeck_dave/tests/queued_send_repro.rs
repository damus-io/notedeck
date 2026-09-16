//! Real-backend reproduction for **queued message sending**.
//!
//! "Queued sending" is dave's behavior when the user sends another message while
//! the assistant is still mid-turn: the message is appended to `chat` but NOT
//! dispatched, and `needs_redispatch_after_stream_end()` re-dispatches it once
//! the current turn finishes (see `session.rs` / `lib.rs::handle_stream_end`).
//!
//! The state machine is heavily unit-tested with a mock, but the user reports it
//! "doesn't really work" against a real Claude backend. These tests exercise the
//! REAL `ClaudeBackend` (persistent stream) through the exact redispatch flow so
//! we observe genuine subprocess behavior, not a mock.
//!
//! Run with:
//!   cargo test -p notedeck_dave --test queued_send_repro -- --ignored --nocapture

use agentium_core::messages::PermissionResponse;
use claude_agent_sdk_rs::{get_claude_code_version, PermissionMode};
use notedeck::Waker;
use notedeck_dave::backend::{AiBackend, BackendType, ClaudeBackend};
use notedeck_dave::config::AiMode;
use notedeck_dave::session::ChatSession;
use notedeck_dave::{AssistantMessage, DaveApiResponse, Message, UserMessage};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A fast, cheap model so the repro is quick and low-cost.
const MODEL: &str = "claude-haiku-4-5-20251001";

fn cli_available() -> bool {
    get_claude_code_version().is_some()
}

fn user(text: &str) -> Message {
    Message::User(UserMessage::new(text.to_string(), vec![]))
}

/// Drain the session receiver until this turn's `QueryComplete` (the
/// persistent-stream turn boundary), collecting streamed token text.
///
/// Returns `(collected_text, saw_query_complete)`. `saw_query_complete == false`
/// means the turn never completed within `timeout` — i.e. the backend never
/// answered.
fn drain_turn(rx: &mpsc::Receiver<DaveApiResponse>, timeout: Duration) -> (String, bool) {
    let start = Instant::now();
    let mut text = String::new();
    loop {
        if start.elapsed() > timeout {
            return (text, false);
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(DaveApiResponse::Token(t)) => text.push_str(&t),
            Ok(DaveApiResponse::QueryComplete(_)) => return (text, true),
            Ok(DaveApiResponse::Failed(e)) => {
                eprintln!("[drain] backend Failed: {e}");
                return (text, false);
            }
            Ok(other) => {
                eprintln!("[drain] other: {:?}", std::mem::discriminant(&other));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("[drain] channel disconnected");
                return (text, false);
            }
        }
    }
}

/// Faithfully reproduce dave's queued-send redispatch against a real Claude
/// backend:
///   1. dispatch turn 1 (a fresh session) and drain to completion
///   2. append the finalized assistant + a *queued* second user message
///   3. redispatch on the SAME `session_id` (persistent stream → no new rx)
///   4. drain the same receiver and assert the queued message got answered
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Claude Code CLI to be installed and authenticated"]
async fn queued_message_gets_answered_after_turn_ends() {
    assert!(
        cli_available(),
        "claude CLI required; this test is #[ignore]d for that reason"
    );

    let backend = ClaudeBackend::new();
    let session_id = "dave-session-queued-repro".to_string();
    let tools = Arc::new(HashMap::new());
    let waker = Waker::noop();

    // ---- Turn 1: first message in a new session ----
    let mut chat = vec![user(
        "Reply with exactly the single word FIRST and nothing else.",
    )];

    let (rx, _h1) = backend.stream_request(
        chat.clone(),
        tools.clone(),
        Some(MODEL.to_string()),
        "user".to_string(),
        session_id.clone(),
        None,
        None,
        None,
        PermissionMode::Default,
        waker.clone(),
    );
    let rx = rx.expect("first turn on a persistent backend mints the session receiver");

    let (text1, done1) = drain_turn(&rx, Duration::from_secs(90));
    eprintln!("turn 1: done={done1} text={text1:?}");
    assert!(done1, "turn 1 must reach QueryComplete");
    assert!(
        text1.to_uppercase().contains("FIRST"),
        "turn 1 should answer FIRST, got {text1:?}"
    );

    // ---- Queue a second message, mirroring handle_stream_end + handle_user_send ----
    // dave finalizes the assistant into chat, then the user's queued message is
    // appended after it.
    chat.push(Message::Assistant(AssistantMessage::from_text(text1)));
    chat.push(user(
        "Now reply with exactly the single word SECOND and nothing else.",
    ));

    // ---- Turn 2: redispatch on the SAME session (what needs_redispatch does) ----
    let (rx2, _h2) = backend.stream_request(
        chat.clone(),
        tools.clone(),
        Some(MODEL.to_string()),
        "user".to_string(),
        session_id.clone(),
        None,
        None,
        None,
        PermissionMode::Default,
        waker.clone(),
    );
    assert!(
        rx2.is_none(),
        "persistent stream: a subsequent turn must reuse the existing channel"
    );

    // Keep draining the ORIGINAL receiver — that's what dave keeps installed.
    let (text2, done2) = drain_turn(&rx, Duration::from_secs(90));
    eprintln!("turn 2: done={done2} text={text2:?}");

    backend.cleanup_session(session_id);

    assert!(
        done2,
        "turn 2 (the redispatched queued message) must reach QueryComplete — \
         if this hangs, queued sending is broken at the backend"
    );
    assert!(
        text2.to_uppercase().contains("SECOND"),
        "queued message should be answered with SECOND, got {text2:?}"
    );
}

/// Feed one `DaveApiResponse` into a real [`ChatSession`] exactly as
/// `Dave::process_events` does, so the chat is built the same way the app builds
/// it. Auto-allows permission prompts. Returns `true` at the turn boundary.
fn apply_response(session: &mut ChatSession, res: DaveApiResponse) -> bool {
    match res {
        DaveApiResponse::Token(t) => session.append_token(&t),
        DaveApiResponse::ToolRunning(r) => session.push_running_tool(r),
        DaveApiResponse::ToolResult(result) => {
            if let Some(result) = session.fold_tool_result(result) {
                session.place_tool_result(result);
            }
        }
        DaveApiResponse::TodoUpdate(todos) => {
            session.insert_turn_content(Message::TodoUpdate(todos));
        }
        DaveApiResponse::PermissionRequest(pending) => {
            let _ = pending
                .response_tx
                .send(PermissionResponse::Allow { message: None });
        }
        DaveApiResponse::Failed(e) => panic!("backend Failed: {e}"),
        DaveApiResponse::QueryComplete(_) => return true,
        _ => {}
    }
    false
}

/// End-to-end against a real Claude backend, exercising the ACTUAL bug: a
/// message queued during a *tool-using* turn. Turn 1 is driven through a real
/// `ChatSession` the same way `process_events` builds chat; a follow-up is
/// queued the moment the first tool starts. Before the fix the trailing tool
/// rows buried the queued message (`chat.last()` became a tool result) and it
/// was silently dropped; after the fix it stays trailing and redispatches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Claude Code CLI to be installed and authenticated"]
async fn queued_message_survives_a_real_tool_using_turn() {
    assert!(
        cli_available(),
        "claude CLI required; this test is #[ignore]d for that reason"
    );

    let backend = ClaudeBackend::new();
    let cwd = std::env::current_dir().unwrap();
    let mut session = ChatSession::new(1, cwd.clone(), AiMode::Agentic, BackendType::Claude);
    let session_id = "dave-session-tool-queue-repro".to_string();
    let tools = Arc::new(HashMap::new());
    let waker = Waker::noop();

    // Turn 1: strongly require a tool call.
    session.chat.push(user(
        "Use the Bash tool to run exactly `echo hello-first`, then tell me the output. \
         You MUST actually run the command with the Bash tool.",
    ));
    session.mark_dispatched();

    let (rx, _h1) = backend.stream_request(
        session.chat.clone(),
        tools.clone(),
        Some(MODEL.to_string()),
        "user".to_string(),
        session_id.clone(),
        None,
        Some(cwd.clone()),
        None,
        PermissionMode::Default,
        waker.clone(),
    );
    let rx = rx.expect("first turn mints the session receiver");

    // Drive turn 1, queuing a follow-up the instant the first tool starts.
    let mut saw_tool = false;
    let mut queued = false;
    let start = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "turn 1 timed out"
        );
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(res) => {
                if matches!(res, DaveApiResponse::ToolRunning(_)) && !queued {
                    saw_tool = true;
                    session
                        .chat
                        .push(user("Also use Bash to run `echo hello-second`."));
                    queued = true;
                }
                if apply_response(&mut session, res) {
                    break; // QueryComplete
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("turn 1 channel closed early"),
        }
    }
    session.finalize_last_assistant();
    session.finalize_running_tools();

    assert!(
        saw_tool,
        "turn 1 was expected to use a tool (that's the scenario under test)"
    );

    // THE FIX: the queued message must have survived the tool-using turn. Before
    // the fix `chat.last()` was a tool result, so this was false and the message
    // was dropped.
    assert!(
        session.needs_redispatch_after_stream_end(),
        "queued message was buried by tool activity; chat tail = {:?}",
        session.chat.last().map(std::mem::discriminant)
    );

    // Redispatch it (what handle_stream_end does) and confirm the real backend
    // answers the queued message.
    session.mark_dispatched();
    let (rx2, _h2) = backend.stream_request(
        session.chat.clone(),
        tools.clone(),
        Some(MODEL.to_string()),
        "user".to_string(),
        session_id.clone(),
        None,
        Some(cwd.clone()),
        None,
        PermissionMode::Default,
        waker.clone(),
    );
    assert!(rx2.is_none(), "persistent stream reuses the channel");

    let mut turn2_text = String::new();
    let start = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "turn 2 timed out"
        );
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(DaveApiResponse::Token(t)) => {
                turn2_text.push_str(&t);
                session.append_token(&t);
            }
            Ok(res) => {
                if apply_response(&mut session, res) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("turn 2 channel closed early"),
        }
    }

    backend.cleanup_session(session_id);

    eprintln!("turn 2 text: {turn2_text:?}");
    assert!(
        turn2_text.to_lowercase().contains("hello-second"),
        "the redispatched queued message should have been answered (mentioning \
         hello-second), got {turn2_text:?}"
    );
}
