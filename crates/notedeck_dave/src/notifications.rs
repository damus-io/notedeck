use crate::focus_queue::FocusQueue;
use crate::session::{SessionId, SessionManager};

/// Tracks notification state to avoid spamming
pub struct NotificationState {
    /// Session ID we last notified about (to avoid repeat notifications)
    last_notified_session: Option<SessionId>,
    /// Previous window focus state
    was_focused: bool,
}

impl Default for NotificationState {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationState {
    pub fn new() -> Self {
        Self {
            last_notified_session: None,
            was_focused: true, // Assume focused at startup
        }
    }

    /// Check if we should send a notification and send it if needed.
    /// Call this after status updates in the main update loop.
    pub fn maybe_notify(
        &mut self,
        ctx: &egui::Context,
        focus_queue: &FocusQueue,
        session_manager: &SessionManager,
    ) {
        let is_focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);

        // If window just gained focus, clear notification state
        if is_focused && !self.was_focused {
            self.last_notified_session = None;
        }

        self.was_focused = is_focused;

        // Only notify when unfocused
        if is_focused {
            return;
        }

        // Check if there's a session needing input
        let needs_input_session = focus_queue
            .current()
            .filter(|entry| entry.priority == crate::focus_queue::FocusPriority::NeedsInput)
            .map(|entry| entry.session_id);

        let Some(session_id) = needs_input_session else {
            return;
        };

        // Don't re-notify for the same session
        if self.last_notified_session == Some(session_id) {
            return;
        }

        // Get session title for notification
        let title = session_manager
            .get(session_id)
            .map(|s| s.details.display_title().to_string())
            .unwrap_or_else(|| "Session".to_string());

        // Send notification
        self.send_notification(&title);
        self.last_notified_session = Some(session_id);
    }

    #[cfg(target_os = "linux")]
    fn send_notification(&self, session_title: &str) {
        use notify_rust::Notification;

        if let Err(e) = Notification::new()
            .summary("Dave needs attention")
            .body(&format!("{} is waiting for input", session_title))
            .show()
        {
            tracing::warn!("Failed to send notification: {}", e);
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn send_notification(&self, _session_title: &str) {
        // No-op on non-Linux platforms for now
    }
}
