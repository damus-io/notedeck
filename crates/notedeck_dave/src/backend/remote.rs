use crate::messages::DaveApiResponse;
use crate::tools::Tool;
use claude_agent_sdk_rs::PermissionMode;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use super::AiBackend;

/// A no-op backend for devices without API keys.
///
/// Allows creating local Chat sessions (input is ignored) while viewing
/// and controlling remote Agentic sessions discovered from ndb/relays.
pub struct RemoteOnlyBackend;

impl AiBackend for RemoteOnlyBackend {
    fn stream_request(
        &self,
        _messages: Vec<crate::Message>,
        _tools: Arc<HashMap<String, Tool>>,
        _model: String,
        _user_id: String,
        _session_id: String,
        _cwd: Option<PathBuf>,
        _resume_session_id: Option<String>,
        _ctx: egui::Context,
    ) -> (
        mpsc::Receiver<DaveApiResponse>,
        Option<tokio::task::JoinHandle<()>>,
    ) {
        // Return a closed channel — no local AI processing
        let (_tx, rx) = mpsc::channel();
        (rx, None)
    }

    fn cleanup_session(&self, _session_id: String) {}

    fn interrupt_session(&self, _session_id: String, _ctx: egui::Context) {}

    fn set_permission_mode(&self, _session_id: String, _mode: PermissionMode, _ctx: egui::Context) {
    }
}
