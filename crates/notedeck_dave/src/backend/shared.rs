//! Shared utilities used by multiple AI backend implementations.

use crate::messages::DaveApiResponse;
use crate::Message;
use claude_agent_sdk_rs::PermissionMode;
use std::sync::mpsc;
use tokio::sync::mpsc as tokio_mpsc;

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
