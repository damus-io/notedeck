use egui::Color32;

use crate::messages::Message;

/// UI component for displaying the task list sidebar (TodoWrite items)
pub struct TaskListPanel;

impl TaskListPanel {
    /// Check if there are any todos in the chat (from TodoWrite tool calls)
    pub fn has_tasks(chat: &[Message]) -> bool {
        Self::find_latest_todowrite(chat).is_some()
    }

    /// Find the most recent TodoUpdate message
    fn find_latest_todowrite(chat: &[Message]) -> Option<&serde_json::Value> {
        for msg in chat.iter().rev() {
            if let Message::TodoUpdate(todos) = msg {
                return Some(todos);
            }
        }
        None
    }

    /// Render the task list panel
    pub fn ui(ui: &mut egui::Ui, chat: &[Message]) {
        let todowrite_input = Self::find_latest_todowrite(chat);

        ui.vertical(|ui| {
            // Header
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Tasks").size(16.0).strong());

                // Show count of in-progress tasks
                if let Some(input) = todowrite_input {
                    if let Some(todos) = input.get("todos").and_then(|v| v.as_array()) {
                        let in_progress = todos
                            .iter()
                            .filter(|t| {
                                t.get("status").and_then(|s| s.as_str()) == Some("in_progress")
                            })
                            .count();
                        if in_progress > 0 {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(format!("{} active", in_progress))
                                    .size(11.0)
                                    .color(Color32::from_rgb(147, 197, 253)),
                            );
                        }
                    }
                }
            });

            ui.add_space(8.0);

            match todowrite_input {
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new("No tasks yet")
                                .color(ui.visuals().weak_text_color())
                                .italics(),
                        );
                    });
                }
                Some(input) => {
                    if let Some(todos) = input.get("todos").and_then(|v| v.as_array()) {
                        egui::ScrollArea::vertical()
                            .id_salt("task_list_scroll")
                            .auto_shrink([false; 2])
                            .show(ui, |ui| {
                                for todo in todos {
                                    Self::todo_item_ui(ui, todo);
                                }
                            });
                    }
                }
            }
        });
    }

    /// Render a single todo item as a checkbox-style row
    fn todo_item_ui(ui: &mut egui::Ui, todo: &serde_json::Value) {
        let content = todo.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let active_form = todo
            .get("activeForm")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let status = todo
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");

        let item_height = 24.0;

        ui.horizontal(|ui| {
            ui.set_height(item_height);
            ui.add_space(4.0);

            // Checkbox-style indicator colors
            let (checkbox_color, text_color) = match status {
                "completed" => (
                    Color32::from_rgb(34, 197, 94), // Green
                    ui.visuals().weak_text_color(),
                ),
                "in_progress" => (
                    Color32::from_rgb(59, 130, 246), // Blue
                    ui.visuals().text_color(),
                ),
                _ => (ui.visuals().weak_text_color(), ui.visuals().text_color()),
            };

            // Draw checkbox
            let checkbox_size = 14.0;
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(checkbox_size, checkbox_size),
                egui::Sense::hover(),
            );

            let checkbox_rect = egui::Rect::from_center_size(
                egui::pos2(rect.center().x, ui.min_rect().center().y),
                egui::vec2(checkbox_size, checkbox_size),
            );

            ui.painter().rect_stroke(
                checkbox_rect,
                2.0,
                egui::Stroke::new(1.5, checkbox_color),
                egui::StrokeKind::Inside,
            );

            // Fill based on status
            match status {
                "completed" => {
                    // Checkmark
                    let check_points = [
                        checkbox_rect.left_center() + egui::vec2(3.0, 0.0),
                        checkbox_rect.center() + egui::vec2(-1.0, 3.0),
                        checkbox_rect.right_top() + egui::vec2(-2.0, 3.0),
                    ];
                    ui.painter().line_segment(
                        [check_points[0], check_points[1]],
                        egui::Stroke::new(2.0, checkbox_color),
                    );
                    ui.painter().line_segment(
                        [check_points[1], check_points[2]],
                        egui::Stroke::new(2.0, checkbox_color),
                    );
                }
                "in_progress" => {
                    // Filled dot
                    ui.painter()
                        .circle_filled(checkbox_rect.center(), 4.0, checkbox_color);
                }
                _ => {}
            }

            ui.add_space(6.0);

            // Task text - use activeForm for in-progress, content otherwise
            let display_text = if status == "in_progress" && !active_form.is_empty() {
                active_form
            } else {
                content
            };

            // Apply strikethrough for completed
            let text = if status == "completed" {
                egui::RichText::new(display_text)
                    .size(12.0)
                    .color(text_color)
                    .strikethrough()
            } else {
                egui::RichText::new(display_text)
                    .size(12.0)
                    .color(text_color)
            };

            ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Truncate));
        });
    }
}
