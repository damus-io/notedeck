use crate::tools::{ToolCall, ToolResponse};
use async_openai::types::*;
use nostrdb::{Ndb, Transaction};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

/// A question option from AskUserQuestion
#[derive(Debug, Clone, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// A single question from AskUserQuestion
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserQuestion {
    pub question: String,
    pub header: String,
    #[serde(default)]
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
}

/// Parsed AskUserQuestion tool input
#[derive(Debug, Clone, Deserialize)]
pub struct AskUserQuestionInput {
    pub questions: Vec<UserQuestion>,
}

/// User's answer to a question
#[derive(Debug, Clone, Default, Serialize)]
pub struct QuestionAnswer {
    /// Selected option indices
    pub selected: Vec<usize>,
    /// Custom "Other" text if provided
    pub other_text: Option<String>,
}

/// A request for user permission to use a tool (displayable data only)
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    /// Unique identifier for this permission request
    pub id: Uuid,
    /// The tool that wants to be used
    pub tool_name: String,
    /// The arguments the tool will be called with
    pub tool_input: serde_json::Value,
    /// The user's response (None if still pending)
    pub response: Option<PermissionResponseType>,
    /// For AskUserQuestion: pre-computed summary of answers for display
    pub answer_summary: Option<AnswerSummary>,
}

/// A single entry in an answer summary
#[derive(Debug, Clone)]
pub struct AnswerSummaryEntry {
    /// The question header (e.g., "Library", "Approach")
    pub header: String,
    /// The selected answer text, comma-separated if multiple
    pub answer: String,
}

/// Pre-computed summary of an AskUserQuestion response for display
#[derive(Debug, Clone)]
pub struct AnswerSummary {
    pub entries: Vec<AnswerSummaryEntry>,
}

/// A permission request with the response channel (for channel communication)
pub struct PendingPermission {
    /// The displayable request data
    pub request: PermissionRequest,
    /// Channel to send the user's response back
    pub response_tx: oneshot::Sender<PermissionResponse>,
}

/// The user's response to a permission request
#[derive(Debug, Clone)]
pub enum PermissionResponse {
    /// Allow the tool to execute, with an optional message for the AI
    Allow { message: Option<String> },
    /// Deny the tool execution with a reason
    Deny { reason: String },
}

/// The recorded response type for display purposes (without channel details)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionResponseType {
    Allowed,
    Denied,
}

/// Metadata about a completed tool execution
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_name: String,
    pub summary: String, // e.g., "154 lines", "exit 0", "3 matches"
}

#[derive(Debug, Clone)]
pub enum Message {
    System(String),
    Error(String),
    User(String),
    Assistant(String),
    ToolCalls(Vec<ToolCall>),
    ToolResponse(ToolResponse),
    /// A permission request from the AI that needs user response
    PermissionRequest(PermissionRequest),
    /// Result metadata from a completed tool execution
    ToolResult(ToolResult),
}

/// The ai backends response. Since we are using streaming APIs these are
/// represented as individual tokens or tool calls
pub enum DaveApiResponse {
    ToolCalls(Vec<ToolCall>),
    Token(String),
    Failed(String),
    /// A permission request that needs to be displayed to the user
    PermissionRequest(PendingPermission),
    /// Metadata from a completed tool execution
    ToolResult(ToolResult),
}

impl Message {
    pub fn tool_error(id: String, msg: String) -> Self {
        Self::ToolResponse(ToolResponse::error(id, msg))
    }

    pub fn to_api_msg(&self, txn: &Transaction, ndb: &Ndb) -> Option<ChatCompletionRequestMessage> {
        match self {
            Message::Error(_err) => None,

            Message::User(msg) => Some(ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessage {
                    name: None,
                    content: ChatCompletionRequestUserMessageContent::Text(msg.clone()),
                },
            )),

            Message::Assistant(msg) => Some(ChatCompletionRequestMessage::Assistant(
                ChatCompletionRequestAssistantMessage {
                    content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                        msg.clone(),
                    )),
                    ..Default::default()
                },
            )),

            Message::System(msg) => Some(ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessage {
                    content: ChatCompletionRequestSystemMessageContent::Text(msg.clone()),
                    ..Default::default()
                },
            )),

            Message::ToolCalls(calls) => Some(ChatCompletionRequestMessage::Assistant(
                ChatCompletionRequestAssistantMessage {
                    tool_calls: Some(calls.iter().map(|c| c.to_api()).collect()),
                    ..Default::default()
                },
            )),

            Message::ToolResponse(resp) => {
                let tool_response = resp.responses().format_for_dave(txn, ndb);

                Some(ChatCompletionRequestMessage::Tool(
                    ChatCompletionRequestToolMessage {
                        tool_call_id: resp.id().to_owned(),
                        content: ChatCompletionRequestToolMessageContent::Text(tool_response),
                    },
                ))
            }

            // Permission requests are UI-only, not sent to the API
            Message::PermissionRequest(_) => None,

            // Tool results are UI-only, not sent to the API
            Message::ToolResult(_) => None,
        }
    }
}
