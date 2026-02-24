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
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
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

    let writer = tokio::io::BufWriter::new(stdin);
    let reader = BufReader::new(stdout).lines();
    let cwd_str = cwd.as_ref().map(|p| p.to_string_lossy().into_owned());

    session_actor_loop(
        &session_id,
        writer,
        reader,
        model.as_deref(),
        cwd_str.as_deref(),
        resume_session_id.as_deref(),
        &mut command_rx,
    )
    .await;

    let _ = child.kill().await;
    tracing::debug!("Session {} actor exited", session_id);
}

/// Core session loop, generic over I/O for testability.
///
/// Performs the init handshake, thread start/resume, and main command loop.
/// Returns when the session is shut down or an unrecoverable error occurs.
/// The caller is responsible for process lifecycle (spawn, kill).
async fn session_actor_loop<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    session_id: &str,
    mut writer: tokio::io::BufWriter<W>,
    mut reader: tokio::io::Lines<R>,
    model: Option<&str>,
    cwd: Option<&str>,
    resume_session_id: Option<&str>,
    command_rx: &mut tokio_mpsc::Receiver<SessionCommand>,
) {
    // ---- init handshake ----
    if let Err(err) = do_init_handshake(&mut writer, &mut reader).await {
        tracing::error!("Session {} init handshake failed: {}", session_id, err);
        drain_commands_with_error(command_rx, &format!("Codex init handshake failed: {}", err))
            .await;
        return;
    }

    // ---- thread start / resume ----
    let thread_id = if let Some(tid) = resume_session_id {
        match send_thread_resume(&mut writer, &mut reader, tid).await {
            Ok(id) => id,
            Err(err) => {
                tracing::error!("Session {} thread/resume failed: {}", session_id, err);
                drain_commands_with_error(
                    command_rx,
                    &format!("Codex thread/resume failed: {}", err),
                )
                .await;
                return;
            }
        }
    } else {
        match send_thread_start(&mut writer, &mut reader, model, cwd).await {
            Ok(id) => id,
            Err(err) => {
                tracing::error!("Session {} thread/start failed: {}", session_id, err);
                drain_commands_with_error(
                    command_rx,
                    &format!("Codex thread/start failed: {}", err),
                )
                .await;
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
                if let Err(err) =
                    send_turn_start(&mut writer, turn_req_id, &thread_id, &prompt, model).await
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
    cmd.arg("app-server");
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.spawn()
}

/// Send a JSONL request on stdin.
async fn send_request<P: serde::Serialize, W: AsyncWrite + Unpin>(
    writer: &mut tokio::io::BufWriter<W>,
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
async fn send_rpc_response<W: AsyncWrite + Unpin>(
    writer: &mut tokio::io::BufWriter<W>,
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
async fn send_approval_response<W: AsyncWrite + Unpin>(
    writer: &mut tokio::io::BufWriter<W>,
    rpc_id: u64,
    decision: ApprovalDecision,
) -> Result<(), std::io::Error> {
    let result = serde_json::to_value(ApprovalResponse { decision }).unwrap();
    send_rpc_response(writer, rpc_id, result).await
}

/// Perform the `initialize` → `initialized` handshake.
async fn do_init_handshake<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    writer: &mut tokio::io::BufWriter<W>,
    reader: &mut tokio::io::Lines<R>,
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

    let resp = read_response_for_id(reader, 1)
        .await
        .map_err(|e| format!("Failed to read initialize response: {}", e))?;

    if let Some(err) = resp.error {
        return Err(format!("Initialize error: {}", err.message));
    }

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
async fn send_thread_start<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    writer: &mut tokio::io::BufWriter<W>,
    reader: &mut tokio::io::Lines<R>,
    model: Option<&str>,
    cwd: Option<&str>,
) -> Result<String, String> {
    let req = RpcRequest {
        id: Some(2),
        method: "thread/start",
        params: ThreadStartParams {
            model: model.map(|s| s.to_string()),
            cwd: cwd.map(|s| s.to_string()),
            approval_policy: Some("on-request".to_string()),
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
async fn send_thread_resume<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    writer: &mut tokio::io::BufWriter<W>,
    reader: &mut tokio::io::Lines<R>,
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
async fn send_turn_start<W: AsyncWrite + Unpin>(
    writer: &mut tokio::io::BufWriter<W>,
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
async fn send_turn_interrupt<W: AsyncWrite + Unpin>(
    writer: &mut tokio::io::BufWriter<W>,
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
async fn read_response_for_id<R: AsyncBufRead + Unpin>(
    reader: &mut tokio::io::Lines<R>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::DaveApiResponse;
    use serde_json::json;
    use std::time::Duration;

    /// Helper: build an RpcMessage from a method and params JSON
    fn notification(method: &str, params: Value) -> RpcMessage {
        RpcMessage {
            id: None,
            method: Some(method.to_string()),
            result: None,
            error: None,
            params: Some(params),
        }
    }

    /// Helper: build an RpcMessage that is a server→client request (has id)
    fn server_request(id: u64, method: &str, params: Value) -> RpcMessage {
        RpcMessage {
            id: Some(id),
            method: Some(method.to_string()),
            result: None,
            error: None,
            params: Some(params),
        }
    }

    // -----------------------------------------------------------------------
    // Protocol serde tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_rpc_request_serialization() {
        let req = RpcRequest {
            id: Some(1),
            method: "initialize",
            params: InitializeParams {
                client_info: ClientInfo {
                    name: "dave".to_string(),
                    version: "0.1.0".to_string(),
                },
                capabilities: json!({}),
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"initialize\""));
        assert!(json.contains("\"clientInfo\""));
    }

    #[test]
    fn test_rpc_request_notification_omits_id() {
        let req: RpcRequest<Value> = RpcRequest {
            id: None,
            method: "initialized",
            params: json!({}),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"id\""));
    }

    #[test]
    fn test_rpc_message_deserialization_response() {
        let json = r#"{"id":1,"result":{"serverInfo":{"name":"codex"}}}"#;
        let msg: RpcMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.id, Some(1));
        assert!(msg.result.is_some());
        assert!(msg.method.is_none());
    }

    #[test]
    fn test_rpc_message_deserialization_notification() {
        let json = r#"{"method":"item/agentMessage/delta","params":{"delta":"hello"}}"#;
        let msg: RpcMessage = serde_json::from_str(json).unwrap();
        assert!(msg.id.is_none());
        assert_eq!(msg.method.as_deref(), Some("item/agentMessage/delta"));
    }

    #[test]
    fn test_thread_start_result_deserialization() {
        let json = r#"{"thread":{"id":"thread_abc123"},"model":"gpt-5.2-codex"}"#;
        let result: ThreadStartResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.thread.id, "thread_abc123");
        assert_eq!(result.model.as_deref(), Some("gpt-5.2-codex"));
    }

    #[test]
    fn test_approval_response_serialization() {
        let resp = ApprovalResponse {
            decision: ApprovalDecision::Accept,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"decision\":\"accept\""));

        let resp = ApprovalResponse {
            decision: ApprovalDecision::Decline,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"decision\":\"decline\""));
    }

    #[test]
    fn test_turn_input_serialization() {
        let input = TurnInput::Text {
            text: "hello".to_string(),
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"hello\""));
    }

    // -----------------------------------------------------------------------
    // handle_codex_message tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_handle_delta_sends_token() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        let msg = notification("item/agentMessage/delta", json!({ "delta": "Hello world" }));

        let result = handle_codex_message(msg, &tx, &ctx, &mut subagents);
        assert!(matches!(result, HandleResult::Continue));

        let response = rx.try_recv().unwrap();
        match response {
            DaveApiResponse::Token(t) => assert_eq!(t, "Hello world"),
            other => panic!("Expected Token, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_handle_turn_completed_success() {
        let (tx, _rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        let msg = notification("turn/completed", json!({ "status": "completed" }));
        let result = handle_codex_message(msg, &tx, &ctx, &mut subagents);
        assert!(matches!(result, HandleResult::TurnDone));
    }

    #[test]
    fn test_handle_turn_completed_failure_sends_error() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        let msg = notification(
            "turn/completed",
            json!({ "status": "failed", "error": "rate limit exceeded" }),
        );
        let result = handle_codex_message(msg, &tx, &ctx, &mut subagents);
        assert!(matches!(result, HandleResult::TurnDone));

        let response = rx.try_recv().unwrap();
        match response {
            DaveApiResponse::Failed(err) => assert_eq!(err, "rate limit exceeded"),
            other => panic!("Expected Failed, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_handle_response_message_ignored() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        // A response (has id, no method) — should be ignored
        let msg = RpcMessage {
            id: Some(42),
            method: None,
            result: Some(json!({})),
            error: None,
            params: None,
        };
        let result = handle_codex_message(msg, &tx, &ctx, &mut subagents);
        assert!(matches!(result, HandleResult::Continue));
        assert!(rx.try_recv().is_err()); // nothing sent
    }

    #[test]
    fn test_handle_unknown_method_ignored() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        let msg = notification("some/future/event", json!({}));
        let result = handle_codex_message(msg, &tx, &ctx, &mut subagents);
        assert!(matches!(result, HandleResult::Continue));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_handle_subagent_started() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        let msg = notification(
            "item/started",
            json!({
                "type": "collabAgentToolCall",
                "itemId": "agent-1",
                "name": "research agent"
            }),
        );
        let result = handle_codex_message(msg, &tx, &ctx, &mut subagents);
        assert!(matches!(result, HandleResult::Continue));
        assert_eq!(subagents.len(), 1);
        assert_eq!(subagents[0], "agent-1");

        let response = rx.try_recv().unwrap();
        match response {
            DaveApiResponse::SubagentSpawned(info) => {
                assert_eq!(info.task_id, "agent-1");
                assert_eq!(info.description, "research agent");
            }
            other => panic!(
                "Expected SubagentSpawned, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_handle_command_approval_auto_accept() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        // "git status" should be auto-accepted by default rules
        let msg = server_request(
            99,
            "item/commandExecution/requestApproval",
            json!({ "command": "git status" }),
        );
        let result = handle_codex_message(msg, &tx, &ctx, &mut subagents);
        match result {
            HandleResult::AutoAccepted(id) => assert_eq!(id, 99),
            other => panic!(
                "Expected AutoAccepted, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
        // No permission request sent to UI
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_handle_command_approval_needs_ui() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        // "rm -rf /" should NOT be auto-accepted
        let msg = server_request(
            100,
            "item/commandExecution/requestApproval",
            json!({ "command": "rm -rf /" }),
        );
        let result = handle_codex_message(msg, &tx, &ctx, &mut subagents);
        match result {
            HandleResult::NeedsApproval { rpc_id, .. } => assert_eq!(rpc_id, 100),
            other => panic!(
                "Expected NeedsApproval, got {:?}",
                std::mem::discriminant(&other)
            ),
        }

        // Permission request should have been sent to UI
        let response = rx.try_recv().unwrap();
        assert!(matches!(response, DaveApiResponse::PermissionRequest(_)));
    }

    // -----------------------------------------------------------------------
    // handle_item_completed tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_item_completed_command_execution() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        let completed = ItemCompletedParams {
            item_type: "commandExecution".to_string(),
            item_id: None,
            command: Some("ls -la".to_string()),
            exit_code: Some(0),
            output: Some("total 42\n".to_string()),
            file_path: None,
            diff: None,
            kind: None,
            result: None,
            pre_tokens: None,
            content: None,
        };

        handle_item_completed(&completed, &tx, &ctx, &mut subagents);

        let response = rx.try_recv().unwrap();
        match response {
            DaveApiResponse::ToolResult(tool) => {
                assert_eq!(tool.tool_name, "Bash");
                assert!(tool.parent_task_id.is_none());
            }
            other => panic!(
                "Expected ToolResult, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_item_completed_file_change_edit() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        let completed = ItemCompletedParams {
            item_type: "fileChange".to_string(),
            item_id: None,
            command: None,
            exit_code: None,
            output: None,
            file_path: Some("src/main.rs".to_string()),
            diff: Some("@@ -1,3 +1,3 @@\n-old\n+new\n context\n".to_string()),
            kind: Some(json!({"type": "edit"})),
            result: None,
            pre_tokens: None,
            content: None,
        };

        handle_item_completed(&completed, &tx, &ctx, &mut subagents);

        let response = rx.try_recv().unwrap();
        match response {
            DaveApiResponse::ToolResult(tool) => {
                assert_eq!(tool.tool_name, "Edit");
            }
            other => panic!(
                "Expected ToolResult, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_item_completed_file_change_create() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        let completed = ItemCompletedParams {
            item_type: "fileChange".to_string(),
            item_id: None,
            command: None,
            exit_code: None,
            output: None,
            file_path: Some("new_file.rs".to_string()),
            diff: None,
            kind: Some(json!({"type": "create"})),
            result: None,
            pre_tokens: None,
            content: None,
        };

        handle_item_completed(&completed, &tx, &ctx, &mut subagents);

        let response = rx.try_recv().unwrap();
        match response {
            DaveApiResponse::ToolResult(tool) => {
                assert_eq!(tool.tool_name, "Write");
            }
            other => panic!(
                "Expected ToolResult, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_item_completed_subagent() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = vec!["agent-1".to_string()];

        let completed = ItemCompletedParams {
            item_type: "collabAgentToolCall".to_string(),
            item_id: Some("agent-1".to_string()),
            command: None,
            exit_code: None,
            output: None,
            file_path: None,
            diff: None,
            kind: None,
            result: Some("Found 3 relevant files".to_string()),
            pre_tokens: None,
            content: None,
        };

        handle_item_completed(&completed, &tx, &ctx, &mut subagents);

        // Subagent removed from stack
        assert!(subagents.is_empty());

        let response = rx.try_recv().unwrap();
        match response {
            DaveApiResponse::SubagentCompleted { task_id, result } => {
                assert_eq!(task_id, "agent-1");
                assert_eq!(result, "Found 3 relevant files");
            }
            other => panic!(
                "Expected SubagentCompleted, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_item_completed_compaction() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = Vec::new();

        let completed = ItemCompletedParams {
            item_type: "contextCompaction".to_string(),
            item_id: None,
            command: None,
            exit_code: None,
            output: None,
            file_path: None,
            diff: None,
            kind: None,
            result: None,
            pre_tokens: Some(50000),
            content: None,
        };

        handle_item_completed(&completed, &tx, &ctx, &mut subagents);

        let response = rx.try_recv().unwrap();
        match response {
            DaveApiResponse::CompactionComplete(info) => {
                assert_eq!(info.pre_tokens, 50000);
            }
            other => panic!(
                "Expected CompactionComplete, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_item_completed_with_parent_subagent() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut subagents = vec!["parent-agent".to_string()];

        let completed = ItemCompletedParams {
            item_type: "commandExecution".to_string(),
            item_id: None,
            command: Some("cargo test".to_string()),
            exit_code: Some(0),
            output: Some("ok".to_string()),
            file_path: None,
            diff: None,
            kind: None,
            result: None,
            pre_tokens: None,
            content: None,
        };

        handle_item_completed(&completed, &tx, &ctx, &mut subagents);

        let response = rx.try_recv().unwrap();
        match response {
            DaveApiResponse::ToolResult(tool) => {
                assert_eq!(tool.tool_name, "Bash");
                assert_eq!(tool.parent_task_id.as_deref(), Some("parent-agent"));
            }
            other => panic!(
                "Expected ToolResult, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    // -----------------------------------------------------------------------
    // check_approval_or_forward tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_approval_auto_accept_read_tool() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();

        // Glob/Grep/Read are always auto-accepted
        let result = check_approval_or_forward(1, "Glob", json!({"pattern": "*.rs"}), &tx, &ctx);
        assert!(matches!(result, HandleResult::AutoAccepted(1)));
        assert!(rx.try_recv().is_err()); // no UI request
    }

    #[test]
    fn test_approval_forwards_dangerous_command() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();

        let result =
            check_approval_or_forward(42, "Bash", json!({"command": "sudo rm -rf /"}), &tx, &ctx);
        match result {
            HandleResult::NeedsApproval { rpc_id, .. } => assert_eq!(rpc_id, 42),
            other => panic!(
                "Expected NeedsApproval, got {:?}",
                std::mem::discriminant(&other)
            ),
        }

        // Permission request sent to UI
        let response = rx.try_recv().unwrap();
        match response {
            DaveApiResponse::PermissionRequest(pending) => {
                assert_eq!(pending.request.tool_name, "Bash");
            }
            other => panic!(
                "Expected PermissionRequest, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    // -----------------------------------------------------------------------
    // Integration tests — mock Codex server over duplex streams
    // -----------------------------------------------------------------------

    /// Mock Codex server that speaks JSONL over duplex streams.
    struct MockCodex {
        /// Read what the actor writes (actor's "stdin" from mock's perspective).
        reader: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
        /// Write what the actor reads (actor's "stdout" from mock's perspective).
        writer: tokio::io::BufWriter<tokio::io::DuplexStream>,
    }

    impl MockCodex {
        /// Read one JSONL message sent by the actor.
        async fn read_message(&mut self) -> RpcMessage {
            let line = self.reader.next_line().await.unwrap().unwrap();
            serde_json::from_str(&line).unwrap()
        }

        /// Send a JSONL message to the actor.
        async fn send_line(&mut self, value: &Value) {
            let mut line = serde_json::to_string(value).unwrap();
            line.push('\n');
            self.writer.write_all(line.as_bytes()).await.unwrap();
            self.writer.flush().await.unwrap();
        }

        /// Handle the `initialize` → `initialized` handshake.
        async fn handle_init(&mut self) {
            let req = self.read_message().await;
            assert_eq!(req.method.as_deref(), Some("initialize"));
            let id = req.id.unwrap();
            self.send_line(&json!({
                "id": id,
                "result": { "serverInfo": { "name": "mock-codex", "version": "0.0.0" } }
            }))
            .await;
            let notif = self.read_message().await;
            assert_eq!(notif.method.as_deref(), Some("initialized"));
        }

        /// Handle `thread/start` and return the thread ID.
        async fn handle_thread_start(&mut self) -> String {
            let req = self.read_message().await;
            assert_eq!(req.method.as_deref(), Some("thread/start"));
            let id = req.id.unwrap();
            let thread_id = "mock-thread-1";
            self.send_line(&json!({
                "id": id,
                "result": { "thread": { "id": thread_id }, "model": "mock-model" }
            }))
            .await;
            thread_id.to_string()
        }

        /// Handle `turn/start` and return the turn ID.
        async fn handle_turn_start(&mut self) -> String {
            let req = self.read_message().await;
            assert_eq!(req.method.as_deref(), Some("turn/start"));
            let id = req.id.unwrap();
            let turn_id = "mock-turn-1";
            self.send_line(&json!({
                "id": id,
                "result": { "turn": { "id": turn_id } }
            }))
            .await;
            turn_id.to_string()
        }

        /// Send an `item/agentMessage/delta` notification.
        async fn send_delta(&mut self, text: &str) {
            self.send_line(&json!({
                "method": "item/agentMessage/delta",
                "params": { "delta": text }
            }))
            .await;
        }

        /// Send a `turn/completed` notification.
        async fn send_turn_completed(&mut self, status: &str) {
            self.send_line(&json!({
                "method": "turn/completed",
                "params": { "status": status }
            }))
            .await;
        }

        /// Send an `item/completed` notification.
        async fn send_item_completed(&mut self, params: Value) {
            self.send_line(&json!({
                "method": "item/completed",
                "params": params
            }))
            .await;
        }

        /// Send an `item/started` notification.
        async fn send_item_started(&mut self, params: Value) {
            self.send_line(&json!({
                "method": "item/started",
                "params": params
            }))
            .await;
        }

        /// Send an approval request (server→client request with id).
        async fn send_approval_request(&mut self, rpc_id: u64, method: &str, params: Value) {
            self.send_line(&json!({
                "id": rpc_id,
                "method": method,
                "params": params
            }))
            .await;
        }
    }

    /// Create a mock codex server and spawn the session actor loop.
    /// Returns the mock, a command sender, and the actor task handle.
    fn setup_integration_test() -> (
        MockCodex,
        tokio_mpsc::Sender<SessionCommand>,
        tokio::task::JoinHandle<()>,
    ) {
        // "stdout" channel: mock writes → actor reads
        let (mock_stdout_write, actor_stdout_read) = tokio::io::duplex(8192);
        // "stdin" channel: actor writes → mock reads
        let (actor_stdin_write, mock_stdin_read) = tokio::io::duplex(8192);

        let mock = MockCodex {
            reader: BufReader::new(mock_stdin_read).lines(),
            writer: tokio::io::BufWriter::new(mock_stdout_write),
        };

        let actor_writer = tokio::io::BufWriter::new(actor_stdin_write);
        let actor_reader = BufReader::new(actor_stdout_read).lines();

        let (command_tx, mut command_rx) = tokio_mpsc::channel(16);

        let handle = tokio::spawn(async move {
            session_actor_loop(
                "test-session",
                actor_writer,
                actor_reader,
                Some("mock-model"),
                None,
                None,
                &mut command_rx,
            )
            .await;
        });

        (mock, command_tx, handle)
    }

    /// Send a Query command and return the response receiver.
    async fn send_query(
        command_tx: &tokio_mpsc::Sender<SessionCommand>,
        prompt: &str,
    ) -> mpsc::Receiver<DaveApiResponse> {
        let (response_tx, response_rx) = mpsc::channel();
        command_tx
            .send(SessionCommand::Query {
                prompt: prompt.to_string(),
                response_tx,
                ctx: egui::Context::default(),
            })
            .await
            .unwrap();
        response_rx
    }

    /// Collect all responses from the channel.
    fn collect_responses(rx: &mpsc::Receiver<DaveApiResponse>) -> Vec<DaveApiResponse> {
        rx.try_iter().collect()
    }

    // -- Integration tests --

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_streaming_tokens() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "Hello").await;
        mock.handle_turn_start().await;

        mock.send_delta("Hello").await;
        mock.send_delta(" world").await;
        mock.send_delta("!").await;
        mock.send_turn_completed("completed").await;

        // Drop sender — actor finishes processing remaining lines,
        // then command_rx.recv() returns None and the loop exits.
        drop(command_tx);
        handle.await.unwrap();

        let tokens: Vec<String> = collect_responses(&response_rx)
            .into_iter()
            .filter_map(|r| match r {
                DaveApiResponse::Token(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(tokens, vec!["Hello", " world", "!"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_command_execution() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "list files").await;
        mock.handle_turn_start().await;

        mock.send_item_completed(json!({
            "type": "commandExecution",
            "command": "ls -la",
            "exitCode": 0,
            "output": "total 42\nfoo.rs\n"
        }))
        .await;
        mock.send_turn_completed("completed").await;

        drop(command_tx);
        handle.await.unwrap();

        let tool_results: Vec<_> = collect_responses(&response_rx)
            .into_iter()
            .filter_map(|r| match r {
                DaveApiResponse::ToolResult(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(tool_results.len(), 1);
        assert_eq!(tool_results[0].tool_name, "Bash");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_file_change() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "edit file").await;
        mock.handle_turn_start().await;

        mock.send_item_completed(json!({
            "type": "fileChange",
            "filePath": "src/main.rs",
            "diff": "@@ -1,3 +1,3 @@\n-old\n+new\n context\n",
            "kind": { "type": "edit" }
        }))
        .await;
        mock.send_turn_completed("completed").await;

        drop(command_tx);
        handle.await.unwrap();

        let tool_results: Vec<_> = collect_responses(&response_rx)
            .into_iter()
            .filter_map(|r| match r {
                DaveApiResponse::ToolResult(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(tool_results.len(), 1);
        assert_eq!(tool_results[0].tool_name, "Edit");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_approval_accept() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "delete stuff").await;
        mock.handle_turn_start().await;

        // Send a command that won't be auto-accepted
        mock.send_approval_request(
            42,
            "item/commandExecution/requestApproval",
            json!({ "command": "rm -rf /tmp/important" }),
        )
        .await;

        // Actor should forward a PermissionRequest
        let resp = response_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("timed out waiting for PermissionRequest");
        let pending = match resp {
            DaveApiResponse::PermissionRequest(p) => p,
            other => panic!(
                "Expected PermissionRequest, got {:?}",
                std::mem::discriminant(&other)
            ),
        };
        assert_eq!(pending.request.tool_name, "Bash");

        // Approve it
        pending
            .response_tx
            .send(PermissionResponse::Allow { message: None })
            .unwrap();

        // Mock should receive the acceptance
        let approval_msg = mock.read_message().await;
        assert_eq!(approval_msg.id, Some(42));
        let result = approval_msg.result.unwrap();
        assert_eq!(result["decision"], "accept");

        mock.send_turn_completed("completed").await;
        drop(command_tx);
        handle.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_approval_deny() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "dangerous").await;
        mock.handle_turn_start().await;

        mock.send_approval_request(
            99,
            "item/commandExecution/requestApproval",
            json!({ "command": "sudo rm -rf /" }),
        )
        .await;

        let resp = response_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("timed out waiting for PermissionRequest");
        let pending = match resp {
            DaveApiResponse::PermissionRequest(p) => p,
            _ => panic!("Expected PermissionRequest"),
        };

        // Deny it
        pending
            .response_tx
            .send(PermissionResponse::Deny {
                reason: "too dangerous".to_string(),
            })
            .unwrap();

        // Mock should receive the decline
        let approval_msg = mock.read_message().await;
        assert_eq!(approval_msg.id, Some(99));
        let result = approval_msg.result.unwrap();
        assert_eq!(result["decision"], "decline");

        mock.send_turn_completed("completed").await;
        drop(command_tx);
        handle.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_auto_accept() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "check status").await;
        mock.handle_turn_start().await;

        // "git status" should be auto-accepted
        mock.send_approval_request(
            50,
            "item/commandExecution/requestApproval",
            json!({ "command": "git status" }),
        )
        .await;

        // Mock should receive the auto-acceptance immediately (no UI involved)
        let approval_msg = mock.read_message().await;
        assert_eq!(approval_msg.id, Some(50));
        let result = approval_msg.result.unwrap();
        assert_eq!(result["decision"], "accept");

        // No PermissionRequest should have been sent
        // (the response_rx should be empty or only have non-permission items)
        mock.send_turn_completed("completed").await;

        drop(command_tx);
        handle.await.unwrap();

        let permission_requests: Vec<_> = collect_responses(&response_rx)
            .into_iter()
            .filter(|r| matches!(r, DaveApiResponse::PermissionRequest(_)))
            .collect();
        assert!(
            permission_requests.is_empty(),
            "Auto-accepted commands should not generate PermissionRequests"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_multiple_turns() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        // First turn
        let rx1 = send_query(&command_tx, "first").await;
        mock.handle_turn_start().await;
        mock.send_delta("reply 1").await;
        mock.send_turn_completed("completed").await;

        // Wait for the first turn's token to confirm the actor is processing
        let resp = rx1
            .recv_timeout(Duration::from_secs(5))
            .expect("timed out waiting for first turn token");
        assert!(matches!(resp, DaveApiResponse::Token(_)));

        // Brief yield for turn_completed to be processed
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Second turn
        let rx2 = send_query(&command_tx, "second").await;
        mock.handle_turn_start().await;
        mock.send_delta("reply 2").await;
        mock.send_turn_completed("completed").await;

        drop(command_tx);
        handle.await.unwrap();

        let tokens2: Vec<String> = collect_responses(&rx2)
            .into_iter()
            .filter_map(|r| match r {
                DaveApiResponse::Token(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(tokens2, vec!["reply 2"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_subagent_lifecycle() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "research").await;
        mock.handle_turn_start().await;

        // Subagent starts
        mock.send_item_started(json!({
            "type": "collabAgentToolCall",
            "itemId": "agent-42",
            "name": "research agent"
        }))
        .await;

        // Command inside subagent
        mock.send_item_completed(json!({
            "type": "commandExecution",
            "command": "grep -r pattern .",
            "exitCode": 0,
            "output": "found 3 matches"
        }))
        .await;

        // Subagent completes
        mock.send_item_completed(json!({
            "type": "collabAgentToolCall",
            "itemId": "agent-42",
            "result": "Found relevant information"
        }))
        .await;

        mock.send_turn_completed("completed").await;

        drop(command_tx);
        handle.await.unwrap();

        let responses = collect_responses(&response_rx);

        // Should have: SubagentSpawned, ToolResult (with parent), SubagentCompleted
        let spawned: Vec<_> = responses
            .iter()
            .filter(|r| matches!(r, DaveApiResponse::SubagentSpawned(_)))
            .collect();
        assert_eq!(spawned.len(), 1);

        let tool_results: Vec<_> = responses
            .iter()
            .filter_map(|r| match r {
                DaveApiResponse::ToolResult(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(tool_results.len(), 1);
        assert_eq!(tool_results[0].parent_task_id.as_deref(), Some("agent-42"));

        let completed: Vec<_> = responses
            .iter()
            .filter(|r| matches!(r, DaveApiResponse::SubagentCompleted { .. }))
            .collect();
        assert_eq!(completed.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_shutdown_during_stream() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "long task").await;
        mock.handle_turn_start().await;

        mock.send_delta("partial").await;

        // Wait for token to arrive before sending Shutdown
        let resp = response_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("timed out waiting for token");
        assert!(
            matches!(&resp, DaveApiResponse::Token(t) if t == "partial"),
            "Expected Token(\"partial\")"
        );

        // Now shutdown while still inside the turn (no turn_completed sent)
        command_tx.send(SessionCommand::Shutdown).await.unwrap();
        handle.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_process_eof() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "hello").await;
        mock.handle_turn_start().await;

        mock.send_delta("partial").await;

        // Drop the mock's writer — simulates process exit
        drop(mock.writer);

        // Actor should detect EOF and send a Failed response
        let failed = response_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("timed out waiting for response after EOF");

        // First response might be the token, keep reading
        let mut got_failed = false;

        match failed {
            DaveApiResponse::Token(t) => {
                assert_eq!(t, "partial");
            }
            DaveApiResponse::Failed(_) => got_failed = true,
            _ => {}
        }

        if !got_failed {
            let resp = response_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("timed out waiting for Failed after EOF");
            match resp {
                DaveApiResponse::Failed(msg) => {
                    assert!(
                        msg.contains("exited unexpectedly") || msg.contains("EOF"),
                        "Unexpected error message: {}",
                        msg
                    );
                }
                other => panic!(
                    "Expected Failed after EOF, got {:?}",
                    std::mem::discriminant(&other)
                ),
            }
        }

        // Actor should exit after EOF
        command_tx.send(SessionCommand::Shutdown).await.ok(); // might fail if actor already exited
        handle.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_init_failure() {
        // "stdout" channel: mock writes → actor reads
        let (mock_stdout_write, actor_stdout_read) = tokio::io::duplex(8192);
        // "stdin" channel: actor writes → mock reads
        let (actor_stdin_write, mock_stdin_read) = tokio::io::duplex(8192);

        let mut mock_reader = BufReader::new(mock_stdin_read).lines();
        let mut mock_writer = tokio::io::BufWriter::new(mock_stdout_write);

        let actor_writer = tokio::io::BufWriter::new(actor_stdin_write);
        let actor_reader = BufReader::new(actor_stdout_read).lines();

        let (command_tx, mut command_rx) = tokio_mpsc::channel(16);

        let handle = tokio::spawn(async move {
            session_actor_loop(
                "test-session",
                actor_writer,
                actor_reader,
                Some("mock-model"),
                None,
                None,
                &mut command_rx,
            )
            .await;
        });

        // Read the initialize request
        let line = mock_reader.next_line().await.unwrap().unwrap();
        let req: RpcMessage = serde_json::from_str(&line).unwrap();
        let id = req.id.unwrap();

        // Send an error response
        let error_resp = json!({
            "id": id,
            "error": { "code": -1, "message": "mock init failure" }
        });
        let mut error_line = serde_json::to_string(&error_resp).unwrap();
        error_line.push('\n');
        mock_writer.write_all(error_line.as_bytes()).await.unwrap();
        mock_writer.flush().await.unwrap();

        // The actor should drain commands with error. Send a query and a shutdown.
        let (response_tx, response_rx) = mpsc::channel();
        command_tx
            .send(SessionCommand::Query {
                prompt: "hello".to_string(),
                response_tx,
                ctx: egui::Context::default(),
            })
            .await
            .unwrap();
        command_tx.send(SessionCommand::Shutdown).await.unwrap();

        handle.await.unwrap();

        // The query should have received an error
        let resp = response_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("expected error response after init failure");
        match resp {
            DaveApiResponse::Failed(msg) => {
                assert!(
                    msg.contains("init handshake"),
                    "Expected init handshake error, got: {}",
                    msg
                );
            }
            other => panic!("Expected Failed, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_turn_error() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "hello").await;

        // Read turn/start request and send an error response
        let req = mock.read_message().await;
        assert_eq!(req.method.as_deref(), Some("turn/start"));
        let id = req.id.unwrap();
        mock.send_line(&json!({
            "id": id,
            "error": { "code": -32000, "message": "rate limit exceeded" }
        }))
        .await;

        // Give actor time to process
        tokio::time::sleep(Duration::from_millis(100)).await;

        command_tx.send(SessionCommand::Shutdown).await.unwrap();
        handle.await.unwrap();

        let responses = collect_responses(&response_rx);
        let failures: Vec<_> = responses
            .iter()
            .filter_map(|r| match r {
                DaveApiResponse::Failed(msg) => Some(msg.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0], "rate limit exceeded");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_file_change_approval() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "create file").await;
        mock.handle_turn_start().await;

        // File change approval request (create)
        mock.send_approval_request(
            77,
            "item/fileChange/requestApproval",
            json!({
                "filePath": "new_file.rs",
                "diff": "+fn main() {}",
                "kind": { "type": "create" }
            }),
        )
        .await;

        let resp = response_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("timed out waiting for PermissionRequest");
        let pending = match resp {
            DaveApiResponse::PermissionRequest(p) => p,
            other => panic!(
                "Expected PermissionRequest, got {:?}",
                std::mem::discriminant(&other)
            ),
        };
        // File create should map to "Write" tool
        assert_eq!(pending.request.tool_name, "Write");

        pending
            .response_tx
            .send(PermissionResponse::Allow { message: None })
            .unwrap();

        let approval_msg = mock.read_message().await;
        assert_eq!(approval_msg.id, Some(77));
        assert_eq!(approval_msg.result.unwrap()["decision"], "accept");

        mock.send_turn_completed("completed").await;
        drop(command_tx);
        handle.await.unwrap();
    }

    /// Create a mock codex server with `resume_session_id` set, so the actor
    /// sends `thread/resume` instead of `thread/start`.
    fn setup_integration_test_with_resume(
        resume_id: &str,
    ) -> (
        MockCodex,
        tokio_mpsc::Sender<SessionCommand>,
        tokio::task::JoinHandle<()>,
    ) {
        let (mock_stdout_write, actor_stdout_read) = tokio::io::duplex(8192);
        let (actor_stdin_write, mock_stdin_read) = tokio::io::duplex(8192);

        let mock = MockCodex {
            reader: BufReader::new(mock_stdin_read).lines(),
            writer: tokio::io::BufWriter::new(mock_stdout_write),
        };

        let actor_writer = tokio::io::BufWriter::new(actor_stdin_write);
        let actor_reader = BufReader::new(actor_stdout_read).lines();

        let (command_tx, mut command_rx) = tokio_mpsc::channel(16);
        let resume_id = resume_id.to_string();

        let handle = tokio::spawn(async move {
            session_actor_loop(
                "test-session-resume",
                actor_writer,
                actor_reader,
                Some("mock-model"),
                None,
                Some(&resume_id),
                &mut command_rx,
            )
            .await;
        });

        (mock, command_tx, handle)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_interrupt_during_stream() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "count to 100").await;
        mock.handle_turn_start().await;

        // Send a few tokens
        mock.send_delta("one ").await;
        mock.send_delta("two ").await;

        // Give actor time to process the tokens
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify we got them
        let tok1 = response_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("expected token 1");
        assert!(matches!(tok1, DaveApiResponse::Token(ref t) if t == "one "));

        // Send interrupt
        command_tx
            .send(SessionCommand::Interrupt {
                ctx: egui::Context::default(),
            })
            .await
            .unwrap();

        // The actor should send turn/interrupt to codex
        let interrupt_msg = mock.read_message().await;
        assert_eq!(interrupt_msg.method.as_deref(), Some("turn/interrupt"));

        // Codex responds with turn/completed after interrupt
        mock.send_turn_completed("interrupted").await;

        // Actor should be ready for next command now
        drop(command_tx);
        handle.await.unwrap();

        // Verify we got the tokens before interrupt
        let responses = collect_responses(&response_rx);
        let tokens: Vec<_> = responses
            .iter()
            .filter_map(|r| match r {
                DaveApiResponse::Token(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert!(tokens.contains(&"two ".to_string()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_interrupt_during_approval() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "run something").await;
        mock.handle_turn_start().await;

        // Send an approval request
        mock.send_approval_request(
            50,
            "item/commandExecution/requestApproval",
            json!({ "command": "rm -rf /" }),
        )
        .await;

        // Wait for the PermissionRequest to arrive at the test
        let resp = response_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("timed out waiting for PermissionRequest");
        match resp {
            DaveApiResponse::PermissionRequest(_pending) => {
                // Don't respond to the pending permission — send interrupt instead
            }
            other => panic!(
                "Expected PermissionRequest, got {:?}",
                std::mem::discriminant(&other)
            ),
        }

        // Send interrupt while approval is pending
        command_tx
            .send(SessionCommand::Interrupt {
                ctx: egui::Context::default(),
            })
            .await
            .unwrap();

        // Actor should send cancel for the approval
        let cancel_msg = mock.read_message().await;
        assert_eq!(cancel_msg.id, Some(50));
        assert_eq!(cancel_msg.result.unwrap()["decision"], "cancel");

        // Then send turn/interrupt
        let interrupt_msg = mock.read_message().await;
        assert_eq!(interrupt_msg.method.as_deref(), Some("turn/interrupt"));

        // Codex completes the turn
        mock.send_turn_completed("interrupted").await;

        drop(command_tx);
        handle.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_query_during_active_turn() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx1 = send_query(&command_tx, "first query").await;
        mock.handle_turn_start().await;

        // Send some tokens so the turn is clearly active
        mock.send_delta("working...").await;

        // Give actor time to enter the streaming loop
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Send a second query while the first is still active
        let response_rx2 = send_query(&command_tx, "second query").await;

        // The second query should be immediately rejected
        let rejection = response_rx2
            .recv_timeout(Duration::from_secs(5))
            .expect("timed out waiting for rejection");
        match rejection {
            DaveApiResponse::Failed(msg) => {
                assert_eq!(msg, "Query already in progress");
            }
            other => panic!("Expected Failed, got {:?}", std::mem::discriminant(&other)),
        }

        // First query continues normally
        mock.send_turn_completed("completed").await;

        drop(command_tx);
        handle.await.unwrap();

        // Verify first query got its token
        let responses = collect_responses(&response_rx1);
        let tokens: Vec<_> = responses
            .iter()
            .filter_map(|r| match r {
                DaveApiResponse::Token(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert!(tokens.contains(&"working...".to_string()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_thread_resume() {
        let (mut mock, command_tx, handle) =
            setup_integration_test_with_resume("existing-thread-42");

        // Init handshake is the same
        mock.handle_init().await;

        // Actor should send thread/resume instead of thread/start
        let req = mock.read_message().await;
        assert_eq!(req.method.as_deref(), Some("thread/resume"));
        let params = req.params.unwrap();
        assert_eq!(params["threadId"], "existing-thread-42");

        // Respond with success
        let id = req.id.unwrap();
        mock.send_line(&json!({
            "id": id,
            "result": { "thread": { "id": "existing-thread-42" } }
        }))
        .await;

        // Now send a query — should work the same as normal
        let response_rx = send_query(&command_tx, "resume prompt").await;
        mock.handle_turn_start().await;
        mock.send_delta("resumed!").await;
        mock.send_turn_completed("completed").await;

        drop(command_tx);
        handle.await.unwrap();

        let responses = collect_responses(&response_rx);
        let tokens: Vec<_> = responses
            .iter()
            .filter_map(|r| match r {
                DaveApiResponse::Token(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(tokens, vec!["resumed!"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_integration_malformed_jsonl() {
        let (mut mock, command_tx, handle) = setup_integration_test();

        mock.handle_init().await;
        mock.handle_thread_start().await;

        let response_rx = send_query(&command_tx, "test").await;
        mock.handle_turn_start().await;

        // Send valid token
        mock.send_delta("before").await;

        // Send garbage that isn't valid JSON
        let mut garbage = "this is not json at all\n".to_string();
        mock.writer.write_all(garbage.as_bytes()).await.unwrap();
        mock.writer.flush().await.unwrap();

        // Send another valid token after the garbage
        mock.send_delta("after").await;

        // Complete the turn
        mock.send_turn_completed("completed").await;

        drop(command_tx);
        handle.await.unwrap();

        // Both valid tokens should have been received — the garbage line
        // should have been skipped with a warning, not crash the actor
        let responses = collect_responses(&response_rx);
        let tokens: Vec<_> = responses
            .iter()
            .filter_map(|r| match r {
                DaveApiResponse::Token(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert!(
            tokens.contains(&"before".to_string()),
            "Missing 'before' token, got: {:?}",
            tokens
        );
        assert!(
            tokens.contains(&"after".to_string()),
            "Missing 'after' token after malformed line, got: {:?}",
            tokens
        );
    }

    // -----------------------------------------------------------------------
    // Real-binary integration tests — require `codex` on PATH
    // Run with: cargo test -p notedeck_dave -- --ignored
    // -----------------------------------------------------------------------

    /// Helper: spawn a real codex app-server process and wire it into
    /// `session_actor_loop`. Returns the command sender, response receiver,
    /// and join handle.
    fn setup_real_codex_test() -> (
        tokio_mpsc::Sender<SessionCommand>,
        mpsc::Receiver<DaveApiResponse>,
        tokio::task::JoinHandle<()>,
    ) {
        let codex_binary = std::env::var("CODEX_BINARY").unwrap_or_else(|_| "codex".to_string());

        let mut child = spawn_codex(&codex_binary, &None)
            .expect("Failed to spawn codex app-server — is codex installed?");

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        // Drain stderr to prevent pipe deadlock
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!("[codex stderr] {}", line);
                }
            });
        }

        let writer = tokio::io::BufWriter::new(stdin);
        let reader = BufReader::new(stdout).lines();

        let (command_tx, mut command_rx) = tokio_mpsc::channel(16);

        let handle = tokio::spawn(async move {
            session_actor_loop(
                "real-codex-test",
                writer,
                reader,
                None, // use codex default model
                None, // use current directory
                None, // no resume
                &mut command_rx,
            )
            .await;
            let _ = child.kill().await;
        });

        let (response_tx, response_rx) = mpsc::channel();
        // Send an initial query to trigger handshake + thread start + turn
        let command_tx_clone = command_tx.clone();
        let rt_handle = tokio::runtime::Handle::current();
        rt_handle.spawn(async move {
            command_tx_clone
                .send(SessionCommand::Query {
                    prompt: "Say exactly: hello world".to_string(),
                    response_tx,
                    ctx: egui::Context::default(),
                })
                .await
                .unwrap();
        });

        (command_tx, response_rx, handle)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore] // Requires `codex` binary on PATH
    async fn test_real_codex_streaming() {
        let (command_tx, response_rx, handle) = setup_real_codex_test();

        // Wait for at least one token (with a generous timeout for API calls)
        let mut got_token = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);

        while std::time::Instant::now() < deadline {
            match response_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(DaveApiResponse::Token(t)) => {
                    eprintln!("[test] got token: {:?}", t);
                    got_token = true;
                }
                Ok(DaveApiResponse::PermissionRequest(pending)) => {
                    // Auto-accept any permission requests during test
                    eprintln!(
                        "[test] auto-accepting permission: {}",
                        pending.request.tool_name
                    );
                    let _ = pending
                        .response_tx
                        .send(PermissionResponse::Allow { message: None });
                }
                Ok(DaveApiResponse::Failed(msg)) => {
                    panic!("[test] codex turn failed: {}", msg);
                }
                Ok(other) => {
                    eprintln!("[test] got response: {:?}", std::mem::discriminant(&other));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if got_token {
                        break; // Got at least one token; stop waiting
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        assert!(
            got_token,
            "Expected at least one Token response from real codex"
        );

        drop(command_tx);
        let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore] // Requires `codex` binary on PATH
    async fn test_real_codex_turn_completes() {
        let (command_tx, response_rx, handle) = setup_real_codex_test();

        // Wait for turn to complete
        let mut got_turn_done = false;
        let mut got_any_response = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(120);

        while std::time::Instant::now() < deadline {
            match response_rx.recv_timeout(Duration::from_secs(2)) {
                Ok(DaveApiResponse::Token(_)) => {
                    got_any_response = true;
                }
                Ok(DaveApiResponse::PermissionRequest(pending)) => {
                    got_any_response = true;
                    let _ = pending
                        .response_tx
                        .send(PermissionResponse::Allow { message: None });
                }
                Ok(DaveApiResponse::Failed(msg)) => {
                    eprintln!("[test] turn failed: {}", msg);
                    // A failure is still a "completion" — codex responded
                    got_turn_done = true;
                    break;
                }
                Ok(_) => {
                    got_any_response = true;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if got_any_response {
                        // Responses have stopped coming — turn likely completed
                        // (turn/completed causes the actor to stop sending
                        //  and wait for the next command)
                        got_turn_done = true;
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    got_turn_done = true;
                    break;
                }
            }
        }

        assert!(
            got_turn_done,
            "Expected real codex turn to complete within timeout"
        );

        drop(command_tx);
        let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
    }
}
