use crate::tools::{ToolCall, ToolResponse};
use async_openai::types::*;
use egui_md_stream::{MdElement, Partial, StreamParser};
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

/// Session initialization info from Claude Code CLI
#[derive(Debug, Clone, Default)]
pub struct SessionInfo {
    /// Available tools in this session
    pub tools: Vec<String>,
    /// Model being used (e.g., "claude-opus-4-5-20251101")
    pub model: Option<String>,
    /// Permission mode (e.g., "default", "plan")
    pub permission_mode: Option<String>,
    /// Available slash commands
    pub slash_commands: Vec<String>,
    /// Available agent types for Task tool
    pub agents: Vec<String>,
    /// Claude Code CLI version
    pub cli_version: Option<String>,
    /// Current working directory
    pub cwd: Option<String>,
    /// Session ID from Claude Code
    pub claude_session_id: Option<String>,
}

/// Status of a subagent spawned by the Task tool
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentStatus {
    /// Subagent is running
    Running,
    /// Subagent completed successfully
    Completed,
    /// Subagent failed with an error
    Failed,
}

/// Information about a subagent spawned by the Task tool
#[derive(Debug, Clone)]
pub struct SubagentInfo {
    /// Unique ID for this subagent task
    pub task_id: String,
    /// Description of what the subagent is doing
    pub description: String,
    /// Type of subagent (e.g., "Explore", "Plan", "Bash")
    pub subagent_type: String,
    /// Current status
    pub status: SubagentStatus,
    /// Output content (truncated for display)
    pub output: String,
    /// Maximum output size to keep (for size-restricted window)
    pub max_output_size: usize,
}

/// An assistant message with incremental markdown parsing support.
///
/// During streaming, tokens are pushed to the parser incrementally.
/// After finalization (stream end), parsed elements are cached.
pub struct AssistantMessage {
    /// Raw accumulated text (kept for API serialization)
    text: String,
    /// Incremental parser for this message (None after finalization)
    parser: Option<StreamParser>,
    /// Cached parsed elements (populated after finalization)
    cached_elements: Option<Vec<MdElement>>,
}

impl std::fmt::Debug for AssistantMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantMessage")
            .field("text", &self.text)
            .field("is_streaming", &self.parser.is_some())
            .field(
                "cached_elements",
                &self.cached_elements.as_ref().map(|e| e.len()),
            )
            .finish()
    }
}

impl Clone for AssistantMessage {
    fn clone(&self) -> Self {
        // StreamParser doesn't implement Clone, so we need special handling.
        // For cloned messages (which are typically finalized), we just clone
        // the text and cached elements. If there's an active parser, we
        // re-parse from the raw text.
        if let Some(cached) = &self.cached_elements {
            Self {
                text: self.text.clone(),
                parser: None,
                cached_elements: Some(cached.clone()),
            }
        } else {
            // Active streaming - re-parse from text
            let mut parser = StreamParser::new();
            parser.push(&self.text);
            Self {
                text: self.text.clone(),
                parser: Some(parser),
                cached_elements: None,
            }
        }
    }
}

impl AssistantMessage {
    /// Create a new assistant message with a fresh parser.
    pub fn new() -> Self {
        Self {
            text: String::new(),
            parser: Some(StreamParser::new()),
            cached_elements: None,
        }
    }

    /// Create from existing text (e.g., when loading from storage).
    pub fn from_text(text: String) -> Self {
        let mut parser = StreamParser::new();
        parser.push(&text);
        parser.finalize();
        let cached = parser.parsed().to_vec();
        Self {
            text,
            parser: None,
            cached_elements: Some(cached),
        }
    }

    /// Push a new token and update the parser.
    pub fn push_token(&mut self, token: &str) {
        self.text.push_str(token);
        if let Some(parser) = &mut self.parser {
            parser.push(token);
        }
    }

    /// Finalize the message (call when stream ends).
    /// This caches the parsed elements and drops the parser.
    pub fn finalize(&mut self) {
        if let Some(mut parser) = self.parser.take() {
            parser.finalize();
            self.cached_elements = Some(parser.parsed().to_vec());
        }
    }

    /// Get the raw text content.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Get parsed markdown elements.
    pub fn parsed_elements(&self) -> &[MdElement] {
        if let Some(cached) = &self.cached_elements {
            cached
        } else if let Some(parser) = &self.parser {
            parser.parsed()
        } else {
            &[]
        }
    }

    /// Get the current partial (in-progress) element, if any.
    pub fn partial(&self) -> Option<&Partial> {
        self.parser.as_ref().and_then(|p| p.partial())
    }

    /// Check if the message is still being streamed.
    pub fn is_streaming(&self) -> bool {
        self.parser.is_some()
    }
}

impl Default for AssistantMessage {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    System(String),
    Error(String),
    User(String),
    Assistant(AssistantMessage),
    ToolCalls(Vec<ToolCall>),
    ToolResponse(ToolResponse),
    /// A permission request from the AI that needs user response
    PermissionRequest(PermissionRequest),
    /// Result metadata from a completed tool execution
    ToolResult(ToolResult),
    /// Conversation was compacted
    CompactionComplete(CompactionInfo),
    /// A subagent spawned by Task tool
    Subagent(SubagentInfo),
}

/// Compaction info from compact_boundary system message
#[derive(Debug, Clone)]
pub struct CompactionInfo {
    /// Number of tokens before compaction
    pub pre_tokens: u64,
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
    /// Session initialization info from Claude Code CLI
    SessionInfo(SessionInfo),
    /// Subagent spawned by Task tool
    SubagentSpawned(SubagentInfo),
    /// Subagent output update
    SubagentOutput {
        task_id: String,
        output: String,
    },
    /// Subagent completed
    SubagentCompleted {
        task_id: String,
        result: String,
    },
    /// Conversation compaction started
    CompactionStarted,
    /// Conversation compaction completed with token info
    CompactionComplete(CompactionInfo),
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
                        msg.text().to_string(),
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

            // Compaction complete is UI-only, not sent to the API
            Message::CompactionComplete(_) => None,

            // Subagent info is UI-only, not sent to the API
            Message::Subagent(_) => None,
        }
    }
}
