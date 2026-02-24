use crate::messages::DaveApiResponse;
use crate::tools::Tool;
use claude_agent_sdk_rs::PermissionMode;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

/// Backend type selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendType {
    OpenAI,
    Claude,
    Codex,
    /// No local AI — only view/control remote agentic sessions from ndb
    Remote,
}

impl BackendType {
    pub fn display_name(&self) -> &'static str {
        match self {
            BackendType::OpenAI => "OpenAI",
            BackendType::Claude => "Claude Code",
            BackendType::Codex => "Codex",
            BackendType::Remote => "Remote",
        }
    }

    pub fn is_agentic(&self) -> bool {
        matches!(self, BackendType::Claude | BackendType::Codex)
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            BackendType::OpenAI => "gpt-4.1-mini",
            BackendType::Claude => "claude-sonnet-4.5",
            BackendType::Codex => "gpt-5.2-codex",
            BackendType::Remote => "",
        }
    }
}

/// Trait for AI backend implementations
pub trait AiBackend: Send + Sync {
    /// Stream a request to the AI backend
    ///
    /// Returns a receiver that will receive tokens and tool calls as they arrive,
    /// plus an optional JoinHandle to the spawned task for cleanup on session deletion.
    ///
    /// If `resume_session_id` is Some, the backend should resume the specified Claude
    /// session instead of starting a new conversation.
    #[allow(clippy::too_many_arguments)]
    fn stream_request(
        &self,
        messages: Vec<crate::Message>,
        tools: Arc<HashMap<String, Tool>>,
        model: String,
        user_id: String,
        session_id: String,
        cwd: Option<PathBuf>,
        resume_session_id: Option<String>,
        ctx: egui::Context,
    ) -> (
        mpsc::Receiver<DaveApiResponse>,
        Option<tokio::task::JoinHandle<()>>,
    );

    /// Clean up resources associated with a session.
    /// Called when a session is deleted to allow backends to shut down any persistent connections.
    fn cleanup_session(&self, session_id: String);

    /// Interrupt the current query for a session.
    /// This stops any in-progress work but preserves the session history.
    fn interrupt_session(&self, session_id: String, ctx: egui::Context);

    /// Set the permission mode for a session.
    /// Plan mode makes Claude plan actions without executing them.
    fn set_permission_mode(&self, session_id: String, mode: PermissionMode, ctx: egui::Context);
}
