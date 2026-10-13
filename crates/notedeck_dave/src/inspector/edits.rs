//! The Files card's state, part two: every file this session edited or
//! wrote, with its added and removed line counts.
//!
//! The source is the [`FileUpdate`](crate::file_update::FileUpdate) each
//! Edit/Write result carries, in the chat itself and in each subagent's tool
//! list. A restored or remote session's chat carries them too (the update
//! rides the `tool_result` note as `FileUpdateWire`), and a remote session's
//! chat grows by note fold rather than by backend response, so the list is
//! folded from the chat, not from the stream: one path covers local, restored
//! and remote sessions alike.
//!
//! The fold runs only when [`EditsKey`] moves, which counting the edits in
//! the chat finds without allocating.

use super::LabelMemo;
use crate::file_update::{DiffTag, FileUpdate, FileUpdateType};
use crate::tools::ToolResponses;
use crate::Message;
use std::collections::HashMap;

/// One file the session touched, summed over every edit to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditedFile {
    /// The path as the tool was given it (absolute, for Claude Code).
    pub path: String,
    /// How many Edit/Write results touched it.
    pub edits: usize,
    /// Lines added, over all its edits.
    pub added: usize,
    /// Lines removed, over all its edits.
    pub removed: usize,
    /// Some edit was a whole-file Write.
    pub written: bool,
    /// The chat row of its last edit: the tool row itself, or the row of the
    /// subagent whose tool list holds it.
    pub last_row: usize,
}

/// Every file `chat` edited, most recently edited first.
pub fn edited_files(chat: &[Message]) -> Vec<EditedFile> {
    let mut by_path: HashMap<&str, EditedFile> = HashMap::new();
    for edit in chat_edits(chat) {
        let lines = line_counts(edit.update);
        let file = by_path
            .entry(edit.update.file_path.as_str())
            .or_insert_with(|| EditedFile {
                path: edit.update.file_path.clone(),
                edits: 0,
                added: 0,
                removed: 0,
                written: false,
                last_row: 0,
            });
        file.edits += 1;
        file.added += lines.added;
        file.removed += lines.removed;
        file.written |= matches!(edit.update.update_type, FileUpdateType::Write { .. });
        file.last_row = edit.row;
    }
    let mut files: Vec<EditedFile> = by_path.into_values().collect();
    files.sort_by(|a, b| b.last_row.cmp(&a.last_row).then(a.path.cmp(&b.path)));
    files
}

/// One file update in the chat, and the row it sits in.
struct ChatEdit<'a> {
    row: usize,
    update: &'a FileUpdate,
}

/// Every file update in `chat`, in chat order: each foreground tool row's,
/// and each subagent's tool results' under the subagent's row.
fn chat_edits(chat: &[Message]) -> impl Iterator<Item = ChatEdit<'_>> {
    chat.iter().enumerate().flat_map(|(row, message)| {
        let foreground = match message {
            Message::ToolResponse(response) => match response.responses() {
                ToolResponses::ExecutedTool(result) => result.file_update.as_ref(),
                _ => None,
            },
            _ => None,
        };
        let subagent = match message {
            Message::Subagent(info) => info.tool_results.as_slice(),
            _ => &[],
        };
        foreground
            .into_iter()
            .chain(subagent.iter().filter_map(|r| r.file_update.as_ref()))
            .map(move |update| ChatEdit { row, update })
    })
}

/// Added and removed lines of one update.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LineCounts {
    pub added: usize,
    pub removed: usize,
}

/// The update's diff, counted: inserted lines are added, deleted removed.
/// A Write counts every line it wrote as added, since the diff it carries
/// is against nothing.
pub fn line_counts(update: &FileUpdate) -> LineCounts {
    let mut counts = LineCounts::default();
    for line in update.diff_lines() {
        match line.tag {
            DiffTag::Insert => counts.added += 1,
            DiffTag::Delete => counts.removed += 1,
            DiffTag::Equal => {}
        }
    }
    counts
}

/// What the edited-files rows were built from: the chat's length, so a row
/// index stays right, and how many edits it holds. Both only grow, and an
/// in-place upgrade of a running row to its result moves the edit count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditsKey {
    pub rows: usize,
    pub edits: usize,
}

impl EditsKey {
    /// `chat`'s key, counted without allocating.
    pub fn of(chat: &[Message]) -> Self {
        Self {
            rows: chat.len(),
            edits: chat_edits(chat).count(),
        }
    }
}

/// The edited-files group's display strings, rebuilt when [`EditsKey`]
/// moves.
pub type EditedMemo = LabelMemo<EditsKey, EditedRows>;

/// The edited-files group's display strings.
#[derive(Debug, Default)]
pub struct EditedRows {
    /// "edited · 3 · +120 −30"
    pub heading: String,
    /// One per file, most recently edited first.
    pub rows: Vec<EditedRow>,
    /// What the group shows before the session edits anything.
    pub empty: String,
}

/// One edited file's display strings.
#[derive(Debug, Default)]
pub struct EditedRow {
    /// The path, relative to the session's cwd when it is under it.
    pub name: String,
    /// "+12", or empty for none.
    pub added: String,
    /// "−3", or empty for none.
    pub removed: String,
    /// "2 edits", or "new" for a file the session wrote whole.
    pub detail: String,
    /// The chat row of its last edit, for the jump back to it.
    pub last_row: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{ExecutedTool, SubagentInfo, SubagentStatus};
    use crate::tools::ToolResponse;

    fn edit(path: &str, old: &str, new: &str) -> FileUpdate {
        FileUpdate::new(
            path.into(),
            FileUpdateType::Edit {
                old_string: old.into(),
                new_string: new.into(),
            },
        )
    }

    fn write(path: &str, content: &str) -> FileUpdate {
        FileUpdate::new(
            path.into(),
            FileUpdateType::Write {
                content: content.into(),
            },
        )
    }

    fn result(update: Option<FileUpdate>) -> ExecutedTool {
        ExecutedTool {
            tool_name: "Edit".into(),
            summary: String::new(),
            output: None,
            parent_task_id: None,
            file_update: update,
            tool_use_id: None,
            description: None,
        }
    }

    fn tool_row(update: Option<FileUpdate>) -> Message {
        Message::ToolResponse(ToolResponse::executed_tool(result(update)))
    }

    fn subagent_row(updates: Vec<FileUpdate>) -> Message {
        Message::Subagent(SubagentInfo {
            task_id: "task-1".into(),
            description: "refactor".into(),
            subagent_type: "general-purpose".into(),
            status: SubagentStatus::Completed,
            output: String::new(),
            max_output_size: 0,
            tool_results: updates.into_iter().map(|u| result(Some(u))).collect(),
            background: false,
            agent_id: None,
            transcript: Default::default(),
        })
    }

    /// Two edits to one file and a write to another add up per file; a
    /// subagent's edit counts, at its subagent's row; a tool row without an
    /// update counts nothing.
    #[test]
    fn edits_sum_per_file_and_include_subagents() {
        let chat = vec![
            Message::System("".into()),
            // a → b c: one removed, two added.
            tool_row(Some(edit("/w/src/lib.rs", "a\n", "b\nc\n"))),
            tool_row(None),
            tool_row(Some(write("/w/notes.md", "one\ntwo\nthree\n"))),
            // Same lines kept, one removed.
            tool_row(Some(edit("/w/src/lib.rs", "b\nc\n", "b\n"))),
            subagent_row(vec![edit("/w/src/main.rs", "x\n", "y\n")]),
        ];

        let files = edited_files(&chat);
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["/w/src/main.rs", "/w/src/lib.rs", "/w/notes.md"]);

        let lib = &files[1];
        assert_eq!((lib.edits, lib.added, lib.removed), (2, 2, 2));
        assert_eq!(lib.last_row, 4);
        assert!(!lib.written);

        let notes = &files[2];
        assert_eq!((notes.edits, notes.added, notes.removed), (1, 3, 0));
        assert!(notes.written);

        let main = &files[0];
        assert_eq!((main.edits, main.added, main.removed), (1, 1, 1));
        assert_eq!(main.last_row, 5);

        assert_eq!(EditsKey::of(&chat), EditsKey { rows: 6, edits: 4 });
    }

    /// A subagent's later edit moves the key without the chat growing.
    #[test]
    fn a_subagent_edit_moves_the_key() {
        let mut chat = vec![subagent_row(vec![])];
        let before = EditsKey::of(&chat);
        if let Message::Subagent(info) = &mut chat[0] {
            info.tool_results
                .push(result(Some(edit("/w/a.rs", "1\n", "2\n"))));
        }
        assert_ne!(EditsKey::of(&chat), before);
    }
}
