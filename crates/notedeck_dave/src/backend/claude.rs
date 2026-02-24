use crate::auto_accept::AutoAcceptRules;
use crate::backend::session_info::parse_session_info;
use crate::backend::tool_summary::{
    extract_response_content, format_tool_summary, truncate_output,
};
use crate::backend::traits::AiBackend;
use crate::messages::{
    CompactionInfo, DaveApiResponse, ExecutedTool, ParsedMarkdown, PendingPermission,
    PermissionRequest, PermissionResponse, SubagentInfo, SubagentStatus,
};
use crate::tools::Tool;
use crate::Message;
use claude_agent_sdk_rs::{
    ClaudeAgentOptions, ClaudeClient, ContentBlock, Message as ClaudeMessage, PermissionMode,
    PermissionResult, PermissionResultAllow, PermissionResultDeny, ToolResultContent, ToolUseBlock,
    UserContentBlock,
};
use dashmap::DashMap;
use futures::future::BoxFuture;
use futures::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Convert a ToolResultContent to a serde_json::Value for use with tool summary formatting
fn tool_result_content_to_value(content: &Option<ToolResultContent>) -> serde_json::Value {
    match content {
        Some(ToolResultContent::Text(s)) => serde_json::Value::String(s.clone()),
        Some(ToolResultContent::Blocks(blocks)) => serde_json::Value::Array(blocks.to_vec()),
        None => serde_json::Value::Null,
    }
}

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
                    prompt.push_str(content.text());
                    prompt.push_str("\n\n");
                }
                Message::ToolCalls(_)
                | Message::ToolResponse(_)
                | Message::Error(_)
                | Message::PermissionRequest(_)
                | Message::CompactionComplete(_)
                | Message::Subagent(_) => {
                    // Skip tool-related, error, permission, compaction, and subagent messages
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
async fn session_actor(
    session_id: String,
    cwd: Option<PathBuf>,
    resume_session_id: Option<String>,
    mut command_rx: tokio_mpsc::Receiver<SessionCommand>,
) {
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

    // Log if we're resuming a session
    if let Some(ref resume_id) = resume_session_id {
        tracing::info!(
            "Session {} will resume Claude session: {}",
            session_id,
            resume_id
        );
    }

    // Create client once - this maintains the persistent connection
    // Using match to handle the TypedBuilder's strict type requirements
    let options = match (&cwd, &resume_session_id) {
        (Some(dir), Some(resume_id)) => ClaudeAgentOptions::builder()
            .permission_mode(PermissionMode::Default)
            .stderr_callback(stderr_callback)
            .can_use_tool(can_use_tool)
            .include_partial_messages(true)
            .cwd(dir)
            .resume(resume_id)
            .build(),
        (Some(dir), None) => ClaudeAgentOptions::builder()
            .permission_mode(PermissionMode::Default)
            .stderr_callback(stderr_callback)
            .can_use_tool(can_use_tool)
            .include_partial_messages(true)
            .cwd(dir)
            .build(),
        (None, Some(resume_id)) => ClaudeAgentOptions::builder()
            .permission_mode(PermissionMode::Default)
            .stderr_callback(stderr_callback)
            .can_use_tool(can_use_tool)
            .include_partial_messages(true)
            .resume(resume_id)
            .build(),
        (None, None) => ClaudeAgentOptions::builder()
            .permission_mode(PermissionMode::Default)
            .stderr_callback(stderr_callback)
            .can_use_tool(can_use_tool)
            .include_partial_messages(true)
            .build(),
    };
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
                // Track active subagent nesting: tool results emitted while
                // a Task is in-flight belong to the top-of-stack subagent.
                let mut subagent_stack: Vec<String> = Vec::new();

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
                            // Check auto-accept rules
                            let auto_accept_rules = AutoAcceptRules::default();
                            if auto_accept_rules.should_auto_accept(&perm_req.tool_name, &perm_req.tool_input) {
                                tracing::debug!("Auto-accepting {}: matched auto-accept rule", perm_req.tool_name);
                                let _ = perm_req.response_tx.send(PermissionResult::Allow(PermissionResultAllow::default()));
                                continue;
                            }

                            // Forward permission request to UI
                            let request_id = Uuid::new_v4();
                            let (ui_resp_tx, ui_resp_rx) = oneshot::channel();

                            let cached_plan = if perm_req.tool_name == "ExitPlanMode" {
                                perm_req
                                    .tool_input
                                    .get("plan")
                                    .and_then(|v| v.as_str())
                                    .map(ParsedMarkdown::parse)
                            } else {
                                None
                            };

                            let request = PermissionRequest {
                                id: request_id,
                                tool_name: perm_req.tool_name.clone(),
                                tool_input: perm_req.tool_input.clone(),
                                response: None,
                                answer_summary: None,
                                cached_plan,
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

                            // Wait for UI response inline - blocking is OK since stream is
                            // waiting for permission result anyway
                            let tool_name = perm_req.tool_name.clone();
                            let result = match ui_resp_rx.await {
                                Ok(PermissionResponse::Allow { message }) => {
                                    if let Some(msg) = &message {
                                        tracing::debug!("User allowed tool {} with message: {}", tool_name, msg);
                                        // Inject user message into conversation so AI sees it
                                        if let Err(err) = client.query_with_content_and_session(
                                            vec![UserContentBlock::text(msg.as_str())],
                                            &session_id
                                        ).await {
                                            tracing::error!("Failed to inject user message: {}", err);
                                        }
                                    } else {
                                        tracing::debug!("User allowed tool: {}", tool_name);
                                    }
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
                            let _ = perm_req.response_tx.send(result);
                        }

                        stream_result = stream.next() => {
                            match stream_result {
                                Some(Ok(message)) => {
                                    match message {
                                        ClaudeMessage::Assistant(assistant_msg) => {
                                            for block in &assistant_msg.message.content {
                                                if let ContentBlock::ToolUse(ToolUseBlock { id, name, input }) = block {
                                                    pending_tools.insert(id.clone(), (name.clone(), input.clone()));

                                                    // Emit SubagentSpawned for Task tool calls
                                                    if name == "Task" {
                                                        let description = input
                                                            .get("description")
                                                            .and_then(|v| v.as_str())
                                                            .unwrap_or("task")
                                                            .to_string();
                                                        let subagent_type = input
                                                            .get("subagent_type")
                                                            .and_then(|v| v.as_str())
                                                            .unwrap_or("unknown")
                                                            .to_string();

                                                        subagent_stack.push(id.clone());
                                                        let subagent_info = SubagentInfo {
                                                            task_id: id.clone(),
                                                            description,
                                                            subagent_type,
                                                            status: SubagentStatus::Running,
                                                            output: String::new(),
                                                            max_output_size: 4000,
                                                            tool_results: Vec::new(),
                                                        };
                                                        let _ = response_tx.send(DaveApiResponse::SubagentSpawned(subagent_info));
                                                        ctx.request_repaint();
                                                    }
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

                                            // Extract usage metrics
                                            let (input_tokens, output_tokens) = result_msg
                                                .usage
                                                .as_ref()
                                                .map(|u| {
                                                    let inp = u.get("input_tokens")
                                                        .and_then(|v| v.as_u64())
                                                        .unwrap_or(0);
                                                    let out = u.get("output_tokens")
                                                        .and_then(|v| v.as_u64())
                                                        .unwrap_or(0);
                                                    (inp, out)
                                                })
                                                .unwrap_or((0, 0));

                                            let usage_info = crate::messages::UsageInfo {
                                                input_tokens,
                                                output_tokens,
                                                cost_usd: result_msg.total_cost_usd,
                                                num_turns: result_msg.num_turns,
                                            };
                                            let _ = response_tx.send(DaveApiResponse::QueryComplete(usage_info));

                                            stream_done = true;
                                        }
                                        ClaudeMessage::User(user_msg) => {
                                            // Tool results are nested in extra["message"]["content"]
                                            // since the SDK's UserMessage.content field doesn't
                                            // capture the inner message's content array.
                                            let content_blocks: Vec<ContentBlock> = user_msg
                                                .extra
                                                .get("message")
                                                .and_then(|m| m.get("content"))
                                                .and_then(|c| c.as_array())
                                                .map(|arr| {
                                                    arr.iter()
                                                        .filter_map(|v| serde_json::from_value::<ContentBlock>(v.clone()).ok())
                                                        .collect()
                                                })
                                                .unwrap_or_default();

                                            for block in &content_blocks {
                                                if let ContentBlock::ToolResult(tool_result_block) = block {
                                                    let tool_use_id = &tool_result_block.tool_use_id;
                                                    if let Some((tool_name, tool_input)) = pending_tools.remove(tool_use_id) {
                                                        let result_value = tool_result_content_to_value(&tool_result_block.content);

                                                        // Check if this is a Task tool completion
                                                        if tool_name == "Task" {
                                                            // Pop this subagent from the stack
                                                            subagent_stack.retain(|id| id != tool_use_id);
                                                            let result_text = extract_response_content(&result_value)
                                                                .unwrap_or_else(|| "completed".to_string());
                                                            let _ = response_tx.send(DaveApiResponse::SubagentCompleted {
                                                                task_id: tool_use_id.to_string(),
                                                                result: truncate_output(&result_text, 2000),
                                                            });
                                                        }

                                                        // Attach parent subagent context (top of stack)
                                                        let parent_task_id = subagent_stack.last().cloned();
                                                        let summary = format_tool_summary(&tool_name, &tool_input, &result_value);
                                                        let tool_result = ExecutedTool { tool_name, summary, parent_task_id };
                                                        let _ = response_tx.send(DaveApiResponse::ToolResult(tool_result));
                                                        ctx.request_repaint();
                                                    }
                                                }
                                            }
                                        }
                                        ClaudeMessage::System(system_msg) => {
                                            // Handle system init message - extract session info
                                            if system_msg.subtype == "init" {
                                                let session_info = parse_session_info(&system_msg);
                                                let _ = response_tx.send(DaveApiResponse::SessionInfo(session_info));
                                                ctx.request_repaint();
                                            } else if system_msg.subtype == "status" {
                                                // Handle status messages (compaction start/end)
                                                let status = system_msg.data.get("status")
                                                    .and_then(|v| v.as_str());
                                                if status == Some("compacting") {
                                                    let _ = response_tx.send(DaveApiResponse::CompactionStarted);
                                                    ctx.request_repaint();
                                                }
                                                // status: null means compaction finished (handled by compact_boundary)
                                            } else if system_msg.subtype == "compact_boundary" {
                                                // Compaction completed - extract token savings info
                                                tracing::debug!("compact_boundary data: {:?}", system_msg.data);
                                                let pre_tokens = system_msg.data.get("pre_tokens")
                                                    .and_then(|v| v.as_u64())
                                                    .unwrap_or(0);
                                                let info = CompactionInfo { pre_tokens };
                                                let _ = response_tx.send(DaveApiResponse::CompactionComplete(info));
                                                ctx.request_repaint();
                                            } else {
                                                tracing::debug!("Received system message subtype: {}", system_msg.subtype);
                                            }
                                        }
                                        ClaudeMessage::ControlCancelRequest(_) => {
                                            // Ignore internal control messages
                                        }
                                    }
                                }
                                Some(Err(err)) => {
                                    // Non-fatal: unknown message types (e.g. rate_limit_event)
                                    // cause deserialization errors but the stream continues.
                                    tracing::warn!("Claude stream message skipped: {}", err);
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
                tracing::debug!(
                    "Session {} setting permission mode to {:?}",
                    session_id,
                    mode
                );
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
        cwd: Option<PathBuf>,
        resume_session_id: Option<String>,
        ctx: egui::Context,
    ) -> (
        mpsc::Receiver<DaveApiResponse>,
        Option<tokio::task::JoinHandle<()>>,
    ) {
        let (response_tx, response_rx) = mpsc::channel();

        // For resumed sessions, always send just the latest message since
        // Claude Code already has the full conversation context via --resume.
        // For new sessions, send full prompt on the first message.
        let prompt = if resume_session_id.is_some() {
            Self::get_latest_user_message(&messages)
        } else {
            let is_first_message = messages
                .iter()
                .filter(|m| matches!(m, Message::User(_)))
                .count()
                == 1;
            if is_first_message {
                Self::messages_to_prompt(&messages)
            } else {
                Self::get_latest_user_message(&messages)
            }
        };

        tracing::debug!(
            "Sending request to Claude Code: session={}, resumed={}, prompt length: {}, preview: {:?}",
            session_id,
            resume_session_id.is_some(),
            prompt.len(),
            &prompt[..prompt.len().min(100)]
        );

        // Get or create session actor
        let command_tx = {
            let entry = self.sessions.entry(session_id.clone());
            let handle = entry.or_insert_with(|| {
                let (command_tx, command_rx) = tokio_mpsc::channel(16);

                // Spawn session actor with cwd and optional resume session ID
                let session_id_clone = session_id.clone();
                let cwd_clone = cwd.clone();
                let resume_session_id_clone = resume_session_id.clone();
                tokio::spawn(async move {
                    session_actor(
                        session_id_clone,
                        cwd_clone,
                        resume_session_id_clone,
                        command_rx,
                    )
                    .await;
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
