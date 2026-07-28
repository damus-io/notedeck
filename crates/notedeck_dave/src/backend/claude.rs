use crate::backend::session_info::parse_session_info;
use crate::backend::shared::{self, SessionCommand, SessionHandle};
use crate::backend::task_tracker::TaskTracker;
use crate::backend::tool_summary::extract_response_content;
use crate::backend::traits::AiBackend;
use crate::file_update::FileUpdate;
use crate::messages::{
    CompactionInfo, DaveApiResponse, PermissionResponse, SubagentInfo, SubagentStatus,
};
use crate::tools::Tool;
use crate::Message;
use claude_agent_sdk_rs::{
    ClaudeAgentOptions, ClaudeClient, ContentBlock, Message as ClaudeMessage, PermissionMode,
    PermissionResult, PermissionResultAllow, PermissionResultDeny, ToolResultBlock,
    ToolResultContent, ToolUseBlock, UserContentBlock, UserMessage,
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

/// Build a list of `UserContentBlock`s from image attachments and optional prompt text.
/// Images are placed first, then the text block (if non-empty).
fn build_content_blocks(
    images: &[crate::messages::ImageAttachment],
    prompt: &str,
) -> Vec<UserContentBlock> {
    use base64::Engine as _;
    let mut blocks: Vec<UserContentBlock> = images
        .iter()
        .filter_map(|img| {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&img.bytes);
            match UserContentBlock::image_base64(&img.mime_type, &b64) {
                Ok(block) => Some(block),
                Err(err) => {
                    tracing::warn!("Skipping invalid image attachment: {}", err);
                    None
                }
            }
        })
        .collect();
    if !prompt.is_empty() {
        blocks.push(UserContentBlock::text(prompt));
    }
    blocks
}

/// Convert a ToolResultContent to a serde_json::Value for use with tool summary formatting
fn tool_result_content_to_value(content: &Option<ToolResultContent>) -> serde_json::Value {
    match content {
        Some(ToolResultContent::Text(s)) => serde_json::Value::String(s.clone()),
        Some(ToolResultContent::Blocks(blocks)) => serde_json::Value::Array(blocks.to_vec()),
        None => serde_json::Value::Null,
    }
}

/// Tool results are nested in `extra["message"]["content"]` because the SDK's
/// `UserMessage.content` field doesn't capture the inner message's content array.
fn parse_user_content_blocks(user_msg: &UserMessage) -> Vec<ContentBlock> {
    user_msg
        .extra
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value::<ContentBlock>(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a tool_use requests background execution (`run_in_background: true`).
fn is_background_task(input: &serde_json::Value) -> bool {
    input
        .get("run_in_background")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Process a single tool result: complete any in-flight subagent, fold the
/// harness task list into the sidebar, and forward the result to the UI.
///
/// `parent_override` is the message's `parent_tool_use_id` when set — a
/// subagent-internal result attributes to that (root) subagent regardless of
/// the foreground `subagent_stack`.
fn handle_tool_result(
    tool_result: &ToolResultBlock,
    parent_override: Option<&str>,
    pending_tools: &mut HashMap<String, (String, serde_json::Value)>,
    subagent_stack: &mut Vec<String>,
    task_tracker: &mut TaskTracker,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    ctx: &egui::Context,
) {
    let tool_use_id = &tool_result.tool_use_id;
    let Some((tool_name, tool_input)) = pending_tools.remove(tool_use_id) else {
        return;
    };
    let result_value = tool_result_content_to_value(&tool_result.content);

    // A foreground Task tool completion ends the current subagent. A background
    // subagent's launch produces an immediate tool result ("Async agent
    // launched successfully") that is NOT completion — it completes later via
    // `task_notification`, so skip it here.
    if tool_name == "Task" && !is_background_task(&tool_input) {
        let result_text =
            extract_response_content(&result_value).unwrap_or_else(|| "completed".to_string());
        shared::complete_subagent(tool_use_id, &result_text, subagent_stack, response_tx, ctx);
    }

    // Fold TaskCreate/TaskUpdate into the task list sidebar (the id is
    // assigned in the result, so this has to happen at result time).
    if let Some(todos) = task_tracker.handle_tool(&tool_name, &tool_input, &result_value) {
        let _ = response_tx.send(DaveApiResponse::TodoUpdate(todos));
        ctx.request_repaint();
    }

    let file_update = FileUpdate::from_tool_call(&tool_name, &tool_input);
    shared::send_tool_result(
        &tool_name,
        &tool_input,
        &result_value,
        file_update,
        parent_override,
        subagent_stack,
        response_tx,
        ctx,
    );
}

/// Handle a `system` / `task_started` message: a background task began.
///
/// Only `local_agent` tasks (background subagents) get a sidebar entry — a
/// background `local_bash` still renders as an ordinary Bash tool result in
/// chat. The entry is keyed by the originating `tool_use_id`, which matches
/// both the `parent_tool_use_id` on the subagent's internal messages and the
/// `tool_use_id` on its eventual `task_notification`.
fn handle_task_started(
    data: &serde_json::Value,
    pending_tools: &HashMap<String, (String, serde_json::Value)>,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    ctx: &egui::Context,
) {
    if data.get("task_type").and_then(|v| v.as_str()) != Some("local_agent") {
        return;
    }
    let Some(tool_use_id) = data.get("tool_use_id").and_then(|v| v.as_str()) else {
        return;
    };

    // `task_started` carries a description but not the subagent type; recover
    // the type from the originating Task tool_use input still in `pending_tools`
    // (it's removed only when its launch tool result lands).
    let spawn_input = pending_tools.get(tool_use_id).map(|(_, input)| input);
    let description = data
        .get("description")
        .and_then(|v| v.as_str())
        .or_else(|| spawn_input.and_then(|i| i.get("description").and_then(|v| v.as_str())))
        .unwrap_or("background task")
        .to_string();
    let subagent_type = spawn_input
        .and_then(|i| i.get("subagent_type").and_then(|v| v.as_str()))
        .unwrap_or("agent")
        .to_string();

    let subagent_info = SubagentInfo {
        task_id: tool_use_id.to_string(),
        description,
        subagent_type,
        status: SubagentStatus::Running,
        output: String::new(),
        max_output_size: 4000,
        tool_results: Vec::new(),
        background: true,
    };
    let _ = response_tx.send(DaveApiResponse::SubagentSpawned(subagent_info));
    ctx.request_repaint();
}

/// Handle a `system` / `task_notification` message: a background task finished.
///
/// Completes (or fails) the subagent entry keyed by `tool_use_id`. This is the
/// authoritative completion for a background subagent — its launch tool result
/// only confirmed the task started.
fn handle_task_notification(
    data: &serde_json::Value,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    ctx: &egui::Context,
) {
    let Some(tool_use_id) = data.get("tool_use_id").and_then(|v| v.as_str()) else {
        return;
    };
    let status = data.get("status").and_then(|v| v.as_str());
    let summary = data
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("completed")
        .to_string();

    let response = if status == Some("completed") {
        DaveApiResponse::SubagentCompleted {
            task_id: tool_use_id.to_string(),
            result: summary,
        }
    } else {
        DaveApiResponse::SubagentFailed {
            task_id: tool_use_id.to_string(),
            error: summary,
        }
    };
    let _ = response_tx.send(response);
    ctx.request_repaint();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelledTurnMessageAction {
    Ignore,
    FinishTurn,
}

/// Decide how to handle a Claude stream message after the user has cancelled the turn.
fn cancelled_turn_message_action(message: &ClaudeMessage) -> CancelledTurnMessageAction {
    match message {
        ClaudeMessage::Result(_) => CancelledTurnMessageAction::FinishTurn,
        // These variants are still part of the cancelled turn and must not
        // leak into chat after the user exits the tool call.
        ClaudeMessage::Assistant(_)
        | ClaudeMessage::System(_)
        | ClaudeMessage::StreamEvent(_)
        | ClaudeMessage::User(_)
        | ClaudeMessage::ControlCancelRequest(_) => CancelledTurnMessageAction::Ignore,
    }
}

/// Handle a single message from the continuous Claude stream.
///
/// This runs for every message the CLI emits, whether it belongs to a
/// user-initiated turn or a spontaneous wake-up turn (a `run_in_background`
/// task completing). On a `Result` it emits `QueryComplete`, which is the
/// explicit turn boundary the UI keys off (the session channel stays open).
fn handle_stream_message(
    message: ClaudeMessage,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    ctx: &egui::Context,
    pending_tools: &mut HashMap<String, (String, serde_json::Value)>,
    subagent_stack: &mut Vec<String>,
    task_tracker: &mut TaskTracker,
) {
    match message {
        ClaudeMessage::Assistant(assistant_msg) => {
            // Emit a per-turn UsageUpdate so the context bar
            // reflects the current context window state.
            // input_tokens alone is wrong when caching is active —
            // actual context = input + cache_creation + cache_read.
            if let Some(usage) = &assistant_msg.message.usage {
                let extract = |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
                let usage_info = crate::messages::UsageInfo {
                    input_tokens: extract("input_tokens"),
                    cache_creation_input_tokens: extract("cache_creation_input_tokens"),
                    cache_read_input_tokens: extract("cache_read_input_tokens"),
                    output_tokens: extract("output_tokens"),
                    ..Default::default()
                };
                let _ = response_tx.send(DaveApiResponse::UsageUpdate(usage_info));
                ctx.request_repaint();
            }

            for block in &assistant_msg.message.content {
                if let ContentBlock::ToolUse(ToolUseBlock { id, name, input }) = block {
                    pending_tools.insert(id.clone(), (name.clone(), input.clone()));

                    // Emit SubagentSpawned for foreground Task tool calls. A
                    // background subagent (`run_in_background`) is spawned from
                    // its `task_started` system message instead — it outlives
                    // this turn and completes on a wake-up, so it must not join
                    // the foreground `subagent_stack` nor complete on its launch
                    // tool result.
                    if name == "Task" && !is_background_task(input) {
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
                            background: false,
                        };
                        let _ = response_tx.send(DaveApiResponse::SubagentSpawned(subagent_info));
                        ctx.request_repaint();
                    }

                    // Emit TodoUpdate for TodoWrite tool calls
                    if name == "TodoWrite" {
                        let _ = response_tx.send(DaveApiResponse::TodoUpdate(input.clone()));
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
                        if response_tx
                            .send(DaveApiResponse::Token(text.to_string()))
                            .is_err()
                        {
                            tracing::error!("Failed to send token to UI");
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
            tracing::debug!(
                "ResultMessage usage: {:?}, total_cost_usd: {:?}, num_turns: {}",
                result_msg.usage,
                result_msg.total_cost_usd,
                result_msg.num_turns
            );
            let usage_info = result_msg
                .usage
                .as_ref()
                .map(|u| {
                    let extract = |key: &str| u.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
                    crate::messages::UsageInfo {
                        input_tokens: extract("input_tokens"),
                        cache_creation_input_tokens: extract("cache_creation_input_tokens"),
                        cache_read_input_tokens: extract("cache_read_input_tokens"),
                        output_tokens: extract("output_tokens"),
                        cost_usd: result_msg.total_cost_usd,
                        num_turns: result_msg.num_turns,
                    }
                })
                .unwrap_or_else(|| crate::messages::UsageInfo {
                    cost_usd: result_msg.total_cost_usd,
                    num_turns: result_msg.num_turns,
                    ..Default::default()
                });
            let _ = response_tx.send(DaveApiResponse::QueryComplete(usage_info));
        }
        ClaudeMessage::User(user_msg) => {
            // A subagent's internal tool results carry `parent_tool_use_id` =
            // the originating (root) Task tool_use id, which is the key of its
            // sidebar entry. Route by it so background-subagent output folds
            // into the right entry even though it arrives on a wake-up turn with
            // no foreground `subagent_stack` context.
            let parent_override = user_msg.parent_tool_use_id.as_deref();
            for block in parse_user_content_blocks(&user_msg) {
                if let ContentBlock::ToolResult(tool_result) = block {
                    handle_tool_result(
                        &tool_result,
                        parent_override,
                        pending_tools,
                        subagent_stack,
                        task_tracker,
                        response_tx,
                        ctx,
                    );
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
                let status = system_msg.data.get("status").and_then(|v| v.as_str());
                if status == Some("compacting") {
                    let _ = response_tx.send(DaveApiResponse::CompactionStarted);
                    ctx.request_repaint();
                }
                // status: null means compaction finished (handled by compact_boundary)
            } else if system_msg.subtype == "compact_boundary" {
                // Compaction completed - extract token savings info
                tracing::debug!("compact_boundary data: {:?}", system_msg.data);
                let pre_tokens = system_msg
                    .data
                    .get("pre_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let info = CompactionInfo { pre_tokens };
                let _ = response_tx.send(DaveApiResponse::CompactionComplete(info));
                ctx.request_repaint();
            } else if system_msg.subtype == "task_started" {
                handle_task_started(&system_msg.data, pending_tools, response_tx, ctx);
            } else if system_msg.subtype == "task_notification" {
                handle_task_notification(&system_msg.data, response_tx, ctx);
            } else {
                tracing::debug!("Received system message subtype: {}", system_msg.subtype);
            }
        }
        ClaudeMessage::ControlCancelRequest(_) => {
            // Ignore internal control messages
        }
    }
}

/// Handle a permission request forwarded from the `can_use_tool` callback.
///
/// Forwards the request to the UI and relays the user's decision back to the
/// SDK. If the user exits the tool (Cancel) or the channel closes, the current
/// turn is cancelled: `*cancel_current_turn` is set and the client is
/// interrupted so the in-flight turn stops cleanly.
async fn handle_permission_request(
    perm_req: PermissionRequestInternal,
    client: &ClaudeClient,
    session_id: &str,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    ctx: &egui::Context,
    cancel_current_turn: &mut bool,
) {
    if shared::should_auto_accept(&perm_req.tool_name, &perm_req.tool_input) {
        let _ = perm_req
            .response_tx
            .send(PermissionResult::Allow(PermissionResultAllow::default()));
        return;
    }

    let ui_resp_rx = match shared::forward_permission_to_ui(
        &perm_req.tool_name,
        perm_req.tool_input.clone(),
        response_tx,
        ctx,
    ) {
        Some(rx) => rx,
        None => {
            let _ = perm_req
                .response_tx
                .send(PermissionResult::Deny(PermissionResultDeny {
                    message: "UI channel closed".to_string(),
                    interrupt: true,
                }));
            return;
        }
    };

    // Wait for the UI response. Permission requests should remain pending until
    // the user explicitly answers or the channel closes.
    let tool_name = perm_req.tool_name.clone();
    let (result, should_cancel_turn) = match ui_resp_rx.await {
        Ok(PermissionResponse::Allow { message }) => {
            if let Some(msg) = &message {
                tracing::debug!("User allowed tool {} with message: {}", tool_name, msg);
                // Inject user message into conversation so AI sees it
                if let Err(err) = client
                    .query_with_content_and_session(
                        vec![UserContentBlock::text(msg.as_str())],
                        session_id,
                    )
                    .await
                {
                    tracing::error!("Failed to inject user message: {}", err);
                    (
                        PermissionResult::Deny(PermissionResultDeny {
                            message: "The user approved this tool with a condition, but the condition could not be delivered. Deny to prevent unconditional execution. Ask the user to try again.".to_string(),
                            interrupt: false,
                        }),
                        false,
                    )
                } else {
                    (
                        PermissionResult::Allow(PermissionResultAllow::default()),
                        false,
                    )
                }
            } else {
                tracing::debug!("User allowed tool: {}", tool_name);
                (
                    PermissionResult::Allow(PermissionResultAllow::default()),
                    false,
                )
            }
        }
        Ok(PermissionResponse::Deny { reason }) => {
            tracing::debug!("User denied tool {}: {}", tool_name, reason);
            (
                PermissionResult::Deny(PermissionResultDeny {
                    message: reason,
                    interrupt: false,
                }),
                false,
            )
        }
        Ok(PermissionResponse::Cancel { reason }) => {
            tracing::debug!(
                "User exited tool {} and cancelled the turn: {}",
                tool_name,
                reason
            );
            (
                PermissionResult::Deny(PermissionResultDeny {
                    message: reason,
                    interrupt: true,
                }),
                true,
            )
        }
        Err(_) => {
            tracing::error!("Permission response channel closed");
            (
                PermissionResult::Deny(PermissionResultDeny {
                    message: "Permission request cancelled".to_string(),
                    interrupt: true,
                }),
                true,
            )
        }
    };
    let _ = perm_req.response_tx.send(result);
    if should_cancel_turn {
        *cancel_current_turn = true;
        if let Err(err) = client.interrupt().await {
            tracing::error!(
                "Failed to interrupt Claude session {} after tool exit: {}",
                session_id,
                err
            );
        }
    }
}

pub struct ClaudeBackend {
    /// Registry of active sessions (using dashmap for lock-free access)
    sessions: DashMap<String, SessionHandle>,
}

impl Default for ClaudeBackend {
    fn default() -> Self {
        Self {
            sessions: DashMap::new(),
        }
    }
}

impl ClaudeBackend {
    pub fn new() -> Self {
        Self::default()
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
    model: Option<String>,
    mut command_rx: tokio_mpsc::Receiver<SessionCommand>,
    // The session-lifetime UI channel. Created once when the actor is spawned and
    // used for every turn — user-initiated AND spontaneous wake-up turns — so
    // background-task completions reach the UI even between user queries.
    response_tx: mpsc::Sender<DaveApiResponse>,
    // Fallback egui context; refreshed from each Query/Compact command so
    // wake-up turns (which carry no command) can still request repaints.
    initial_ctx: egui::Context,
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
    let mut options = match (&cwd, &resume_session_id) {
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
    if model.is_some() {
        options.model = model;
    }
    let mut client = ClaudeClient::new(options);

    // Connect once - this starts the subprocess
    if let Err(err) = client.connect().await {
        tracing::error!("Session {} failed to connect: {}", session_id, err);
        // Report the failure on the session channel, then drain commands until
        // shutdown so callers don't block on a dead session.
        let _ = response_tx.send(DaveApiResponse::Failed(format!(
            "Failed to connect to Claude: {}",
            err
        )));
        while let Some(cmd) = command_rx.recv().await {
            if matches!(cmd, SessionCommand::Shutdown) {
                break;
            }
        }
        return;
    }

    tracing::debug!("Session {} connected successfully", session_id);

    // Tracks the harness task list across turns. `TaskCreate`/`TaskUpdate` are
    // incremental, so this must outlive the per-query loop below.
    let mut task_tracker = TaskTracker::new();

    // Persistent per-session state. `pending_tools`/`subagent_stack` must
    // outlive a single turn: a `run_in_background` task's tool_use lands in one
    // turn while its tool_result / completion lands in a later wake-up turn, so
    // attribution needs them to survive across turns.
    let mut ctx = initial_ctx;
    let mut pending_tools: HashMap<String, (String, serde_json::Value)> = HashMap::new();
    let mut subagent_stack: Vec<String> = Vec::new();
    // Set when the user exits a tool call; suppresses the rest of that turn's
    // messages until its `Result`, then clears at the turn boundary.
    let mut cancel_current_turn = false;

    // Pump the CLI message stream continuously, not just while servicing a
    // Query. This is the non-breaking `receive_messages()` variant, held for the
    // whole actor lifetime, so spontaneous wake-up turns (a background task
    // completing) flow through the same handler as user-initiated turns. All
    // client calls below are `&self` (query_with_content_and_session /
    // interrupt / set_permission_mode) so they coexist with this borrow;
    // `disconnect` (&mut) runs only after the loop, once the stream is dropped.
    let mut message_stream = client.receive_messages();

    loop {
        tokio::select! {
            biased;

            // Commands from the UI / backend.
            cmd = command_rx.recv() => {
                let Some(cmd) = cmd else {
                    // Command channel closed — the backend dropped the handle.
                    break;
                };
                match cmd {
                    SessionCommand::Query { prompt, images, ctx: query_ctx, .. } => {
                        // A fresh user turn: refresh ctx and clear any leftover
                        // cancellation from a previous turn.
                        ctx = query_ctx;
                        cancel_current_turn = false;
                        let blocks = build_content_blocks(&images, &prompt);
                        if let Err(err) = client
                            .query_with_content_and_session(blocks, &session_id)
                            .await
                        {
                            tracing::error!("Session {} query error: {}", session_id, err);
                            let _ = response_tx.send(DaveApiResponse::Failed(err.to_string()));
                        }
                    }
                    SessionCommand::Interrupt { ctx: interrupt_ctx } => {
                        tracing::debug!("Session {} received interrupt", session_id);
                        if let Err(err) = client.interrupt().await {
                            tracing::error!("Failed to send interrupt: {}", err);
                        }
                        // The stream ends naturally with a Result; the CLI
                        // preserves session history.
                        interrupt_ctx.request_repaint();
                    }
                    SessionCommand::SetPermissionMode { mode, ctx: mode_ctx } => {
                        tracing::debug!(
                            "Session {} setting permission mode to {:?}",
                            session_id,
                            mode
                        );
                        if let Err(err) = client.set_permission_mode(mode).await {
                            tracing::error!("Failed to set permission mode: {}", err);
                        }
                        mode_ctx.request_repaint();
                    }
                    SessionCommand::Compact { ctx: compact_ctx, .. } => {
                        // Claude compaction is driven by sending `/compact` as a
                        // query on the persistent channel (see compact_session).
                        ctx = compact_ctx;
                        if let Err(err) = client
                            .query_with_content_and_session(
                                vec![UserContentBlock::text("/compact")],
                                &session_id,
                            )
                            .await
                        {
                            tracing::error!("Session {} compact error: {}", session_id, err);
                            let _ = response_tx.send(DaveApiResponse::Failed(err.to_string()));
                        }
                    }
                    SessionCommand::Shutdown => {
                        tracing::debug!("Session actor {} shutting down", session_id);
                        break;
                    }
                }
            }

            // Permission requests (they block the SDK until answered).
            Some(perm_req) = perm_rx.recv() => {
                handle_permission_request(
                    perm_req,
                    &client,
                    &session_id,
                    &response_tx,
                    &ctx,
                    &mut cancel_current_turn,
                )
                .await;
            }

            // The continuous CLI message stream.
            msg = message_stream.next() => {
                let Some(result) = msg else {
                    // Stream closed — the CLI exited. Nothing more will arrive.
                    break;
                };
                let message = match result {
                    Ok(message) => message,
                    Err(err) => {
                        // Non-fatal: unknown message types (e.g. rate_limit_event)
                        // fail to deserialize but the stream continues.
                        tracing::warn!("Claude stream message skipped: {}", err);
                        continue;
                    }
                };

                // While a turn is cancelled, drop its remaining messages until
                // the Result, which we still handle (to emit completion) before
                // clearing the flag at the turn boundary.
                if cancel_current_turn {
                    match cancelled_turn_message_action(&message) {
                        CancelledTurnMessageAction::Ignore => {
                            tracing::debug!(
                                "Suppressing Claude message after cancelled turn: {:?}",
                                std::mem::discriminant(&message)
                            );
                            continue;
                        }
                        CancelledTurnMessageAction::FinishTurn => {
                            cancel_current_turn = false;
                        }
                    }
                }

                handle_stream_message(
                    message,
                    &response_tx,
                    &ctx,
                    &mut pending_tools,
                    &mut subagent_stack,
                    &mut task_tracker,
                );
            }
        }
    }

    // Drop the stream's borrow of `client` before the &mut disconnect.
    drop(message_stream);
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
        model: Option<String>,
        _user_id: String,
        session_id: String,
        cwd: Option<PathBuf>,
        resume_session_id: Option<String>,
        ctx: egui::Context,
    ) -> (
        Option<mpsc::Receiver<DaveApiResponse>>,
        Option<tokio::task::JoinHandle<()>>,
    ) {
        let (prompt, images) = shared::prepare_prompt_and_images(&messages, &resume_session_id);

        tracing::debug!(
            "Sending request to Claude Code: session={}, resumed={}, prompt length: {}, preview: {:?}",
            session_id,
            resume_session_id.is_some(),
            prompt.len(),
            &prompt[..prompt.len().min(100)]
        );

        // Get or create the session actor. The UI response channel is created
        // ONCE, when the actor is first spawned, and lives for the whole session
        // — the actor forwards both user-initiated and spontaneous wake-up turns
        // on it. On subsequent turns the caller keeps its existing receiver, so
        // `created_rx` stays None and we don't hand back a second one.
        let mut created_rx: Option<mpsc::Receiver<DaveApiResponse>> = None;
        let command_tx = {
            let entry = self.sessions.entry(session_id.clone());
            let handle = entry.or_insert_with(|| {
                let (command_tx, command_rx) = tokio_mpsc::channel(16);
                let (response_tx, response_rx) = mpsc::channel();
                created_rx = Some(response_rx);

                // Spawn session actor with cwd, optional resume session ID, model,
                // and the session-lifetime response channel + initial ctx.
                let session_id_clone = session_id.clone();
                let cwd_clone = cwd.clone();
                let resume_session_id_clone = resume_session_id.clone();
                let model_clone = model.clone();
                let ctx_clone = ctx.clone();
                tokio::spawn(async move {
                    session_actor(
                        session_id_clone,
                        cwd_clone,
                        resume_session_id_clone,
                        model_clone,
                        command_rx,
                        response_tx,
                        ctx_clone,
                    )
                    .await;
                });

                SessionHandle { command_tx }
            });
            handle.command_tx.clone()
        };

        // Spawn a task to send the query command. Claude's actor owns the
        // persistent response channel, so the command carries none.
        let handle = tokio::spawn(async move {
            if let Err(err) = command_tx
                .send(SessionCommand::Query {
                    prompt,
                    images,
                    response_tx: None,
                    ctx,
                })
                .await
            {
                tracing::error!("Failed to send query command to session actor: {}", err);
            }
        });

        (created_rx, Some(handle))
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

    fn compact_session(
        &self,
        session_id: String,
        ctx: egui::Context,
    ) -> Option<mpsc::Receiver<DaveApiResponse>> {
        let handle = self.sessions.get(&session_id)?;
        let command_tx = handle.command_tx.clone();
        // Compaction responses flow on the session's persistent channel (already
        // installed by the caller), so send `/compact` as a query with no
        // response channel and return None — the caller keeps its receiver.
        tokio::spawn(async move {
            if let Err(err) = command_tx
                .send(SessionCommand::Query {
                    prompt: "/compact".to_string(),
                    images: vec![],
                    response_tx: None,
                    ctx,
                })
                .await
            {
                tracing::warn!("Failed to send compact query to claude session: {}", err);
            }
        });
        None
    }

    fn persistent_stream(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::AssistantMessage;

    #[test]
    fn cancelled_turn_suppresses_follow_up_messages_until_result() {
        let assistant = serde_json::from_value::<ClaudeMessage>(serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{ "type": "text", "text": "extra output" }]
            }
        }))
        .expect("assistant message should deserialize");
        let stream_event = serde_json::from_value::<ClaudeMessage>(serde_json::json!({
            "type": "stream_event",
            "uuid": "evt-1",
            "session_id": "sess-1",
            "event": {
                "type": "content_block_delta",
                "delta": { "text": "more tokens" }
            }
        }))
        .expect("stream event should deserialize");
        let result = serde_json::from_value::<ClaudeMessage>(serde_json::json!({
            "type": "result",
            "subtype": "success",
            "duration_ms": 1,
            "duration_api_ms": 1,
            "is_error": false,
            "num_turns": 1,
            "session_id": "sess-1"
        }))
        .expect("result message should deserialize");

        assert_eq!(
            cancelled_turn_message_action(&assistant),
            CancelledTurnMessageAction::Ignore
        );
        assert_eq!(
            cancelled_turn_message_action(&stream_event),
            CancelledTurnMessageAction::Ignore
        );
        assert_eq!(
            cancelled_turn_message_action(&result),
            CancelledTurnMessageAction::FinishTurn
        );
    }

    #[test]
    fn task_started_local_agent_spawns_background_subagent() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();

        // The originating Task tool_use is still pending (its launch result
        // hasn't landed), so the subagent type is recoverable from its input.
        let mut pending: HashMap<String, (String, serde_json::Value)> = HashMap::new();
        pending.insert(
            "toolu_root".to_string(),
            (
                "Task".to_string(),
                serde_json::json!({ "subagent_type": "general-purpose", "run_in_background": true }),
            ),
        );

        let data = serde_json::json!({
            "task_id": "abc123",
            "tool_use_id": "toolu_root",
            "description": "do background work",
            "task_type": "local_agent",
        });
        handle_task_started(&data, &pending, &tx, &ctx);

        match rx.try_recv().expect("expected a spawn response") {
            DaveApiResponse::SubagentSpawned(info) => {
                // Keyed by tool_use_id so parent_tool_use_id + task_notification align.
                assert_eq!(info.task_id, "toolu_root");
                assert_eq!(info.subagent_type, "general-purpose");
                assert_eq!(info.description, "do background work");
                assert_eq!(info.status, SubagentStatus::Running);
                assert!(info.background);
            }
            other => panic!(
                "expected SubagentSpawned, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn task_started_local_bash_is_not_a_subagent() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let pending: HashMap<String, (String, serde_json::Value)> = HashMap::new();

        let data = serde_json::json!({
            "task_id": "b4dg5o2ra",
            "tool_use_id": "toolu_bash",
            "description": "sleep 6",
            "task_type": "local_bash",
        });
        handle_task_started(&data, &pending, &tx, &ctx);

        assert!(
            rx.try_recv().is_err(),
            "a background shell must not create a subagent sidebar entry"
        );
    }

    #[test]
    fn task_notification_completes_and_fails_by_tool_use_id() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();

        handle_task_notification(
            &serde_json::json!({
                "tool_use_id": "toolu_root",
                "status": "completed",
                "summary": "all done",
            }),
            &tx,
            &ctx,
        );
        match rx.try_recv().expect("expected a completion") {
            DaveApiResponse::SubagentCompleted { task_id, result } => {
                assert_eq!(task_id, "toolu_root");
                assert_eq!(result, "all done");
            }
            other => panic!(
                "expected SubagentCompleted, got {:?}",
                std::mem::discriminant(&other)
            ),
        }

        handle_task_notification(
            &serde_json::json!({
                "tool_use_id": "toolu_root",
                "status": "failed",
                "summary": "it broke",
            }),
            &tx,
            &ctx,
        );
        match rx.try_recv().expect("expected a failure") {
            DaveApiResponse::SubagentFailed { task_id, error } => {
                assert_eq!(task_id, "toolu_root");
                assert_eq!(error, "it broke");
            }
            other => panic!(
                "expected SubagentFailed, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn subagent_internal_tool_result_routes_by_parent_tool_use_id() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut pending: HashMap<String, (String, serde_json::Value)> = HashMap::new();
        let mut subagent_stack: Vec<String> = Vec::new();
        let mut task_tracker = TaskTracker::new();

        // The subagent's internal Bash tool_use registers in pending_tools.
        let assistant = serde_json::from_value::<ClaudeMessage>(serde_json::json!({
            "type": "assistant",
            "parent_tool_use_id": "toolu_root",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_bash",
                    "name": "Bash",
                    "input": { "command": "echo hi" }
                }]
            }
        }))
        .expect("assistant should deserialize");
        handle_stream_message(
            assistant,
            &tx,
            &ctx,
            &mut pending,
            &mut subagent_stack,
            &mut task_tracker,
        );

        // Its tool_result arrives with parent_tool_use_id = the root subagent.
        let user = serde_json::from_value::<ClaudeMessage>(serde_json::json!({
            "type": "user",
            "parent_tool_use_id": "toolu_root",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_bash",
                    "content": "hi"
                }]
            }
        }))
        .expect("user should deserialize");
        handle_stream_message(
            user,
            &tx,
            &ctx,
            &mut pending,
            &mut subagent_stack,
            &mut task_tracker,
        );

        let routed = rx.try_iter().any(|resp| {
            matches!(
                resp,
                DaveApiResponse::ToolResult(tool)
                    if tool.parent_task_id.as_deref() == Some("toolu_root")
            )
        });
        assert!(
            routed,
            "tool result should attribute to the root subagent via parent_tool_use_id"
        );
    }

    #[test]
    fn pending_messages_single_user() {
        let messages = vec![Message::User("hello".into())];
        assert_eq!(shared::get_pending_user_messages(&messages), "hello");
    }

    #[test]
    fn pending_messages_multiple_trailing_users() {
        let messages = vec![
            Message::User("first".into()),
            Message::Assistant(AssistantMessage::from_text("reply".into())),
            Message::User("second".into()),
            Message::User("third".into()),
            Message::User("fourth".into()),
        ];
        assert_eq!(
            shared::get_pending_user_messages(&messages),
            "second\nthird\nfourth"
        );
    }

    #[test]
    fn pending_messages_stops_at_non_user() {
        let messages = vec![
            Message::User("old".into()),
            Message::User("also old".into()),
            Message::Assistant(AssistantMessage::from_text("reply".into())),
            Message::User("pending".into()),
        ];
        assert_eq!(shared::get_pending_user_messages(&messages), "pending");
    }

    #[test]
    fn pending_messages_empty_when_last_is_assistant() {
        let messages = vec![
            Message::User("hello".into()),
            Message::Assistant(AssistantMessage::from_text("reply".into())),
        ];
        assert_eq!(shared::get_pending_user_messages(&messages), "");
    }

    #[test]
    fn pending_messages_empty_chat() {
        let messages: Vec<Message> = vec![];
        assert_eq!(shared::get_pending_user_messages(&messages), "");
    }

    #[test]
    fn pending_messages_stops_at_tool_response() {
        let messages = vec![
            Message::User("do something".into()),
            Message::Assistant(AssistantMessage::from_text("ok".into())),
            Message::ToolCalls(vec![crate::tools::ToolCall::invalid(
                "c1".into(),
                Some("Read".into()),
                None,
                "test".into(),
            )]),
            Message::ToolResponse(crate::tools::ToolResponse::error(
                "c1".into(),
                "result".into(),
            )),
            Message::User("queued 1".into()),
            Message::User("queued 2".into()),
        ];
        assert_eq!(
            shared::get_pending_user_messages(&messages),
            "queued 1\nqueued 2"
        );
    }

    #[test]
    fn pending_messages_preserves_order() {
        let messages = vec![
            Message::User("a".into()),
            Message::User("b".into()),
            Message::User("c".into()),
        ];
        assert_eq!(shared::get_pending_user_messages(&messages), "a\nb\nc");
    }
}
