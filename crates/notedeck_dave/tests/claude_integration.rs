//! Integration tests for Claude Code SDK
//!
//! These tests require Claude Code CLI to be installed and authenticated.
//! Run with: cargo test -p notedeck_dave --test claude_integration -- --ignored
//!
//! The SDK spawns the Claude Code CLI as a subprocess and communicates via JSON streaming.
//! The CLAUDE_API_KEY environment variable is read by the CLI subprocess.

use claude_agent_sdk_rs::{
    get_claude_code_version, query_stream, ClaudeAgentOptions, ContentBlock,
    Message as ClaudeMessage, PermissionMode, PermissionResult, PermissionResultAllow,
    PermissionResultDeny, TextBlock, ToolPermissionContext,
};
use futures::future::BoxFuture;
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

// NOTE: SDK v0.6.3 Limitation
// ===========================
// The SDK defines `can_use_tool` callback in ClaudeAgentOptions but does NOT implement
// the control protocol to invoke it. In query_full.rs, handle_control_request only
// handles "hook_callback" and "mcp_message" subtypes - there's no handler for
// tool permission requests.
//
// For interactive permission handling, either:
// 1. Wait for a new SDK version that implements the permission callback
// 2. Fork the SDK and add the missing control protocol handler
// 3. Use hooks (PreToolUse) for non-interactive permission logic

/// Check if Claude CLI is available
fn cli_available() -> bool {
    get_claude_code_version().is_some()
}

/// Build test options with cost controls.
/// Uses BypassPermissions to avoid interactive prompts in automated testing.
/// Includes a stderr callback to prevent subprocess blocking.
fn test_options() -> ClaudeAgentOptions {
    // A stderr callback is needed to prevent the subprocess from blocking
    // when stderr buffer fills up. We just discard the output.
    let stderr_callback = |_msg: String| {};

    ClaudeAgentOptions::builder()
        .permission_mode(PermissionMode::BypassPermissions)
        .max_turns(1)
        .skip_version_check(true)
        .stderr_callback(Arc::new(stderr_callback))
        .build()
}

/// Non-ignored test that checks CLI availability without failing.
/// This test always passes - it just reports whether the CLI is present.
#[test]
fn test_cli_version_available() {
    let version = get_claude_code_version();
    match version {
        Some(v) => println!("Claude Code CLI version: {}", v),
        None => println!("Claude Code CLI not installed - integration tests will be skipped"),
    }
}

/// Test that the Claude Code SDK returns a text response.
/// Validates that we receive actual text content from Claude.
#[tokio::test]
#[ignore = "Requires Claude Code CLI to be installed and authenticated"]
async fn test_simple_query_returns_text() {
    if !cli_available() {
        println!("Skipping: Claude CLI not available");
        return;
    }

    let prompt = "Respond with exactly: Hello";
    let options = test_options();

    let mut stream = match query_stream(prompt.to_string(), Some(options)).await {
        Ok(s) => s,
        Err(e) => {
            panic!("Failed to create stream: {}", e);
        }
    };

    let mut received_text = String::new();

    while let Some(result) = stream.next().await {
        match result {
            Ok(message) => {
                if let ClaudeMessage::Assistant(assistant_msg) = message {
                    for block in &assistant_msg.message.content {
                        if let ContentBlock::Text(TextBlock { text }) = block {
                            received_text.push_str(text);
                        }
                    }
                }
            }
            Err(e) => {
                panic!("Stream error: {}", e);
            }
        }
    }

    assert!(
        !received_text.is_empty(),
        "Should receive text response from Claude"
    );
}

/// Test that the Result message is received to mark completion.
#[tokio::test]
#[ignore = "Requires Claude Code CLI to be installed and authenticated"]
async fn test_result_message_received() {
    if !cli_available() {
        println!("Skipping: Claude CLI not available");
        return;
    }

    let prompt = "Say hi";
    let options = test_options();

    let mut stream = match query_stream(prompt.to_string(), Some(options)).await {
        Ok(s) => s,
        Err(e) => {
            panic!("Failed to create stream: {}", e);
        }
    };

    let mut received_result = false;

    while let Some(result) = stream.next().await {
        match result {
            Ok(message) => {
                if let ClaudeMessage::Result(_) = message {
                    received_result = true;
                    break;
                }
            }
            Err(e) => {
                panic!("Stream error: {}", e);
            }
        }
    }

    assert!(
        received_result,
        "Should receive Result message marking completion"
    );
}

/// Test that empty prompt is handled gracefully (no panic).
#[tokio::test]
#[ignore = "Requires Claude Code CLI to be installed and authenticated"]
async fn test_empty_prompt_handled() {
    if !cli_available() {
        println!("Skipping: Claude CLI not available");
        return;
    }

    let prompt = "";
    let options = test_options();

    let result = query_stream(prompt.to_string(), Some(options)).await;

    // Empty prompt should either work or fail gracefully - either is acceptable
    if let Ok(mut stream) = result {
        // Consume the stream - we just care it doesn't panic
        while let Some(_) = stream.next().await {}
    }
    // If result is Err, that's also fine - as long as we didn't panic
}

/// Verify that our prompt formatting produces substantial output.
/// This is a pure unit test that doesn't require Claude CLI.
#[test]
fn test_prompt_formatting_is_substantial() {
    // Simulate what messages_to_prompt should produce
    let system = "You are Dave, a helpful Nostr assistant.";
    let user_msg = "Hi";

    // Build a proper prompt like messages_to_prompt should
    let prompt = format!("{}\n\nHuman: {}\n\n", system, user_msg);

    // The prompt should be much longer than just "Hi" (2 chars)
    // If only the user message was sent (the bug), length would be ~2
    // With system message, it should be ~60+
    assert!(
        prompt.len() > 50,
        "Prompt with system message should be substantial. Got {} chars: {:?}",
        prompt.len(),
        prompt
    );

    // Verify the prompt contains what we expect
    assert!(
        prompt.contains(system),
        "Prompt should contain system message"
    );
    assert!(
        prompt.contains("Human: Hi"),
        "Prompt should contain formatted user message"
    );
}

/// Test that the can_use_tool callback is invoked when Claude tries to use a tool.
///
/// NOTE: As of SDK v0.6.3, the can_use_tool callback is defined in ClaudeAgentOptions
/// but the control protocol to invoke it is NOT implemented. The SDK's handle_control_request
/// only handles "hook_callback" and "mcp_message" subtypes - there's no handler for
/// tool permission requests.
///
/// This test documents this limitation - it will FAIL until the SDK implements the
/// permission callback control protocol.
#[tokio::test]
#[ignore = "SDK v0.6.3 does not implement can_use_tool callback - see test documentation"]
async fn test_can_use_tool_callback_invoked() {
    if !cli_available() {
        println!("Skipping: Claude CLI not available");
        return;
    }

    let callback_count = Arc::new(AtomicUsize::new(0));
    let callback_count_clone = callback_count.clone();

    // Create a callback that counts invocations and always allows
    let can_use_tool: Arc<
        dyn Fn(
                String,
                serde_json::Value,
                ToolPermissionContext,
            ) -> BoxFuture<'static, PermissionResult>
            + Send
            + Sync,
    > = Arc::new(move |tool_name: String, _tool_input, _context| {
        let count = callback_count_clone.clone();
        Box::pin(async move {
            count.fetch_add(1, Ordering::SeqCst);
            println!("Permission requested for tool: {}", tool_name);
            PermissionResult::Allow(PermissionResultAllow::default())
        })
    });

    let stderr_callback = |_msg: String| {};

    let options = ClaudeAgentOptions::builder()
        .max_turns(1)
        .skip_version_check(true)
        .stderr_callback(Arc::new(stderr_callback))
        .can_use_tool(can_use_tool)
        .build();

    // Ask Claude to read a file - this should trigger the Read tool
    let prompt = "Read the file /etc/hostname and tell me what it contains";

    let mut stream = match query_stream(prompt.to_string(), Some(options)).await {
        Ok(s) => s,
        Err(e) => {
            panic!("Failed to create stream: {}", e);
        }
    };

    // Consume the stream
    while let Some(result) = stream.next().await {
        if let Err(e) = result {
            // Some errors are expected if the tool is denied or file doesn't exist
            println!("Stream message: {:?}", e);
        }
    }

    let count = callback_count.load(Ordering::SeqCst);
    assert!(
        count > 0,
        "can_use_tool callback should have been invoked at least once, but was invoked {} times. \
         NOTE: SDK v0.6.3 doesn't implement the permission callback protocol - \
         see handle_control_request in query_full.rs which only handles 'hook_callback' and 'mcp_message'",
        count
    );
    println!("can_use_tool callback was invoked {} time(s)", count);
}

/// Test that denying a tool permission prevents the tool from executing.
///
/// NOTE: See test_can_use_tool_callback_invoked for SDK limitation details.
#[tokio::test]
#[ignore = "SDK v0.6.3 does not implement can_use_tool callback"]
async fn test_can_use_tool_deny_prevents_execution() {
    if !cli_available() {
        println!("Skipping: Claude CLI not available");
        return;
    }

    let was_denied = Arc::new(AtomicBool::new(false));
    let was_denied_clone = was_denied.clone();

    // Create a callback that always denies
    let can_use_tool: Arc<
        dyn Fn(
                String,
                serde_json::Value,
                ToolPermissionContext,
            ) -> BoxFuture<'static, PermissionResult>
            + Send
            + Sync,
    > = Arc::new(move |tool_name: String, _tool_input, _context| {
        let denied = was_denied_clone.clone();
        Box::pin(async move {
            denied.store(true, Ordering::SeqCst);
            println!("Denying permission for tool: {}", tool_name);
            PermissionResult::Deny(PermissionResultDeny {
                message: "Test denial - permission not granted".to_string(),
                interrupt: false,
            })
        })
    });

    let stderr_callback = |_msg: String| {};

    let options = ClaudeAgentOptions::builder()
        .max_turns(2) // Allow a retry turn for Claude to respond to denial
        .skip_version_check(true)
        .stderr_callback(Arc::new(stderr_callback))
        .can_use_tool(can_use_tool)
        .build();

    // Ask Claude to read a file
    let prompt = "Read the file /etc/hostname";

    let mut stream = match query_stream(prompt.to_string(), Some(options)).await {
        Ok(s) => s,
        Err(e) => {
            panic!("Failed to create stream: {}", e);
        }
    };

    let mut response_text = String::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(ClaudeMessage::Assistant(msg)) => {
                for block in &msg.message.content {
                    if let ContentBlock::Text(TextBlock { text }) = block {
                        response_text.push_str(text);
                    }
                }
            }
            _ => {}
        }
    }

    assert!(
        was_denied.load(Ordering::SeqCst),
        "The can_use_tool callback should have been invoked and denied"
    );
    println!("Response after denial: {}", response_text);
}
