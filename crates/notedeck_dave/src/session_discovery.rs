//! Discovers resumable Claude Code sessions from the filesystem.
//!
//! Claude Code stores session data in ~/.claude/projects/<project-path>/
//! where <project-path> is the cwd with slashes replaced by dashes and leading slash removed.

use serde::Deserialize;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Information about a resumable Claude session
#[derive(Debug, Clone)]
pub struct ResumableSession {
    /// The UUID session identifier used by Claude CLI
    pub session_id: String,
    /// Path to the session JSONL file
    pub file_path: PathBuf,
    /// Timestamp of the most recent message
    pub last_timestamp: chrono::DateTime<chrono::Utc>,
    /// Summary/title derived from first user message
    pub summary: String,
    /// Number of messages in the session
    pub message_count: usize,
}

/// A message entry from the JSONL file
#[derive(Deserialize)]
struct SessionEntry {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "type")]
    entry_type: Option<String>,
    message: Option<MessageContent>,
}

#[derive(Deserialize)]
struct MessageContent {
    role: Option<String>,
    content: Option<serde_json::Value>,
}

/// Converts a working directory to its Claude project path
/// e.g., /home/jb55/dev/notedeck-dave -> -home-jb55-dev-notedeck-dave
fn cwd_to_project_path(cwd: &Path) -> String {
    let path_str = cwd.to_string_lossy();
    // Replace path separators with dashes, keep the leading dash
    path_str.replace('/', "-")
}

/// Get the Claude projects directory
fn get_claude_projects_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude").join("projects"))
}

/// Extract the first user message content as a summary
fn extract_first_user_message(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(s) => {
            // Clean up the message - remove "Human: " prefix if present
            let cleaned = s.trim().strip_prefix("Human:").unwrap_or(s).trim();
            // Take first 60 chars
            let summary: String = cleaned.chars().take(60).collect();
            if cleaned.len() > 60 {
                Some(format!("{}...", summary))
            } else {
                Some(summary.to_string())
            }
        }
        serde_json::Value::Array(arr) => {
            // Content might be an array of content blocks
            for item in arr {
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    let summary: String = text.chars().take(60).collect();
                    if text.len() > 60 {
                        return Some(format!("{}...", summary));
                    } else {
                        return Some(summary.to_string());
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Parse a session JSONL file to extract session info
fn parse_session_file(path: &Path) -> Option<ResumableSession> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut session_id: Option<String> = None;
    let mut last_timestamp: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut first_user_message: Option<String> = None;
    let mut message_count = 0;

    for line in reader.lines() {
        let line = line.ok()?;
        if line.trim().is_empty() {
            continue;
        }

        if let Ok(entry) = serde_json::from_str::<SessionEntry>(&line) {
            // Get session ID from first entry that has it
            if session_id.is_none() {
                session_id = entry.session_id.clone();
            }

            // Track timestamp
            if let Some(ts_str) = &entry.timestamp {
                if let Ok(ts) = ts_str.parse::<chrono::DateTime<chrono::Utc>>() {
                    if last_timestamp.is_none() || ts > last_timestamp.unwrap() {
                        last_timestamp = Some(ts);
                    }
                }
            }

            // Count user/assistant messages
            if matches!(
                entry.entry_type.as_deref(),
                Some("user") | Some("assistant")
            ) {
                message_count += 1;

                // Get first user message for summary
                if entry.entry_type.as_deref() == Some("user") && first_user_message.is_none() {
                    if let Some(msg) = &entry.message {
                        if msg.role.as_deref() == Some("user") {
                            if let Some(content) = &msg.content {
                                first_user_message = extract_first_user_message(content);
                            }
                        }
                    }
                }
            }
        }
    }

    // Need at least a session_id and some messages
    let session_id = session_id?;
    if message_count == 0 {
        return None;
    }

    Some(ResumableSession {
        session_id,
        file_path: path.to_path_buf(),
        last_timestamp: last_timestamp.unwrap_or_else(chrono::Utc::now),
        summary: first_user_message.unwrap_or_else(|| "(no summary)".to_string()),
        message_count,
    })
}

/// Discover all resumable sessions for a given working directory
pub fn discover_sessions(cwd: &Path) -> Vec<ResumableSession> {
    let projects_dir = match get_claude_projects_dir() {
        Some(dir) => dir,
        None => return Vec::new(),
    };

    let project_path = cwd_to_project_path(cwd);
    let session_dir = projects_dir.join(&project_path);

    if !session_dir.exists() || !session_dir.is_dir() {
        return Vec::new();
    }

    let mut sessions = Vec::new();

    // Read all .jsonl files in the session directory
    if let Ok(entries) = fs::read_dir(&session_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                if let Some(session) = parse_session_file(&path) {
                    sessions.push(session);
                }
            }
        }
    }

    // Sort by most recent first
    sessions.sort_by(|a, b| b.last_timestamp.cmp(&a.last_timestamp));

    sessions
}

/// Format a timestamp for display (relative time like "2 hours ago")
pub fn format_relative_time(timestamp: &chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(*timestamp);

    if duration.num_seconds() < 60 {
        "just now".to_string()
    } else if duration.num_minutes() < 60 {
        let mins = duration.num_minutes();
        format!("{} min{} ago", mins, if mins == 1 { "" } else { "s" })
    } else if duration.num_hours() < 24 {
        let hours = duration.num_hours();
        format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" })
    } else if duration.num_days() < 7 {
        let days = duration.num_days();
        format!("{} day{} ago", days, if days == 1 { "" } else { "s" })
    } else {
        timestamp.format("%Y-%m-%d").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cwd_to_project_path() {
        assert_eq!(
            cwd_to_project_path(Path::new("/home/jb55/dev/notedeck-dave")),
            "-home-jb55-dev-notedeck-dave"
        );
        assert_eq!(cwd_to_project_path(Path::new("/tmp/test")), "-tmp-test");
    }

    #[test]
    fn test_extract_first_user_message_string() {
        let content = serde_json::json!("Human: Hello, world!\n\n");
        let result = extract_first_user_message(&content);
        assert_eq!(result, Some("Hello, world!".to_string()));
    }

    #[test]
    fn test_extract_first_user_message_array() {
        let content = serde_json::json!([{"type": "text", "text": "Test message"}]);
        let result = extract_first_user_message(&content);
        assert_eq!(result, Some("Test message".to_string()));
    }

    #[test]
    fn test_extract_first_user_message_truncation() {
        let long_content = serde_json::json!("Human: This is a very long message that should be truncated because it exceeds sixty characters in length");
        let result = extract_first_user_message(&long_content);
        assert!(result.unwrap().ends_with("..."));
    }
}
