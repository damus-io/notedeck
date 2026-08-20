//! Formatting utilities for tool execution summaries shown in the UI.
//!
//! These functions convert raw tool inputs and outputs into human-readable
//! summary strings that are displayed to users after tool execution.

/// Extract string content from a tool response, handling various JSON structures
pub fn extract_response_content(response: &serde_json::Value) -> Option<String> {
    // Try direct string first
    if let Some(s) = response.as_str() {
        return Some(s.to_string());
    }
    // Try "content" field (common wrapper)
    if let Some(s) = response.get("content").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    // Try file.content for Read tool responses
    if let Some(s) = response
        .get("file")
        .and_then(|f| f.get("content"))
        .and_then(|v| v.as_str())
    {
        return Some(s.to_string());
    }
    // Try "output" field
    if let Some(s) = response.get("output").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    // Try "result" field
    if let Some(s) = response.get("result").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    // Fallback: serialize the whole response if it's not null
    if !response.is_null() {
        return Some(response.to_string());
    }
    None
}

/// Format a human-readable summary for tool execution results
pub fn format_tool_summary(
    tool_name: &str,
    input: &serde_json::Value,
    response: &serde_json::Value,
) -> String {
    match tool_name {
        "Read" => format_read_summary(input, response),
        "Write" => format_write_summary(input),
        "Bash" => format_bash_summary(input),
        "Grep" => format_grep_summary(input),
        "Glob" => format_glob_summary(input),
        "Edit" => format_edit_summary(input),
        "Task" => format_task_summary(input),
        "Agent" => format_agent_summary(input),
        "Skill" => format_skill_summary(input),
        "SendMessage" => format_sendmessage_summary(input),
        _ => String::new(),
    }
}

fn format_read_summary(input: &serde_json::Value, response: &serde_json::Value) -> String {
    let file = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let filename = file.rsplit('/').next().unwrap_or(file);
    // Try to get numLines directly from file metadata (most accurate)
    let lines = response
        .get("file")
        .and_then(|f| f.get("numLines").or_else(|| f.get("totalLines")))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        // Fallback to counting lines in content
        .or_else(|| {
            extract_response_content(response)
                .as_ref()
                .map(|s| s.lines().count())
        })
        .unwrap_or(0);
    format!("{} ({} lines)", filename, lines)
}

fn format_write_summary(input: &serde_json::Value) -> String {
    let file = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let filename = file.rsplit('/').next().unwrap_or(file);
    let bytes = input
        .get("content")
        .and_then(|v| v.as_str())
        .map(|s| s.len())
        .unwrap_or(0);
    format!("{} ({} bytes)", filename, bytes)
}

/// Summarize a `Bash` tool call as its full command, backtick-quoted.
///
/// The command is shown untruncated; any stdout/stderr is surfaced separately
/// via the collapsible output body, so the summary carries no output-length
/// count — the old `"(N chars)"` only ever measured the output, never the
/// command, which read as an uninformative `"(13 chars)"` next to a truncated
/// command.
fn format_bash_summary(input: &serde_json::Value) -> String {
    let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
    format!("`{}`", cmd)
}

fn format_grep_summary(input: &serde_json::Value) -> String {
    let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
    format!("'{}'", pattern)
}

fn format_glob_summary(input: &serde_json::Value) -> String {
    let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
    format!("'{}'", pattern)
}

fn format_edit_summary(input: &serde_json::Value) -> String {
    let file = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let filename = file.rsplit('/').next().unwrap_or(file);
    filename.to_string()
}

fn format_task_summary(input: &serde_json::Value) -> String {
    let description = input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("task");
    let subagent_type = input
        .get("subagent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    format!("{} ({})", description, subagent_type)
}

/// Summarize an `Agent` tool call — the modern subagent launcher. It carries
/// the same `description`/`subagent_type` shape as the legacy `Task` tool, so
/// its summary mirrors Task's `"description (subagent_type)"`.
fn format_agent_summary(input: &serde_json::Value) -> String {
    format_task_summary(input)
}

/// Summarize a `Skill` tool call as the invoked skill name, optionally followed
/// by a preview of its `args`. The skill name leads because the collapsed header
/// truncates the summary, so the most identifying part must come first.
fn format_skill_summary(input: &serde_json::Value) -> String {
    let skill = input.get("skill").and_then(|v| v.as_str()).unwrap_or("?");
    match input.get("args").and_then(|v| v.as_str()) {
        Some(args) if !args.is_empty() => format!("{} {}", skill, args),
        _ => skill.to_string(),
    }
}

/// Summarize a `SendMessage` agent-to-agent call as `→ <to>: <preview>`, where
/// the preview is the compact `summary` field when present (the sender's own
/// 5-10 word gloss) and otherwise the raw `message`. The recipient leads because
/// the collapsed header truncates the summary, so the most identifying part —
/// who the message is for — must come first.
fn format_sendmessage_summary(input: &serde_json::Value) -> String {
    let to = input.get("to").and_then(|v| v.as_str()).unwrap_or("?");
    let preview = input
        .get("summary")
        .and_then(|v| v.as_str())
        .or_else(|| input.get("message").and_then(|v| v.as_str()))
        .unwrap_or("");
    if preview.is_empty() {
        format!("→ {to}")
    } else {
        format!("→ {to}: {preview}")
    }
}

/// Truncate output to a maximum size, keeping the end (most recent) content
pub fn truncate_output(output: &str, max_size: usize) -> String {
    if output.len() <= max_size {
        output.to_string()
    } else {
        // Must ceil to a valid UTF-8 char boundary to avoid panics
        let raw_start = output.len() - max_size;
        let start = notedeck::abbrev::ceil_char_boundary(output, raw_start);
        // Find a newline near the start to avoid cutting mid-line
        let adjusted_start = output[start..]
            .find('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(start);
        format!("...\n{}", &output[adjusted_start..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- Bash summaries show the full command, backtick-quoted, no count ----

    #[test]
    fn bash_summary_shows_full_command_untruncated() {
        // Long, multi-byte commands are shown in full: no truncation (which once
        // risked slicing mid-UTF-8-char) and no trailing output-length count.
        let cmd = "🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥 git add crates/agentium-core/src/messages.rs crates/notedeck_dave/src/ui/dave.rs";
        assert!(cmd.len() > 40);
        let input = json!({"command": cmd});
        let summary = format_bash_summary(&input);
        assert_eq!(summary, format!("`{cmd}`"));
        // The uninformative "(N chars)" output count is gone.
        assert!(!summary.contains("chars)"));
    }

    #[test]
    fn bash_summary_missing_command_is_empty_backticks() {
        assert_eq!(format_bash_summary(&json!({})), "``");
    }

    // ---- Bug fix: truncate_output panicked on multi-byte UTF-8 ----

    #[test]
    fn truncate_output_multibyte_without_panic() {
        // Create a string where the truncation point falls mid-emoji
        let output = "line1\n🔥🔥🔥🔥🔥end\n"; // "line1\n" = 6 bytes, 5 emojis = 20 bytes, "end\n" = 4 bytes = 30 total
        let max_size = 25; // start = 30 - 25 = 5, which is valid (before \n)
        let result = truncate_output(output, max_size);
        assert!(result.starts_with("...\n"));

        // Now test where truncation point hits mid-emoji
        let output2 = "ab🔥🔥🔥🔥🔥🔥🔥🔥end\n"; // "ab" = 2, 8 emojis = 32, "end\n" = 4 = 38 total
        let max_size2 = 35; // start = 38 - 35 = 3, byte 3 is inside first emoji (bytes 2-5)
        assert!(!output2.is_char_boundary(3));
        // This would panic before the fix
        let result2 = truncate_output(output2, max_size2);
        assert!(result2.starts_with("...\n"));
    }

    // ---- Agent mirrors Task: "description (subagent_type)" ----

    #[test]
    fn agent_summary_shows_description_and_subagent_type() {
        let input = json!({
            "description": "audit the login flow",
            "subagent_type": "Explore",
            "prompt": "look at everything",
        });
        assert_eq!(
            format_agent_summary(&input),
            "audit the login flow (Explore)"
        );
    }

    #[test]
    fn agent_summary_missing_fields_falls_back() {
        assert_eq!(format_agent_summary(&json!({})), "task (unknown)");
    }

    // ---- Skill leads with the skill name, optionally + args ----

    #[test]
    fn skill_summary_shows_name_and_args() {
        let input = json!({"skill": "headway", "args": "show dave/foo"});
        assert_eq!(format_skill_summary(&input), "headway show dave/foo");
    }

    #[test]
    fn skill_summary_without_args_is_just_the_name() {
        assert_eq!(
            format_skill_summary(&json!({"skill": "code-review"})),
            "code-review"
        );
        // An empty args string is treated the same as no args.
        assert_eq!(
            format_skill_summary(&json!({"skill": "code-review", "args": ""})),
            "code-review"
        );
    }

    #[test]
    fn skill_summary_missing_name_is_placeholder() {
        assert_eq!(format_skill_summary(&json!({})), "?");
    }

    // ---- SendMessage leads with the recipient, then a content preview ----

    #[test]
    fn sendmessage_summary_prefers_summary_field() {
        let input = json!({
            "to": "researcher",
            "summary": "assign task 1",
            "message": "start on task #1 and report back",
        });
        assert_eq!(
            format_sendmessage_summary(&input),
            "→ researcher: assign task 1"
        );
    }

    #[test]
    fn sendmessage_summary_falls_back_to_message() {
        let input = json!({"to": "main", "message": "done, all green"});
        assert_eq!(
            format_sendmessage_summary(&input),
            "→ main: done, all green"
        );
    }

    #[test]
    fn sendmessage_summary_missing_fields() {
        // No recipient and no content: just the arrow + placeholder target.
        assert_eq!(format_sendmessage_summary(&json!({})), "→ ?");
    }

    #[test]
    fn truncate_output_fits_returns_unchanged() {
        assert_eq!(truncate_output("hello", 10), "hello");
    }

    #[test]
    fn truncate_output_ascii_truncates_at_newline() {
        let output = "line1\nline2\nline3\n";
        // start = 18-12 = 6 = the '\n' after "line1"
        // find('\n') in "line2\nline3\n" finds at offset 5, so adjusted_start = 6+5+1 = 12
        let result = truncate_output(output, 12);
        assert_eq!(result, "...\nline3\n");
    }
}
