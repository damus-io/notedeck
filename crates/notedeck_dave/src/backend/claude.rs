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
use dashmap::DashMap;
use futures::future::BoxFuture;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Commands sent to a session's actor task
enum SessionCommand {
    Query {
        prompt: String,
        response_tx: mpsc::Sender<DaveApiResponse>,
        ctx: egui::Context,
    },
    /// Interrupt the current query - stops the stream but preserves session
    Interrupt {
        ctx: egui::Context,
    },
    /// Set the permission mode (Default or Plan)
    SetPermissionMode {
        mode: PermissionMode,
        ctx: egui::Context,
    },
    Shutdown,
}

/// Handle to a session's actor
struct SessionHandle {
    command_tx: tokio_mpsc::Sender<SessionCommand>,
}

pub struct ClaudeBackend {
    #[allow(dead_code)] // May be used in the future for API key validation
    api_key: String,
    /// Registry of active sessions (using dashmap for lock-free access)
    sessions: DashMap<String, SessionHandle>,
}

impl ClaudeBackend {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            sessions: DashMap::new(),
        }
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

/// Permission request forwarded from the callback to the actor
struct PermissionRequestInternal {
    tool_name: String,
    tool_input: serde_json::Value,
    response_tx: oneshot::Sender<PermissionResult>,
}

/// Session actor task that owns a single ClaudeClient with persistent connection
async fn session_actor(session_id: String, mut command_rx: tokio_mpsc::Receiver<SessionCommand>) {
    // Permission channel - the callback sends to perm_tx, actor receives on perm_rx
    let (perm_tx, mut perm_rx) = tokio_mpsc::channel::<PermissionRequestInternal>(16);

    // Create the can_use_tool callback that forwards to our permission channel
    let can_use_tool: Arc<
        dyn Fn(
                String,
                serde_json::Value,
                claude_agent_sdk_rs::ToolPermissionContext,
            ) -> BoxFuture<'static, PermissionResult>
            + Send
            + Sync,
    > = Arc::new({
        let perm_tx = perm_tx.clone();
        move |tool_name: String,
              tool_input: serde_json::Value,
              _context: claude_agent_sdk_rs::ToolPermissionContext| {
            let perm_tx = perm_tx.clone();
            Box::pin(async move {
                let (resp_tx, resp_rx) = oneshot::channel();
                if perm_tx
                    .send(PermissionRequestInternal {
                        tool_name: tool_name.clone(),
                        tool_input,
                        response_tx: resp_tx,
                    })
                    .await
                    .is_err()
                {
                    return PermissionResult::Deny(PermissionResultDeny {
                        message: "Session actor channel closed".to_string(),
                        interrupt: true,
                    });
                }
                // Wait for response from session actor (which forwards from UI)
                match resp_rx.await {
                    Ok(result) => result,
                    Err(_) => PermissionResult::Deny(PermissionResultDeny {
                        message: "Permission response cancelled".to_string(),
                        interrupt: true,
                    }),
                }
            })
        }
    });

    // A stderr callback to prevent the subprocess from blocking
    let stderr_callback = Arc::new(|msg: String| {
        tracing::trace!("Claude CLI stderr: {}", msg);
    });

    // Create client once - this maintains the persistent connection
    let options = ClaudeAgentOptions::builder()
        .permission_mode(PermissionMode::Default)
        .stderr_callback(stderr_callback)
        .can_use_tool(can_use_tool)
        .include_partial_messages(true)
        .build();
    let mut client = ClaudeClient::new(options);

    // Connect once - this starts the subprocess
    if let Err(err) = client.connect().await {
        tracing::error!("Session {} failed to connect: {}", session_id, err);
        // Process any pending commands to report the error
        while let Some(cmd) = command_rx.recv().await {
            if let SessionCommand::Query {
                ref response_tx, ..
            } = cmd
            {
                let _ = response_tx.send(DaveApiResponse::Failed(format!(
                    "Failed to connect to Claude: {}",
                    err
                )));
            }
            if matches!(cmd, SessionCommand::Shutdown) {
                break;
            }
        }
        return;
    }

    tracing::debug!("Session {} connected successfully", session_id);

    // Process commands
    while let Some(cmd) = command_rx.recv().await {
        match cmd {
            SessionCommand::Query {
                prompt,
                response_tx,
                ctx,
            } => {
                // Send query using session_id for context
                if let Err(err) = client.query_with_session(&prompt, &session_id).await {
                    tracing::error!("Session {} query error: {}", session_id, err);
                    let _ = response_tx.send(DaveApiResponse::Failed(err.to_string()));
                    continue;
                }

                // Track pending tool uses: tool_use_id -> (tool_name, tool_input)
                let mut pending_tools: HashMap<String, (String, serde_json::Value)> =
                    HashMap::new();

                // Stream response with select! to handle stream, permission requests, and interrupts
                let mut stream = client.receive_response();
                let mut stream_done = false;

                while !stream_done {
                    tokio::select! {
                        biased;

                        // Check for interrupt command (highest priority)
                        Some(cmd) = command_rx.recv() => {
                            match cmd {
                                SessionCommand::Interrupt { ctx: interrupt_ctx } => {
                                    tracing::debug!("Session {} received interrupt", session_id);
                                    if let Err(err) = client.interrupt().await {
                                        tracing::error!("Failed to send interrupt: {}", err);
                                    }
                                    // Let the stream end naturally - it will send a Result message
                                    // The session history is preserved by the CLI
                                    interrupt_ctx.request_repaint();
                                }
                                SessionCommand::Query { response_tx: new_tx, .. } => {
                                    // A new query came in while we're still streaming - shouldn't happen
                                    // but handle gracefully by rejecting it
                                    let _ = new_tx.send(DaveApiResponse::Failed(
                                        "Query already in progress".to_string()
                                    ));
                                }
                                SessionCommand::SetPermissionMode { mode, ctx: mode_ctx } => {
                                    // Permission mode change during query - apply it
                                    tracing::debug!("Session {} setting permission mode to {:?} during query", session_id, mode);
                                    if let Err(err) = client.set_permission_mode(mode).await {
                                        tracing::error!("Failed to set permission mode: {}", err);
                                    }
                                    mode_ctx.request_repaint();
                                }
                                SessionCommand::Shutdown => {
                                    tracing::debug!("Session actor {} shutting down during query", session_id);
                                    // Drop stream and disconnect - break to exit loop first
                                    drop(stream);
                                    if let Err(err) = client.disconnect().await {
                                        tracing::warn!("Error disconnecting session {}: {}", session_id, err);
                                    }
                                    tracing::debug!("Session {} actor exited", session_id);
                                    return;
                                }
                            }
                        }

                        // Handle permission requests (they're blocking the SDK)
                        Some(perm_req) = perm_rx.recv() => {
                            // Forward permission request to UI
                            let request_id = Uuid::new_v4();
                            let (ui_resp_tx, ui_resp_rx) = oneshot::channel();

                            let request = PermissionRequest {
                                id: request_id,
                                tool_name: perm_req.tool_name.clone(),
                                tool_input: perm_req.tool_input.clone(),
                                response: None,
                            };

                            let pending = PendingPermission {
                                request,
                                response_tx: ui_resp_tx,
                            };

                            if response_tx.send(DaveApiResponse::PermissionRequest(pending)).is_err() {
                                tracing::error!("Failed to send permission request to UI");
                                let _ = perm_req.response_tx.send(PermissionResult::Deny(PermissionResultDeny {
                                    message: "UI channel closed".to_string(),
                                    interrupt: true,
                                }));
                                continue;
                            }

                            ctx.request_repaint();

                            // Spawn task to wait for UI response and forward to callback
                            let tool_name = perm_req.tool_name.clone();
                            let callback_tx = perm_req.response_tx;
                            tokio::spawn(async move {
                                let result = match ui_resp_rx.await {
                                    Ok(PermissionResponse::Allow { message }) => {
                                        if let Some(msg) = &message {
                                            tracing::debug!("User allowed tool {} with message: {}", tool_name, msg);
                                        } else {
                                            tracing::debug!("User allowed tool: {}", tool_name);
                                        }
                                        // Note: message is handled in lib.rs by adding a User message to chat
                                        // SDK's PermissionResultAllow doesn't have a message field
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
                                };
                                let _ = callback_tx.send(result);
                            });
                        }

                        stream_result = stream.next() => {
                            match stream_result {
                                Some(Ok(message)) => {
                                    match message {
                                        ClaudeMessage::Assistant(assistant_msg) => {
                                            for block in &assistant_msg.message.content {
                                                if let ContentBlock::ToolUse(ToolUseBlock { id, name, input }) = block {
                                                    pending_tools.insert(id.clone(), (name.clone(), input.clone()));
                                                }
                                            }
                                        }
                                        ClaudeMessage::StreamEvent(event) => {
                                            if let Some(event_type) = event.event.get("type").and_then(|v| v.as_str()) {
                                                if event_type == "content_block_delta" {
                                                    if let Some(text) = event
                                                        .event
                                                        .get("delta")
                                                        .and_then(|d| d.get("text"))
                                                        .and_then(|t| t.as_str())
                                                    {
                                                        if response_tx.send(DaveApiResponse::Token(text.to_string())).is_err() {
                                                            tracing::error!("Failed to send token to UI");
                                                            // Setting stream_done isn't needed since we break immediately
                                                            break;
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
                                                let _ = response_tx.send(DaveApiResponse::Failed(error_text));
                                            }
                                            stream_done = true;
                                        }
                                        ClaudeMessage::User(user_msg) => {
                                            if let Some(tool_use_result) = user_msg.extra.get("tool_use_result") {
                                                let tool_use_id = user_msg
                                                    .extra
                                                    .get("message")
                                                    .and_then(|m| m.get("content"))
                                                    .and_then(|c| c.as_array())
                                                    .and_then(|arr| arr.first())
                                                    .and_then(|item| item.get("tool_use_id"))
                                                    .and_then(|id| id.as_str());

                                                if let Some(tool_use_id) = tool_use_id {
                                                    if let Some((tool_name, tool_input)) = pending_tools.remove(tool_use_id) {
                                                        let summary = format_tool_summary(&tool_name, &tool_input, tool_use_result);
                                                        let tool_result = ToolResult { tool_name, summary };
                                                        let _ = response_tx.send(DaveApiResponse::ToolResult(tool_result));
                                                        ctx.request_repaint();
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                Some(Err(err)) => {
                                    tracing::error!("Claude stream error: {}", err);
                                    let _ = response_tx.send(DaveApiResponse::Failed(err.to_string()));
                                    stream_done = true;
                                }
                                None => {
                                    stream_done = true;
                                }
                            }
                        }
                    }
                }

                tracing::debug!("Query complete for session {}", session_id);
                // Don't disconnect - keep the connection alive for subsequent queries
            }
            SessionCommand::Interrupt { ctx } => {
                // Interrupt received when not in a query - just request repaint
                tracing::debug!(
                    "Session {} received interrupt but no query active",
                    session_id
                );
                ctx.request_repaint();
            }
            SessionCommand::SetPermissionMode { mode, ctx } => {
                tracing::debug!("Session {} setting permission mode to {:?}", session_id, mode);
                if let Err(err) = client.set_permission_mode(mode).await {
                    tracing::error!("Failed to set permission mode: {}", err);
                }
                ctx.request_repaint();
            }
            SessionCommand::Shutdown => {
                tracing::debug!("Session actor {} shutting down", session_id);
                break;
            }
        }
    }

    // Disconnect when shutting down
    if let Err(err) = client.disconnect().await {
        tracing::warn!("Error disconnecting session {}: {}", session_id, err);
    }
    tracing::debug!("Session {} actor exited", session_id);
}

impl AiBackend for ClaudeBackend {
    fn stream_request(
        &self,
        messages: Vec<Message>,
        _tools: Arc<HashMap<String, Tool>>,
        _model: String,
        _user_id: String,
        session_id: String,
        ctx: egui::Context,
    ) -> (
        mpsc::Receiver<DaveApiResponse>,
        Option<tokio::task::JoinHandle<()>>,
    ) {
        let (response_tx, response_rx) = mpsc::channel();

        // Determine if this is the first message in the session
        let is_first_message = messages
            .iter()
            .filter(|m| matches!(m, Message::User(_)))
            .count()
            == 1;

        // For first message, send full prompt; for continuation, just the latest message
        let prompt = if is_first_message {
            Self::messages_to_prompt(&messages)
        } else {
            Self::get_latest_user_message(&messages)
        };

        tracing::debug!(
            "Sending request to Claude Code: session={}, is_first={}, prompt length: {}, preview: {:?}",
            session_id,
            is_first_message,
            prompt.len(),
            &prompt[..prompt.len().min(100)]
        );

        // Get or create session actor
        let command_tx = {
            let entry = self.sessions.entry(session_id.clone());
            let handle = entry.or_insert_with(|| {
                let (command_tx, command_rx) = tokio_mpsc::channel(16);

                // Spawn session actor
                let session_id_clone = session_id.clone();
                tokio::spawn(async move {
                    session_actor(session_id_clone, command_rx).await;
                });

                SessionHandle { command_tx }
            });
            handle.command_tx.clone()
        };

        // Spawn a task to send the query command
        let handle = tokio::spawn(async move {
            if let Err(err) = command_tx
                .send(SessionCommand::Query {
                    prompt,
                    response_tx,
                    ctx,
                })
                .await
            {
                tracing::error!("Failed to send query command to session actor: {}", err);
            }
        });

        (response_rx, Some(handle))
    }

    fn cleanup_session(&self, session_id: String) {
        if let Some((_, handle)) = self.sessions.remove(&session_id) {
            tokio::spawn(async move {
                if let Err(err) = handle.command_tx.send(SessionCommand::Shutdown).await {
                    tracing::warn!("Failed to send shutdown command: {}", err);
                }
            });
        }
    }

    fn interrupt_session(&self, session_id: String, ctx: egui::Context) {
        if let Some(handle) = self.sessions.get(&session_id) {
            let command_tx = handle.command_tx.clone();
            tokio::spawn(async move {
                if let Err(err) = command_tx.send(SessionCommand::Interrupt { ctx }).await {
                    tracing::warn!("Failed to send interrupt command: {}", err);
                }
            });
        }
    }

    fn set_permission_mode(&self, session_id: String, mode: PermissionMode, ctx: egui::Context) {
        if let Some(handle) = self.sessions.get(&session_id) {
            let command_tx = handle.command_tx.clone();
            tokio::spawn(async move {
                if let Err(err) = command_tx
                    .send(SessionCommand::SetPermissionMode { mode, ctx })
                    .await
                {
                    tracing::warn!("Failed to send set_permission_mode command: {}", err);
                }
            });
        } else {
            tracing::debug!(
                "Session {} not active, permission mode will apply on next query",
                session_id
            );
        }
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
