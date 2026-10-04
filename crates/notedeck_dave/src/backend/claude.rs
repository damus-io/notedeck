use crate::backend::session_info::parse_session_info;
use crate::backend::shared::{self, SessionCommand, SessionHandle};
use crate::backend::task_tracker::TaskTracker;
use crate::backend::tool_summary::{extract_response_content, format_tool_summary};
use crate::backend::traits::AiBackend;
use crate::file_update::FileUpdate;
use crate::messages::{
    denial_marker_for_model, denial_message_for_model, permission_reply_message,
    turn_exit_message_for_model, CompactionInfo, DaveApiResponse, PermissionResponse, RunningTool,
    SubagentInfo, SubagentStatus,
};
use crate::tools::Tool;
use crate::Message;
use claude_agent_sdk_rs::{
    ClaudeAgentOptions, ClaudeClient, ContentBlock, Message as ClaudeMessage, PermissionMode,
    PermissionResult, PermissionResultAllow, PermissionResultDeny, ResultMessage, ToolResultBlock,
    ToolResultContent, ToolUseBlock, UserContentBlock, UserMessage,
};
use dashmap::DashMap;
use futures::future::BoxFuture;
use futures::StreamExt;
use notedeck::Waker;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::oneshot;
use tokio::time::Instant;

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

/// Whether a tool spawns a subagent. Claude Code renamed its `Task` tool to
/// `Agent`; both names still appear depending on the CLI version, and missing
/// either one silently drops that version's subagents.
fn is_subagent_tool(name: &str) -> bool {
    matches!(name, "Task" | "Agent")
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
    waker: &Waker,
) {
    let tool_use_id = &tool_result.tool_use_id;
    let Some((tool_name, tool_input)) = pending_tools.remove(tool_use_id) else {
        return;
    };
    let result_value = tool_result_content_to_value(&tool_result.content);

    // A foreground Task/Agent completion ends the current subagent. A background
    // subagent's launch produces an immediate tool result ("Async agent
    // launched successfully") that is NOT completion — it completes later via
    // `task_notification`, so skip it here.
    if is_subagent_tool(&tool_name) && !is_background_task(&tool_input) {
        let result_text =
            extract_response_content(&result_value).unwrap_or_else(|| "completed".to_string());
        shared::complete_subagent(
            tool_use_id,
            &result_text,
            subagent_stack,
            response_tx,
            waker,
        );
    }

    // Fold TaskCreate/TaskUpdate into the task list sidebar (the id is
    // assigned in the result, so this has to happen at result time).
    if let Some(todos) = task_tracker.handle_tool(&tool_name, &tool_input, &result_value) {
        let _ = response_tx.send(DaveApiResponse::TodoUpdate(todos));
        waker.wake();
    }

    let file_update = FileUpdate::from_tool_call(&tool_name, &tool_input);
    shared::send_tool_result(
        &tool_name,
        &tool_input,
        &result_value,
        file_update,
        parent_override,
        subagent_stack,
        Some(tool_use_id),
        response_tx,
        waker,
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
    waker: &Waker,
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
    waker.wake();
}

/// Handle a `system` / `task_notification` message: a background task finished.
///
/// Completes (or fails) the subagent entry keyed by `tool_use_id`. This is the
/// authoritative completion for a background subagent — its launch tool result
/// only confirmed the task started.
fn handle_task_notification(
    data: &serde_json::Value,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    waker: &Waker,
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
    waker.wake();
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
        | ClaudeMessage::ToolProgress(_)
        | ClaudeMessage::ControlCancelRequest(_) => CancelledTurnMessageAction::Ignore,
    }
}

/// The chat error a turn's `Result` should show, if any.
///
/// A user who stops a turn (the Stop button, or exiting a tool call) ends it
/// on purpose, and the CLI reports that turn as `is_error` with no result
/// text. That is the user's intent, not a failure, so it shows nothing. A
/// real error with no text names its subtype rather than an opaque
/// "Unknown error".
fn result_error(result_msg: &ResultMessage, stopped_by_user: bool) -> Option<String> {
    if !result_msg.is_error || stopped_by_user {
        return None;
    }
    Some(
        result_msg
            .result
            .clone()
            .unwrap_or_else(|| format!("Claude Code ended the turn ({})", result_msg.subtype)),
    )
}

/// Handle a single message from the continuous Claude stream.
///
/// This runs for every message the CLI emits, whether it belongs to a
/// user-initiated turn or a spontaneous wake-up turn (a `run_in_background`
/// task completing). On a `Result` it emits `QueryComplete`, which is the
/// explicit turn boundary the UI keys off (the session channel stays open).
/// `stopped_by_user` says the user stopped the turn this message belongs to,
/// so its closing `Result` is not reported as an error (see [`result_error`]).
fn handle_stream_message(
    message: ClaudeMessage,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    waker: &Waker,
    pending_tools: &mut HashMap<String, (String, serde_json::Value)>,
    subagent_stack: &mut Vec<String>,
    task_tracker: &mut TaskTracker,
    stopped_by_user: bool,
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
                waker.wake();
            }

            for block in &assistant_msg.message.content {
                if let ContentBlock::ToolUse(ToolUseBlock { id, name, input }) = block {
                    pending_tools.insert(id.clone(), (name.clone(), input.clone()));

                    // Emit SubagentSpawned for foreground Task/Agent tool calls. A
                    // background subagent (`run_in_background`) is spawned from
                    // its `task_started` system message instead — it outlives
                    // this turn and completes on a wake-up, so it must not join
                    // the foreground `subagent_stack` nor complete on its launch
                    // tool result.
                    if is_subagent_tool(name) && !is_background_task(input) {
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
                        waker.wake();
                    }

                    // Emit TodoUpdate for TodoWrite tool calls
                    if name == "TodoWrite" {
                        let _ = response_tx.send(DaveApiResponse::TodoUpdate(input.clone()));
                        waker.wake();
                    }

                    // Emit an in-flight "running" row for a generic foreground
                    // tool so the user sees which tool is executing before its
                    // result lands. Task/Agent/TodoWrite already surface their own
                    // rows, and a subagent-internal tool (a set
                    // `parent_tool_use_id`, or a non-empty foreground
                    // `subagent_stack`) folds into its subagent instead of chat.
                    // This foreground test MUST match `send_tool_result`'s
                    // `parent_task_id` rule so every emitted running row is
                    // guaranteed a foreground result that resolves it in place.
                    let is_foreground =
                        assistant_msg.parent_tool_use_id.is_none() && subagent_stack.is_empty();
                    if !is_subagent_tool(name) && name != "TodoWrite" && is_foreground {
                        let summary = format_tool_summary(name, input, &serde_json::Value::Null);
                        let _ = response_tx.send(DaveApiResponse::ToolRunning(RunningTool {
                            tool_use_id: id.clone(),
                            tool_name: name.clone(),
                            summary,
                        }));
                        waker.wake();
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
                        waker.wake();
                    }
                }
            }
        }
        ClaudeMessage::Result(result_msg) => {
            if let Some(error_text) = result_error(&result_msg, stopped_by_user) {
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
                        waker,
                    );
                }
            }
        }
        ClaudeMessage::System(system_msg) => {
            // Handle system init message - extract session info
            if system_msg.subtype == "init" {
                let session_info = parse_session_info(&system_msg);
                let _ = response_tx.send(DaveApiResponse::SessionInfo(session_info));
                waker.wake();
            } else if system_msg.subtype == "status" {
                // Handle status messages (compaction start/end)
                let status = system_msg.data.get("status").and_then(|v| v.as_str());
                if status == Some("compacting") {
                    let _ = response_tx.send(DaveApiResponse::CompactionStarted);
                    waker.wake();
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
                waker.wake();
            } else if system_msg.subtype == "task_started" {
                handle_task_started(&system_msg.data, pending_tools, response_tx, waker);
            } else if system_msg.subtype == "task_notification" {
                handle_task_notification(&system_msg.data, response_tx, waker);
            } else {
                tracing::debug!("Received system message subtype: {}", system_msg.subtype);
            }
        }
        ClaudeMessage::ToolProgress(progress) => {
            // Incremental progress from a long-running tool, emitted before its
            // final `tool_result`. There's no in-flight progress UI yet (that's
            // tracked separately), so just log it rather than dropping the
            // stream on an unknown variant.
            tracing::debug!(
                "tool_progress for tool_use_id={:?} parent={:?}",
                progress.tool_use_id,
                progress.parent_tool_use_id,
            );
        }
        ClaudeMessage::ControlCancelRequest(_) => {
            // Ignore internal control messages
        }
    }
}

/// How a user's denial reply reached the model.
///
/// A denial's `PermissionResultDeny.message` is surfaced to the model as the
/// tool call's *error* — the same channel that carries `No such file or
/// directory`. Text arriving there cannot prove who wrote it: an injection can
/// print any wrapper the real code prints. So the user's words go somewhere the
/// transport itself vouches for them (a real user turn) and the tool result is
/// left with a marker that asserts nothing.
enum ReplyDelivery {
    /// The user typed nothing. Nothing to deliver; the marker stands alone.
    NoReply,
    /// The reply went out as its own user turn. The tool result carries no user
    /// prose at all — the preferred shape.
    SentAsUserTurn,
    /// The user turn could not be written. Fall back to embedding their words
    /// in the tool result behind the attribution framing: weaker provenance,
    /// but better than dropping what they said.
    Undeliverable,
}

/// Deliver a user's denial reply as a real user turn in the conversation.
///
/// This is the same mechanism the allow-with-message path uses — the SDK writes
/// a `{"type":"user",...}` line to the CLI, indistinguishable on the wire from
/// the user typing it — which is exactly why it fixes the provenance problem
/// that framing alone cannot.
async fn deliver_denial_reply(
    client: &ClaudeClient,
    session_id: &str,
    reason: &str,
) -> ReplyDelivery {
    // Canned placeholders ("User denied", "Denied by remote") are synthesized
    // when the user typed nothing. Sending one as a user turn would put words
    // in their mouth.
    let Some(text) = permission_reply_message(Some(reason)) else {
        return ReplyDelivery::NoReply;
    };

    match client
        .query_with_content_and_session(vec![UserContentBlock::text(text.as_str())], session_id)
        .await
    {
        Ok(()) => ReplyDelivery::SentAsUserTurn,
        Err(err) => {
            tracing::error!("Failed to deliver denial reply as a user turn: {}", err);
            ReplyDelivery::Undeliverable
        }
    }
}

/// Build the SDK denial for a user's deny decision, given how their reply was
/// delivered.
///
/// Split out from [`handle_permission_request`] so the provenance contract is
/// testable without a live SDK client.
fn user_denial(reason: &str, delivery: ReplyDelivery) -> PermissionResultDeny {
    let message = match delivery {
        ReplyDelivery::NoReply => denial_marker_for_model(false).to_string(),
        ReplyDelivery::SentAsUserTurn => denial_marker_for_model(true).to_string(),
        ReplyDelivery::Undeliverable => denial_message_for_model(Some(reason)),
    };
    PermissionResultDeny {
        message,
        interrupt: false,
    }
}

/// Build the SDK denial for a tool call the user exited, which also cancels the
/// turn.
///
/// Unlike a plain deny this cannot hand the reply to a user turn:
/// [`cancelled_turn_message_action`] suppresses every stream message after a
/// cancel until the turn's `Result`, so an injected turn would be swallowed —
/// or worse, start a turn that then gets suppressed. The user's words stay in
/// the tool result behind the attribution framing, which is the weaker
/// provenance but the only one available on this path.
fn user_turn_exit(reason: &str) -> PermissionResultDeny {
    PermissionResultDeny {
        message: turn_exit_message_for_model(Some(reason)),
        interrupt: true,
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
    waker: &Waker,
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
        waker,
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
            // Send the user's words as a real user turn *before* answering the
            // permission request, so the model reads them where human input
            // belongs rather than in the tool's error field.
            let delivery = deliver_denial_reply(client, session_id, &reason).await;
            (
                PermissionResult::Deny(user_denial(&reason, delivery)),
                false,
            )
        }
        Ok(PermissionResponse::Cancel { reason }) => {
            tracing::debug!(
                "User exited tool {} and cancelled the turn: {}",
                tool_name,
                reason
            );
            (PermissionResult::Deny(user_turn_exit(&reason)), true)
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

/// How long a session's Claude CLI may sit idle before its actor stops it.
///
/// Each CLI is a node process holding hundreds of MB, and a session keeps its
/// actor until the session is deleted, so a long `/autowork` chain leaves dozens
/// of finished sessions' CLIs resident. Stopping an idle one is free to undo:
/// the next query reconnects with `--resume` (see [`session_actor`]).
const IDLE_REAP_AFTER: Duration = Duration::from_secs(60 * 60);

/// Decides when a session's Claude CLI has been idle long enough to stop.
///
/// The CLI is only stopped between turns with no background task running: a
/// `run_in_background` task lives inside the CLI process and would die with
/// it. A pending permission prompt needs no tracking here — the actor awaits
/// the user's answer inline, so it is not waiting on the idle timer meanwhile.
struct IdleTracker {
    /// The last command or CLI message the session saw.
    last_activity: Instant,
    /// A turn (user-initiated or a background-task wake-up) has started and
    /// its `Result` has not arrived yet.
    turn_active: bool,
    /// `tool_use_id`s of background tasks that started but have not sent
    /// their `task_notification`.
    background_tasks: HashSet<String>,
}

impl IdleTracker {
    fn new(now: Instant) -> Self {
        Self {
            last_activity: now,
            turn_active: false,
            background_tasks: HashSet::new(),
        }
    }

    /// Something happened that isn't a turn starting (a permission answer, a
    /// mode change, an interrupt).
    fn touch(&mut self, now: Instant) {
        self.last_activity = now;
    }

    /// A query was handed to the CLI, so a turn is underway until its `Result`.
    fn turn_started(&mut self, now: Instant) {
        self.last_activity = now;
        self.turn_active = true;
    }

    /// Account for one message from the CLI.
    fn on_message(&mut self, now: Instant, message: &ClaudeMessage) {
        self.last_activity = now;
        match message {
            ClaudeMessage::Result(_) => self.turn_active = false,
            // Turn content. A wake-up turn starts with no command from us, so
            // its first message is what marks it underway.
            ClaudeMessage::Assistant(_) | ClaudeMessage::User(_) => self.turn_active = true,
            ClaudeMessage::System(system_msg) => {
                let tool_use_id = || {
                    system_msg
                        .data
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                };
                match system_msg.subtype.as_str() {
                    "task_started" => {
                        if let Some(id) = tool_use_id() {
                            self.background_tasks.insert(id);
                        }
                    }
                    "task_notification" => {
                        if let Some(id) = tool_use_id() {
                            self.background_tasks.remove(&id);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// The CLI is gone, and with it any turn or background task it was
    /// running. Returns whether that cut off work in flight.
    fn cli_lost(&mut self, now: Instant) -> bool {
        let work_lost = self.turn_active || !self.background_tasks.is_empty();
        self.last_activity = now;
        self.turn_active = false;
        self.background_tasks.clear();
        work_lost
    }

    /// When the CLI may be stopped, or `None` while work is outstanding.
    fn deadline(&self) -> Option<Instant> {
        if self.turn_active || !self.background_tasks.is_empty() {
            return None;
        }
        Some(self.last_activity + IDLE_REAP_AFTER)
    }
}

/// Session state that must outlive any one CLI connection, since an idle CLI
/// is stopped and a fresh one resumed in its place.
struct ActorState {
    /// Refreshed from each Query/Compact command so wake-up turns (which
    /// carry no command) can still request repaints.
    waker: Waker,
    /// `pending_tools`/`subagent_stack` must outlive a single turn: a
    /// `run_in_background` task's tool_use lands in one turn while its
    /// tool_result / completion lands in a later wake-up turn, so attribution
    /// needs them to survive across turns.
    pending_tools: HashMap<String, (String, serde_json::Value)>,
    subagent_stack: Vec<String>,
    /// Tracks the harness task list across turns. `TaskCreate`/`TaskUpdate`
    /// are incremental, so this must outlive the per-query loop.
    task_tracker: TaskTracker,
    /// Set when the user exits a tool call; suppresses the rest of that turn's
    /// messages until its `Result`, then clears at the turn boundary.
    cancel_current_turn: bool,
    /// Set when the user stops the running turn (Stop, or a tool exit); tells
    /// the turn's closing `Result` not to surface as an error. Cleared at that
    /// `Result` and by the next user turn.
    stopped_by_user: bool,
    idle: IdleTracker,
    /// The mode the next CLI starts in: the spawn-time mode, then whatever the
    /// user last switched to, so a resumed CLI keeps the mode the UI shows.
    permission_mode: PermissionMode,
    /// The Claude CLI session to `--resume` when (re)connecting: the one the
    /// session was restored from, then whatever the CLI's `init` reports.
    cli_session_id: Option<String>,
}

impl ActorState {
    /// A session's state before its first CLI starts.
    fn new(waker: Waker, permission_mode: PermissionMode, cli_session_id: Option<String>) -> Self {
        Self {
            waker,
            pending_tools: HashMap::new(),
            subagent_stack: Vec::new(),
            task_tracker: TaskTracker::new(),
            cancel_current_turn: false,
            stopped_by_user: false,
            idle: IdleTracker::new(Instant::now()),
            permission_mode,
            cli_session_id,
        }
    }

    /// Note what a CLI message says about the session before it is handled.
    fn observe(&mut self, message: &ClaudeMessage) {
        self.idle.on_message(Instant::now(), message);
        if let ClaudeMessage::System(system_msg) = message {
            if system_msg.subtype == "init" {
                if let Some(id) = &system_msg.session_id {
                    self.cli_session_id = Some(id.clone());
                }
            }
        }
    }
}

/// Why [`run_connected`] stopped driving its CLI.
enum ConnectedExit {
    /// The session was shut down, or the backend dropped its handle.
    Shutdown,
    /// The CLI's output ended: it exited, or the SDK stopped reading it.
    StreamClosed {
        /// The SDK's error just before the end, if the stream did not end
        /// cleanly (an unreadable or over-long line, a read error).
        error: Option<String>,
    },
    /// Nothing happened for [`IDLE_REAP_AFTER`]; the CLI can be stopped.
    Idle,
}

/// Sleep until `deadline`, or forever when there is none.
async fn sleep_until_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// The largest single line of Claude CLI output (one JSON message) a session
/// accepts.
///
/// The SDK's 10 MB default is too small for real tool results. An image Read
/// carries its base64 twice on one line (~1.3 MB for a 500 KB screenshot, so
/// a full-size image is over 10 MB), and a PDF Read can be larger still. The
/// cap is there to bound memory against a runaway line, not to trim
/// legitimate output. A line over it ends the CLI's stream, which the actor
/// reports as a lost turn (see [`report_lost_cli`]).
const MAX_CLI_LINE_BYTES: usize = 128 * 1024 * 1024;

/// The Claude CLI options a session's every connection starts from, before
/// the per-session cwd/model/env and per-connection resume id and mode.
///
/// `max_buffer_size` limits each line the CLI writes (see
/// [`MAX_CLI_LINE_BYTES`]). Before the SDK made it per-line it was a lifetime
/// total, and a session went silent for good after 10 MB of output
/// (headway:dave/code-hollow-spy).
fn session_base_options(
    can_use_tool: claude_agent_sdk_rs::CanUseToolCallback,
) -> ClaudeAgentOptions {
    // A stderr callback to prevent the subprocess from blocking
    let stderr_callback = Arc::new(|msg: String| {
        tracing::trace!("Claude CLI stderr: {}", msg);
    });

    ClaudeAgentOptions::builder()
        .stderr_callback(stderr_callback)
        .can_use_tool(can_use_tool)
        .include_partial_messages(true)
        .max_buffer_size(MAX_CLI_LINE_BYTES)
        .build()
}

/// Session actor task that owns the session's Claude CLI.
///
/// The CLI is started lazily by the first Query or Compact, stopped after
/// [`IDLE_REAP_AFTER`] of inactivity, and started again — resuming the same
/// CLI session — by the next one. The actor itself, and with it the UI's
/// response channel, lives until the session is shut down, so stopping an
/// idle CLI is invisible to the UI.
#[allow(clippy::too_many_arguments)]
async fn session_actor(
    session_id: String,
    session_env: BTreeMap<String, String>,
    cwd: Option<PathBuf>,
    resume_session_id: Option<String>,
    model: Option<String>,
    // The permission mode the CLI subprocess starts in. Mirrors the session's UI
    // mode at spawn time so a session shown as `Auto` actually runs the CLI in
    // `Auto` — not `Default` until the user cycles the mode.
    permission_mode: PermissionMode,
    mut command_rx: tokio_mpsc::Receiver<SessionCommand>,
    // The session-lifetime UI channel. Created once when the actor is spawned and
    // used for every turn — user-initiated AND spontaneous wake-up turns — so
    // background-task completions reach the UI even between user queries.
    response_tx: mpsc::Sender<DaveApiResponse>,
    // Fallback egui context; refreshed from each Query/Compact command so
    // wake-up turns (which carry no command) can still request repaints.
    initial_waker: Waker,
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

    // The options every connection shares; the resume id and permission mode
    // are filled in per connection from `ActorState`.
    let mut base_options = session_base_options(can_use_tool);
    base_options.cwd = cwd;
    base_options.model = model;
    // Export the session env (the configured `session_env` plus this session's
    // agentium identity; see `shared::session_env`) into the spawned CLI.
    base_options.env.extend(session_env);

    let mut state = ActorState::new(initial_waker, permission_mode, resume_session_id);

    serve_session(
        &session_id,
        &base_options,
        &mut command_rx,
        &mut perm_rx,
        &response_tx,
        &mut state,
    )
    .await;
    tracing::debug!("Session {} actor exited", session_id);
}

/// Run the session's CLI connections until the session shuts down.
///
/// A CLI is started (resuming the session's CLI session, if it has one) for
/// the first Query or Compact, and stopped when it goes idle or its output
/// ends. Either way the session goes dormant until the next command, which
/// starts a fresh CLI.
async fn serve_session(
    session_id: &str,
    base_options: &ClaudeAgentOptions,
    command_rx: &mut tokio_mpsc::Receiver<SessionCommand>,
    perm_rx: &mut tokio_mpsc::Receiver<PermissionRequestInternal>,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    state: &mut ActorState,
) {
    // No CLI runs until there is something for it to do.
    while let Some(wake_cmd) = wait_dormant(command_rx, state).await {
        let mut options = base_options.clone();
        options.resume = state.cli_session_id.clone();
        options.permission_mode = Some(state.permission_mode);
        if let Some(resume_id) = &options.resume {
            tracing::info!(
                "Session {} will resume Claude session: {}",
                session_id,
                resume_id
            );
        }

        let mut client = ClaudeClient::new(options);
        if let Err(err) = client.connect().await {
            tracing::error!("Session {} failed to connect: {}", session_id, err);
            // Report the failure for the command that needed the CLI, then stay
            // dormant so the next one retries.
            let _ = response_tx.send(DaveApiResponse::Failed(format!(
                "Failed to connect to Claude: {}",
                err
            )));
            state.waker.wake();
            continue;
        }
        tracing::debug!("Session {} connected successfully", session_id);

        let exit = run_connected(
            &client,
            wake_cmd,
            session_id,
            command_rx,
            perm_rx,
            response_tx,
            state,
        )
        .await;

        if let Err(err) = client.disconnect().await {
            tracing::warn!("Error disconnecting session {}: {}", session_id, err);
        }

        match exit {
            ConnectedExit::Idle => {
                tracing::info!(
                    "Session {} idle for {:?}, stopped its Claude CLI",
                    session_id,
                    IDLE_REAP_AFTER
                );
            }
            ConnectedExit::StreamClosed { error } => {
                report_lost_cli(session_id, error, response_tx, state);
            }
            ConnectedExit::Shutdown => break,
        }
    }
}

/// Account for a CLI whose output ended, and end any turn it cut off.
///
/// The session stays up and goes dormant: the next message starts a fresh CLI
/// that `--resume`s the same CLI session, as after an idle stop. A lost turn is
/// reported rather than retried. The CLI's own transcript has whatever it
/// finished, the user sees why the turn stopped, and nothing re-prompts the
/// model on its own (which could loop on whatever killed the stream).
fn report_lost_cli(
    session_id: &str,
    error: Option<String>,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    state: &mut ActorState,
) {
    let work_lost = state.idle.cli_lost(Instant::now());
    state.cancel_current_turn = false;
    state.stopped_by_user = false;
    // Tool and subagent attribution died with the CLI's turn.
    state.pending_tools.clear();
    state.subagent_stack.clear();

    let reason = error.unwrap_or_else(|| "the Claude CLI exited".to_string());
    if !work_lost {
        tracing::info!(
            "Session {} lost its Claude CLI between turns ({}); the next query resumes it",
            session_id,
            reason
        );
        return;
    }

    tracing::error!(
        "Session {} lost its Claude CLI mid-turn: {}",
        session_id,
        reason
    );
    let _ = response_tx.send(DaveApiResponse::Failed(format!(
        "Lost the connection to Claude mid-turn: {reason}. Send a message to resume the session."
    )));
    // The turn's Result will never come, so end the turn here.
    let _ = response_tx.send(DaveApiResponse::QueryComplete(
        crate::messages::UsageInfo::default(),
    ));
    state.waker.wake();
}

/// Wait, with no CLI running, for a command that needs one.
///
/// Returns the Query or Compact that should start the CLI, or `None` once the
/// session shuts down. Mode changes are recorded for the next CLI to start
/// in; an interrupt has nothing to stop.
async fn wait_dormant(
    command_rx: &mut tokio_mpsc::Receiver<SessionCommand>,
    state: &mut ActorState,
) -> Option<SessionCommand> {
    while let Some(cmd) = command_rx.recv().await {
        match cmd {
            SessionCommand::Query { .. } | SessionCommand::Compact { .. } => return Some(cmd),
            SessionCommand::SetPermissionMode { mode, waker } => {
                state.permission_mode = mode;
                waker.wake();
            }
            SessionCommand::Interrupt { waker } => waker.wake(),
            SessionCommand::Shutdown => return None,
        }
    }
    None
}

/// Drive one connected CLI until the session shuts down, the CLI exits, or it
/// has sat idle for [`IDLE_REAP_AFTER`].
///
/// `wake_cmd` is the command that started this CLI; it is handled first.
async fn run_connected(
    client: &ClaudeClient,
    wake_cmd: SessionCommand,
    session_id: &str,
    command_rx: &mut tokio_mpsc::Receiver<SessionCommand>,
    perm_rx: &mut tokio_mpsc::Receiver<PermissionRequestInternal>,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    state: &mut ActorState,
) -> ConnectedExit {
    // Pump the CLI message stream continuously, not just while servicing a
    // Query. This is the non-breaking `receive_messages()` variant, held for the
    // whole connection, so spontaneous wake-up turns (a background task
    // completing) flow through the same handler as user-initiated turns. All
    // client calls below are `&self` (query_with_content_and_session /
    // interrupt / set_permission_mode) so they coexist with this borrow; the
    // caller's `disconnect` (&mut) runs once this returns and the stream drops.
    let mut message_stream = client.receive_messages();
    // The latest stream error, kept until a message reads fine. When the SDK
    // stops reading the CLI it yields the reason and then ends the stream.
    let mut last_error: Option<String> = None;

    if let Some(exit) = handle_command(wake_cmd, client, session_id, response_tx, state).await {
        return exit;
    }

    loop {
        let idle_deadline = state.idle.deadline();
        tokio::select! {
            biased;

            // Commands from the UI / backend.
            cmd = command_rx.recv() => {
                let Some(cmd) = cmd else {
                    // Command channel closed — the backend dropped the handle.
                    return ConnectedExit::Shutdown;
                };
                if let Some(exit) = handle_command(cmd, client, session_id, response_tx, state).await {
                    return exit;
                }
            }

            // Permission requests (they block the SDK until answered).
            Some(perm_req) = perm_rx.recv() => {
                handle_permission_request(
                    perm_req,
                    client,
                    session_id,
                    response_tx,
                    &state.waker,
                    &mut state.cancel_current_turn,
                )
                .await;
                state.idle.touch(Instant::now());
            }

            // The continuous CLI message stream.
            msg = message_stream.next() => {
                let Some(result) = msg else {
                    // Stream closed: the CLI exited, or the SDK stopped reading
                    // it. Nothing more will arrive from this CLI.
                    return ConnectedExit::StreamClosed { error: last_error };
                };
                let message = match result {
                    Ok(message) => {
                        last_error = None;
                        message
                    }
                    Err(err) => {
                        // Non-fatal unless the stream ends right after it:
                        // unknown message types (e.g. rate_limit_event) fail to
                        // deserialize but the stream continues.
                        tracing::warn!("Claude stream message skipped: {}", err);
                        last_error = Some(err.to_string());
                        continue;
                    }
                };
                state.observe(&message);

                // While a turn is cancelled, drop its remaining messages until
                // the Result, which we still handle (to emit completion) before
                // clearing the flag at the turn boundary.
                if state.cancel_current_turn {
                    match cancelled_turn_message_action(&message) {
                        CancelledTurnMessageAction::Ignore => {
                            tracing::debug!(
                                "Suppressing Claude message after cancelled turn: {:?}",
                                std::mem::discriminant(&message)
                            );
                            continue;
                        }
                        CancelledTurnMessageAction::FinishTurn => {
                            state.cancel_current_turn = false;
                            state.stopped_by_user = true;
                        }
                    }
                }

                let is_result = matches!(message, ClaudeMessage::Result(_));
                handle_stream_message(
                    message,
                    response_tx,
                    &state.waker,
                    &mut state.pending_tools,
                    &mut state.subagent_stack,
                    &mut state.task_tracker,
                    state.stopped_by_user,
                );
                if is_result {
                    state.stopped_by_user = false;
                }
            }

            // Nothing in flight and nothing heard for the idle window.
            _ = sleep_until_deadline(idle_deadline) => {
                return ConnectedExit::Idle;
            }
        }
    }
}

/// Handle one command against a connected CLI. Returns an exit when the
/// command ends the session.
async fn handle_command(
    cmd: SessionCommand,
    client: &ClaudeClient,
    session_id: &str,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    state: &mut ActorState,
) -> Option<ConnectedExit> {
    match cmd {
        SessionCommand::Query {
            prompt,
            images,
            waker: query_waker,
            ..
        } => {
            // A fresh user turn: refresh waker and clear any leftover
            // cancellation from a previous turn.
            state.waker = query_waker;
            state.cancel_current_turn = false;
            state.stopped_by_user = false;
            let blocks = build_content_blocks(&images, &prompt);
            match client
                .query_with_content_and_session(blocks, session_id)
                .await
            {
                Ok(()) => state.idle.turn_started(Instant::now()),
                Err(err) => {
                    tracing::error!("Session {} query error: {}", session_id, err);
                    let _ = response_tx.send(DaveApiResponse::Failed(err.to_string()));
                }
            }
        }
        SessionCommand::Interrupt {
            waker: interrupt_waker,
        } => {
            tracing::debug!("Session {} received interrupt", session_id);
            state.stopped_by_user = true;
            state.idle.touch(Instant::now());
            if let Err(err) = client.interrupt().await {
                tracing::error!("Failed to send interrupt: {}", err);
            }
            // The stream ends naturally with a Result; the CLI
            // preserves session history.
            interrupt_waker.wake();
        }
        SessionCommand::SetPermissionMode {
            mode,
            waker: mode_waker,
        } => {
            tracing::debug!(
                "Session {} setting permission mode to {:?}",
                session_id,
                mode
            );
            state.permission_mode = mode;
            state.idle.touch(Instant::now());
            if let Err(err) = client.set_permission_mode(mode).await {
                tracing::error!("Failed to set permission mode: {}", err);
            }
            mode_waker.wake();
        }
        SessionCommand::Compact {
            waker: compact_waker,
            ..
        } => {
            // Claude compaction is driven by sending `/compact` as a
            // query on the persistent channel (see compact_session).
            state.waker = compact_waker;
            match client
                .query_with_content_and_session(
                    vec![UserContentBlock::text("/compact")],
                    session_id,
                )
                .await
            {
                Ok(()) => state.idle.turn_started(Instant::now()),
                Err(err) => {
                    tracing::error!("Session {} compact error: {}", session_id, err);
                    let _ = response_tx.send(DaveApiResponse::Failed(err.to_string()));
                }
            }
        }
        SessionCommand::Shutdown => {
            tracing::debug!("Session actor {} shutting down", session_id);
            return Some(ConnectedExit::Shutdown);
        }
    }
    None
}

impl AiBackend for ClaudeBackend {
    fn stream_request(
        &self,
        messages: Vec<Message>,
        _tools: Arc<HashMap<String, Tool>>,
        model: Option<String>,
        _user_id: String,
        session_id: String,
        session_env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
        resume_session_id: Option<String>,
        permission_mode: PermissionMode,
        waker: Waker,
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
                // and the session-lifetime response channel + initial waker.
                let session_id_clone = session_id.clone();
                let cwd_clone = cwd.clone();
                let resume_session_id_clone = resume_session_id.clone();
                let model_clone = model.clone();
                let waker_clone = waker.clone();
                tokio::spawn(async move {
                    session_actor(
                        session_id_clone,
                        session_env,
                        cwd_clone,
                        resume_session_id_clone,
                        model_clone,
                        permission_mode,
                        command_rx,
                        response_tx,
                        waker_clone,
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
                    waker,
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

    fn interrupt_session(&self, session_id: String, waker: Waker) {
        if let Some(handle) = self.sessions.get(&session_id) {
            let command_tx = handle.command_tx.clone();
            tokio::spawn(async move {
                if let Err(err) = command_tx.send(SessionCommand::Interrupt { waker }).await {
                    tracing::warn!("Failed to send interrupt command: {}", err);
                }
            });
        }
    }

    fn set_permission_mode(&self, session_id: String, mode: PermissionMode, waker: Waker) {
        if let Some(handle) = self.sessions.get(&session_id) {
            let command_tx = handle.command_tx.clone();
            tokio::spawn(async move {
                if let Err(err) = command_tx
                    .send(SessionCommand::SetPermissionMode { mode, waker })
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
        waker: Waker,
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
                    waker,
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
    use crate::backend::CountingWaker;

    /// Number of 1 MiB tool results the fake CLI below emits for one query:
    /// past the SDK's 10 MB default, like a session that has read a handful
    /// of screenshots (each image Read is a ~1.3 MB stdout line).
    const FAKE_CLI_BIG_RESULTS: usize = 12;

    /// A stand-in `claude` that answers the SDK's initialize handshake, then
    /// answers the first query with [`FAKE_CLI_BIG_RESULTS`] 1 MiB tool
    /// results and a `result`, and stays up until stdin closes.
    #[cfg(unix)]
    const FAKE_CLI: &str = r#"#!/bin/sh
case "$*" in *--version*) echo "2.1.288 (Claude Code)"; exit 0 ;; esac
read -r init
id=$(printf '%s' "$init" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s"}}\n' "$id"
read -r query
pad=$(head -c 1048576 /dev/zero | tr '\0' a)
i=0
while [ "$i" -lt "$BIG_RESULTS" ]; do
  printf '{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_%s","content":"%s"}]}}\n' "$i" "$pad"
  i=$((i + 1))
done
printf '{"type":"result","subtype":"success","duration_ms":1,"duration_api_ms":1,"is_error":false,"num_turns":1,"session_id":"fake"}\n'
cat > /dev/null
"#;

    /// Regression for a session freezing mid-batch (agentium:good-slot-service,
    /// headway:dave/code-hollow-spy): three parallel screenshot Reads, and the
    /// third result never arrived while the agent worked on unseen.
    ///
    /// The SDK used to count every stdout line against one lifetime total and
    /// stop reading once it passed `max_buffer_size`, so the session's 10th
    /// MiB was the last thing Dave ever read: the later tool results and the
    /// `Result` never came and this timed out. The SDK's limit is per line
    /// now; driving a CLI with [`session_base_options`] — what every session
    /// connects with — must still deliver all of it.
    #[cfg(unix)]
    #[tokio::test]
    async fn session_keeps_reading_past_ten_megabytes_of_cli_output() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let cli = dir.path().join("claude");
        std::fs::write(&cli, FAKE_CLI).expect("write fake cli");
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake cli");

        let allow_all: claude_agent_sdk_rs::CanUseToolCallback = Arc::new(|_, _, _| {
            Box::pin(async {
                PermissionResult::Allow(PermissionResultAllow {
                    updated_input: None,
                    updated_permissions: None,
                })
            })
        });
        let mut options = session_base_options(allow_all);
        options.cli_path = Some(cli);
        options.skip_version_check = true;
        options
            .env
            .insert("BIG_RESULTS".to_string(), FAKE_CLI_BIG_RESULTS.to_string());

        let mut client = ClaudeClient::new(options);
        client.connect().await.expect("connect to fake cli");
        client
            .query_with_content_and_session(vec![UserContentBlock::text("go")], "default")
            .await
            .expect("send query");

        let mut tool_results = 0;
        let mut messages = client.receive_messages();
        let finished = tokio::time::timeout(Duration::from_secs(20), async {
            while let Some(message) = messages.next().await {
                match message.expect("parse cli message") {
                    ClaudeMessage::User(_) => tool_results += 1,
                    ClaudeMessage::Result(_) => return true,
                    _ => {}
                }
            }
            false
        })
        .await;
        drop(messages);
        // Only a CLI whose output was read can be shut down cleanly. When the
        // reader has died, the fake blocks writing to a full stdout pipe and
        // `disconnect` would wait on it forever; dropping the client instead
        // kills it.
        if finished == Ok(true) {
            client.disconnect().await.expect("disconnect fake cli");
        }

        assert_eq!(
            finished,
            Ok(true),
            "the turn's Result never arrived; read {tool_results} of \
             {FAKE_CLI_BIG_RESULTS} tool results before the stream went quiet"
        );
        assert_eq!(tool_results, FAKE_CLI_BIG_RESULTS);
    }

    /// A stand-in `claude` whose first run breaks its output stream: after the
    /// query it reports its CLI session, writes a line that is not UTF-8 and
    /// then hangs without exiting, even once stdin closes (like a CLI blocked
    /// on a stdout nobody reads). Run with `--resume fake-session` it answers
    /// the query with a `result` and waits for stdin to close.
    #[cfg(unix)]
    const FAKE_CLI_BREAKS_ITS_STREAM: &str = r#"#!/bin/sh
case "$*" in *--version*) echo "2.1.288 (Claude Code)"; exit 0 ;; esac
read -r init
id=$(printf '%s' "$init" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s"}}\n' "$id"
read -r query
case "$*" in
  *"--resume fake-session"*)
    printf '{"type":"result","subtype":"success","duration_ms":1,"duration_api_ms":1,"is_error":false,"num_turns":1,"session_id":"fake-session"}\n'
    cat > /dev/null
    ;;
  *)
    printf '{"type":"system","subtype":"init","session_id":"fake-session"}\n'
    printf '\377\376\n'
    exec sleep 3600
    ;;
esac
"#;

    /// A short description of `responses` for assertion messages
    /// (`DaveApiResponse` is not `Debug`).
    fn describe(responses: &[DaveApiResponse]) -> Vec<String> {
        responses
            .iter()
            .map(|r| match r {
                DaveApiResponse::Failed(err) => format!("Failed({err})"),
                DaveApiResponse::QueryComplete(_) => "QueryComplete".to_string(),
                _ => "other".to_string(),
            })
            .collect()
    }

    /// Wait for the session to send a response matching `want`, failing after
    /// a minute. Returns every response seen up to and including the match.
    async fn responses_until(
        rx: &mpsc::Receiver<DaveApiResponse>,
        want: impl Fn(&DaveApiResponse) -> bool,
    ) -> Vec<DaveApiResponse> {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut seen = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(response) => {
                    let done = want(&response);
                    seen.push(response);
                    if done {
                        return seen;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "the session went silent; responses so far: {:?}",
                        describe(&seen)
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!(
                        "the session actor exited; responses so far: {:?}",
                        describe(&seen)
                    )
                }
            }
        }
    }

    /// A query for the session, as the backend sends it.
    fn query(waker: &Waker) -> SessionCommand {
        SessionCommand::Query {
            prompt: "go".to_string(),
            images: vec![],
            response_tx: None,
            waker: waker.clone(),
        }
    }

    /// When the SDK stops reading a CLI mid-turn, the session must say so and
    /// end the turn, not hang. It used to wait forever: the SDK left the
    /// message stream open with nothing feeding it, so no error, no `Result`,
    /// and the idle reaper never fired on a turn still in flight. Then the
    /// next message must bring the session back by resuming the CLI session.
    #[cfg(unix)]
    #[tokio::test]
    async fn session_reports_a_dead_cli_stream_and_resumes_on_the_next_query() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let cli = dir.path().join("claude");
        std::fs::write(&cli, FAKE_CLI_BREAKS_ITS_STREAM).expect("write fake cli");
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake cli");

        let allow_all: claude_agent_sdk_rs::CanUseToolCallback = Arc::new(|_, _, _| {
            Box::pin(async {
                PermissionResult::Allow(PermissionResultAllow {
                    updated_input: None,
                    updated_permissions: None,
                })
            })
        });
        let mut options = session_base_options(allow_all);
        options.cli_path = Some(cli);
        options.skip_version_check = true;

        let waker = CountingWaker::new();
        let (command_tx, mut command_rx) = tokio_mpsc::channel(4);
        let (_perm_tx, mut perm_rx) = tokio_mpsc::channel(4);
        let (response_tx, response_rx) = mpsc::channel();
        let mut state = ActorState::new(waker.waker().clone(), PermissionMode::Default, None);
        let session = tokio::spawn(async move {
            serve_session(
                "dave-session",
                &options,
                &mut command_rx,
                &mut perm_rx,
                &response_tx,
                &mut state,
            )
            .await;
        });

        command_tx.send(query(waker.waker())).await.unwrap();
        let first_turn = responses_until(&response_rx, |r| {
            matches!(r, DaveApiResponse::QueryComplete(_))
        })
        .await;
        let failure = first_turn.iter().find_map(|r| match r {
            DaveApiResponse::Failed(err) => Some(err.as_str()),
            _ => None,
        });
        let failure =
            failure.unwrap_or_else(|| panic!("no error reported: {:?}", describe(&first_turn)));
        assert!(
            failure.contains("UTF-8"),
            "the error should say why the stream ended: {failure}"
        );

        // The broken CLI is stopped and a new one resumes `fake-session`.
        command_tx.send(query(waker.waker())).await.unwrap();
        let second_turn = responses_until(&response_rx, |r| {
            matches!(r, DaveApiResponse::QueryComplete(_))
        })
        .await;
        assert!(
            !second_turn
                .iter()
                .any(|r| matches!(r, DaveApiResponse::Failed(_))),
            "the resumed CLI answers normally: {:?}",
            describe(&second_turn)
        );

        command_tx.send(SessionCommand::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), session)
            .await
            .expect("the session shuts down")
            .unwrap();
    }

    /// The reason for the whole fix: when the user's reply goes out as its own
    /// user turn, the tool result must contain *none* of their words.
    ///
    /// The denial message lands in the tool call's error field, where nothing
    /// can prove who wrote it — an injection prints the same wrapper the real
    /// code prints. Leaving the user's prose out entirely is what makes the
    /// provenance real: it comes from the transport, not from a claim in the
    /// text. A real session read jb55's denials in that field, correctly judged
    /// them unverifiable, and ignored him.
    ///
    /// Fails if someone restores `message: reason`, or drops the user turn and
    /// goes back to embedding the text.
    #[test]
    fn user_denial_keeps_the_users_words_out_of_the_tool_result() {
        let reason = "why are you ignoring all these messages. there is nothing to recover";
        let deny = user_denial(reason, ReplyDelivery::SentAsUserTurn);

        assert_ne!(deny.message, reason);
        assert!(
            !deny.message.contains("why are you ignoring"),
            "the user's words belong in the user turn, not the tool result: {}",
            deny.message
        );
        assert!(
            deny.message.contains("delivered separately"),
            "the marker must point the model at the separate user message: {}",
            deny.message
        );
        assert!(
            deny.message.contains("STOP what you are doing"),
            "the marker must tell the agent what to do next: {}",
            deny.message
        );
        assert!(!deny.interrupt, "a plain deny does not interrupt the turn");
    }

    /// A plain deny with no typed reply gets the same contentless marker, and
    /// never the canned "User denied" placeholder quoted as the user's words.
    #[test]
    fn user_denial_without_a_reply_says_no_reason_was_given() {
        let deny = user_denial(crate::messages::DEFAULT_DENY_REASON, ReplyDelivery::NoReply);

        assert!(deny.message.contains("gave no reason"), "{}", deny.message);
        assert!(
            !deny.message.contains(crate::messages::DEFAULT_DENY_REASON),
            "the synthesized placeholder must not reach the model: {}",
            deny.message
        );
        assert!(!deny.interrupt);
    }

    /// If the user turn cannot be written, the words still have to reach the
    /// model — so the fallback embeds them behind the attribution framing
    /// rather than dropping them. Weaker provenance, but not silence.
    #[test]
    fn undeliverable_reply_falls_back_to_framing_it_in_the_tool_result() {
        let reason = "they were both from me. THIS IS ME. THE USER.";
        let deny = user_denial(reason, ReplyDelivery::Undeliverable);

        assert_ne!(deny.message, reason, "never raw, even in the fallback");
        assert!(
            deny.message.contains(reason),
            "the fallback must not drop the user's words: {}",
            deny.message
        );
        assert!(
            deny.message
                .contains("typed by the human operating this session")
                && deny.message.contains("<message_from_user>"),
            "the fallback must attribute and delimit: {}",
            deny.message
        );
    }

    /// A tool exit cannot use the user-turn channel — the cancelled-turn filter
    /// would swallow it — so it keeps the framed-in-tool-result form and
    /// interrupts.
    #[test]
    fn user_turn_exit_frames_in_place_and_interrupts() {
        let reason = "stop trying";
        let deny = user_turn_exit(reason);

        assert_ne!(deny.message, reason);
        assert!(deny.message.contains(reason), "{}", deny.message);
        assert!(
            deny.message.contains("<message_from_user>"),
            "{}",
            deny.message
        );
        assert!(deny.interrupt, "a tool exit cancels the turn");

        // The reason a cancel can't hand off to a user turn: everything after a
        // cancel is dropped until the turn's Result.
        assert_eq!(
            cancelled_turn_message_action(&ClaudeMessage::User(
                serde_json::from_value(serde_json::json!({
                    "type": "user",
                    "message": { "role": "user", "content": [] }
                }))
                .expect("user message should deserialize")
            )),
            CancelledTurnMessageAction::Ignore
        );
    }

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
        let waker = CountingWaker::new();

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
        handle_task_started(&data, &pending, &tx, waker.waker());

        assert_eq!(
            waker.wakes(),
            1,
            "a spawned subagent must repaint, or it never appears in the sidebar"
        );
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
        let waker = Waker::noop();
        let pending: HashMap<String, (String, serde_json::Value)> = HashMap::new();

        let data = serde_json::json!({
            "task_id": "b4dg5o2ra",
            "tool_use_id": "toolu_bash",
            "description": "sleep 6",
            "task_type": "local_bash",
        });
        handle_task_started(&data, &pending, &tx, &waker);

        assert!(
            rx.try_recv().is_err(),
            "a background shell must not create a subagent sidebar entry"
        );
    }

    #[test]
    fn task_notification_completes_and_fails_by_tool_use_id() {
        let (tx, rx) = mpsc::channel();
        let waker = CountingWaker::new();

        handle_task_notification(
            &serde_json::json!({
                "tool_use_id": "toolu_root",
                "status": "completed",
                "summary": "all done",
            }),
            &tx,
            waker.waker(),
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
            waker.waker(),
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

        assert_eq!(
            waker.wakes(),
            2,
            "both notifications must repaint, or the entry stays running"
        );
    }

    #[test]
    fn subagent_internal_tool_result_routes_by_parent_tool_use_id() {
        let (tx, rx) = mpsc::channel();
        let waker = Waker::noop();
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
            &waker,
            &mut pending,
            &mut subagent_stack,
            &mut task_tracker,
            false,
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
            &waker,
            &mut pending,
            &mut subagent_stack,
            &mut task_tracker,
            false,
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

    /// The per-session state `run_stream` threads through
    /// `handle_stream_message`, so a test can feed it real CLI messages and
    /// read back exactly what the UI would receive.
    struct StreamHarness {
        tx: mpsc::Sender<DaveApiResponse>,
        rx: mpsc::Receiver<DaveApiResponse>,
        waker: Waker,
        pending_tools: HashMap<String, (String, serde_json::Value)>,
        subagent_stack: Vec<String>,
        task_tracker: TaskTracker,
        /// What `run_stream` would pass after the user stopped the turn.
        stopped_by_user: bool,
    }

    impl StreamHarness {
        fn new() -> Self {
            let (tx, rx) = mpsc::channel();
            Self {
                tx,
                rx,
                waker: Waker::noop(),
                pending_tools: HashMap::new(),
                subagent_stack: Vec::new(),
                task_tracker: TaskTracker::new(),
                stopped_by_user: false,
            }
        }

        /// Feed one message, in the wire shape the CLI emits it.
        fn feed(&mut self, message: serde_json::Value) {
            let message = serde_json::from_value::<ClaudeMessage>(message)
                .expect("message should deserialize");
            handle_stream_message(
                message,
                &self.tx,
                &self.waker,
                &mut self.pending_tools,
                &mut self.subagent_stack,
                &mut self.task_tracker,
                self.stopped_by_user,
            );
        }

        /// Drain the channel, keeping the tool results.
        fn tool_results(&self) -> Vec<crate::messages::ExecutedTool> {
            self.rx
                .try_iter()
                .filter_map(|r| match r {
                    DaveApiResponse::ToolResult(tool) => Some(tool),
                    _ => None,
                })
                .collect()
        }

        /// Drain the channel, keeping the in-flight running rows.
        fn running_tools(&self) -> Vec<crate::messages::RunningTool> {
            self.rx
                .try_iter()
                .filter_map(|r| match r {
                    DaveApiResponse::ToolRunning(running) => Some(running),
                    _ => None,
                })
                .collect()
        }
    }

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "type": "tool_use", "id": id, "name": name, "input": input })
    }

    /// Tool results arrive on a `user` message, nested one level deeper than the
    /// SDK's own `content` field — see `parse_user_content_blocks`.
    fn user_with(blocks: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({ "type": "user", "message": { "content": blocks } })
    }

    fn tool_result(tool_use_id: &str, content: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": false,
        })
    }

    #[test]
    fn tool_use_then_result_emits_an_executed_tool() {
        let mut harness = StreamHarness::new();

        harness.feed(serde_json::json!({
            "type": "assistant",
            "message": { "content": [
                tool_use("toolu_123", "Read", serde_json::json!({ "file_path": "/etc/hostname" })),
            ]},
        }));
        assert!(
            harness.pending_tools.contains_key("toolu_123"),
            "the tool_use is held until its result lands"
        );
        assert!(
            harness.tool_results().is_empty(),
            "nothing is shown to the user until the tool has actually run"
        );

        harness.feed(user_with(vec![tool_result(
            "toolu_123",
            "hostname content",
        )]));

        assert!(
            harness.pending_tools.is_empty(),
            "a correlated tool_use is dropped from the pending map"
        );
        let results = harness.tool_results();
        assert_eq!(results.len(), 1, "one result for one correlated tool_use");
        assert_eq!(results[0].tool_name, "Read");
        assert!(
            !results[0].summary.is_empty(),
            "the summary is what the chat row renders"
        );
    }

    #[test]
    fn tool_result_without_a_matching_tool_use_emits_nothing() {
        let mut harness = StreamHarness::new();

        harness.feed(user_with(vec![tool_result(
            "toolu_unknown",
            "some content",
        )]));

        assert!(
            harness.tool_results().is_empty(),
            "an uncorrelated result has no tool name or input to render"
        );
    }

    #[test]
    fn tool_results_correlate_out_of_order() {
        let mut harness = StreamHarness::new();

        harness.feed(serde_json::json!({
            "type": "assistant",
            "message": { "content": [
                tool_use("toolu_1", "Read", serde_json::json!({ "file_path": "/a" })),
                tool_use("toolu_2", "Bash", serde_json::json!({ "command": "true" })),
                tool_use("toolu_3", "Grep", serde_json::json!({ "pattern": "x" })),
            ]},
        }));
        assert_eq!(harness.pending_tools.len(), 3);

        // The CLI is free to return results in any order.
        harness.feed(user_with(vec![tool_result("toolu_2", "bash output")]));
        harness.feed(user_with(vec![tool_result("toolu_1", "file body")]));
        harness.feed(user_with(vec![tool_result("toolu_3", "3 matches")]));

        assert!(harness.pending_tools.is_empty());
        let results = harness.tool_results();
        let names: Vec<&str> = results.iter().map(|r| r.tool_name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Bash", "Read", "Grep"],
            "each result is attributed to the tool_use it correlates with, \
             not to arrival order"
        );
        assert_eq!(
            results[0].output.as_deref(),
            Some("bash output"),
            "Bash output is kept verbatim for inline rendering"
        );
        assert_eq!(
            results[1].output, None,
            "a non-Bash tool is covered by its summary alone"
        );
    }

    #[test]
    fn foreground_tool_use_emits_a_running_row_before_its_result() {
        let mut harness = StreamHarness::new();

        harness.feed(serde_json::json!({
            "type": "assistant",
            "message": { "content": [
                tool_use("toolu_r", "Read", serde_json::json!({ "file_path": "/etc/hostname" })),
            ]},
        }));

        // The running row is emitted at call time, before any result, and
        // carries the tool name + a call-time summary derived from the input.
        let running = harness.running_tools();
        assert_eq!(running.len(), 1, "one running row for one foreground tool");
        assert_eq!(running[0].tool_name, "Read");
        assert_eq!(running[0].tool_use_id, "toolu_r");
        assert!(
            running[0].summary.contains("hostname"),
            "the running summary is derived from the tool input"
        );
        assert!(
            harness.tool_results().is_empty(),
            "no result until the tool actually runs"
        );

        // The result correlates to the same tool_use_id so the UI can resolve
        // the running row in place.
        harness.feed(user_with(vec![tool_result("toolu_r", "hostname content")]));
        let results = harness.tool_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_use_id.as_deref(), Some("toolu_r"));
    }

    #[test]
    fn subagent_internal_tool_use_emits_no_running_row() {
        let mut harness = StreamHarness::new();

        // A subagent-internal tool_use carries a `parent_tool_use_id`; its
        // result folds into the subagent's own list, so it must not get a
        // top-level running row.
        harness.feed(serde_json::json!({
            "type": "assistant",
            "parent_tool_use_id": "toolu_root",
            "message": { "content": [
                tool_use("toolu_b", "Bash", serde_json::json!({ "command": "echo hi" })),
            ]},
        }));

        assert!(
            harness.running_tools().is_empty(),
            "a subagent-internal tool has no top-level running row"
        );
    }

    /// Newer Claude Code names its subagent tool `Agent` rather than `Task`.
    /// Both must spawn and complete a subagent, or that CLI version's subagents
    /// never reach the transcript or the wire.
    #[test]
    fn agent_tool_spawns_and_completes_a_subagent() {
        let mut harness = StreamHarness::new();

        harness.feed(serde_json::json!({
            "type": "assistant",
            "message": { "content": [
                tool_use("toolu_a", "Agent", serde_json::json!({
                    "description": "look around", "subagent_type": "Explore"
                })),
            ]},
        }));
        harness.feed(user_with(vec![tool_result("toolu_a", "all done")]));

        let lifecycle: Vec<String> = harness
            .rx
            .try_iter()
            .filter_map(|r| match r {
                DaveApiResponse::SubagentSpawned(info) => {
                    Some(format!("spawned {} {}", info.task_id, info.subagent_type))
                }
                DaveApiResponse::SubagentCompleted { task_id, result } => {
                    Some(format!("completed {task_id} {result}"))
                }
                DaveApiResponse::ToolRunning(running) => {
                    Some(format!("running {}", running.tool_name))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            lifecycle,
            vec!["spawned toolu_a Explore", "completed toolu_a all done"],
            "an Agent tool_use is a subagent, not a generic running tool"
        );
    }

    #[test]
    fn task_and_todowrite_emit_no_running_row() {
        let mut harness = StreamHarness::new();

        // Task already surfaces a subagent row and TodoWrite a task-list row, so
        // neither gets a generic running row.
        harness.feed(serde_json::json!({
            "type": "assistant",
            "message": { "content": [
                tool_use("toolu_t", "Task", serde_json::json!({
                    "description": "do work", "subagent_type": "general-purpose"
                })),
                tool_use("toolu_w", "TodoWrite", serde_json::json!({ "todos": [] })),
            ]},
        }));

        assert!(
            harness.running_tools().is_empty(),
            "Task/TodoWrite keep their dedicated rows, not a generic running row"
        );
    }

    /// The `Result` the CLI closes a stopped turn with: an error with no text.
    fn interrupted_result() -> serde_json::Value {
        serde_json::json!({
            "type": "result",
            "subtype": "error_during_execution",
            "duration_ms": 1200,
            "duration_api_ms": 900,
            "is_error": true,
            "num_turns": 1,
            "session_id": "s1"
        })
    }

    /// Stopping a turn is the user's intent, so its closing `Result` must not
    /// put an error in the chat, only end the turn.
    #[test]
    fn stopped_turn_result_is_not_an_error() {
        let mut harness = StreamHarness::new();
        harness.stopped_by_user = true;
        harness.feed(interrupted_result());

        let responses: Vec<_> = harness.rx.try_iter().collect();
        assert!(
            !responses
                .iter()
                .any(|r| matches!(r, DaveApiResponse::Failed(_))),
            "a stopped turn must not surface as an error"
        );
        assert!(
            responses
                .iter()
                .any(|r| matches!(r, DaveApiResponse::QueryComplete(_))),
            "the stopped turn still ends"
        );
    }

    /// An error the user didn't cause still shows, and without result text it
    /// names the subtype instead of "Unknown error".
    #[test]
    fn unstopped_error_result_names_its_subtype() {
        let mut harness = StreamHarness::new();
        harness.feed(interrupted_result());

        let failed: Vec<String> = harness
            .rx
            .try_iter()
            .filter_map(|r| match r {
                DaveApiResponse::Failed(err) => Some(err),
                _ => None,
            })
            .collect();
        assert_eq!(
            failed,
            vec!["Claude Code ended the turn (error_during_execution)".to_string()]
        );
    }

    /// One CLI message, in the wire shape the CLI emits it.
    fn wire(message: serde_json::Value) -> ClaudeMessage {
        serde_json::from_value(message).expect("message should deserialize")
    }

    fn assistant_text() -> ClaudeMessage {
        wire(serde_json::json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "working" }] }
        }))
    }

    fn turn_result() -> ClaudeMessage {
        wire(serde_json::json!({
            "type": "result",
            "subtype": "success",
            "duration_ms": 1,
            "duration_api_ms": 1,
            "is_error": false,
            "num_turns": 1,
            "session_id": "sess-1"
        }))
    }

    fn task_system(subtype: &str, tool_use_id: &str) -> ClaudeMessage {
        wire(serde_json::json!({
            "type": "system",
            "subtype": subtype,
            "task_type": "local_bash",
            "tool_use_id": tool_use_id,
            "status": "completed"
        }))
    }

    /// A turn in flight is never reaped, however long it runs; its `Result`
    /// opens the idle window, measured from the last thing the CLI said.
    #[test]
    fn idle_deadline_waits_for_the_turn_to_finish() {
        let t0 = Instant::now();
        let mut idle = IdleTracker::new(t0);
        assert_eq!(idle.deadline(), Some(t0 + IDLE_REAP_AFTER));

        idle.turn_started(t0);
        assert_eq!(idle.deadline(), None, "a sent query is a turn in flight");

        let t1 = t0 + Duration::from_secs(2 * 60 * 60);
        idle.on_message(t1, &assistant_text());
        assert_eq!(idle.deadline(), None, "a long turn is not idle");

        let t2 = t1 + Duration::from_secs(5);
        idle.on_message(t2, &turn_result());
        assert_eq!(idle.deadline(), Some(t2 + IDLE_REAP_AFTER));
    }

    /// A wake-up turn starts without a command from us; its first message
    /// must still hold the CLI open until its `Result`.
    #[test]
    fn idle_deadline_holds_for_a_spontaneous_wake_up_turn() {
        let t0 = Instant::now();
        let mut idle = IdleTracker::new(t0);
        idle.on_message(t0, &assistant_text());
        assert_eq!(idle.deadline(), None);
        idle.on_message(t0, &turn_result());
        assert!(idle.deadline().is_some());
    }

    /// A `run_in_background` task lives in the CLI process: stopping the CLI
    /// between turns would kill it, so the window stays shut until the task
    /// reports back.
    #[test]
    fn idle_deadline_waits_for_background_tasks() {
        let t0 = Instant::now();
        let mut idle = IdleTracker::new(t0);
        idle.turn_started(t0);
        idle.on_message(t0, &task_system("task_started", "toolu_bg"));
        idle.on_message(t0, &turn_result());
        assert_eq!(idle.deadline(), None, "background task still running");

        let t1 = t0 + Duration::from_secs(90 * 60);
        idle.on_message(t1, &task_system("task_notification", "toolu_bg"));
        assert_eq!(idle.deadline(), Some(t1 + IDLE_REAP_AFTER));
    }

    /// Any activity between turns pushes the window out.
    #[test]
    fn idle_deadline_moves_with_activity() {
        let t0 = Instant::now();
        let mut idle = IdleTracker::new(t0);
        let t1 = t0 + Duration::from_secs(30 * 60);
        idle.touch(t1);
        assert_eq!(idle.deadline(), Some(t1 + IDLE_REAP_AFTER));
    }
}
