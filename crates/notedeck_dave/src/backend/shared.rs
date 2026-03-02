//! Shared utilities used by multiple AI backend implementations.

use crate::auto_accept::AutoAcceptRules;
use crate::backend::tool_summary::{format_tool_summary, truncate_output};
use crate::file_update::FileUpdate;
use crate::messages::{
    DaveApiResponse, ExecutedTool, ImageAttachment, PendingPermission, PermissionRequest,
    UserMessage,
};
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
        images: Vec<crate::messages::ImageAttachment>,
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
            Message::User(msg) => {
                prompt.push_str("Human: ");
                prompt.push_str(&msg.text);
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
#[cfg(test)]
pub fn get_pending_user_messages(messages: &[Message]) -> String {
    pending_user_prompt_and_images(messages).0
}

/// Collect trailing user messages from newest to oldest.
fn trailing_user_messages(messages: &[Message]) -> Vec<&UserMessage> {
    messages
        .iter()
        .rev()
        .take_while(|m| matches!(m, Message::User(_)))
        .filter_map(|m| match m {
            Message::User(msg) => Some(msg),
            _ => None,
        })
        .collect()
}

/// Build prompt and image list from trailing queued user messages in one pass.
///
/// Returns user text joined with `\n` and images in chronological order.
fn pending_user_prompt_and_images(messages: &[Message]) -> (String, Vec<ImageAttachment>) {
    let trailing = trailing_user_messages(messages);
    if trailing.is_empty() {
        return (String::new(), Vec::new());
    }

    // Iterate newest->oldest in reverse so output is chronological.
    let mut prompt = String::new();
    let mut images = Vec::new();
    for (idx, msg) in trailing.iter().rev().enumerate() {
        if idx > 0 {
            prompt.push('\n');
        }
        prompt.push_str(msg.as_str());
        images.extend(msg.images.iter().cloned());
    }

    (prompt, images)
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

/// Prepare prompt text and image attachments from the same logical user turn(s).
///
/// This keeps text and image selection aligned:
/// - resumed sessions: trailing queued user messages
/// - first message in a new session: the single user message
/// - later turns in a new session: trailing queued user messages
pub fn prepare_prompt_and_images(
    messages: &[Message],
    resume_session_id: &Option<String>,
) -> (String, Vec<ImageAttachment>) {
    if resume_session_id.is_some() {
        return pending_user_prompt_and_images(messages);
    }

    let is_first_message = messages
        .iter()
        .filter(|m| matches!(m, Message::User(_)))
        .count()
        == 1;

    if is_first_message {
        let images = messages
            .iter()
            .find_map(|m| match m {
                Message::User(msg) => Some(msg.images.clone()),
                _ => None,
            })
            .unwrap_or_default();
        (messages_to_prompt(messages), images)
    } else {
        pending_user_prompt_and_images(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::prepare_prompt_and_images;
    use crate::messages::{AssistantMessage, ImageAttachment, UserMessage};
    use crate::Message;

    fn img(bytes: &[u8], mime: &str) -> ImageAttachment {
        ImageAttachment::new(bytes.to_vec(), mime)
    }

    #[test]
    fn prepare_prompt_and_images_uses_all_trailing_user_images_in_order() {
        let first = img(&[1], "image/png");
        let second = img(&[2], "image/jpeg");
        let third = img(&[3], "image/gif");
        let old = img(&[9], "image/png");

        let messages = vec![
            Message::User(UserMessage::new("old", vec![old.clone()])),
            Message::Assistant(AssistantMessage::from_text("ack".to_string())),
            Message::User(UserMessage::new("queued one", vec![first.clone()])),
            Message::User(UserMessage::new(
                "queued two",
                vec![second.clone(), third.clone()],
            )),
        ];

        let (prompt, images) = prepare_prompt_and_images(&messages, &None);
        assert_eq!(prompt, "queued one\nqueued two");
        assert_eq!(images.len(), 3);
        assert_eq!(&*images[0].bytes, &*first.bytes);
        assert_eq!(&*images[1].bytes, &*second.bytes);
        assert_eq!(&*images[2].bytes, &*third.bytes);
    }

    #[test]
    fn prepare_prompt_and_images_first_turn_keeps_single_user_images() {
        let image = img(&[7, 8], "image/png");
        let messages = vec![
            Message::System("sys".to_string()),
            Message::User(UserMessage::new("hello", vec![image.clone()])),
        ];

        let (prompt, images) = prepare_prompt_and_images(&messages, &None);
        assert!(prompt.contains("Human: hello"));
        assert_eq!(images.len(), 1);
        assert_eq!(&*images[0].bytes, &*image.bytes);
    }

    #[test]
    fn prepare_prompt_and_images_resumed_session_uses_pending_images() {
        let first = img(&[11], "image/png");
        let second = img(&[12], "image/png");
        let messages = vec![
            Message::Assistant(AssistantMessage::from_text("prev".to_string())),
            Message::User(UserMessage::new("next", vec![first.clone()])),
            Message::User(UserMessage::new("last", vec![second.clone()])),
        ];

        let (prompt, images) = prepare_prompt_and_images(&messages, &Some("sid".to_string()));
        assert_eq!(prompt, "next\nlast");
        assert_eq!(images.len(), 2);
        assert_eq!(&*images[0].bytes, &*first.bytes);
        assert_eq!(&*images[1].bytes, &*second.bytes);
    }
}
