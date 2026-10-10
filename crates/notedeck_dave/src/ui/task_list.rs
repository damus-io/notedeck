use egui::Color32;
use serde_json::Value;

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
        let todos = latest_todos(chat);

        ui.vertical(|ui| {
            // Header
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Tasks").size(16.0).strong());

                // Show count of in-progress tasks
                let in_progress = todos.map(|t| TodoCounts::of(t).in_progress).unwrap_or(0);
                if in_progress > 0 {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("{} active", in_progress))
                            .size(11.0)
                            .color(Color32::from_rgb(147, 197, 253)),
                    );
                }
            });

            ui.add_space(8.0);

            match todos {
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new("No tasks yet")
                                .color(ui.visuals().weak_text_color())
                                .italics(),
                        );
                    });
                }
                Some(todos) => {
                    egui::ScrollArea::vertical()
                        .id_salt("task_list_scroll")
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            for todo in todos {
                                todo_row_ui(ui, &TodoItem::from_json(todo));
                            }
                        });
                }
            }
        });
    }
}

/// The todos of the chat's latest `TodoUpdate`, from TodoWrite or the task
/// tracker's TaskCreate/TaskUpdate fold. `None` when the session has none,
/// or the latest one carries no `todos` array.
pub fn latest_todos(chat: &[Message]) -> Option<&[Value]> {
    TaskListPanel::find_latest_todowrite(chat)?
        .get("todos")?
        .as_array()
        .map(Vec::as_slice)
}

/// Where a todo stands, as TodoWrite and TaskUpdate spell it. Anything else
/// reads as pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// One todo, borrowed from a `TodoUpdate`'s JSON, so drawing it allocates
/// nothing.
#[derive(Clone, Copy, Debug)]
pub struct TodoItem<'a> {
    /// What the task is: "Run the tests".
    pub content: &'a str,
    /// The same, as it is happening: "Running the tests". May be empty.
    pub active_form: &'a str,
    pub status: TodoStatus,
}

impl<'a> TodoItem<'a> {
    /// Read one entry of a `todos` array. Missing fields are empty.
    pub fn from_json(todo: &'a Value) -> Self {
        let field = |name| todo.get(name).and_then(Value::as_str).unwrap_or("");
        let status = match field("status") {
            "completed" => TodoStatus::Completed,
            "in_progress" => TodoStatus::InProgress,
            _ => TodoStatus::Pending,
        };
        Self {
            content: field("content"),
            active_form: field("activeForm"),
            status,
        }
    }

    /// What its row says: the active form while it is in progress, the
    /// content otherwise.
    pub fn display_text(&self) -> &'a str {
        if self.status == TodoStatus::InProgress && !self.active_form.is_empty() {
            self.active_form
        } else {
            self.content
        }
    }
}

/// How many todos a list has, and how many are done or under way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TodoCounts {
    pub total: usize,
    pub done: usize,
    pub in_progress: usize,
}

impl TodoCounts {
    /// Count `todos`, a `TodoUpdate`'s `todos` array.
    pub fn of(todos: &[Value]) -> Self {
        let mut counts = Self::default();
        for todo in todos {
            counts.total += 1;
            match TodoItem::from_json(todo).status {
                TodoStatus::Completed => counts.done += 1,
                TodoStatus::InProgress => counts.in_progress += 1,
                TodoStatus::Pending => {}
            }
        }
        counts
    }
}

/// One todo as a checkbox-style row: an empty box when pending, a dot while
/// in progress, a check and struck-through text once done. Shared by the
/// chat's task sidebar and the Details tab's Tasks card.
pub fn todo_row_ui(ui: &mut egui::Ui, todo: &TodoItem<'_>) {
    let item_height = 24.0;

    ui.horizontal(|ui| {
        ui.set_height(item_height);
        ui.add_space(4.0);

        // Checkbox-style indicator colors
        let (checkbox_color, text_color) = match todo.status {
            TodoStatus::Completed => (
                Color32::from_rgb(34, 197, 94), // Green
                ui.visuals().weak_text_color(),
            ),
            TodoStatus::InProgress => (
                Color32::from_rgb(59, 130, 246), // Blue
                ui.visuals().text_color(),
            ),
            TodoStatus::Pending => (ui.visuals().weak_text_color(), ui.visuals().text_color()),
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
            egui::Stroke::new(1.5_f32, checkbox_color),
            egui::StrokeKind::Inside,
        );

        // Fill based on status
        match todo.status {
            TodoStatus::Completed => {
                // Checkmark
                let check_points = [
                    checkbox_rect.left_center() + egui::vec2(3.0, 0.0),
                    checkbox_rect.center() + egui::vec2(-1.0, 3.0),
                    checkbox_rect.right_top() + egui::vec2(-2.0, 3.0),
                ];
                ui.painter().line_segment(
                    [check_points[0], check_points[1]],
                    egui::Stroke::new(2.0_f32, checkbox_color),
                );
                ui.painter().line_segment(
                    [check_points[1], check_points[2]],
                    egui::Stroke::new(2.0_f32, checkbox_color),
                );
            }
            TodoStatus::InProgress => {
                // Filled dot
                ui.painter()
                    .circle_filled(checkbox_rect.center(), 4.0, checkbox_color);
            }
            TodoStatus::Pending => {}
        }

        ui.add_space(6.0);

        let text = egui::RichText::new(todo.display_text())
            .size(12.0)
            .color(text_color);
        // Apply strikethrough for completed
        let text = if todo.status == TodoStatus::Completed {
            text.strikethrough()
        } else {
            text
        };

        ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Truncate));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The latest TodoUpdate wins, its statuses parse, and an in-progress
    /// todo reads as its active form.
    #[test]
    fn reads_the_latest_todos() {
        let chat = vec![
            Message::TodoUpdate(json!({"todos": [{"content": "old", "status": "pending"}]})),
            Message::TodoUpdate(json!({"todos": [
                {"content": "Write it", "activeForm": "Writing it", "status": "completed"},
                {"content": "Test it", "activeForm": "Testing it", "status": "in_progress"},
                {"content": "Ship it", "status": "pending"},
                {"content": "Odd one", "status": "deleted"},
            ]})),
        ];
        let todos = latest_todos(&chat).unwrap();
        let texts: Vec<&str> = todos
            .iter()
            .map(|t| TodoItem::from_json(t).display_text())
            .collect();
        assert_eq!(texts, ["Write it", "Testing it", "Ship it", "Odd one"]);
        assert_eq!(
            TodoCounts::of(todos),
            TodoCounts {
                total: 4,
                done: 1,
                in_progress: 1
            }
        );
        assert!(latest_todos(&[]).is_none());
    }
}
