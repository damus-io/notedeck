//! Auto-accept rules for tool permission requests.
//!
//! This module provides a configurable rules-based system for automatically
//! accepting certain tool calls without requiring user confirmation.

use crate::file_update::FileUpdate;
use serde_json::Value;

/// A rule for auto-accepting tool calls
#[derive(Debug, Clone)]
pub enum AutoAcceptRule {
    /// Auto-accept Edit tool calls that change at most N lines
    SmallEdit { max_lines: usize },
    /// Auto-accept Bash tool calls matching these command prefixes
    BashCommand { prefixes: Vec<String> },
    /// Auto-accept specific read-only tools unconditionally
    ReadOnlyTool { tools: Vec<String> },
}

impl AutoAcceptRule {
    /// Check if this rule matches the given tool call
    fn matches(&self, tool_name: &str, tool_input: &Value) -> bool {
        match self {
            AutoAcceptRule::SmallEdit { max_lines } => {
                if let Some(file_update) = FileUpdate::from_tool_call(tool_name, tool_input) {
                    file_update.is_small_edit(*max_lines)
                } else {
                    false
                }
            }
            AutoAcceptRule::BashCommand { prefixes } => {
                if tool_name != "Bash" {
                    return false;
                }
                let Some(command) = tool_input.get("command").and_then(|v| v.as_str()) else {
                    return false;
                };
                let command_trimmed = command.trim();
                prefixes.iter().any(|prefix| {
                    command_trimmed.starts_with(prefix)
                        && (command_trimmed.len() == prefix.len()
                            || command_trimmed.as_bytes()[prefix.len()].is_ascii_whitespace())
                })
            }
            AutoAcceptRule::ReadOnlyTool { tools } => tools.iter().any(|t| t == tool_name),
        }
    }
}

/// Collection of auto-accept rules
#[derive(Debug, Clone)]
pub struct AutoAcceptRules {
    rules: Vec<AutoAcceptRule>,
}

impl Default for AutoAcceptRules {
    fn default() -> Self {
        Self {
            rules: vec![
                AutoAcceptRule::SmallEdit { max_lines: 2 },
                AutoAcceptRule::BashCommand {
                    prefixes: vec![
                        // Cargo commands
                        "cargo build".into(),
                        "cargo check".into(),
                        "cargo test".into(),
                        "cargo fmt".into(),
                        "cargo clippy".into(),
                        "cargo run".into(),
                        "cargo doc".into(),
                        // Read-only bash commands
                        "grep".into(),
                        "rg".into(),
                        "find".into(),
                        "ls".into(),
                        "cat".into(),
                        "head".into(),
                        "tail".into(),
                        "wc".into(),
                        "file".into(),
                        "stat".into(),
                        "which".into(),
                        "type".into(),
                        "pwd".into(),
                        "tree".into(),
                        "du".into(),
                        "df".into(),
                        // Git read-only commands
                        "git status".into(),
                        "git log".into(),
                        "git diff".into(),
                        "git show".into(),
                        "git branch".into(),
                        "git remote".into(),
                        "git rev-parse".into(),
                        "git ls-files".into(),
                        "git describe".into(),
                        // GitHub CLI (read-only)
                        "gh pr view".into(),
                        "gh pr list".into(),
                        "gh pr diff".into(),
                        "gh pr checks".into(),
                        "gh pr status".into(),
                        "gh issue view".into(),
                        "gh issue list".into(),
                        "gh issue status".into(),
                        "gh repo view".into(),
                        "gh search".into(),
                        "gh release list".into(),
                        "gh release view".into(),
                        // Beads issue tracker
                        "bd".into(),
                        "beads list".into(),
                    ],
                },
                AutoAcceptRule::ReadOnlyTool {
                    tools: vec![
                        "Glob".into(),
                        "Grep".into(),
                        "Read".into(),
                        "WebSearch".into(),
                        "WebFetch".into(),
                    ],
                },
            ],
        }
    }
}

impl AutoAcceptRules {
    /// Check if any rule matches the given tool call
    pub fn should_auto_accept(&self, tool_name: &str, tool_input: &Value) -> bool {
        self.rules
            .iter()
            .any(|rule| rule.matches(tool_name, tool_input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn default_rules() -> AutoAcceptRules {
        AutoAcceptRules::default()
    }

    #[test]
    fn test_small_edit_auto_accept() {
        let rules = default_rules();
        let input = json!({
            "file_path": "/path/to/file.rs",
            "old_string": "let x = 1;",
            "new_string": "let x = 2;"
        });
        assert!(rules.should_auto_accept("Edit", &input));
    }

    #[test]
    fn test_large_edit_not_auto_accept() {
        let rules = default_rules();
        let input = json!({
            "file_path": "/path/to/file.rs",
            "old_string": "line1\nline2\nline3\nline4",
            "new_string": "a\nb\nc\nd"
        });
        assert!(!rules.should_auto_accept("Edit", &input));
    }

    #[test]
    fn test_cargo_build_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "cargo build" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_cargo_check_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "cargo check" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_cargo_test_with_args_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "cargo test --release" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_cargo_fmt_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "cargo fmt" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_cargo_clippy_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "cargo clippy" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_rm_not_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "rm -rf /tmp/test" });
        assert!(!rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_curl_not_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "curl https://example.com" });
        assert!(!rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_read_auto_accept() {
        let rules = default_rules();
        let input = json!({ "file_path": "/path/to/file.rs" });
        assert!(rules.should_auto_accept("Read", &input));
    }

    #[test]
    fn test_glob_auto_accept() {
        let rules = default_rules();
        let input = json!({ "pattern": "**/*.rs" });
        assert!(rules.should_auto_accept("Glob", &input));
    }

    #[test]
    fn test_grep_auto_accept() {
        let rules = default_rules();
        let input = json!({ "pattern": "TODO", "path": "/src" });
        assert!(rules.should_auto_accept("Grep", &input));
    }

    #[test]
    fn test_write_not_auto_accept() {
        let rules = default_rules();
        let input = json!({
            "file_path": "/path/to/file.rs",
            "content": "new content"
        });
        assert!(!rules.should_auto_accept("Write", &input));
    }

    #[test]
    fn test_unknown_tool_not_auto_accept() {
        let rules = default_rules();
        let input = json!({});
        assert!(!rules.should_auto_accept("UnknownTool", &input));
    }

    #[test]
    fn test_bash_with_leading_whitespace() {
        let rules = default_rules();
        let input = json!({ "command": "  cargo build" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_grep_bash_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "grep -rn \"pattern\" /path" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_rg_bash_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "rg \"pattern\" /path" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_find_bash_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "find . -name \"*.rs\"" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_git_status_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "git status" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_git_log_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "git log --oneline -10" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_git_push_not_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "git push origin main" });
        assert!(!rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_git_commit_not_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "git commit -m \"test\"" });
        assert!(!rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_ls_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "ls -la /tmp" });
        assert!(rules.should_auto_accept("Bash", &input));
    }

    #[test]
    fn test_cat_auto_accept() {
        let rules = default_rules();
        let input = json!({ "command": "cat /path/to/file.txt" });
        assert!(rules.should_auto_accept("Bash", &input));
    }
}
