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
    /// on both the old and new side. Never returns true for Write operations.
    pub fn is_small_edit(&self, max_lines: usize) -> bool {
        match &self.update_type {
            FileUpdateType::Edit {
                old_string,
                new_string,
            } => {
                let old_lines = old_string.lines().count();
                let new_lines = new_string.lines().count();
                old_lines <= max_lines && new_lines <= max_lines
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
