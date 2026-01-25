use egui::{Align, Layout, Sense};

use crate::session::{SessionId, SessionManager};
use crate::Message;

/// Actions that can be triggered from the session list UI
#[derive(Debug, Clone)]
pub enum SessionListAction {
    NewSession,
    SwitchTo(SessionId),
    Delete(SessionId),
}

/// UI component for displaying the session list sidebar
pub struct SessionListUi<'a> {
    session_manager: &'a SessionManager,
}

impl<'a> SessionListUi<'a> {
    pub fn new(session_manager: &'a SessionManager) -> Self {
        SessionListUi { session_manager }
    }

    pub fn ui(&self, ui: &mut egui::Ui) -> Option<SessionListAction> {
        let mut action: Option<SessionListAction> = None;

        ui.vertical(|ui| {
            // Header with New Chat button
            action = self.header_ui(ui);

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);

            // Scrollable list of sessions
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    if let Some(session_action) = self.sessions_list_ui(ui) {
                        action = Some(session_action);
                    }
                });
        });

        action
    }

    fn header_ui(&self, ui: &mut egui::Ui) -> Option<SessionListAction> {
        let mut action = None;

        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.heading("Chats");

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(12.0);
                if ui
                    .button("+")
                    .on_hover_text("New Chat")
                    .clicked()
                {
                    action = Some(SessionListAction::NewSession);
                }
            });
        });

        action
    }

    fn sessions_list_ui(&self, ui: &mut egui::Ui) -> Option<SessionListAction> {
        let mut action = None;
        let active_id = self.session_manager.active_id();

        for session in self.session_manager.sessions_ordered() {
            let is_active = Some(session.id) == active_id;

            let response = self.session_item_ui(ui, &session.title, Self::get_preview(&session.chat), is_active);

            if response.clicked() {
                action = Some(SessionListAction::SwitchTo(session.id));
            }

            // Right-click context menu for delete
            response.context_menu(|ui| {
                if ui.button("Delete").clicked() {
                    action = Some(SessionListAction::Delete(session.id));
                    ui.close_menu();
                }
            });
        }

        action
    }

    fn session_item_ui(
        &self,
        ui: &mut egui::Ui,
        title: &str,
        preview: Option<String>,
        is_active: bool,
    ) -> egui::Response {
        let fill = if is_active {
            ui.visuals().widgets.active.bg_fill
        } else {
            egui::Color32::TRANSPARENT
        };

        let frame = egui::Frame::new()
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .corner_radius(8.0);

        frame
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                ui.vertical(|ui| {
                    // Title
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(title)
                                .strong()
                                .size(14.0),
                        )
                        .truncate(),
                    );

                    // Preview of last message
                    if let Some(preview) = preview {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(preview)
                                    .weak()
                                    .size(12.0),
                            )
                            .truncate(),
                        );
                    }
                });
            })
            .response
            .interact(Sense::click())
    }

    /// Get a preview string from the chat history
    fn get_preview(chat: &[Message]) -> Option<String> {
        // Find the last user or assistant message
        for msg in chat.iter().rev() {
            match msg {
                Message::User(text) | Message::Assistant(text) => {
                    let preview: String = text.chars().take(50).collect();
                    return Some(if text.len() > 50 {
                        format!("{}...", preview)
                    } else {
                        preview
                    });
                }
                _ => continue,
            }
        }
        None
    }
}
