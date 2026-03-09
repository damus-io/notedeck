use crate::messages::DaveApiResponse;
use crate::tools::Tool;
use claude_agent_sdk_rs::PermissionMode;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

/// AI model selection.
///
/// Variants represent model families (always the latest version).
/// `Default` lets the backend CLI pick its own default.
/// `Custom` is an escape hatch for arbitrary model ID strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Model {
    /// Let the backend use its own default model.
    Default,
    /// Latest Claude Opus
    Opus,
    /// Latest Claude Sonnet
    Sonnet,
    /// Latest Claude Haiku
    Haiku,
    /// Arbitrary model ID string (for OpenAI, Codex, or future models)
    Custom(String),
}

impl Model {
    /// Human-friendly display name for the picker UI.
    pub fn display_name(&self) -> &str {
        match self {
            Model::Default => "Default",
            Model::Opus => "Opus",
            Model::Sonnet => "Sonnet",
            Model::Haiku => "Haiku",
            Model::Custom(id) => id,
        }
    }

    /// Resolve to a concrete model ID string for the backend.
    /// Returns `None` for `Default` (let CLI pick).
    pub fn to_model_id(&self) -> Option<&str> {
        match self {
            Model::Default => None,
            Model::Opus => Some("claude-opus-4-6-20250514"),
            Model::Sonnet => Some("claude-sonnet-4-6-20250514"),
            Model::Haiku => Some("claude-haiku-4-5-20251001"),
            Model::Custom(id) => Some(id),
        }
    }

    /// Parse a raw model ID string into a Model variant.
    /// Recognizes known Claude model prefixes; everything else
    /// becomes `Custom`.
    pub fn from_model_id(id: &str) -> Self {
        if id.starts_with("claude-opus") {
            Model::Opus
        } else if id.starts_with("claude-sonnet") {
            Model::Sonnet
        } else if id.starts_with("claude-haiku") {
            Model::Haiku
        } else {
            Model::Custom(id.to_string())
        }
    }
}

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

    /// Model overrides available for selection in the backend picker.
    /// Does not include `Default` — that's always implicitly available.
    pub fn available_models(&self) -> Vec<Model> {
        match self {
            BackendType::Claude => vec![Model::Opus, Model::Sonnet, Model::Haiku],
            BackendType::Codex => [
                "gpt-5.3-codex",
                "gpt-5.2-codex",
                "gpt-5-codex",
                "codex-mini-latest",
                "o4-mini",
            ]
            .iter()
            .map(|id| Model::Custom(id.to_string()))
            .collect(),
            BackendType::OpenAI => ["gpt-4.1-mini", "gpt-4.1", "o4-mini"]
                .iter()
                .map(|id| Model::Custom(id.to_string()))
                .collect(),
            BackendType::Remote => vec![],
        }
    }

    /// Stable string for Nostr event tags.
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendType::OpenAI => "openai",
            BackendType::Claude => "claude",
            BackendType::Codex => "codex",
            BackendType::Remote => "remote",
        }
    }

    /// Parse from a Nostr event tag value.
    pub fn from_tag_str(s: &str) -> Option<BackendType> {
        match s {
            "openai" => Some(BackendType::OpenAI),
            "claude" => Some(BackendType::Claude),
            "codex" => Some(BackendType::Codex),
            "remote" => Some(BackendType::Remote),
            _ => None,
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
        model: Option<String>,
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

    /// Trigger manual context compaction for a session.
    /// Returns a receiver for CompactionStarted/CompactionComplete events.
    /// Default implementation does nothing (backends that don't support it).
    fn compact_session(
        &self,
        _session_id: String,
        _ctx: egui::Context,
    ) -> Option<mpsc::Receiver<DaveApiResponse>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_type_roundtrip() {
        for bt in [
            BackendType::OpenAI,
            BackendType::Claude,
            BackendType::Codex,
            BackendType::Remote,
        ] {
            let tag = bt.as_str();
            let parsed = BackendType::from_tag_str(tag);
            assert_eq!(
                parsed,
                Some(bt),
                "roundtrip failed for {:?} (tag={:?})",
                bt,
                tag
            );
        }
    }
}
