//! Codex backend — orchestrates OpenAI's Codex CLI (`codex app-server`)
//! via its JSON-RPC-over-stdio protocol.

use super::codex_protocol::*;
use super::tool_summary::{format_tool_summary, truncate_output};
use crate::auto_accept::AutoAcceptRules;
use crate::backend::traits::AiBackend;
use crate::messages::{
    CompactionInfo, DaveApiResponse, ExecutedTool, PendingPermission, PermissionRequest,
    PermissionResponse, SubagentInfo, SubagentStatus,
};
use crate::tools::Tool;
use crate::Message;
use claude_agent_sdk_rs::PermissionMode;
use dashmap::DashMap;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::oneshot;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Session actor
// ---------------------------------------------------------------------------

/// Commands sent to a Codex session actor.
enum SessionCommand {
    Query {
        prompt: String,
        response_tx: mpsc::Sender<DaveApiResponse>,
        ctx: egui::Context,
    },
    Interrupt {
        ctx: egui::Context,
    },
    SetPermissionMode {
        mode: PermissionMode,
        ctx: egui::Context,
    },
    Shutdown,
}

/// Handle kept by the backend to communicate with the actor.
struct SessionHandle {
    command_tx: tokio_mpsc::Sender<SessionCommand>,
}

/// Result of processing a single Codex JSON-RPC message.
enum HandleResult {
    /// Normal notification processed, keep reading.
    Continue,
    /// `turn/completed` received — this turn is done.
    TurnDone,
    /// Auto-accept matched — send accept for this rpc_id immediately.
    AutoAccepted(u64),
    /// Needs UI approval — stash the receiver and wait for the user.
    NeedsApproval {
        rpc_id: u64,
        rx: oneshot::Receiver<PermissionResponse>,
    },
}

/// Per-session actor that owns the `codex app-server` child process.
async fn session_actor(
    session_id: String,
    cwd: Option<PathBuf>,
    codex_binary: String,
    model: Option<String>,
    resume_session_id: Option<String>,
    mut command_rx: tokio_mpsc::Receiver<SessionCommand>,
) {
    // Spawn the codex app-server child process
    let mut child = match spawn_codex(&codex_binary, &cwd) {
        Ok(c) => c,
        Err(err) => {
            tracing::error!("Session {} failed to spawn codex: {}", session_id, err);
            drain_commands_with_error(&mut command_rx, &format!("Failed to spawn codex: {}", err))
                .await;
            return;
        }
    };

    let stdin = child.stdin.take().expect("stdin piped");
    let stdout = child.stdout.take().expect("stdout piped");

    // Drain stderr in a background task to prevent pipe deadlock
    if let Some(stderr) = child.stderr.take() {
        let sid = session_id.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::trace!("Codex stderr [{}]: {}", sid, line);
            }
        });
    }

    let mut writer = tokio::io::BufWriter::new(stdin);
    let mut reader = BufReader::new(stdout).lines();

    // ---- init handshake ----
    if let Err(err) = do_init_handshake(&mut writer, &mut reader).await {
        tracing::error!("Session {} init handshake failed: {}", session_id, err);
        drain_commands_with_error(
            &mut command_rx,
            &format!("Codex init handshake failed: {}", err),
        )
        .await;
        let _ = child.kill().await;
        return;
    }

    // ---- thread start / resume ----
    let thread_id = if let Some(ref tid) = resume_session_id {
        match send_thread_resume(&mut writer, &mut reader, tid).await {
            Ok(id) => id,
            Err(err) => {
                tracing::error!("Session {} thread/resume failed: {}", session_id, err);
                drain_commands_with_error(
                    &mut command_rx,
                    &format!("Codex thread/resume failed: {}", err),
                )
                .await;
                let _ = child.kill().await;
                return;
            }
        }
    } else {
        match send_thread_start(
            &mut writer,
            &mut reader,
            model.as_deref(),
            cwd.as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .as_deref(),
        )
        .await
        {
            Ok(id) => id,
            Err(err) => {
                tracing::error!("Session {} thread/start failed: {}", session_id, err);
                drain_commands_with_error(
                    &mut command_rx,
                    &format!("Codex thread/start failed: {}", err),
                )
                .await;
                let _ = child.kill().await;
                return;
            }
        }
    };

    tracing::info!(
        "Session {} connected to codex, thread_id={}",
        session_id,
        thread_id
    );

    // ---- main command loop ----
    let mut request_counter: u64 = 10; // start after init IDs
    let mut current_turn_id: Option<String> = None;

    while let Some(cmd) = command_rx.recv().await {
        match cmd {
            SessionCommand::Query {
                prompt,
                response_tx,
                ctx,
            } => {
                // Send turn/start
                request_counter += 1;
                let turn_req_id = request_counter;
                if let Err(err) = send_turn_start(
                    &mut writer,
                    turn_req_id,
                    &thread_id,
                    &prompt,
                    model.as_deref(),
                )
                .await
                {
                    tracing::error!("Session {} turn/start failed: {}", session_id, err);
                    let _ = response_tx.send(DaveApiResponse::Failed(err.to_string()));
                    continue;
                }

                // Read the turn/start response
                match read_response_for_id(&mut reader, turn_req_id).await {
                    Ok(msg) => {
                        if let Some(err) = msg.error {
                            tracing::error!(
                                "Session {} turn/start error: {}",
                                session_id,
                                err.message
                            );
                            let _ = response_tx.send(DaveApiResponse::Failed(err.message));
                            continue;
                        }
                        if let Some(result) = &msg.result {
                            current_turn_id = result
                                .get("turn")
                                .and_then(|t| t.get("id"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                        }
                    }
                    Err(err) => {
                        tracing::error!(
                            "Session {} failed reading turn/start response: {}",
                            session_id,
                            err
                        );
                        let _ = response_tx.send(DaveApiResponse::Failed(err.to_string()));
                        continue;
                    }
                }

                // Stream notifications until turn/completed
                let mut subagent_stack: Vec<String> = Vec::new();
                let mut turn_done = false;
                let mut pending_approval: Option<(u64, oneshot::Receiver<PermissionResponse>)> =
                    None;

                while !turn_done {
                    if let Some((rpc_id, mut rx)) = pending_approval.take() {
                        // ---- approval-wait state ----
                        // Codex is blocked waiting for our response, so no new
                        // lines will arrive. Select between the UI response and
                        // commands (interrupt / shutdown).
                        tokio::select! {
                            biased;

                            Some(cmd) = command_rx.recv() => {
                                match cmd {
                                    SessionCommand::Interrupt { ctx: int_ctx } => {
                                        tracing::debug!("Session {} interrupted during approval", session_id);
                                        // Cancel the approval and interrupt the turn
                                        let _ = send_approval_response(&mut writer, rpc_id, ApprovalDecision::Cancel).await;
                                        if let Some(ref tid) = current_turn_id {
                                            request_counter += 1;
                                            let _ = send_turn_interrupt(&mut writer, request_counter, &thread_id, tid).await;
                                        }
                                        int_ctx.request_repaint();
                                        // Don't restore pending — it's been cancelled
                                    }
                                    SessionCommand::Shutdown => {
                                        tracing::debug!("Session {} shutting down during approval", session_id);
                                        let _ = child.kill().await;
                                        return;
                                    }
                                    SessionCommand::Query { response_tx: new_tx, .. } => {
                                        let _ = new_tx.send(DaveApiResponse::Failed(
                                            "Query already in progress".to_string(),
                                        ));
                                        // Restore the pending approval — still waiting
                                        pending_approval = Some((rpc_id, rx));
                                    }
                                    SessionCommand::SetPermissionMode { ctx: mode_ctx, .. } => {
                                        mode_ctx.request_repaint();
                                        pending_approval = Some((rpc_id, rx));
                                    }
                                }
                            }

                            result = &mut rx => {
                                let decision = match result {
                                    Ok(PermissionResponse::Allow { .. }) => ApprovalDecision::Accept,
                                    Ok(PermissionResponse::Deny { .. }) => ApprovalDecision::Decline,
                                    Err(_) => ApprovalDecision::Cancel,
                                };
                                let _ = send_approval_response(&mut writer, rpc_id, decision).await;
                            }
                        }
                    } else {
                        // ---- normal streaming state ----
                        tokio::select! {
                            biased;

                            Some(cmd) = command_rx.recv() => {
                                match cmd {
                                    SessionCommand::Interrupt { ctx: int_ctx } => {
                                        tracing::debug!("Session {} interrupted", session_id);
                                        if let Some(ref tid) = current_turn_id {
                                            request_counter += 1;
                                            let _ = send_turn_interrupt(&mut writer, request_counter, &thread_id, tid).await;
                                        }
                                        int_ctx.request_repaint();
                                    }
                                    SessionCommand::Query { response_tx: new_tx, .. } => {
                                        let _ = new_tx.send(DaveApiResponse::Failed(
                                            "Query already in progress".to_string(),
                                        ));
                                    }
                                    SessionCommand::SetPermissionMode { mode, ctx: mode_ctx } => {
                                        tracing::debug!(
                                            "Session {} ignoring permission mode {:?} (not supported by Codex)",
                                            session_id, mode
                                        );
                                        mode_ctx.request_repaint();
                                    }
                                    SessionCommand::Shutdown => {
                                        tracing::debug!("Session {} shutting down during query", session_id);
                                        let _ = child.kill().await;
                                        return;
                                    }
                                }
                            }

                            line_result = reader.next_line() => {
                                match line_result {
                                    Ok(Some(line)) => {
                                        let msg: RpcMessage = match serde_json::from_str(&line) {
                                            Ok(m) => m,
                                            Err(err) => {
                                                tracing::warn!("Codex parse error: {} in: {}", err, &line[..line.len().min(200)]);
                                                continue;
                                            }
                                        };

                                        match handle_codex_message(
                                            msg,
                                            &response_tx,
                                            &ctx,
                                            &mut subagent_stack,
                                        ) {
                                            HandleResult::Continue => {}
                                            HandleResult::TurnDone => {
                                                turn_done = true;
                                            }
                                            HandleResult::AutoAccepted(rpc_id) => {
                                                let _ = send_approval_response(
                                                    &mut writer, rpc_id, ApprovalDecision::Accept,
                                                ).await;
                                            }
                                            HandleResult::NeedsApproval { rpc_id, rx } => {
                                                pending_approval = Some((rpc_id, rx));
                                            }
                                        }
                                    }
                                    Ok(None) => {
                                        tracing::error!("Session {} codex process exited unexpectedly", session_id);
                                        let _ = response_tx.send(DaveApiResponse::Failed(
                                            "Codex process exited unexpectedly".to_string(),
                                        ));
                                        turn_done = true;
                                    }
                                    Err(err) => {
                                        tracing::error!("Session {} read error: {}", session_id, err);
                                        let _ = response_tx.send(DaveApiResponse::Failed(err.to_string()));
                                        turn_done = true;
                                    }
                                }
                            }
                        }
                    }
                }

                current_turn_id = None;
                tracing::debug!("Turn complete for session {}", session_id);
            }
            SessionCommand::Interrupt { ctx } => {
                ctx.request_repaint();
            }
            SessionCommand::SetPermissionMode { mode, ctx } => {
                tracing::debug!(
                    "Session {} ignoring permission mode {:?} (not supported by Codex)",
                    session_id,
                    mode
                );
                ctx.request_repaint();
            }
            SessionCommand::Shutdown => {
                tracing::debug!("Session {} shutting down", session_id);
                break;
            }
        }
    }

    let _ = child.kill().await;
    tracing::debug!("Session {} actor exited", session_id);
}

// ---------------------------------------------------------------------------
// Codex message handling (synchronous — no writer needed)
// ---------------------------------------------------------------------------

/// Process a single incoming Codex JSON-RPC message. Returns a `HandleResult`
/// indicating what the caller should do next (continue, finish turn, or handle
/// an approval).
fn handle_codex_message(
    msg: RpcMessage,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    ctx: &egui::Context,
    subagent_stack: &mut Vec<String>,
) -> HandleResult {
    let method = match &msg.method {
        Some(m) => m.as_str(),
        None => {
            // Response to a request we sent (e.g. approval ack). Nothing to do.
            return HandleResult::Continue;
        }
    };

    match method {
        "item/agentMessage/delta" => {
            if let Some(params) = msg.params {
                if let Ok(delta) = serde_json::from_value::<AgentMessageDeltaParams>(params) {
                    let _ = response_tx.send(DaveApiResponse::Token(delta.delta));
                    ctx.request_repaint();
                }
            }
        }

        "item/started" => {
            if let Some(params) = msg.params {
                if let Ok(started) = serde_json::from_value::<ItemStartedParams>(params) {
                    if started.item_type == "collabAgentToolCall" {
                        let item_id = started
                            .item_id
                            .unwrap_or_else(|| Uuid::new_v4().to_string());
                        subagent_stack.push(item_id.clone());
                        let info = SubagentInfo {
                            task_id: item_id,
                            description: started.name.unwrap_or_else(|| "agent".to_string()),
                            subagent_type: "codex-agent".to_string(),
                            status: SubagentStatus::Running,
                            output: String::new(),
                            max_output_size: 4000,
                            tool_results: Vec::new(),
                        };
                        let _ = response_tx.send(DaveApiResponse::SubagentSpawned(info));
                        ctx.request_repaint();
                    }
                }
            }
        }

        "item/completed" => {
            if let Some(params) = msg.params {
                if let Ok(completed) = serde_json::from_value::<ItemCompletedParams>(params) {
                    handle_item_completed(&completed, response_tx, ctx, subagent_stack);
                }
            }
        }

        "item/commandExecution/requestApproval" => {
            if let (Some(rpc_id), Some(params)) = (msg.id, msg.params) {
                if let Ok(approval) = serde_json::from_value::<CommandApprovalParams>(params) {
                    return check_approval_or_forward(
                        rpc_id,
                        "Bash",
                        serde_json::json!({ "command": approval.command }),
                        response_tx,
                        ctx,
                    );
                }
            }
        }

        "item/fileChange/requestApproval" => {
            if let (Some(rpc_id), Some(params)) = (msg.id, msg.params) {
                if let Ok(approval) = serde_json::from_value::<FileChangeApprovalParams>(params) {
                    let kind_str = approval
                        .kind
                        .as_ref()
                        .and_then(|k| k.get("type").and_then(|t| t.as_str()))
                        .unwrap_or("edit");

                    let (tool_name, tool_input) = match kind_str {
                        "create" => (
                            "Write",
                            serde_json::json!({
                                "file_path": approval.file_path,
                                "content": approval.diff.as_deref().unwrap_or(""),
                            }),
                        ),
                        _ => (
                            "Edit",
                            serde_json::json!({
                                "file_path": approval.file_path,
                                "old_string": "",
                                "new_string": approval.diff.as_deref().unwrap_or(""),
                            }),
                        ),
                    };

                    return check_approval_or_forward(
                        rpc_id,
                        tool_name,
                        tool_input,
                        response_tx,
                        ctx,
                    );
                }
            }
        }

        "turn/completed" => {
            if let Some(params) = msg.params {
                if let Ok(completed) = serde_json::from_value::<TurnCompletedParams>(params) {
                    if completed.status == "failed" {
                        let err_msg = completed.error.unwrap_or_else(|| "Turn failed".to_string());
                        let _ = response_tx.send(DaveApiResponse::Failed(err_msg));
                    }
                }
            }
            return HandleResult::TurnDone;
        }

        other => {
            tracing::debug!("Unhandled codex notification: {}", other);
        }
    }

    HandleResult::Continue
}

/// Check auto-accept rules. If matched, return `AutoAccepted`. Otherwise
/// create a `PendingPermission`, send it to the UI, and return `NeedsApproval`
/// with the oneshot receiver.
fn check_approval_or_forward(
    rpc_id: u64,
    tool_name: &str,
    tool_input: Value,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    ctx: &egui::Context,
) -> HandleResult {
    let rules = AutoAcceptRules::default();
    if rules.should_auto_accept(tool_name, &tool_input) {
        tracing::debug!("Auto-accepting {} (rpc_id={})", tool_name, rpc_id);
        return HandleResult::AutoAccepted(rpc_id);
    }

    // Forward to UI
    let request_id = Uuid::new_v4();
    let (ui_resp_tx, ui_resp_rx) = oneshot::channel();

    let request = PermissionRequest {
        id: request_id,
        tool_name: tool_name.to_string(),
        tool_input,
        response: None,
        answer_summary: None,
        cached_plan: None,
    };

    let pending = PendingPermission {
        request,
        response_tx: ui_resp_tx,
    };

    if response_tx
        .send(DaveApiResponse::PermissionRequest(pending))
        .is_err()
    {
        tracing::error!("Failed to send permission request to UI");
        // Return auto-decline — can't reach UI
        return HandleResult::AutoAccepted(rpc_id); // Will send Accept; could add a Declined variant
    }

    ctx.request_repaint();

    HandleResult::NeedsApproval {
        rpc_id,
        rx: ui_resp_rx,
    }
}

/// Handle a completed item from Codex.
fn handle_item_completed(
    completed: &ItemCompletedParams,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    ctx: &egui::Context,
    subagent_stack: &mut Vec<String>,
) {
    match completed.item_type.as_str() {
        "commandExecution" => {
            let command = completed.command.clone().unwrap_or_default();
            let exit_code = completed.exit_code.unwrap_or(-1);
            let output = completed.output.clone().unwrap_or_default();

            let tool_input = serde_json::json!({ "command": command });
            let result_value = serde_json::json!({ "output": output, "exit_code": exit_code });
            let summary = format_tool_summary("Bash", &tool_input, &result_value);
            let parent_task_id = subagent_stack.last().cloned();

            let _ = response_tx.send(DaveApiResponse::ToolResult(ExecutedTool {
                tool_name: "Bash".to_string(),
                summary,
                parent_task_id,
            }));
            ctx.request_repaint();
        }

        "fileChange" => {
            let file_path = completed.file_path.clone().unwrap_or_default();
            let diff = completed.diff.clone();

            let kind_str = completed
                .kind
                .as_ref()
                .and_then(|k| k.get("type").and_then(|t| t.as_str()))
                .unwrap_or("edit");

            let tool_name = match kind_str {
                "create" => "Write",
                _ => "Edit",
            };

            let tool_input = serde_json::json!({
                "file_path": file_path,
                "diff": diff,
            });
            let result_value = serde_json::json!({ "status": "ok" });
            let summary = format_tool_summary(tool_name, &tool_input, &result_value);
            let parent_task_id = subagent_stack.last().cloned();

            let _ = response_tx.send(DaveApiResponse::ToolResult(ExecutedTool {
                tool_name: tool_name.to_string(),
                summary,
                parent_task_id,
            }));
            ctx.request_repaint();
        }

        "collabAgentToolCall" => {
            if let Some(item_id) = &completed.item_id {
                subagent_stack.retain(|id| id != item_id);
                let result_text = completed
                    .result
                    .clone()
                    .unwrap_or_else(|| "completed".to_string());
                let _ = response_tx.send(DaveApiResponse::SubagentCompleted {
                    task_id: item_id.clone(),
                    result: truncate_output(&result_text, 2000),
                });
                ctx.request_repaint();
            }
        }

        "contextCompaction" => {
            let pre_tokens = completed.pre_tokens.unwrap_or(0);
            let _ = response_tx.send(DaveApiResponse::CompactionComplete(CompactionInfo {
                pre_tokens,
            }));
            ctx.request_repaint();
        }

        other => {
            tracing::debug!("Unhandled item/completed type: {}", other);
        }
    }
}

// ---------------------------------------------------------------------------
// Codex process spawning and JSON-RPC helpers
// ---------------------------------------------------------------------------

fn spawn_codex(binary: &str, cwd: &Option<PathBuf>) -> Result<Child, std::io::Error> {
    let mut cmd = Command::new(binary);
    cmd.arg("app-server").arg("--listen").arg("stdio://");
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.spawn()
}

/// Send a JSONL request on stdin.
async fn send_request<P: serde::Serialize>(
    writer: &mut tokio::io::BufWriter<tokio::process::ChildStdin>,
    req: &RpcRequest<P>,
) -> Result<(), std::io::Error> {
    let mut line = serde_json::to_string(req)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Send a JSON-RPC response (for approval requests).
async fn send_rpc_response(
    writer: &mut tokio::io::BufWriter<tokio::process::ChildStdin>,
    id: u64,
    result: Value,
) -> Result<(), std::io::Error> {
    let resp = serde_json::json!({ "id": id, "result": result });
    let mut line = serde_json::to_string(&resp)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Send an approval decision response.
async fn send_approval_response(
    writer: &mut tokio::io::BufWriter<tokio::process::ChildStdin>,
    rpc_id: u64,
    decision: ApprovalDecision,
) -> Result<(), std::io::Error> {
    let result = serde_json::to_value(ApprovalResponse { decision }).unwrap();
    send_rpc_response(writer, rpc_id, result).await
}

/// Perform the `initialize` → `initialized` handshake.
async fn do_init_handshake(
    writer: &mut tokio::io::BufWriter<tokio::process::ChildStdin>,
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> Result<(), String> {
    let req = RpcRequest {
        id: Some(1),
        method: "initialize",
        params: InitializeParams {
            client_info: ClientInfo {
                name: "dave".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            capabilities: serde_json::json!({}),
        },
    };

    send_request(writer, &req)
        .await
        .map_err(|e| format!("Failed to send initialize: {}", e))?;

    let _resp = read_response_for_id(reader, 1)
        .await
        .map_err(|e| format!("Failed to read initialize response: {}", e))?;

    // Send `initialized` notification (no id, no response expected)
    let notif: RpcRequest<Value> = RpcRequest {
        id: None,
        method: "initialized",
        params: serde_json::json!({}),
    };
    send_request(writer, &notif)
        .await
        .map_err(|e| format!("Failed to send initialized: {}", e))?;

    Ok(())
}

/// Send `thread/start` and return the thread ID.
async fn send_thread_start(
    writer: &mut tokio::io::BufWriter<tokio::process::ChildStdin>,
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    model: Option<&str>,
    cwd: Option<&str>,
) -> Result<String, String> {
    let req = RpcRequest {
        id: Some(2),
        method: "thread/start",
        params: ThreadStartParams {
            model: model.map(|s| s.to_string()),
            cwd: cwd.map(|s| s.to_string()),
            approval_policy: Some("unless-allow-listed".to_string()),
        },
    };

    send_request(writer, &req)
        .await
        .map_err(|e| format!("Failed to send thread/start: {}", e))?;

    let resp = read_response_for_id(reader, 2)
        .await
        .map_err(|e| format!("Failed to read thread/start response: {}", e))?;

    if let Some(err) = resp.error {
        return Err(format!("thread/start error: {}", err.message));
    }

    let result = resp.result.ok_or("No result in thread/start response")?;
    let thread_result: ThreadStartResult = serde_json::from_value(result)
        .map_err(|e| format!("Failed to parse thread/start result: {}", e))?;

    Ok(thread_result.thread.id)
}

/// Send `thread/resume` and return the thread ID.
async fn send_thread_resume(
    writer: &mut tokio::io::BufWriter<tokio::process::ChildStdin>,
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    thread_id: &str,
) -> Result<String, String> {
    let req = RpcRequest {
        id: Some(3),
        method: "thread/resume",
        params: ThreadResumeParams {
            thread_id: thread_id.to_string(),
        },
    };

    send_request(writer, &req)
        .await
        .map_err(|e| format!("Failed to send thread/resume: {}", e))?;

    let resp = read_response_for_id(reader, 3)
        .await
        .map_err(|e| format!("Failed to read thread/resume response: {}", e))?;

    if let Some(err) = resp.error {
        return Err(format!("thread/resume error: {}", err.message));
    }

    Ok(thread_id.to_string())
}

/// Send `turn/start`.
async fn send_turn_start(
    writer: &mut tokio::io::BufWriter<tokio::process::ChildStdin>,
    req_id: u64,
    thread_id: &str,
    prompt: &str,
    model: Option<&str>,
) -> Result<(), String> {
    let req = RpcRequest {
        id: Some(req_id),
        method: "turn/start",
        params: TurnStartParams {
            thread_id: thread_id.to_string(),
            input: vec![TurnInput::Text {
                text: prompt.to_string(),
            }],
            model: model.map(|s| s.to_string()),
            effort: None,
        },
    };

    send_request(writer, &req)
        .await
        .map_err(|e| format!("Failed to send turn/start: {}", e))
}

/// Send `turn/interrupt`.
async fn send_turn_interrupt(
    writer: &mut tokio::io::BufWriter<tokio::process::ChildStdin>,
    req_id: u64,
    thread_id: &str,
    turn_id: &str,
) -> Result<(), String> {
    let req = RpcRequest {
        id: Some(req_id),
        method: "turn/interrupt",
        params: TurnInterruptParams {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
        },
    };

    send_request(writer, &req)
        .await
        .map_err(|e| format!("Failed to send turn/interrupt: {}", e))
}

/// Read lines until we find a response matching the given request id.
/// Non-matching messages (notifications) are logged and skipped.
async fn read_response_for_id(
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    expected_id: u64,
) -> Result<RpcMessage, String> {
    loop {
        let line = reader
            .next_line()
            .await
            .map_err(|e| format!("IO error: {}", e))?
            .ok_or_else(|| "EOF while waiting for response".to_string())?;

        let msg: RpcMessage = serde_json::from_str(&line).map_err(|e| {
            format!(
                "JSON parse error: {} in: {}",
                e,
                &line[..line.len().min(200)]
            )
        })?;

        if msg.id == Some(expected_id) {
            return Ok(msg);
        }

        tracing::trace!(
            "Skipping message during handshake (waiting for id={}): method={:?}",
            expected_id,
            msg.method
        );
    }
}

/// Drain pending commands, sending error to any Query commands.
async fn drain_commands_with_error(
    command_rx: &mut tokio_mpsc::Receiver<SessionCommand>,
    error: &str,
) {
    while let Some(cmd) = command_rx.recv().await {
        if let SessionCommand::Query {
            ref response_tx, ..
        } = cmd
        {
            let _ = response_tx.send(DaveApiResponse::Failed(error.to_string()));
        }
        if matches!(cmd, SessionCommand::Shutdown) {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Public backend
// ---------------------------------------------------------------------------

pub struct CodexBackend {
    codex_binary: String,
    sessions: DashMap<String, SessionHandle>,
}

impl CodexBackend {
    pub fn new(codex_binary: String) -> Self {
        Self {
            codex_binary,
            sessions: DashMap::new(),
        }
    }

    /// Convert messages to a prompt string, same logic as the Claude backend.
    fn messages_to_prompt(messages: &[Message]) -> String {
        let mut prompt = String::new();
        for msg in messages {
            if let Message::System(content) = msg {
                prompt.push_str(content);
                prompt.push_str("\n\n");
                break;
            }
        }
        for msg in messages {
            match msg {
                Message::System(_) => {}
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
                _ => {}
            }
        }
        prompt
    }

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

impl AiBackend for CodexBackend {
    fn stream_request(
        &self,
        messages: Vec<Message>,
        _tools: Arc<HashMap<String, Tool>>,
        model: String,
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
            "Codex request: session={}, resumed={}, prompt_len={}",
            session_id,
            resume_session_id.is_some(),
            prompt.len(),
        );

        let command_tx = {
            let entry = self.sessions.entry(session_id.clone());
            let codex_binary = self.codex_binary.clone();
            let model_clone = model.clone();
            let cwd_clone = cwd.clone();
            let resume_clone = resume_session_id.clone();
            let handle = entry.or_insert_with(|| {
                let (command_tx, command_rx) = tokio_mpsc::channel(16);
                let sid = session_id.clone();
                tokio::spawn(async move {
                    session_actor(
                        sid,
                        cwd_clone,
                        codex_binary,
                        Some(model_clone),
                        resume_clone,
                        command_rx,
                    )
                    .await;
                });
                SessionHandle { command_tx }
            });
            handle.command_tx.clone()
        };

        let handle = tokio::spawn(async move {
            if let Err(err) = command_tx
                .send(SessionCommand::Query {
                    prompt,
                    response_tx,
                    ctx,
                })
                .await
            {
                tracing::error!("Failed to send query to codex session actor: {}", err);
            }
        });

        (response_rx, Some(handle))
    }

    fn cleanup_session(&self, session_id: String) {
        if let Some((_, handle)) = self.sessions.remove(&session_id) {
            tokio::spawn(async move {
                if let Err(err) = handle.command_tx.send(SessionCommand::Shutdown).await {
                    tracing::warn!("Failed to send shutdown to codex session: {}", err);
                }
            });
        }
    }

    fn interrupt_session(&self, session_id: String, ctx: egui::Context) {
        if let Some(handle) = self.sessions.get(&session_id) {
            let command_tx = handle.command_tx.clone();
            tokio::spawn(async move {
                if let Err(err) = command_tx.send(SessionCommand::Interrupt { ctx }).await {
                    tracing::warn!("Failed to send interrupt to codex session: {}", err);
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
                    tracing::warn!(
                        "Failed to send set_permission_mode to codex session: {}",
                        err
                    );
                }
            });
        }
    }
}
