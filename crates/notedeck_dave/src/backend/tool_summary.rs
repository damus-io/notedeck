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
    // Truncate long commands (must respect UTF-8 char boundaries)
    let cmd_display = if cmd.len() > 40 {
        let cut = notedeck::abbrev::floor_char_boundary(cmd, 37);
        format!("{}...", &cmd[..cut])
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

    // ---- Bug fix: format_bash_summary panicked on multi-byte UTF-8 at byte 37 ----

    #[test]
    fn bash_summary_truncates_multibyte_command_without_panic() {
        // "🔥" is 4 bytes. Place emojis so byte 37 falls mid-character.
        // 9 emojis = 36 bytes, then "ab" = 38 bytes total, then more chars to exceed 40.
        let cmd = "🔥🔥🔥🔥🔥🔥🔥🔥🔥abcdefgh"; // 36 + 8 = 44 bytes
        assert!(cmd.len() > 40);
        // Before the fix, &cmd[..37] would panic because byte 37 is inside "a"... wait
        // Actually 9 emojis = 36 bytes, byte 37 is 'a', which is fine.
        // Better test: 9 emojis + "a" = 37 bytes, byte 37 is start of next char.
        // Let's use a string where byte 37 is mid-emoji:
        // 8 emojis = 32 bytes, "12345" = 5 bytes = 37 bytes, then emoji at byte 37
        // That's fine too. We need byte 37 to be inside a multi-byte char.
        // 9 emojis = 36 bytes, then a 4-byte emoji starts at 36.
        // byte 37 is the 2nd byte of that emoji - NOT a char boundary.
        let cmd2 = "🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥padding"; // 40 + 7 = 47 bytes
        assert!(cmd2.len() > 40);
        // byte 37 is inside the 10th emoji (bytes 36-39)
        assert!(!cmd2.is_char_boundary(37));

        let input = json!({"command": cmd2});
        let response = json!(null);
        // This would panic before the fix
        let summary = format_bash_summary(&input, &response);
        assert!(summary.contains("..."));
        assert!(summary.starts_with('`'));
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
