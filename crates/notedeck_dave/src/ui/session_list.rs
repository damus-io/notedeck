use std::path::{Path, PathBuf};

use egui::{Align, Color32, Layout, Sense};
use notedeck_ui::app_images;

use crate::agent_status::AgentStatus;
use crate::config::AiMode;
use crate::focus_queue::{FocusPriority, FocusQueue};
use crate::session::{SessionId, SessionManager};
use crate::ui::keybind_hint::paint_keybind_hint;

/// Actions that can be triggered from the session list UI
#[derive(Debug, Clone)]
pub enum SessionListAction {
    NewSession,
    SwitchTo(SessionId),
    Delete(SessionId),
    Rename(SessionId, String),
}

/// UI component for displaying the session list sidebar
pub struct SessionListUi<'a> {
    session_manager: &'a SessionManager,
    focus_queue: &'a FocusQueue,
    ctrl_held: bool,
}

impl<'a> SessionListUi<'a> {
    pub fn new(
        session_manager: &'a SessionManager,
        focus_queue: &'a FocusQueue,
        ctrl_held: bool,
    ) -> Self {
        SessionListUi {
            session_manager,
            focus_queue,
            ctrl_held,
        }
    }

    pub fn ui(&self, ui: &mut egui::Ui) -> Option<SessionListAction> {
        let mut action: Option<SessionListAction> = None;

        ui.vertical(|ui| {
            // Header with New Agent button
            action = self.header_ui(ui);

            ui.add_space(8.0);

            // Scrollable list of sessions
            egui::ScrollArea::vertical()
                .id_salt("session_list_scroll")
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
            ui.label(egui::RichText::new("Sessions").size(18.0).strong());

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

        // Agents grouped by hostname (pre-computed, no per-frame allocation)
        for (hostname, ids) in self.session_manager.host_groups() {
            let label = if hostname.is_empty() {
                "Local"
            } else {
                hostname
            };
            ui.label(
                egui::RichText::new(label)
                    .size(12.0)
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(4.0);
            for &id in ids {
                if let Some(session) = self.session_manager.get(id) {
                    let index = self.session_manager.session_index(id).unwrap_or(0);
                    if let Some(a) = self.render_session_item(ui, session, index, active_id) {
                        action = Some(a);
                    }
                }
            }
            ui.add_space(8.0);
        }

        // Chats section (pre-computed IDs)
        let chat_ids = self.session_manager.chat_ids();
        if !chat_ids.is_empty() {
            ui.label(
                egui::RichText::new("Chats")
                    .size(12.0)
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(4.0);
            for &id in chat_ids {
                if let Some(session) = self.session_manager.get(id) {
                    let index = self.session_manager.session_index(id).unwrap_or(0);
                    if let Some(a) = self.render_session_item(ui, session, index, active_id) {
                        action = Some(a);
                    }
                }
            }
        }

        action
    }

    fn render_session_item(
        &self,
        ui: &mut egui::Ui,
        session: &crate::session::ChatSession,
        index: usize,
        active_id: Option<SessionId>,
    ) -> Option<SessionListAction> {
        let is_active = Some(session.id) == active_id;
        let shortcut_hint = if self.ctrl_held && index < 9 {
            Some(index + 1)
        } else {
            None
        };
        let queue_priority = self.focus_queue.get_session_priority(session.id);
        let empty_path = PathBuf::new();
        let cwd = session.cwd().unwrap_or(&empty_path);

        let rename_id = egui::Id::new("session_rename_state");
        let mut renaming: Option<(SessionId, String)> =
            ui.data(|d| d.get_temp::<(SessionId, String)>(rename_id));
        let is_renaming = renaming
            .as_ref()
            .map(|(id, _)| *id == session.id)
            .unwrap_or(false);

        let display_title = if is_renaming {
            ""
        } else {
            session.details.display_title()
        };
        let response = self.session_item_ui(
            ui,
            display_title,
            cwd,
            &session.details.home_dir,
            is_active,
            shortcut_hint,
            session.status(),
            queue_priority,
            session.ai_mode,
        );

        let mut action = None;

        if is_renaming {
            let outcome = renaming
                .as_mut()
                .and_then(|(_, buf)| inline_rename_ui(ui, &response, buf));
            match outcome {
                Some(RenameOutcome::Confirmed(title)) => {
                    action = Some(SessionListAction::Rename(session.id, title));
                    ui.data_mut(|d| d.remove_by_type::<(SessionId, String)>());
                }
                Some(RenameOutcome::Cancelled) => {
                    ui.data_mut(|d| d.remove_by_type::<(SessionId, String)>());
                }
                None => {
                    if let Some(r) = renaming {
                        ui.data_mut(|d| d.insert_temp(rename_id, r));
                    }
                }
            }
        } else if response.clicked() {
            action = Some(SessionListAction::SwitchTo(session.id));
        }

        // Long-press to rename (mobile)
        if !is_renaming {
            let press_id = egui::Id::new("session_long_press");
            if response.is_pointer_button_down_on() {
                let now = ui.input(|i| i.time);
                let start: Option<PressStart> = ui.data(|d| d.get_temp(press_id));
                if start.is_none() {
                    ui.data_mut(|d| d.insert_temp(press_id, PressStart(now)));
                } else if let Some(s) = start {
                    if now - s.0 > 0.5 {
                        let rename_state =
                            (session.id, session.details.display_title().to_string());
                        ui.data_mut(|d| d.insert_temp(rename_id, rename_state));
                        ui.data_mut(|d| d.remove_by_type::<PressStart>());
                    }
                }
            } else {
                ui.data_mut(|d| d.remove_by_type::<PressStart>());
            }
        }

        response.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                let rename_state = (session.id, session.details.display_title().to_string());
                ui.ctx()
                    .data_mut(|d| d.insert_temp(rename_id, rename_state));
                ui.close_menu();
            }
            if ui.button("Delete").clicked() {
                action = Some(SessionListAction::Delete(session.id));
                ui.close_menu();
            }
        });
        action
    }

    #[allow(clippy::too_many_arguments)]
    fn session_item_ui(
        &self,
        ui: &mut egui::Ui,
        title: &str,
        cwd: &Path,
        home_dir: &str,
        is_active: bool,
        shortcut_hint: Option<usize>,
        status: AgentStatus,
        queue_priority: Option<FocusPriority>,
        session_ai_mode: AiMode,
    ) -> egui::Response {
        // Per-session: Chat sessions get shorter height (no CWD), no status bar
        // Agentic sessions get taller height with CWD and status bar
        let show_cwd = session_ai_mode == AiMode::Agentic;
        let show_status_bar = session_ai_mode == AiMode::Agentic;

        let item_height = if show_cwd { 48.0 } else { 32.0 };
        let desired_size = egui::vec2(ui.available_width(), item_height);
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

        // Status color indicator (left edge vertical bar) - only in Agentic mode
        let text_start_x = if show_status_bar {
            let status_color = status.color();
            let status_bar_rect = egui::Rect::from_min_size(
                rect.left_top() + egui::vec2(2.0, 4.0),
                egui::vec2(3.0, rect.height() - 8.0),
            );
            ui.painter().rect_filled(status_bar_rect, 1.5, status_color);
            12.0 // Left padding (room for status bar)
        } else {
            8.0 // Smaller padding in Chat mode (no status bar)
        };

        // Draw shortcut hint at the far right
        let mut right_offset = 8.0; // Start with normal right padding

        if let Some(num) = shortcut_hint {
            let hint_text = format!("{}", num);
            let hint_size = 18.0;
            let hint_center = rect.right_center() - egui::vec2(8.0 + hint_size / 2.0, 0.0);
            paint_keybind_hint(ui, hint_center, &hint_text, hint_size);
            right_offset = 8.0 + hint_size + 6.0; // padding + hint width + spacing
        }

        // Draw focus queue indicator dot to the left of the shortcut hint
        let text_end_x = if let Some(priority) = queue_priority {
            let dot_radius = 5.0;
            let dot_center = rect.right_center() - egui::vec2(right_offset + dot_radius + 4.0, 0.0);
            ui.painter()
                .circle_filled(dot_center, dot_radius, priority.color());
            right_offset + dot_radius * 2.0 + 8.0 // Space reserved for the dot
        } else {
            right_offset
        };

        // Calculate text position - offset title upward only if showing CWD
        let title_y_offset = if show_cwd { -7.0 } else { 0.0 };
        let text_pos = rect.left_center() + egui::vec2(text_start_x, title_y_offset);
        let max_text_width = rect.width() - text_start_x - text_end_x;

        // Draw title text (with clipping to avoid overlapping the dot)
        let font_id = egui::FontId::proportional(14.0);
        let text_color = ui.visuals().text_color();
        let galley = ui
            .painter()
            .layout_no_wrap(title.to_string(), font_id.clone(), text_color);

        if galley.size().x > max_text_width {
            // Text is too long, use ellipsis
            let clip_rect = egui::Rect::from_min_size(
                text_pos - egui::vec2(0.0, galley.size().y / 2.0),
                egui::vec2(max_text_width, galley.size().y),
            );
            ui.painter().with_clip_rect(clip_rect).galley(
                text_pos - egui::vec2(0.0, galley.size().y / 2.0),
                galley,
                text_color,
            );
        } else {
            ui.painter().text(
                text_pos,
                egui::Align2::LEFT_CENTER,
                title,
                font_id,
                text_color,
            );
        }

        // Draw cwd below title - only in Agentic mode
        if show_cwd {
            let cwd_pos = rect.left_center() + egui::vec2(text_start_x, 7.0);
            cwd_ui(ui, cwd, home_dir, cwd_pos, max_text_width);
        }

        response
    }
}

#[derive(Clone, Copy)]
struct PressStart(f64);

enum RenameOutcome {
    Confirmed(String),
    Cancelled,
}

fn inline_rename_ui(
    ui: &mut egui::Ui,
    response: &egui::Response,
    buf: &mut String,
) -> Option<RenameOutcome> {
    let edit_rect = response.rect.shrink2(egui::vec2(8.0, 4.0));
    let edit = egui::Area::new(egui::Id::new("rename_textedit"))
        .fixed_pos(edit_rect.min)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            ui.set_width(edit_rect.width());
            ui.add(
                egui::TextEdit::singleline(buf)
                    .font(egui::FontId::proportional(14.0))
                    .frame(false),
            )
        })
        .inner;

    if !edit.has_focus() && !edit.lost_focus() {
        edit.request_focus();
    }

    if edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        Some(RenameOutcome::Confirmed(buf.clone()))
    } else if edit.lost_focus() {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            Some(RenameOutcome::Cancelled)
        } else {
            Some(RenameOutcome::Confirmed(buf.clone()))
        }
    } else {
        None
    }
}

/// Draw cwd text (monospace, weak+small) with clipping.
fn cwd_ui(ui: &mut egui::Ui, cwd_path: &Path, home_dir: &str, pos: egui::Pos2, max_width: f32) {
    let display_text = if home_dir.is_empty() {
        crate::path_utils::abbreviate_path(cwd_path)
    } else {
        crate::path_utils::abbreviate_with_home(cwd_path, home_dir)
    };
    let cwd_font = egui::FontId::monospace(10.0);
    let cwd_color = ui.visuals().weak_text_color();

    let cwd_galley = ui
        .painter()
        .layout_no_wrap(display_text.clone(), cwd_font.clone(), cwd_color);

    if cwd_galley.size().x > max_width {
        let clip_rect = egui::Rect::from_min_size(
            pos - egui::vec2(0.0, cwd_galley.size().y / 2.0),
            egui::vec2(max_width, cwd_galley.size().y),
        );
        ui.painter().with_clip_rect(clip_rect).galley(
            pos - egui::vec2(0.0, cwd_galley.size().y / 2.0),
            cwd_galley,
            cwd_color,
        );
    } else {
        ui.painter().text(
            pos,
            egui::Align2::LEFT_CENTER,
            &display_text,
            cwd_font,
            cwd_color,
        );
    }
}
