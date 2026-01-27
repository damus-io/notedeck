use egui::{Align, Color32, Layout, Sense};
use notedeck_ui::app_images;

use crate::agent_status::AgentStatus;
use crate::session::{SessionId, SessionManager};

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
    ctrl_held: bool,
}

impl<'a> SessionListUi<'a> {
    pub fn new(session_manager: &'a SessionManager, ctrl_held: bool) -> Self {
        SessionListUi {
            session_manager,
            ctrl_held,
        }
    }

    pub fn ui(&self, ui: &mut egui::Ui) -> Option<SessionListAction> {
        let mut action: Option<SessionListAction> = None;

        ui.vertical(|ui| {
            // Header with New Chat button
            action = self.header_ui(ui);

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
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Chats").size(18.0).strong());

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let icon = app_images::new_message_image()
                    .max_height(20.0)
                    .sense(Sense::click());

                if ui
                    .add(icon)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
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

        for (index, session) in self.session_manager.sessions_ordered().iter().enumerate() {
            let is_active = Some(session.id) == active_id;
            // Show keyboard shortcut hint for first 9 sessions (1-9 keys), only when Ctrl held
            let shortcut_hint = if self.ctrl_held && index < 9 {
                Some(index + 1)
            } else {
                None
            };

            let response = self.session_item_ui(
                ui,
                &session.title,
                is_active,
                shortcut_hint,
                session.status(),
            );

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
        is_active: bool,
        shortcut_hint: Option<usize>,
        status: AgentStatus,
    ) -> egui::Response {
        let desired_size = egui::vec2(ui.available_width(), 36.0);
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
        let hover_text = format!("Ctrl+{} to switch", shortcut_hint.unwrap_or(0));
        let response = response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text_at_pointer(hover_text);

        // Paint background: active > hovered > transparent
        let fill = if is_active {
            ui.visuals().widgets.active.bg_fill
        } else if response.hovered() {
            ui.visuals().widgets.hovered.weak_bg_fill
        } else {
            Color32::TRANSPARENT
        };

        let corner_radius = 8.0;
        ui.painter().rect_filled(rect, corner_radius, fill);

        // Status color indicator (left edge vertical bar)
        let status_color = status.color();
        let status_bar_rect = egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(2.0, 4.0),
            egui::vec2(3.0, rect.height() - 8.0),
        );
        ui.painter().rect_filled(status_bar_rect, 1.5, status_color);

        // Draw shortcut hint on the left if available (only when Ctrl held)
        let text_start_x = if let Some(num) = shortcut_hint {
            let hint_text = format!("{}", num);
            let hint_pos = rect.left_center() + egui::vec2(16.0, 0.0);
            ui.painter().text(
                hint_pos,
                egui::Align2::LEFT_CENTER,
                &hint_text,
                egui::FontId::monospace(12.0),
                ui.visuals().text_color().gamma_multiply(0.5),
            );
            32.0
        } else {
            12.0 // Leave room for status bar
        };

        // Draw title text
        let text_pos = rect.left_center() + egui::vec2(text_start_x, 0.0);
        ui.painter().text(
            text_pos,
            egui::Align2::LEFT_CENTER,
            title,
            egui::FontId::proportional(14.0),
            ui.visuals().text_color(),
        );

        response
    }
}
