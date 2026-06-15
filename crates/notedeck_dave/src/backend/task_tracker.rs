//! Tracks the harness task list (`TaskCreate`/`TaskUpdate` tools) and renders it
//! into the `todos`-shaped JSON that the task list sidebar already understands.
//!
//! Unlike `TodoWrite`, which sends the entire list on every call, the `Task*`
//! tools are incremental: `TaskCreate` returns the new id in its *result* text
//! and `TaskUpdate` references that id in its *input*. We accumulate the state
//! here and emit the full list as a `TodoUpdate` whenever it changes.

use super::tool_summary::extract_response_content;
use serde_json::{json, Value};

/// Accumulates task state from `Task*` tool calls across a session.
#[derive(Default)]
pub struct TaskTracker {
    /// Ordered tasks, each shaped as `{ id, content, activeForm, status }`.
    tasks: Vec<Value>,
}

impl TaskTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handle a completed tool call. Returns the full `todos` payload to emit as
    /// a `TodoUpdate` when the call changed the task list, or `None` otherwise.
    pub fn handle_tool(&mut self, name: &str, input: &Value, result: &Value) -> Option<Value> {
        match name {
            "TaskCreate" => self.handle_create(input, result),
            "TaskUpdate" => self.handle_update(input),
            _ => None,
        }
    }

    fn handle_create(&mut self, input: &Value, result: &Value) -> Option<Value> {
        let id = parse_created_id(result)?;
        let content = input
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let active_form = input
            .get("activeForm")
            .and_then(Value::as_str)
            .unwrap_or_default();
        self.tasks.push(json!({
            "id": id,
            "content": content,
            "activeForm": active_form,
            "status": "pending",
        }));
        Some(self.todos())
    }

    fn handle_update(&mut self, input: &Value) -> Option<Value> {
        let id = input.get("taskId").and_then(Value::as_str)?;

        // Deleted tasks drop out of the list entirely.
        if input.get("status").and_then(Value::as_str) == Some("deleted") {
            let before = self.tasks.len();
            self.tasks
                .retain(|t| t.get("id").and_then(Value::as_str) != Some(id));
            return (self.tasks.len() != before).then(|| self.todos());
        }

        let task = self
            .tasks
            .iter_mut()
            .find(|t| t.get("id").and_then(Value::as_str) == Some(id))?;

        if let Some(status) = input.get("status").and_then(Value::as_str) {
            task["status"] = json!(status);
        }
        // The sidebar keys off `content`; TaskUpdate calls it `subject`.
        if let Some(subject) = input.get("subject").and_then(Value::as_str) {
            task["content"] = json!(subject);
        }
        if let Some(active_form) = input.get("activeForm").and_then(Value::as_str) {
            task["activeForm"] = json!(active_form);
        }
        Some(self.todos())
    }

    fn todos(&self) -> Value {
        json!({ "todos": self.tasks.clone() })
    }
}

/// Extract the numeric id from a `TaskCreate` result like
/// "Task #1 created successfully: ...". Returns the id as a string ("1") so it
/// matches the `taskId` field used by `TaskUpdate`.
fn parse_created_id(result: &Value) -> Option<String> {
    let text = extract_response_content(result)?;
    let after_hash = text.split('#').nth(1)?;
    let digits: String = after_hash
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (!digits.is_empty()).then_some(digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_result(id: u32, subject: &str) -> Value {
        json!(format!("Task #{id} created successfully: {subject}"))
    }

    fn todos(payload: &Value) -> &Vec<Value> {
        payload.get("todos").and_then(Value::as_array).unwrap()
    }

    #[test]
    fn create_then_update_status() {
        let mut tracker = TaskTracker::new();

        let payload = tracker
            .handle_tool(
                "TaskCreate",
                &json!({ "subject": "Do a thing", "activeForm": "Doing a thing" }),
                &create_result(1, "Do a thing"),
            )
            .unwrap();
        let items = todos(&payload);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["content"], "Do a thing");
        assert_eq!(items[0]["status"], "pending");

        let payload = tracker
            .handle_tool(
                "TaskUpdate",
                &json!({ "taskId": "1", "status": "in_progress" }),
                &Value::Null,
            )
            .unwrap();
        assert_eq!(todos(&payload)[0]["status"], "in_progress");
    }

    #[test]
    fn update_subject_maps_to_content() {
        let mut tracker = TaskTracker::new();
        tracker
            .handle_tool(
                "TaskCreate",
                &json!({ "subject": "Old" }),
                &create_result(1, "Old"),
            )
            .unwrap();

        let payload = tracker
            .handle_tool(
                "TaskUpdate",
                &json!({ "taskId": "1", "subject": "New" }),
                &Value::Null,
            )
            .unwrap();
        assert_eq!(todos(&payload)[0]["content"], "New");
    }

    #[test]
    fn delete_removes_task() {
        let mut tracker = TaskTracker::new();
        tracker
            .handle_tool(
                "TaskCreate",
                &json!({ "subject": "A" }),
                &create_result(1, "A"),
            )
            .unwrap();
        tracker
            .handle_tool(
                "TaskCreate",
                &json!({ "subject": "B" }),
                &create_result(2, "B"),
            )
            .unwrap();

        let payload = tracker
            .handle_tool(
                "TaskUpdate",
                &json!({ "taskId": "1", "status": "deleted" }),
                &Value::Null,
            )
            .unwrap();
        let items = todos(&payload);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["content"], "B");
    }

    #[test]
    fn unknown_id_and_unrelated_tool_are_ignored() {
        let mut tracker = TaskTracker::new();
        assert!(tracker
            .handle_tool(
                "TaskUpdate",
                &json!({ "taskId": "99", "status": "completed" }),
                &Value::Null
            )
            .is_none());
        assert!(tracker
            .handle_tool("Read", &json!({ "file_path": "x" }), &Value::Null)
            .is_none());
    }
}
