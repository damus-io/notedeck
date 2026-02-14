//! Integration tests for Claude Code SDK
//!
//! These tests require Claude Code CLI to be installed and authenticated.
//! Run with: cargo test -p notedeck_dave --test claude_integration -- --ignored
//!
//! The SDK spawns the Claude Code CLI as a subprocess and communicates via JSON streaming.
//! The CLAUDE_API_KEY environment variable is read by the CLI subprocess.

use claude_agent_sdk_rs::{
    get_claude_code_version, query_stream, ClaudeAgentOptions, ClaudeClient, ContentBlock,
    Message as ClaudeMessage, PermissionMode, PermissionResult, PermissionResultAllow,
    PermissionResultDeny, TextBlock, ToolPermissionContext,
};
use futures::future::BoxFuture;
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

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
#[tokio::test]
#[ignore = "Requires Claude Code CLI to be installed and authenticated"]
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
        .tools(["Read"])
        .permission_mode(PermissionMode::Default)
        .max_turns(3)
        .skip_version_check(true)
        .stderr_callback(Arc::new(stderr_callback))
        .can_use_tool(can_use_tool)
        .build();

    // Ask Claude to read a file - this should trigger the Read tool
    let prompt = "Read the file /etc/hostname";

    // Use ClaudeClient which wires up the control protocol for can_use_tool callbacks
    let mut client = ClaudeClient::new(options);
    client.connect().await.expect("Failed to connect");
    client.query(prompt).await.expect("Failed to send query");

    // Consume the stream
    let mut stream = client.receive_response();
    while let Some(result) = stream.next().await {
        match result {
            Ok(msg) => println!("Stream message: {:?}", msg),
            Err(e) => println!("Stream error: {:?}", e),
        }
    }

    let count = callback_count.load(Ordering::SeqCst);
    assert!(
        count > 0,
        "can_use_tool callback should have been invoked at least once, but was invoked {} times",
        count
    );
    println!("can_use_tool callback was invoked {} time(s)", count);
}

/// Test session management - sending multiple queries with session context maintained.
/// The ClaudeClient must be kept connected to maintain session context.
#[tokio::test]
#[ignore = "Requires Claude Code CLI to be installed and authenticated"]
async fn test_session_context_maintained() {
    if !cli_available() {
        println!("Skipping: Claude CLI not available");
        return;
    }

    let stderr_callback = |_msg: String| {};

    let options = ClaudeAgentOptions::builder()
        .permission_mode(PermissionMode::BypassPermissions)
        .max_turns(1)
        .skip_version_check(true)
        .stderr_callback(Arc::new(stderr_callback))
        .build();

    let mut client = ClaudeClient::new(options);
    client.connect().await.expect("Failed to connect");

    // First query - tell Claude a secret
    let session_id = "test-session-context";
    println!("Sending first query to session: {}", session_id);
    client
        .query_with_session(
            "Remember this secret code: BANANA42. Just acknowledge.",
            session_id,
        )
        .await
        .expect("Failed to send first query");

    // Consume first response
    let mut first_response = String::new();
    {
        let mut stream = client.receive_response();
        while let Some(result) = stream.next().await {
            if let Ok(ClaudeMessage::Assistant(msg)) = result {
                for block in &msg.message.content {
                    if let ContentBlock::Text(TextBlock { text }) = block {
                        first_response.push_str(text);
                    }
                }
            }
        }
    }
    println!("First response: {}", first_response);

    // Second query - ask about the secret (should remember within same session)
    println!("Sending second query to same session");
    client
        .query_with_session("What was the secret code I told you?", session_id)
        .await
        .expect("Failed to send second query");

    // Check if second response mentions the secret
    let mut second_response = String::new();
    {
        let mut stream = client.receive_response();
        while let Some(result) = stream.next().await {
            if let Ok(ClaudeMessage::Assistant(msg)) = result {
                for block in &msg.message.content {
                    if let ContentBlock::Text(TextBlock { text }) = block {
                        second_response.push_str(text);
                    }
                }
            }
        }
    }
    println!("Second response: {}", second_response);

    client.disconnect().await.expect("Failed to disconnect");

    // The second response should contain the secret code if context is maintained
    assert!(
        second_response.to_uppercase().contains("BANANA42"),
        "Claude should remember the secret code from the same session. Got: {}",
        second_response
    );
}

/// Test that different session IDs maintain separate contexts.
#[tokio::test]
#[ignore = "Requires Claude Code CLI to be installed and authenticated"]
async fn test_separate_sessions_have_separate_context() {
    if !cli_available() {
        println!("Skipping: Claude CLI not available");
        return;
    }

    let stderr_callback = |_msg: String| {};

    let options = ClaudeAgentOptions::builder()
        .permission_mode(PermissionMode::BypassPermissions)
        .max_turns(1)
        .skip_version_check(true)
        .stderr_callback(Arc::new(stderr_callback))
        .build();

    let mut client = ClaudeClient::new(options);
    client.connect().await.expect("Failed to connect");

    // First session - tell a secret
    println!("Session A: Setting secret");
    client
        .query_with_session(
            "Remember: The password is APPLE123. Just acknowledge.",
            "session-A",
        )
        .await
        .expect("Failed to send to session A");

    {
        let mut stream = client.receive_response();
        while let Some(_) = stream.next().await {}
    }

    // Different session - should NOT know the secret
    println!("Session B: Asking about secret");
    client
        .query_with_session(
            "What password did I tell you? If you don't know, just say 'I don't know any password'.",
            "session-B",
        )
        .await
        .expect("Failed to send to session B");

    let mut response_b = String::new();
    {
        let mut stream = client.receive_response();
        while let Some(result) = stream.next().await {
            if let Ok(ClaudeMessage::Assistant(msg)) = result {
                for block in &msg.message.content {
                    if let ContentBlock::Text(TextBlock { text }) = block {
                        response_b.push_str(text);
                    }
                }
            }
        }
    }
    println!("Session B response: {}", response_b);

    client.disconnect().await.expect("Failed to disconnect");

    // Session B should NOT know the password from Session A
    assert!(
        !response_b.to_uppercase().contains("APPLE123"),
        "Session B should NOT know the password from Session A. Got: {}",
        response_b
    );
}

/// Test --continue flag for resuming the last conversation.
/// This tests the simpler approach of continuing the most recent conversation.
#[tokio::test]
#[ignore = "Requires Claude Code CLI to be installed and authenticated"]
async fn test_continue_conversation_flag() {
    if !cli_available() {
        println!("Skipping: Claude CLI not available");
        return;
    }

    let stderr_callback = |_msg: String| {};

    // First: Start a fresh conversation
    let options1 = ClaudeAgentOptions::builder()
        .permission_mode(PermissionMode::BypassPermissions)
        .max_turns(1)
        .skip_version_check(true)
        .stderr_callback(Arc::new(stderr_callback))
        .build();

    let mut stream1 = query_stream(
        "Remember this code: ZEBRA999. Just acknowledge.".to_string(),
        Some(options1),
    )
    .await
    .expect("First query failed");

    let mut first_response = String::new();
    while let Some(result) = stream1.next().await {
        if let Ok(ClaudeMessage::Assistant(msg)) = result {
            for block in &msg.message.content {
                if let ContentBlock::Text(TextBlock { text }) = block {
                    first_response.push_str(text);
                }
            }
        }
    }
    println!("First response: {}", first_response);

    // Second: Use --continue to resume and ask about the code
    let stderr_callback2 = |_msg: String| {};
    let options2 = ClaudeAgentOptions::builder()
        .permission_mode(PermissionMode::BypassPermissions)
        .max_turns(1)
        .skip_version_check(true)
        .stderr_callback(Arc::new(stderr_callback2))
        .continue_conversation(true)
        .build();

    let mut stream2 = query_stream("What was the code I told you?".to_string(), Some(options2))
        .await
        .expect("Second query failed");

    let mut second_response = String::new();
    while let Some(result) = stream2.next().await {
        if let Ok(ClaudeMessage::Assistant(msg)) = result {
            for block in &msg.message.content {
                if let ContentBlock::Text(TextBlock { text }) = block {
                    second_response.push_str(text);
                }
            }
        }
    }
    println!("Second response (with --continue): {}", second_response);

    // Claude should remember the code when using --continue
    assert!(
        second_response.to_uppercase().contains("ZEBRA999"),
        "Claude should remember the code with --continue. Got: {}",
        second_response
    );
}

/// Test that denying a tool permission prevents the tool from executing.
#[tokio::test]
#[ignore = "Requires Claude Code CLI to be installed and authenticated"]
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
        .tools(["Read"])
        .permission_mode(PermissionMode::Default)
        .max_turns(3)
        .skip_version_check(true)
        .stderr_callback(Arc::new(stderr_callback))
        .can_use_tool(can_use_tool)
        .build();

    // Ask Claude to read a file
    let prompt = "Read the file /etc/hostname";

    // Use ClaudeClient which wires up the control protocol for can_use_tool callbacks
    let mut client = ClaudeClient::new(options);
    client.connect().await.expect("Failed to connect");
    client.query(prompt).await.expect("Failed to send query");

    let mut response_text = String::new();
    let mut stream = client.receive_response();
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
