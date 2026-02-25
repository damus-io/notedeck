//! JSON-RPC serde types for the Codex app-server protocol.
//!
//! The Codex app-server (`codex app-server --listen stdio://`) communicates via
//! JSONL (one JSON object per line) over stdin/stdout. It uses a JSON-RPC-like
//! schema but does NOT include the `"jsonrpc":"2.0"` header.

#![allow(dead_code)] // Protocol fields are defined for completeness; not all are read yet.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Generic JSON-RPC envelope
// ---------------------------------------------------------------------------

/// Outgoing request or notification (client → server).
#[derive(Debug, Serialize)]
pub struct RpcRequest<P: Serialize> {
    /// Present for requests that expect a response; absent for notifications.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub method: &'static str,
    pub params: P,
}

/// Incoming message from the server. Could be a response, notification, or
/// request (for bidirectional approval).
#[derive(Debug, Deserialize)]
pub struct RpcMessage {
    /// Present on responses and server→client requests.
    pub id: Option<u64>,
    /// Present on notifications and server→client requests.
    pub method: Option<String>,
    /// Present on successful responses.
    pub result: Option<Value>,
    /// Present on error responses.
    pub error: Option<RpcError>,
    /// Present on notifications and server→client requests.
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

// ---------------------------------------------------------------------------
// Outgoing (client → server)
// ---------------------------------------------------------------------------

/// `initialize` params
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    pub capabilities: Value, // empty object for now
}

#[derive(Debug, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// `thread/start` params
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
}

/// `thread/resume` params
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeParams {
    pub thread_id: String,
}

/// `turn/start` params
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<TurnInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum TurnInput {
    #[serde(rename = "text")]
    Text { text: String },
}

/// `turn/interrupt` params
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub thread_id: String,
    pub turn_id: String,
}

/// Response to an approval request (client → server).
#[derive(Debug, Serialize)]
pub struct ApprovalResponse {
    pub decision: ApprovalDecision,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalDecision {
    Accept,
    Decline,
    Cancel,
}

// ---------------------------------------------------------------------------
// Incoming (server → client)
// ---------------------------------------------------------------------------

/// Result of `thread/start`
#[derive(Debug, Deserialize)]
pub struct ThreadStartResult {
    pub thread: ThreadInfo,
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ThreadInfo {
    pub id: String,
}

/// `item/started` params
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemStartedParams {
    /// The kind of item: "agentMessage", "commandExecution", "fileChange",
    /// "collabAgentToolCall", "contextCompaction", etc.
    #[serde(rename = "type")]
    pub item_type: String,
    /// Unique item ID
    pub item_id: Option<String>,
    /// For collabAgentToolCall: agent name/description
    pub name: Option<String>,
    /// For commandExecution: the command being run
    pub command: Option<String>,
    /// For fileChange: the file path
    pub file_path: Option<String>,
}

/// `item/agentMessage/delta` params — a streaming text token
#[derive(Debug, Deserialize)]
pub struct AgentMessageDeltaParams {
    pub delta: String,
}

/// `item/completed` params — an item has finished
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemCompletedParams {
    #[serde(rename = "type")]
    pub item_type: String,
    pub item_id: Option<String>,
    /// For commandExecution: the command that was run
    pub command: Option<String>,
    /// For commandExecution: exit code
    pub exit_code: Option<i32>,
    /// For commandExecution: stdout/stderr output
    pub output: Option<String>,
    /// For fileChange: the file path
    pub file_path: Option<String>,
    /// For fileChange: the diff
    pub diff: Option<String>,
    /// For fileChange: kind of change (create, edit, delete)
    pub kind: Option<Value>,
    /// For collabAgentToolCall: result text
    pub result: Option<String>,
    /// For contextCompaction: token info
    pub pre_tokens: Option<u64>,
    /// For agentMessage: full content
    pub content: Option<String>,
}

/// `item/commandExecution/requestApproval` params — server asks client to approve a command.
///
/// In V2, `command` and `cwd` are optional.  When absent the command can be
/// reconstructed from `proposed_execpolicy_amendment` (an argv vec).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandApprovalParams {
    /// The command string (present in V2 when the Command presentation is used).
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Reason the approval is needed (e.g. "Write outside workspace").
    #[serde(default)]
    pub reason: Option<String>,
    /// Proposed execpolicy amendment — an argv array that can serve as a
    /// fallback when `command` is absent.
    #[serde(default)]
    pub proposed_execpolicy_amendment: Option<Vec<String>>,
}

impl CommandApprovalParams {
    /// Best-effort command string for display / auto-accept evaluation.
    pub fn command_string(&self) -> String {
        if let Some(ref cmd) = self.command {
            return cmd.clone();
        }
        if let Some(ref argv) = self.proposed_execpolicy_amendment {
            return argv.join(" ");
        }
        "unknown command".to_string()
    }
}

/// `item/fileChange/requestApproval` params — server asks client to approve a file change.
///
/// In V2, the params are minimal (just item/thread/turn ids + reason).
/// `file_path`, `diff`, and `kind` may be absent.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeApprovalParams {
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub diff: Option<String>,
    #[serde(default)]
    pub kind: Option<Value>,
    /// Reason the approval is needed.
    #[serde(default)]
    pub reason: Option<String>,
    /// Root directory the agent is requesting write access for.
    #[serde(default)]
    pub grant_root: Option<String>,
}

/// `turn/completed` params
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCompletedParams {
    /// "completed" or "failed"
    pub status: String,
    pub turn_id: Option<String>,
    pub error: Option<String>,
}
