use crate::messages::DaveApiResponse;
use crate::tools::Tool;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;

/// Backend type selection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    OpenAI,
    Claude,
}

/// Trait for AI backend implementations
pub trait AiBackend: Send + Sync {
    /// Stream a request to the AI backend
    ///
    /// Returns a receiver that will receive tokens and tool calls as they arrive,
    /// plus an optional JoinHandle to the spawned task for cleanup on session deletion.
    fn stream_request(
        &self,
        messages: Vec<crate::Message>,
        tools: Arc<HashMap<String, Tool>>,
        model: String,
        user_id: String,
        session_id: String,
        ctx: egui::Context,
    ) -> (
        mpsc::Receiver<DaveApiResponse>,
        Option<tokio::task::JoinHandle<()>>,
    );

    /// Clean up resources associated with a session.
    /// Called when a session is deleted to allow backends to shut down any persistent connections.
    fn cleanup_session(&self, session_id: String);
}
