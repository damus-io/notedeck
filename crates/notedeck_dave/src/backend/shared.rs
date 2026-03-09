//! Shared utilities used by multiple AI backend implementations.

use crate::auto_accept::AutoAcceptRules;
use crate::backend::tool_summary::{format_tool_summary, truncate_output};
use crate::file_update::FileUpdate;
use crate::messages::{DaveApiResponse, ExecutedTool, PendingPermission, PermissionRequest};
use crate::Message;
use claude_agent_sdk_rs::PermissionMode;
use std::sync::mpsc;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Commands sent to a session's actor task.
///
/// Used identically by the Claude and Codex backends.
pub(crate) enum SessionCommand {
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
    /// Trigger manual context compaction
    Compact {
        response_tx: mpsc::Sender<DaveApiResponse>,
        ctx: egui::Context,
    },
    Shutdown,
}

/// Handle kept by a backend to communicate with its session actor.
pub(crate) struct SessionHandle {
    pub command_tx: tokio_mpsc::Sender<SessionCommand>,
}

/// Convert our messages to a prompt string for the AI backend.
///
/// Includes the system message (if any) followed by the conversation
/// history formatted as `Human:` / `Assistant:` turns. Tool-related,
/// error, permission, compaction and subagent messages are skipped.
pub fn messages_to_prompt(messages: &[Message]) -> String {
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
            | Message::Subagent(_) => {}
        }
    }

    prompt
}

/// Collect all trailing user messages and join them.
///
/// When multiple messages are queued, they're all sent as one prompt
/// so the AI sees everything at once instead of one at a time.
pub fn get_pending_user_messages(messages: &[Message]) -> String {
    let mut trailing: Vec<&str> = messages
        .iter()
        .rev()
        .take_while(|m| matches!(m, Message::User(_)))
        .filter_map(|m| match m {
            Message::User(content) => Some(content.as_str()),
            _ => None,
        })
        .collect();
    trailing.reverse();
    trailing.join("\n")
}

/// Remove a completed subagent from the stack and notify the UI.
pub fn complete_subagent(
    task_id: &str,
    result_text: &str,
    subagent_stack: &mut Vec<String>,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    ctx: &egui::Context,
) {
    subagent_stack.retain(|id| id != task_id);
    let _ = response_tx.send(DaveApiResponse::SubagentCompleted {
        task_id: task_id.to_string(),
        result: truncate_output(result_text, 2000),
    });
    ctx.request_repaint();
}

/// Build an [`ExecutedTool`] from a completed tool call and send it
/// to the UI along with a repaint request.
pub fn send_tool_result(
    tool_name: &str,
    tool_input: &serde_json::Value,
    result_value: &serde_json::Value,
    file_update: Option<FileUpdate>,
    subagent_stack: &[String],
    response_tx: &mpsc::Sender<DaveApiResponse>,
    ctx: &egui::Context,
) {
    let summary = format_tool_summary(tool_name, tool_input, result_value);
    let parent_task_id = subagent_stack.last().cloned();
    let tool_result = ExecutedTool {
        tool_name: tool_name.to_string(),
        summary,
        parent_task_id,
        file_update,
    };
    let _ = response_tx.send(DaveApiResponse::ToolResult(tool_result));
    ctx.request_repaint();
}

/// Check auto-accept rules for a tool invocation.
///
/// Returns `true` (and logs) when the tool should be silently
/// accepted without asking the user.
pub fn should_auto_accept(tool_name: &str, tool_input: &serde_json::Value) -> bool {
    let rules = AutoAcceptRules::default();
    let accepted = rules.should_auto_accept(tool_name, tool_input);
    if accepted {
        tracing::debug!("Auto-accepting {}: matched auto-accept rule", tool_name);
    }
    accepted
}

/// Build a [`PermissionRequest`] + [`PendingPermission`], send it to
/// the UI via `response_tx`, and return the oneshot receiver the
/// caller can `await` to get the user's decision.
///
/// Returns `None` if the UI channel is closed (the request could not
/// be delivered).
pub fn forward_permission_to_ui(
    tool_name: &str,
    tool_input: serde_json::Value,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    ctx: &egui::Context,
) -> Option<oneshot::Receiver<crate::messages::PermissionResponse>> {
    let request_id = Uuid::new_v4();
    let (ui_resp_tx, ui_resp_rx) = oneshot::channel();

    let cached_plan = if tool_name == "ExitPlanMode" {
        tool_input
            .get("plan")
            .and_then(|v| v.as_str())
            .map(crate::messages::ParsedMarkdown::parse)
    } else {
        None
    };

    let request = PermissionRequest {
        id: request_id,
        tool_name: tool_name.to_string(),
        tool_input,
        response: None,
        answer_summary: None,
        cached_plan,
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
        return None;
    }

    ctx.request_repaint();
    Some(ui_resp_rx)
}

/// Decide which prompt to send based on whether we're resuming a
/// session and how many user messages exist.
///
/// - Resumed sessions always send just the pending messages (the
///   backend already has the full conversation context).
/// - New sessions send the full prompt on the first message, then
///   only pending messages for subsequent turns.
pub fn prepare_prompt(messages: &[Message], resume_session_id: &Option<String>) -> String {
    if resume_session_id.is_some() {
        get_pending_user_messages(messages)
    } else {
        let is_first_message = messages
            .iter()
            .filter(|m| matches!(m, Message::User(_)))
            .count()
            == 1;
        if is_first_message {
            messages_to_prompt(messages)
        } else {
            get_pending_user_messages(messages)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{AssistantMessage, CompactionInfo};

    // ---- messages_to_prompt ----

    #[test]
    fn messages_to_prompt_empty() {
        assert_eq!(messages_to_prompt(&[]), "");
    }

    #[test]
    fn messages_to_prompt_system_first() {
        let msgs = vec![Message::System("You are helpful.".into())];
        assert_eq!(messages_to_prompt(&msgs), "You are helpful.\n\n");
    }

    #[test]
    fn messages_to_prompt_conversation() {
        let msgs = vec![
            Message::User("hello".into()),
            Message::Assistant(AssistantMessage::from_text("hi there".into())),
            Message::User("thanks".into()),
        ];
        let prompt = messages_to_prompt(&msgs);
        assert_eq!(
            prompt,
            "Human: hello\n\nAssistant: hi there\n\nHuman: thanks\n\n"
        );
    }

    #[test]
    fn messages_to_prompt_system_plus_conversation() {
        let msgs = vec![Message::System("system".into()), Message::User("hi".into())];
        let prompt = messages_to_prompt(&msgs);
        assert_eq!(prompt, "system\n\nHuman: hi\n\n");
    }

    #[test]
    fn messages_to_prompt_skips_non_conversation_types() {
        let msgs = vec![
            Message::User("hello".into()),
            Message::Error("oops".into()),
            Message::CompactionComplete(CompactionInfo { pre_tokens: 100 }),
            Message::User("world".into()),
        ];
        let prompt = messages_to_prompt(&msgs);
        assert_eq!(prompt, "Human: hello\n\nHuman: world\n\n");
    }

    // ---- get_pending_user_messages ----

    #[test]
    fn get_pending_empty() {
        assert_eq!(get_pending_user_messages(&[]), "");
    }

    #[test]
    fn get_pending_trailing_users() {
        let msgs = vec![
            Message::Assistant(AssistantMessage::from_text("ok".into())),
            Message::User("first".into()),
            Message::User("second".into()),
        ];
        assert_eq!(get_pending_user_messages(&msgs), "first\nsecond");
    }

    #[test]
    fn get_pending_stops_at_nonuser() {
        let msgs = vec![
            Message::User("old".into()),
            Message::Assistant(AssistantMessage::from_text("reply".into())),
            Message::User("new".into()),
        ];
        assert_eq!(get_pending_user_messages(&msgs), "new");
    }

    #[test]
    fn get_pending_no_trailing_users() {
        let msgs = vec![
            Message::User("hello".into()),
            Message::Assistant(AssistantMessage::from_text("hi".into())),
        ];
        assert_eq!(get_pending_user_messages(&msgs), "");
    }

    // ---- prepare_prompt ----

    #[test]
    fn prepare_prompt_resumed_returns_pending_only() {
        let msgs = vec![
            Message::System("sys".into()),
            Message::User("old".into()),
            Message::Assistant(AssistantMessage::from_text("reply".into())),
            Message::User("new".into()),
        ];
        let resume_id = Some("session-123".into());
        assert_eq!(prepare_prompt(&msgs, &resume_id), "new");
    }

    #[test]
    fn prepare_prompt_first_message_returns_full() {
        let msgs = vec![Message::System("sys".into()), Message::User("hello".into())];
        let prompt = prepare_prompt(&msgs, &None);
        assert_eq!(prompt, "sys\n\nHuman: hello\n\n");
    }

    #[test]
    fn prepare_prompt_subsequent_returns_pending_only() {
        let msgs = vec![
            Message::User("first".into()),
            Message::Assistant(AssistantMessage::from_text("reply".into())),
            Message::User("second".into()),
        ];
        let prompt = prepare_prompt(&msgs, &None);
        assert_eq!(prompt, "second");
    }

    // ---- forward_permission_to_ui ----

    #[test]
    fn forward_permission_delivers() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let input = serde_json::json!({"command": "ls"});
        let result = forward_permission_to_ui("Bash", input.clone(), &tx, &ctx);
        assert!(result.is_some());

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::PermissionRequest(pending) => {
                assert_eq!(pending.request.tool_name, "Bash");
                assert_eq!(pending.request.tool_input, input);
                assert!(pending.request.response.is_none());
                assert!(pending.request.cached_plan.is_none());
                assert!(pending.request.answer_summary.is_none());
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn forward_permission_exit_plan_caches() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let input = serde_json::json!({"plan": "# My Plan\n\nDo stuff"});
        let result = forward_permission_to_ui("ExitPlanMode", input, &tx, &ctx);
        assert!(result.is_some());

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::PermissionRequest(pending) => {
                assert_eq!(pending.request.tool_name, "ExitPlanMode");
                let plan = pending
                    .request
                    .cached_plan
                    .expect("ExitPlanMode should cache plan");
                assert!(plan.source.contains("My Plan"));
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn forward_permission_closed_channel_returns_none() {
        let (tx, rx) = mpsc::channel::<DaveApiResponse>();
        drop(rx);
        let ctx = egui::Context::default();
        let result = forward_permission_to_ui("Bash", serde_json::json!({}), &tx, &ctx);
        assert!(result.is_none());
    }

    // ---- send_tool_result ----

    #[test]
    fn send_tool_result_with_parent() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let stack = vec!["task-1".to_string()];
        send_tool_result(
            "Read",
            &serde_json::json!({"file_path": "/tmp/test"}),
            &serde_json::json!({"content": "hello"}),
            None,
            &stack,
            &tx,
            &ctx,
        );

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::ToolResult(tool) => {
                assert_eq!(tool.tool_name, "Read");
                assert_eq!(tool.parent_task_id, Some("task-1".to_string()));
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn send_tool_result_without_parent() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        send_tool_result(
            "Bash",
            &serde_json::json!({"command": "ls"}),
            &serde_json::json!({"output": "file.txt"}),
            None,
            &[],
            &tx,
            &ctx,
        );

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::ToolResult(tool) => {
                assert_eq!(tool.tool_name, "Bash");
                assert!(tool.parent_task_id.is_none());
                assert!(tool.file_update.is_none());
                // Summary should be non-empty (format_tool_summary produces something)
                assert!(!tool.summary.is_empty(), "summary should not be empty");
            }
            _ => panic!("expected ToolResult"),
        }
    }

    // ---- edge cases ----

    #[test]
    fn messages_to_prompt_multiple_systems_takes_first_only() {
        let msgs = vec![
            Message::System("First system".into()),
            Message::System("Second system".into()),
            Message::User("hello".into()),
        ];
        let prompt = messages_to_prompt(&msgs);
        assert!(prompt.contains("First system"));
        assert!(!prompt.contains("Second system"));
    }

    #[test]
    fn get_pending_all_users() {
        let msgs = vec![
            Message::User("a".into()),
            Message::User("b".into()),
            Message::User("c".into()),
        ];
        assert_eq!(get_pending_user_messages(&msgs), "a\nb\nc");
    }

    #[test]
    fn prepare_prompt_no_messages_returns_empty() {
        assert_eq!(prepare_prompt(&[], &None), "");
    }

    #[test]
    fn prepare_prompt_system_only_no_user_returns_empty() {
        // When there's no User message, prepare_prompt returns empty
        // because is_first_message = (0 == 1) = false, so it falls
        // through to get_pending_user_messages which finds no trailing users.
        // This is correct: prepare_prompt is only called when dispatching
        // a user message, so this state shouldn't occur in practice.
        let msgs = vec![Message::System("sys".into())];
        let prompt = prepare_prompt(&msgs, &None);
        assert_eq!(prompt, "");
    }

    #[test]
    fn prepare_prompt_resumed_no_user_returns_empty() {
        let msgs = vec![
            Message::System("sys".into()),
            Message::Assistant(AssistantMessage::from_text("reply".into())),
        ];
        let resume_id = Some("session-123".into());
        assert_eq!(prepare_prompt(&msgs, &resume_id), "");
    }

    #[test]
    fn forward_permission_exit_plan_non_string_plan_gives_none() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        // "plan" key is a number, not a string
        let input = serde_json::json!({"plan": 123});
        let result = forward_permission_to_ui("ExitPlanMode", input, &tx, &ctx);
        assert!(result.is_some());

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::PermissionRequest(pending) => {
                // Non-string "plan" should gracefully result in None
                assert!(pending.request.cached_plan.is_none());
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    // ---- complete_subagent ----

    #[test]
    fn complete_subagent_removes_from_stack() {
        let (tx, rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let mut stack = vec!["task-a".to_string(), "task-b".to_string()];
        complete_subagent("task-a", "done", &mut stack, &tx, &ctx);

        assert_eq!(stack, vec!["task-b".to_string()]);
        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::SubagentCompleted { task_id, result } => {
                assert_eq!(task_id, "task-a");
                assert_eq!(result, "done");
            }
            _ => panic!("expected SubagentCompleted"),
        }
    }
}
