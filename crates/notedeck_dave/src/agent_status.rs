/// Represents the current status of an agent in the RTS scene
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentStatus {
    /// Agent is idle, no active work
    #[default]
    Idle,
    /// Agent is actively processing (receiving tokens, executing tools)
    Working,
    /// Agent needs user input (permission request pending)
    NeedsInput,
    /// Agent encountered an error
    Error,
    /// Agent completed its task successfully
    Done,
}

impl AgentStatus {
    /// Get the color associated with this status
    pub fn color(&self) -> egui::Color32 {
        match self {
            AgentStatus::Idle => egui::Color32::from_rgb(128, 128, 128), // Gray
            AgentStatus::Working => egui::Color32::from_rgb(50, 205, 50), // Green
            AgentStatus::NeedsInput => egui::Color32::from_rgb(255, 200, 0), // Yellow/amber
            AgentStatus::Error => egui::Color32::from_rgb(220, 60, 60),  // Red
            AgentStatus::Done => egui::Color32::from_rgb(70, 130, 220),  // Blue
        }
    }

    /// Get a human-readable label for this status
    pub fn label(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "Idle",
            AgentStatus::Working => "Working",
            AgentStatus::NeedsInput => "Needs Input",
            AgentStatus::Error => "Error",
            AgentStatus::Done => "Done",
        }
    }

    /// Get the status as a lowercase string for serialization (nostr events).
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::NeedsInput => "needs_input",
            AgentStatus::Error => "error",
            AgentStatus::Done => "done",
        }
    }
}
