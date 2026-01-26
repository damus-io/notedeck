use crate::backend::traits::AiBackend;
use crate::messages::{
    DaveApiResponse, PendingPermission, PermissionRequest, PermissionResponse, ToolResult,
};
use crate::tools::Tool;
use crate::Message;
use claude_agent_sdk_rs::{
    ClaudeAgentOptions, ClaudeClient, ContentBlock, Message as ClaudeMessage, PermissionMode,
    PermissionResult, PermissionResultAllow, PermissionResultDeny, ToolUseBlock,
};
use futures::future::BoxFuture;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use tokio::sync::oneshot;
use uuid::Uuid;

pub struct ClaudeBackend {
    api_key: String,
}

impl ClaudeBackend {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    /// Convert our messages to a prompt for Claude Code
    fn messages_to_prompt(messages: &[Message]) -> String {
        let mut prompt = String::new();

        // Include system message if present
        for msg in messages {
            if let Message::System(content) = msg {
                prompt.push_str(content);
                prompt.push_str("\n\n");
                break;
            }
        }

        // Format conversation history
        for msg in messages {
            match msg {
                Message::System(_) => {} // Already handled
                Message::User(content) => {
                    prompt.push_str("Human: ");
                    prompt.push_str(content);
                    prompt.push_str("\n\n");
                }
                Message::Assistant(content) => {
                    prompt.push_str("Assistant: ");
                    prompt.push_str(content);
                    prompt.push_str("\n\n");
                }
                Message::ToolCalls(_)
                | Message::ToolResponse(_)
                | Message::Error(_)
                | Message::PermissionRequest(_)
                | Message::ToolResult(_) => {
                    // Skip tool-related, error, permission, and tool result messages
                }
            }
        }

        prompt
    }

    /// Extract only the latest user message for session continuation
    fn get_latest_user_message(messages: &[Message]) -> String {
        messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::User(content) => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }
}

impl AiBackend for ClaudeBackend {
    fn stream_request(
        &self,
        messages: Vec<Message>,
        _tools: Arc<HashMap<String, Tool>>,
        _model: String,
        _user_id: String,
        _session_id: String, // TODO: Currently unused - --continue resumes last conversation globally
        ctx: egui::Context,
    ) -> (
        mpsc::Receiver<DaveApiResponse>,
        Option<tokio::task::JoinHandle<()>>,
    ) {
        let (tx, rx) = mpsc::channel();
        let _api_key = self.api_key.clone();

        let tx_for_callback = tx.clone();
        let ctx_for_callback = ctx.clone();

        // First message in session = start fresh conversation
        // Subsequent messages = use --continue to resume the last conversation
        // NOTE: --continue resumes the globally last conversation, not per-session.
        // This works for single-conversation use but multiple UI sessions would interfere.
        // For proper per-session context, we'd need a persistent ClaudeClient connection.
        let is_first_message = messages
            .iter()
            .filter(|m| matches!(m, Message::User(_)))
            .count()
            == 1;

        let handle = tokio::spawn(async move {
            // For first message, send full prompt; for continuation, just the latest message
            let prompt = if is_first_message {
                Self::messages_to_prompt(&messages)
            } else {
                Self::get_latest_user_message(&messages)
            };

            tracing::debug!(
                "Sending request to Claude Code: is_first={}, prompt length: {}, preview: {:?}",
                is_first_message,
                prompt.len(),
                &prompt[..prompt.len().min(100)]
            );

            // A stderr callback is needed to prevent the subprocess from blocking
            // when stderr buffer fills up. We log the output for debugging.
            let stderr_callback = |msg: String| {
                tracing::trace!("Claude CLI stderr: {}", msg);
            };

            // Permission callback - sends requests to UI and waits for user response
            let can_use_tool: Arc<
                dyn Fn(
                        String,
                        serde_json::Value,
                        claude_agent_sdk_rs::ToolPermissionContext,
                    ) -> BoxFuture<'static, PermissionResult>
                    + Send
                    + Sync,
            > = Arc::new({
                let tx = tx_for_callback;
                let ctx = ctx_for_callback;
                move |tool_name: String,
                      tool_input: serde_json::Value,
                      _context: claude_agent_sdk_rs::ToolPermissionContext| {
                    let tx = tx.clone();
                    let ctx = ctx.clone();
                    Box::pin(async move {
                        let (response_tx, response_rx) = oneshot::channel();

                        let request = PermissionRequest {
                            id: Uuid::new_v4(),
                            tool_name: tool_name.clone(),
                            tool_input: tool_input.clone(),
                            response: None,
                        };

                        let pending = PendingPermission {
                            request,
                            response_tx,
                        };

                        // Send permission request to UI
                        if tx
                            .send(DaveApiResponse::PermissionRequest(pending))
                            .is_err()
                        {
                            tracing::error!("Failed to send permission request to UI");
                            return PermissionResult::Deny(PermissionResultDeny {
                                message: "UI channel closed".to_string(),
                                interrupt: true,
                            });
                        }

                        ctx.request_repaint();

                        // Wait for user response
                        match response_rx.await {
                            Ok(PermissionResponse::Allow) => {
                                tracing::debug!("User allowed tool: {}", tool_name);
                                PermissionResult::Allow(PermissionResultAllow::default())
                            }
                            Ok(PermissionResponse::Deny { reason }) => {
                                tracing::debug!("User denied tool {}: {}", tool_name, reason);
                                PermissionResult::Deny(PermissionResultDeny {
                                    message: reason,
                                    interrupt: false,
                                })
                            }
                            Err(_) => {
                                tracing::error!("Permission response channel closed");
                                PermissionResult::Deny(PermissionResultDeny {
                                    message: "Permission request cancelled".to_string(),
                                    interrupt: true,
                                })
                            }
                        }
                    })
                }
            });

            // Use ClaudeClient instead of query_stream to enable control protocol
            // for can_use_tool callbacks
            // For follow-up messages, use --continue to resume the last conversation
            let stderr_callback = Arc::new(stderr_callback);

            let options = if is_first_message {
                ClaudeAgentOptions::builder()
                    .permission_mode(PermissionMode::Default)
                    .stderr_callback(stderr_callback.clone())
                    .can_use_tool(can_use_tool)
                    .include_partial_messages(true)
                    .build()
            } else {
                ClaudeAgentOptions::builder()
                    .permission_mode(PermissionMode::Default)
                    .stderr_callback(stderr_callback.clone())
                    .can_use_tool(can_use_tool)
                    .include_partial_messages(true)
                    .continue_conversation(true)
                    .build()
            };
            let mut client = ClaudeClient::new(options);
            if let Err(err) = client.connect().await {
                tracing::error!("Claude Code connection error: {}", err);
                let _ = tx.send(DaveApiResponse::Failed(err.to_string()));
                let _ = client.disconnect().await;
                return;
            }
            if let Err(err) = client.query(&prompt).await {
                tracing::error!("Claude Code query error: {}", err);
                let _ = tx.send(DaveApiResponse::Failed(err.to_string()));
                let _ = client.disconnect().await;
                return;
            }
            let mut stream = client.receive_response();

            // Track pending tool uses: tool_use_id -> (tool_name, tool_input)
            let mut pending_tools: HashMap<String, (String, serde_json::Value)> = HashMap::new();

            while let Some(result) = stream.next().await {
                match result {
                    Ok(message) => match message {
                        ClaudeMessage::Assistant(assistant_msg) => {
                            // Text is handled by StreamEvent for incremental display
                            for block in &assistant_msg.message.content {
                                if let ContentBlock::ToolUse(ToolUseBlock { id, name, input }) =
                                    block
                                {
                                    // Store for later correlation with tool result
                                    pending_tools.insert(id.clone(), (name.clone(), input.clone()));
                                }
                            }
                        }
                        ClaudeMessage::StreamEvent(event) => {
                            if let Some(event_type) =
                                event.event.get("type").and_then(|v| v.as_str())
                            {
                                if event_type == "content_block_delta" {
                                    if let Some(text) = event
                                        .event
                                        .get("delta")
                                        .and_then(|d| d.get("text"))
                                        .and_then(|t| t.as_str())
                                    {
                                        if let Err(err) =
                                            tx.send(DaveApiResponse::Token(text.to_string()))
                                        {
                                            tracing::error!("Failed to send token to UI: {}", err);
                                            drop(stream);
                                            let _ = client.disconnect().await;
                                            return;
                                        }
                                        ctx.request_repaint();
                                    }
                                }
                            }
                        }
                        ClaudeMessage::Result(result_msg) => {
                            if result_msg.is_error {
                                let error_text = result_msg
                                    .result
                                    .unwrap_or_else(|| "Unknown error".to_string());
                                let _ = tx.send(DaveApiResponse::Failed(error_text));
                            }
                            break;
                        }
                        ClaudeMessage::User(user_msg) => {
                            // Tool results come in user_msg.extra, not content
                            // Structure: extra["tool_use_result"] has the result,
                            // extra["message"]["content"][0]["tool_use_id"] has the correlation ID
                            if let Some(tool_use_result) = user_msg.extra.get("tool_use_result") {
                                // Get tool_use_id from message.content[0].tool_use_id
                                let tool_use_id = user_msg
                                    .extra
                                    .get("message")
                                    .and_then(|m| m.get("content"))
                                    .and_then(|c| c.as_array())
                                    .and_then(|arr| arr.first())
                                    .and_then(|item| item.get("tool_use_id"))
                                    .and_then(|id| id.as_str());

                                if let Some(tool_use_id) = tool_use_id {
                                    if let Some((tool_name, tool_input)) =
                                        pending_tools.remove(tool_use_id)
                                    {
                                        let summary = format_tool_summary(
                                            &tool_name,
                                            &tool_input,
                                            tool_use_result,
                                        );
                                        let tool_result = ToolResult { tool_name, summary };
                                        let _ = tx.send(DaveApiResponse::ToolResult(tool_result));
                                        ctx.request_repaint();
                                    }
                                }
                            }
                        }
                        _ => {}
                    },
                    Err(err) => {
                        tracing::error!("Claude stream error: {}", err);
                        let _ = tx.send(DaveApiResponse::Failed(err.to_string()));
                        drop(stream);
                        let _ = client.disconnect().await;
                        return;
                    }
                }
            }

            drop(stream);
            let _ = client.disconnect().await;
            tracing::debug!("Claude stream closed");
        });

        (rx, Some(handle))
    }
}

/// Extract string content from a tool response, handling various JSON structures
fn extract_response_content(response: &serde_json::Value) -> Option<String> {
    // Try direct string first
    if let Some(s) = response.as_str() {
        return Some(s.to_string());
    }
    // Try "content" field (common wrapper)
    if let Some(s) = response.get("content").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    // Try file.content for Read tool responses
    if let Some(s) = response
        .get("file")
        .and_then(|f| f.get("content"))
        .and_then(|v| v.as_str())
    {
        return Some(s.to_string());
    }
    // Try "output" field
    if let Some(s) = response.get("output").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    // Try "result" field
    if let Some(s) = response.get("result").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    // Fallback: serialize the whole response if it's not null
    if !response.is_null() {
        return Some(response.to_string());
    }
    None
}

/// Format a human-readable summary for tool execution results
fn format_tool_summary(
    tool_name: &str,
    input: &serde_json::Value,
    response: &serde_json::Value,
) -> String {
    match tool_name {
        "Read" => {
            let file = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let filename = file.rsplit('/').next().unwrap_or(file);
            // Try to get numLines directly from file metadata (most accurate)
            let lines = response
                .get("file")
                .and_then(|f| f.get("numLines").or_else(|| f.get("totalLines")))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                // Fallback to counting lines in content
                .or_else(|| {
                    extract_response_content(response)
                        .as_ref()
                        .map(|s| s.lines().count())
                })
                .unwrap_or(0);
            format!("{} ({} lines)", filename, lines)
        }
        "Write" => {
            let file = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let filename = file.rsplit('/').next().unwrap_or(file);
            let bytes = input
                .get("content")
                .and_then(|v| v.as_str())
                .map(|s| s.len())
                .unwrap_or(0);
            format!("{} ({} bytes)", filename, bytes)
        }
        "Bash" => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            // Truncate long commands
            let cmd_display = if cmd.len() > 40 {
                format!("{}...", &cmd[..37])
            } else {
                cmd.to_string()
            };
            let output_len = extract_response_content(response)
                .as_ref()
                .map(|s| s.len())
                .unwrap_or(0);
            if output_len > 0 {
                format!("`{}` ({} chars)", cmd_display, output_len)
            } else {
                format!("`{}`", cmd_display)
            }
        }
        "Grep" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            format!("'{}'", pattern)
        }
        "Glob" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            format!("'{}'", pattern)
        }
        "Edit" => {
            let file = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let filename = file.rsplit('/').next().unwrap_or(file);
            filename.to_string()
        }
        _ => String::new(),
    }
}
