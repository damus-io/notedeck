use std::path::PathBuf;

use egui::{Align, Color32, Layout, Sense};
use notedeck_ui::app_images;

use crate::agent_status::AgentStatus;
use crate::backend::BackendType;
use crate::collapse_state::CollapseState;
use crate::config::AiMode;
use crate::focus_queue::{FocusPriority, FocusQueue};
use crate::session::{SessionId, SessionManager};
use crate::ui::keybind_hint::{keybind_hint, paint_keybind_hint, KeybindHint};

/// Actions that can be triggered from the session list UI
#[derive(Debug, Clone)]
pub enum SessionListAction {
    NewSession,
    SwitchTo(SessionId),
    Delete(SessionId),
    Rename(SessionId, String),
    DismissDone(SessionId),
    Duplicate(SessionId),
    Reset(SessionId),
    NewWorktree(SessionId),
    DeleteWorktree(SessionId),
    ToggleHostCollapse(String),
    ToggleCwdCollapse(String, String),
    NewSessionInCwd(PathBuf),
}

/// UI component for displaying the session list sidebar
pub struct SessionListUi<'a> {
    session_manager: &'a SessionManager,
    focus_queue: &'a FocusQueue,
    collapse_state: &'a CollapseState,
    ctrl_held: bool,
}

impl<'a> SessionListUi<'a> {
    pub fn new(
        session_manager: &'a SessionManager,
        focus_queue: &'a FocusQueue,
        collapse_state: &'a CollapseState,
        ctrl_held: bool,
    ) -> Self {
        SessionListUi {
            session_manager,
            focus_queue,
            collapse_state,
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
        let mut visual_index: usize = 0;

        // Agents grouped by host → cwd (pre-computed, deterministically ordered)
        for host_group in self.session_manager.host_cwd_groups() {
            let host_label = if host_group.hostname.is_empty() {
                "Local"
            } else {
                &host_group.hostname
            };

            let host_collapsed = self.collapse_state.is_host_collapsed(&host_group.hostname);

            let host_response = host_header_ui(ui, host_label);
            if host_response.clicked() {
                action = Some(SessionListAction::ToggleHostCollapse(
                    host_group.hostname.clone(),
                ));
            }
            ui.add_space(4.0);

            if host_collapsed {
                ui.add_space(6.0);
                continue;
            }

            for cwd_group in &host_group.cwd_groups {
                let collapsed = self
                    .collapse_state
                    .is_cwd_collapsed(&host_group.hostname, &cwd_group.display_cwd);

                let header_response = cwd_folder_header(ui, &cwd_group.display_cwd, collapsed);
                if header_response.clicked() {
                    action = Some(SessionListAction::ToggleCwdCollapse(
                        host_group.hostname.clone(),
                        cwd_group.display_cwd.clone(),
                    ));
                }

                notedeck_ui::context_menu::context_menu(&header_response, |ui| {
                    if ui.button("New Session").clicked() {
                        action = Some(SessionListAction::NewSessionInCwd(cwd_group.cwd.clone()));
                        ui.close_menu();
                    }
                });

                if !collapsed {
                    ui.add_space(2.0);
                    for &id in &cwd_group.session_ids {
                        if let Some(session) = self.session_manager.get(id) {
                            if let Some(a) =
                                self.render_session_item(ui, session, visual_index, active_id)
                            {
                                action = Some(a);
                            }
                            visual_index += 1;
                        }
                    }
                }
                ui.add_space(2.0);
            }
            ui.add_space(6.0);
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
                    if let Some(a) = self.render_session_item(ui, session, visual_index, active_id)
                    {
                        action = Some(a);
                    }
                    visual_index += 1;
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
        let (response, dot_action) = if session.ai_mode == AiMode::Agentic {
            self.agent_row_ui(
                ui,
                session.id,
                display_title,
                is_active,
                shortcut_hint,
                session.status(),
                queue_priority,
                session.backend_type,
            )
        } else {
            self.chat_row_ui(
                ui,
                session.id,
                display_title,
                is_active,
                shortcut_hint,
                queue_priority,
            )
        };

        let mut action = dot_action;

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

        let ctrl_held = self.ctrl_held;
        let is_agentic = session.ai_mode == AiMode::Agentic;
        let confirm_id = egui::Id::new("confirm_delete_worktree").with(session.id);

        notedeck_ui::context_menu::context_menu(&response, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Rename").clicked() {
                    let rename_state = (session.id, session.details.display_title().to_string());
                    ui.ctx()
                        .data_mut(|d| d.insert_temp(rename_id, rename_state));
                    ui.close_menu();
                }
                if is_active && ctrl_held {
                    keybind_hint(ui, "⌃⇧R");
                }
            });
            if is_agentic {
                ui.horizontal(|ui| {
                    if ui.button("Duplicate").clicked() {
                        action = Some(SessionListAction::Duplicate(session.id));
                        ui.close_menu();
                    }
                    if is_active && ctrl_held {
                        keybind_hint(ui, "⌃⇧T");
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Clear").clicked() {
                        action = Some(SessionListAction::Reset(session.id));
                        ui.close_menu();
                    }
                    if is_active && ctrl_held {
                        keybind_hint(ui, "⌃⇧K");
                    }
                });
            }
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    action = Some(SessionListAction::Delete(session.id));
                    ui.close_menu();
                }
                if ctrl_held {
                    keybind_hint(ui, "Del");
                }
            });
            let is_git_repo = session
                .agentic
                .as_ref()
                .and_then(|a| a.git_status.current())
                .map(|r| r.is_ok())
                .unwrap_or(false);
            if is_git_repo && ui.button("New worktree from this session").clicked() {
                action = Some(SessionListAction::NewWorktree(session.id));
                ui.close_menu();
            }
            let is_worktree = is_git_repo
                && session
                    .cwd()
                    .map(|p| crate::worktree::is_linked_worktree(p))
                    .unwrap_or(false);
            if is_worktree {
                if let Some(a) = delete_worktree_menu_item(ui, session.id, confirm_id) {
                    action = Some(a);
                }
            }
        });

        // Reset confirmation state when the menu is closed.
        if !response.context_menu_opened() {
            ui.ctx().data_mut(|d| d.insert_temp(confirm_id, false));
        }

        action
    }

    /// Render an agentic session row (status bar, backend icon, focus dot).
    #[allow(clippy::too_many_arguments)]
    fn agent_row_ui(
        &self,
        ui: &mut egui::Ui,
        session_id: SessionId,
        title: &str,
        is_active: bool,
        shortcut_hint: Option<usize>,
        status: AgentStatus,
        queue_priority: Option<FocusPriority>,
        backend_type: BackendType,
    ) -> (egui::Response, Option<SessionListAction>) {
        let desired_size = egui::vec2(ui.available_width(), 32.0);
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

        paint_row_background(ui, rect, is_active, &response);

        // Status color indicator (left edge vertical bar)
        let status_color = status.color();
        let status_bar_rect = egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(2.0, 4.0),
            egui::vec2(3.0, rect.height() - 8.0),
        );
        ui.painter().rect_filled(status_bar_rect, 1.5, status_color);
        let mut text_start_x = 12.0;

        // Backend icon (only for agentic backends)
        if backend_type.is_agentic() {
            let icon_size = 14.0;
            let icon_rect = egui::Rect::from_center_size(
                rect.left_center() + egui::vec2(text_start_x + icon_size / 2.0, 0.0),
                egui::vec2(icon_size, icon_size),
            );
            let icon = crate::ui::backend_icon(backend_type);
            icon.paint_at(ui, icon_rect);
            text_start_x += icon_size + 4.0;
        }

        let hints: &[(&str, &str)] = &[("⇧T", "Duplicate"), ("⇧K", "Clear"), ("⇧R", "Rename")];
        let (right_used, dot_action) = render_row_right_side(
            ui,
            rect,
            session_id,
            is_active,
            self.ctrl_held,
            shortcut_hint,
            queue_priority,
            hints,
        );

        let max_text_width = rect.width() - text_start_x - right_used;
        let font_id = egui::FontId::proportional(14.0);
        let title_height = ui
            .painter()
            .layout_no_wrap(title.to_string(), font_id, ui.visuals().text_color())
            .size()
            .y;
        let title_top = rect.center().y - title_height / 2.0;
        render_title(
            ui,
            title,
            rect.left() + text_start_x,
            title_top,
            max_text_width,
        );

        (response, dot_action)
    }

    /// Render a chat session row (no status bar, no cwd).
    fn chat_row_ui(
        &self,
        ui: &mut egui::Ui,
        session_id: SessionId,
        title: &str,
        is_active: bool,
        shortcut_hint: Option<usize>,
        queue_priority: Option<FocusPriority>,
    ) -> (egui::Response, Option<SessionListAction>) {
        let desired_size = egui::vec2(ui.available_width(), 32.0);
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

        paint_row_background(ui, rect, is_active, &response);

        let text_start_x = 8.0;
        let hints: &[(&str, &str)] = &[("⇧R", "Rename")];
        let (right_used, dot_action) = render_row_right_side(
            ui,
            rect,
            session_id,
            is_active,
            self.ctrl_held,
            shortcut_hint,
            queue_priority,
            hints,
        );

        let max_text_width = rect.width() - text_start_x - right_used;
        let font_id = egui::FontId::proportional(14.0);
        let title_height = ui
            .painter()
            .layout_no_wrap(title.to_string(), font_id, ui.visuals().text_color())
            .size()
            .y;
        let title_top = rect.center().y - title_height / 2.0;
        render_title(
            ui,
            title,
            rect.left() + text_start_x,
            title_top,
            max_text_width,
        );

        (response, dot_action)
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
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            Some(RenameOutcome::Cancelled)
        } else {
            Some(RenameOutcome::Confirmed(buf.clone()))
        }
    } else {
        None
    }
}

/// Truncate text from the start, showing "…" + the longest suffix that fits.
/// Uses binary search over character offsets for O(log n) performance.
pub(crate) fn truncate_start(
    ui: &egui::Ui,
    text: &str,
    font: &egui::FontId,
    max_width: f32,
) -> String {
    if text.is_empty() {
        return String::new();
    }
    let measure = |s: String| -> f32 {
        ui.painter()
            .layout_no_wrap(s, font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if measure(text.to_string()) <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 1usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let candidate: String = std::iter::once('…')
            .chain(chars[mid..].iter().copied())
            .collect();
        if measure(candidate) <= max_width {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    if lo >= chars.len() {
        "…".to_string()
    } else {
        std::iter::once('…')
            .chain(chars[lo..].iter().copied())
            .collect()
    }
}

/// Truncate a `prefix + path` string to fit within `max_width`.
///
/// Three tiers:
/// 1. Everything fits — return as-is.
/// 2. Path fits alone — truncate prefix from start to fill remaining space.
/// 3. Path overflows — drop prefix, truncate path from start.
///
/// Returns `(display_text, was_truncated)`.
pub(crate) fn truncate_host_and_path(
    ui: &egui::Ui,
    prefix: &str,
    path: &str,
    max_width: f32,
) -> (String, bool) {
    let font = egui::FontId::monospace(10.0);
    let weak_color = ui.visuals().weak_text_color();

    let prefix_width = if prefix.is_empty() {
        0.0
    } else {
        ui.painter()
            .layout_no_wrap(prefix.to_string(), font.clone(), weak_color)
            .size()
            .x
    };

    let path_width = ui
        .painter()
        .layout_no_wrap(path.to_string(), font.clone(), weak_color)
        .size()
        .x;

    if path_width <= max_width - prefix_width {
        (format!("{}{}", prefix, path), false)
    } else if path_width <= max_width {
        let host_budget = max_width - path_width;
        let truncated_prefix = truncate_start(ui, prefix, &font, host_budget);
        (format!("{}{}", truncated_prefix, path), true)
    } else {
        let path_text = truncate_start(ui, path, &font, max_width);
        (path_text, true)
    }
}

fn paint_row_background(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    is_active: bool,
    response: &egui::Response,
) {
    let fill = if is_active {
        ui.visuals().widgets.active.bg_fill
    } else if response.hovered() {
        ui.visuals().widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, fill);
}

/// Render the right side of a session row: shortcut number, ctrl+shift hints, and focus dot.
/// Returns (total right-side width consumed, optional dot action).
#[allow(clippy::too_many_arguments)]
fn render_row_right_side(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    session_id: SessionId,
    is_active: bool,
    ctrl_held: bool,
    shortcut_hint: Option<usize>,
    queue_priority: Option<FocusPriority>,
    hints: &[(&str, &str)],
) -> (f32, Option<SessionListAction>) {
    let mut right_offset = 8.0;
    let mut dot_action = None;

    if let Some(num) = shortcut_hint {
        let hint_size = 18.0;
        let hint_text = format!("{}", num);
        let hint_center = rect.right_center() - egui::vec2(8.0 + hint_size / 2.0, 0.0);
        paint_keybind_hint(ui, hint_center, &hint_text, hint_size);
        right_offset = 8.0 + hint_size + 6.0;
    }

    if is_active && ctrl_held {
        let hint_size = 16.0;
        let hint_width = 26.0;
        let gap = 3.0;

        for (hint_text, tooltip) in hints {
            let center = rect.right_center() - egui::vec2(right_offset + hint_width / 2.0, 0.0);
            KeybindHint::new(hint_text)
                .size(hint_size)
                .width(hint_width)
                .paint_at(ui, center);
            let hint_rect = egui::Rect::from_center_size(center, egui::vec2(hint_width, hint_size));
            ui.interact(
                hint_rect,
                ui.id().with(("keybind_tip", *hint_text)),
                Sense::hover(),
            )
            .on_hover_text(*tooltip);
            right_offset += hint_width + gap;
        }
    }

    if let Some(priority) = queue_priority {
        let dot_radius = 5.0;
        let dot_center = rect.right_center() - egui::vec2(right_offset + dot_radius + 4.0, 0.0);
        ui.painter()
            .circle_filled(dot_center, dot_radius, priority.color());

        if priority == FocusPriority::Done {
            let dot_rect = egui::Rect::from_center_size(
                dot_center,
                egui::vec2(dot_radius * 4.0, dot_radius * 4.0),
            );
            let dot_response = ui.interact(
                dot_rect,
                ui.id().with(("dismiss_dot", session_id)),
                egui::Sense::click(),
            );
            if dot_response.clicked() {
                dot_action = Some(SessionListAction::DismissDone(session_id));
            }
            if dot_response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }

        right_offset += dot_radius * 2.0 + 8.0;
    }

    (right_offset, dot_action)
}

/// Render a title string at the given position, clipping if it exceeds max_width.
fn render_title(ui: &mut egui::Ui, title: &str, x: f32, y: f32, max_width: f32) {
    let font_id = egui::FontId::proportional(14.0);
    let text_color = ui.visuals().text_color();
    let galley = ui
        .painter()
        .layout_no_wrap(title.to_string(), font_id.clone(), text_color);
    let title_height = galley.size().y;
    let title_pos = egui::pos2(x, y);

    if galley.size().x > max_width {
        let clip_rect = egui::Rect::from_min_size(title_pos, egui::vec2(max_width, title_height));
        ui.painter()
            .with_clip_rect(clip_rect)
            .galley(title_pos, galley, text_color);
    } else {
        ui.painter().text(
            title_pos,
            egui::Align2::LEFT_TOP,
            title,
            font_id,
            text_color,
        );
    }
}

/// Render a collapsible host header. Truncates with ellipsis and shows
/// full name on hover.
fn host_header_ui(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let text = egui::RichText::new(label)
        .size(14.0)
        .color(ui.visuals().weak_text_color());
    let widget = egui::Label::new(text).truncate().sense(Sense::click());
    ui.add(widget)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Render a collapsible cwd folder header with disclosure triangle and truncated path.
fn cwd_folder_header(ui: &mut egui::Ui, cwd_display: &str, collapsed: bool) -> egui::Response {
    let header_height = 24.0;
    let desired_size = egui::vec2(ui.available_width(), header_height);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, cwd_display));

    let triangle_size = 8.0;
    let triangle_center = egui::pos2(rect.left() + 4.0 + triangle_size / 2.0, rect.center().y);
    let weak_color = ui.visuals().weak_text_color();

    let triangle_points = disclosure_triangle(triangle_center, collapsed);
    ui.painter().add(egui::Shape::convex_polygon(
        triangle_points,
        weak_color,
        egui::Stroke::NONE,
    ));

    let text_start = rect.left() + 4.0 + triangle_size + 4.0;
    let max_text_width = rect.right() - text_start - 4.0;
    let (text, _) = truncate_host_and_path(ui, "", cwd_display, max_text_width);
    let font = egui::FontId::monospace(10.0);
    let text_pos = egui::pos2(text_start, rect.center().y);
    ui.painter()
        .text(text_pos, egui::Align2::LEFT_CENTER, &text, font, weak_color);

    response
}

/// Compute the three vertices for a disclosure triangle.
fn disclosure_triangle(center: egui::Pos2, collapsed: bool) -> Vec<egui::Pos2> {
    if collapsed {
        vec![
            center + egui::vec2(-3.0, -4.0),
            center + egui::vec2(4.0, 0.0),
            center + egui::vec2(-3.0, 4.0),
        ]
    } else {
        vec![
            center + egui::vec2(-4.0, -3.0),
            center + egui::vec2(4.0, -3.0),
            center + egui::vec2(0.0, 4.0),
        ]
    }
}

/// Renders the "Delete worktree" context-menu item with an inline confirmation step.
///
/// First click shows "Delete this worktree? / Cancel / Delete".
/// Confirmation state is stored in egui temp storage keyed per session.
fn delete_worktree_menu_item(
    ui: &mut egui::Ui,
    session_id: SessionId,
    confirm_id: egui::Id,
) -> Option<SessionListAction> {
    let confirming: bool = ui.ctx().data(|d| d.get_temp(confirm_id).unwrap_or(false));

    let mut action = None;

    if confirming {
        ui.separator();
        ui.label("Delete this worktree?");
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(confirm_id, false));
                ui.close_menu();
            }
            if ui.button("Delete").clicked() {
                action = Some(SessionListAction::DeleteWorktree(session_id));
                ui.ctx().data_mut(|d| d.insert_temp(confirm_id, false));
                ui.close_menu();
            }
        });
    } else if ui.button("Delete worktree").clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(confirm_id, true));
    }

    action
}

#[cfg(test)]
mod tests {
    use super::{SessionListAction, SessionListUi};
    use crate::backend::BackendType;
    use crate::collapse_state::CollapseState;
    use crate::config::AiMode;
    use crate::focus_queue::FocusQueue;
    use crate::session::SessionManager;
    use egui::Event;
    use egui_kittest::{kittest::Queryable, Harness};
    use std::path::PathBuf;

    struct SessionListHarnessState {
        ui: SessionListUiOwnedState,
        action: Option<SessionListAction>,
    }

    struct SessionListUiOwnedState {
        session_manager: SessionManager,
        focus_queue: FocusQueue,
        collapse_state: CollapseState,
        ctrl_held: bool,
        cwd_label: String,
        cwd_path: PathBuf,
    }

    impl SessionListUiOwnedState {
        fn new() -> Self {
            let cwd_path = PathBuf::from("/tmp/project");
            let mut session_manager = SessionManager::new();
            let session_id =
                session_manager.new_session(cwd_path.clone(), AiMode::Agentic, BackendType::Claude);
            let session = session_manager
                .get_mut(session_id)
                .expect("session should exist");
            session.details.hostname = "host-a".to_string();
            session.details.title = "Workspace agent".to_string();
            session.details.custom_title = None;
            session.details.home_dir = String::new();
            session_manager.rebuild_cwd_groups();

            let cwd_label = session_manager.host_cwd_groups()[0].cwd_groups[0]
                .display_cwd
                .clone();

            Self {
                session_manager,
                focus_queue: FocusQueue::new(),
                collapse_state: CollapseState::new(),
                ctrl_held: false,
                cwd_label,
                cwd_path,
            }
        }
    }

    #[test]
    fn right_clicking_cwd_header_can_start_a_new_session_in_that_cwd() {
        let mut harness = Harness::new_ui_state(
            |ui, state: &mut SessionListHarnessState| {
                let session_list = SessionListUi::new(
                    &state.ui.session_manager,
                    &state.ui.focus_queue,
                    &state.ui.collapse_state,
                    state.ui.ctrl_held,
                );
                if let Some(action) = session_list.ui(ui) {
                    state.action = Some(action);
                }
            },
            SessionListHarnessState {
                ui: SessionListUiOwnedState::new(),
                action: None,
            },
        );

        harness.run();

        let header = harness.get_by_label(harness.state().ui.cwd_label.as_str());
        let bounds = header.raw_bounds().expect("cwd header bounds");
        let center = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        harness.input_mut().events.push(Event::PointerMoved(center));
        harness.input_mut().events.push(Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Secondary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.input_mut().events.push(Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Secondary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();

        harness.get_by_label("New Session").click();
        harness.run();

        match harness.state().action.as_ref() {
            Some(SessionListAction::NewSessionInCwd(cwd)) => {
                assert_eq!(cwd, &harness.state().ui.cwd_path);
            }
            other => panic!("expected NewSessionInCwd action, got {:?}", other),
        }
    }
}
