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
        "Bash" => format_bash_summary(input, response),
        "Grep" => format_grep_summary(input),
        "Glob" => format_glob_summary(input),
        "Edit" => format_edit_summary(input),
        "Task" => format_task_summary(input),
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

fn format_bash_summary(input: &serde_json::Value, response: &serde_json::Value) -> String {
    let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
    // Truncate long commands
    let cmd_display = if cmd.len() > 40 {
        format!("{}...", &cmd[..37])
    } else {
        cmd.to_string()
    };
    let output_len = extract_response_content(response)
        .as_ref()
        .map(|s| s.len())
        .unwrap_or(0);
    if output_len > 0 {
        format!("`{}` ({} chars)", cmd_display, output_len)
    } else {
        format!("`{}`", cmd_display)
    }
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

/// Truncate output to a maximum size, keeping the end (most recent) content
pub fn truncate_output(output: &str, max_size: usize) -> String {
    if output.len() <= max_size {
        output.to_string()
    } else {
        let start = output.len() - max_size;
        // Find a newline near the start to avoid cutting mid-line
        let adjusted_start = output[start..]
            .find('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(start);
        format!("...\n{}", &output[adjusted_start..])
    }
}
