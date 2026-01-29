use serde_json::Value;
use similar::{ChangeTag, TextDiff};

/// Represents a proposed file modification from an AI tool call
#[derive(Debug, Clone)]
pub struct FileUpdate {
    pub file_path: String,
    pub update_type: FileUpdateType,
}

#[derive(Debug, Clone)]
pub enum FileUpdateType {
    /// Edit: replace old_string with new_string
    Edit {
        old_string: String,
        new_string: String,
    },
    /// Write: create/overwrite entire file
    Write { content: String },
}

/// A single line in a diff
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub tag: DiffTag,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffTag {
    Equal,
    Delete,
    Insert,
}

impl From<ChangeTag> for DiffTag {
    fn from(tag: ChangeTag) -> Self {
        match tag {
            ChangeTag::Equal => DiffTag::Equal,
            ChangeTag::Delete => DiffTag::Delete,
            ChangeTag::Insert => DiffTag::Insert,
        }
    }
}

impl FileUpdate {
    /// Try to parse a FileUpdate from a tool name and tool input JSON
    pub fn from_tool_call(tool_name: &str, tool_input: &Value) -> Option<Self> {
        let obj = tool_input.as_object()?;

        match tool_name {
            "Edit" => {
                let file_path = obj.get("file_path")?.as_str()?.to_string();
                let old_string = obj.get("old_string")?.as_str()?.to_string();
                let new_string = obj.get("new_string")?.as_str()?.to_string();

                Some(FileUpdate {
                    file_path,
                    update_type: FileUpdateType::Edit {
                        old_string,
                        new_string,
                    },
                })
            }
            "Write" => {
                let file_path = obj.get("file_path")?.as_str()?.to_string();
                let content = obj.get("content")?.as_str()?.to_string();

                Some(FileUpdate {
                    file_path,
                    update_type: FileUpdateType::Write { content },
                })
            }
            _ => None,
        }
    }

    /// Returns true if this is an Edit that changes at most `max_lines` lines
    /// (deleted + inserted lines). Never returns true for Write operations.
    ///
    /// This counts actual changed lines using a diff, not total lines in the
    /// strings. This is important because Claude Code typically includes
    /// surrounding context lines for matching, so even a 1-line change may
    /// have multi-line old_string/new_string.
    pub fn is_small_edit(&self, max_lines: usize) -> bool {
        match &self.update_type {
            FileUpdateType::Edit {
                old_string,
                new_string,
            } => {
                let diff = TextDiff::from_lines(old_string.as_str(), new_string.as_str());
                let mut deleted_lines = 0;
                let mut inserted_lines = 0;
                for change in diff.iter_all_changes() {
                    match change.tag() {
                        ChangeTag::Delete => deleted_lines += 1,
                        ChangeTag::Insert => inserted_lines += 1,
                        ChangeTag::Equal => {}
                    }
                }
                deleted_lines <= max_lines && inserted_lines <= max_lines
            }
            FileUpdateType::Write { .. } => false,
        }
    }

    /// Compute the diff lines for this update
    pub fn compute_diff(&self) -> Vec<DiffLine> {
        match &self.update_type {
            FileUpdateType::Edit {
                old_string,
                new_string,
            } => {
                let diff = TextDiff::from_lines(old_string.as_str(), new_string.as_str());
                diff.iter_all_changes()
                    .map(|change| DiffLine {
                        tag: change.tag().into(),
                        content: change.value().to_string(),
                    })
                    .collect()
            }
            FileUpdateType::Write { content } => {
                // For writes, everything is an insertion
                content
                    .lines()
                    .map(|line| DiffLine {
                        tag: DiffTag::Insert,
                        content: format!("{}\n", line),
                    })
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_is_small_edit_single_line() {
        let update = FileUpdate {
            file_path: "test.rs".to_string(),
            update_type: FileUpdateType::Edit {
                old_string: "foo".to_string(),
                new_string: "bar".to_string(),
            },
        };
        assert!(
            update.is_small_edit(2),
            "Single line without newline should be small"
        );
    }

    #[test]
    fn test_is_small_edit_single_line_with_newline() {
        let update = FileUpdate {
            file_path: "test.rs".to_string(),
            update_type: FileUpdateType::Edit {
                old_string: "foo\n".to_string(),
                new_string: "bar\n".to_string(),
            },
        };
        assert!(
            update.is_small_edit(2),
            "Single line with trailing newline should be small"
        );
    }

    #[test]
    fn test_is_small_edit_two_lines() {
        let update = FileUpdate {
            file_path: "test.rs".to_string(),
            update_type: FileUpdateType::Edit {
                old_string: "foo\nbar".to_string(),
                new_string: "baz\nqux".to_string(),
            },
        };
        assert!(
            update.is_small_edit(2),
            "Two lines without trailing newline should be small"
        );
    }

    #[test]
    fn test_is_small_edit_two_lines_with_newline() {
        let update = FileUpdate {
            file_path: "test.rs".to_string(),
            update_type: FileUpdateType::Edit {
                old_string: "foo\nbar\n".to_string(),
                new_string: "baz\nqux\n".to_string(),
            },
        };
        assert!(
            update.is_small_edit(2),
            "Two lines with trailing newline should be small"
        );
    }

    #[test]
    fn test_is_small_edit_three_lines_not_small() {
        let update = FileUpdate {
            file_path: "test.rs".to_string(),
            update_type: FileUpdateType::Edit {
                old_string: "foo\nbar\nbaz".to_string(),
                new_string: "a\nb\nc".to_string(),
            },
        };
        assert!(!update.is_small_edit(2), "Three lines should NOT be small");
    }

    #[test]
    fn test_is_small_edit_write_never_small() {
        let update = FileUpdate {
            file_path: "test.rs".to_string(),
            update_type: FileUpdateType::Write {
                content: "x".to_string(),
            },
        };
        assert!(
            !update.is_small_edit(2),
            "Write operations should never be small"
        );
    }

    #[test]
    fn test_is_small_edit_old_small_new_large() {
        let update = FileUpdate {
            file_path: "test.rs".to_string(),
            update_type: FileUpdateType::Edit {
                old_string: "foo".to_string(),
                new_string: "a\nb\nc\nd".to_string(),
            },
        };
        assert!(
            !update.is_small_edit(2),
            "Large new_string should NOT be small"
        );
    }

    #[test]
    fn test_is_small_edit_old_large_new_small() {
        let update = FileUpdate {
            file_path: "test.rs".to_string(),
            update_type: FileUpdateType::Edit {
                old_string: "a\nb\nc\nd".to_string(),
                new_string: "foo".to_string(),
            },
        };
        assert!(
            !update.is_small_edit(2),
            "Large old_string should NOT be small"
        );
    }

    #[test]
    fn test_from_tool_call_edit() {
        let input = json!({
            "file_path": "/path/to/file.rs",
            "old_string": "old",
            "new_string": "new"
        });
        let update = FileUpdate::from_tool_call("Edit", &input).unwrap();
        assert_eq!(update.file_path, "/path/to/file.rs");
        assert!(update.is_small_edit(2));
    }

    #[test]
    fn test_from_tool_call_write() {
        let input = json!({
            "file_path": "/path/to/file.rs",
            "content": "content"
        });
        let update = FileUpdate::from_tool_call("Write", &input).unwrap();
        assert_eq!(update.file_path, "/path/to/file.rs");
        assert!(!update.is_small_edit(2));
    }

    #[test]
    fn test_from_tool_call_unknown_tool() {
        let input = json!({
            "file_path": "/path/to/file.rs"
        });
        assert!(FileUpdate::from_tool_call("Bash", &input).is_none());
    }

    #[test]
    fn test_is_small_edit_realistic_claude_edit_with_context() {
        // Claude Code typically sends old_string/new_string with context lines
        // for matching. Even a "small" 1-line change includes context.
        // This tests what an actual Edit tool call might look like.
        let input = json!({
            "file_path": "/path/to/file.rs",
            "old_string": "    fn foo() {\n        let x = 1;\n    }",
            "new_string": "    fn foo() {\n        let x = 2;\n    }"
        });
        let update = FileUpdate::from_tool_call("Edit", &input).unwrap();
        // Only 1 line actually changed (let x = 1 -> let x = 2)
        // The context lines (fn foo() and }) are the same
        assert!(
            update.is_small_edit(2),
            "1-line actual change should be small, even with 3 lines of context"
        );
    }

    #[test]
    fn test_is_small_edit_minimal_change() {
        // A truly minimal single-line change
        let input = json!({
            "file_path": "/path/to/file.rs",
            "old_string": "let x = 1;",
            "new_string": "let x = 2;"
        });
        let update = FileUpdate::from_tool_call("Edit", &input).unwrap();
        assert!(
            update.is_small_edit(2),
            "Single-line change should be small"
        );
    }

    #[test]
    fn test_line_count_behavior() {
        // Document how lines().count() behaves
        assert_eq!("foo".lines().count(), 1);
        assert_eq!("foo\n".lines().count(), 1); // trailing newline doesn't add line
        assert_eq!("foo\nbar".lines().count(), 2);
        assert_eq!("foo\nbar\n".lines().count(), 2);
        assert_eq!("foo\nbar\nbaz".lines().count(), 3);
        assert_eq!("foo\nbar\nbaz\n".lines().count(), 3);
        // Empty strings
        assert_eq!("".lines().count(), 0);
        assert_eq!("\n".lines().count(), 1); // just a newline counts as 1 empty line
    }
}
