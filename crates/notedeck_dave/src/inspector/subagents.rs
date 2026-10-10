//! The Subagents card's state: each subagent's latest tokens, tool uses and
//! last tool from the CLI's `task_progress`, and the display rows built from
//! them with the chat's subagent rows.
//!
//! The chat's [`Message::Subagent`] rows stay the source of which subagents
//! exist and how they ended, since a restored or remote session has those
//! and nothing else. Progress only adds numbers to them.

use super::LabelMemo;
use crate::messages::{SubagentProgress, SubagentStatus};
use crate::Message;
use notedeck::Localization;
use std::collections::HashMap;

/// The latest progress per subagent, by task id.
pub type ProgressById = HashMap<String, SubagentProgress>;

/// The Subagents card's state. See the module docs.
#[derive(Debug, Default)]
pub struct SubagentState {
    /// The latest progress per subagent, by its task id (the launching
    /// tool_use id).
    progress: ProgressById,
    /// Bumped on each progress report, so rows built from it know to
    /// rebuild.
    generation: u64,
    /// The card's display strings, rebuilt when a subagent starts or ends or
    /// reports progress.
    rows: LabelMemo<SubagentsKey, SubagentRows>,
}

impl SubagentState {
    /// Fold a progress report: its usage replaces the last, and a report
    /// without a last tool keeps the one before.
    pub fn apply(&mut self, report: &SubagentProgress) {
        self.generation += 1;
        let entry = self
            .progress
            .entry(report.task_id.clone())
            .or_insert_with(|| SubagentProgress {
                task_id: report.task_id.clone(),
                ..Default::default()
            });
        entry.usage = report.usage;
        if report.last_tool_name.is_some() {
            entry.last_tool_name.clone_from(&report.last_tool_name);
        }
    }

    /// The card's rows for `chat`, built with `build` from the chat and the
    /// progress reports only when [`Self::key`] moved since the last call.
    pub fn rows(
        &mut self,
        chat: &[Message],
        i18n: &mut Localization,
        build: impl FnOnce(&[Message], &ProgressById, &mut Localization) -> SubagentRows,
    ) -> &SubagentRows {
        let key = self.key(chat);
        let progress = &self.progress;
        self.rows
            .get(key, i18n, |_, i18n| build(chat, progress, i18n))
    }

    /// The latest progress `task_id` reported, if any.
    pub fn progress(&self, task_id: &str) -> Option<&SubagentProgress> {
        self.progress.get(task_id)
    }

    /// What the rows were built from: how many subagent rows `chat` has,
    /// how many have ended, and the progress generation. Counts, so the
    /// check allocates nothing; a row only ever moves from running to ended.
    pub fn key(&self, chat: &[Message]) -> SubagentsKey {
        let mut key = SubagentsKey {
            rows: 0,
            ended: 0,
            progress: self.generation,
        };
        for info in chat.iter().filter_map(subagent) {
            key.rows += 1;
            if info.status != SubagentStatus::Running {
                key.ended += 1;
            }
        }
        key
    }
}

/// The chat row's subagent, if it is one.
pub fn subagent(message: &Message) -> Option<&crate::messages::SubagentInfo> {
    match message {
        Message::Subagent(info) => Some(info),
        _ => None,
    }
}

/// What a [`SubagentRows`] was built from; see [`SubagentState::key`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubagentsKey {
    pub rows: usize,
    pub ended: usize,
    pub progress: u64,
}

/// The Subagents card's display strings.
#[derive(Debug, Default)]
pub struct SubagentRows {
    /// "3 subagents · 1 running · 1 failed · 182K tokens".
    pub summary: String,
    /// One per subagent, newest first.
    pub rows: Vec<SubagentRow>,
}

/// One subagent's display strings.
#[derive(Debug)]
pub struct SubagentRow {
    /// The chat row's task id, for the jump back to it.
    pub task_id: String,
    /// The subagent type: "Explore", "general-purpose", …
    pub badge: String,
    /// What it was asked to do.
    pub description: String,
    pub status: SubagentStatus,
    /// "running", "in background", "done", "failed".
    pub status_label: String,
    /// "7 tool uses · 12K tokens · 1m 1s · last Grep", or empty before any
    /// progress.
    pub meta: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{SubagentInfo, TaskUsage};

    fn row(task_id: &str, status: SubagentStatus) -> Message {
        Message::Subagent(SubagentInfo {
            task_id: task_id.into(),
            description: String::new(),
            subagent_type: "Explore".into(),
            status,
            output: String::new(),
            max_output_size: 0,
            tool_results: Vec::new(),
            background: false,
        })
    }

    fn report(task_id: &str, tokens: u64, last: Option<&str>) -> SubagentProgress {
        SubagentProgress {
            task_id: task_id.into(),
            usage: TaskUsage {
                total_tokens: tokens,
                tool_uses: 1,
                duration_ms: 0,
            },
            last_tool_name: last.map(str::to_string),
        }
    }

    /// A report's usage replaces the last; a report without a last tool
    /// keeps the one before.
    #[test]
    fn progress_replaces_usage_and_keeps_the_last_tool() {
        let mut state = SubagentState::default();
        state.apply(&report("t1", 100, Some("Grep")));
        state.apply(&report("t1", 250, None));
        let progress = state.progress("t1").unwrap();
        assert_eq!(progress.usage.total_tokens, 250);
        assert_eq!(progress.last_tool_name.as_deref(), Some("Grep"));
        assert!(state.progress("t2").is_none());
    }

    /// The key moves when a subagent starts, ends, or reports progress, and
    /// holds when nothing it shows changed.
    #[test]
    fn the_key_moves_on_spawn_end_and_progress() {
        let mut state = SubagentState::default();
        let mut chat = vec![Message::System("prompt".into())];
        let empty = state.key(&chat);

        chat.push(row("t1", SubagentStatus::Running));
        let spawned = state.key(&chat);
        assert_ne!(spawned, empty);
        assert_eq!(state.key(&chat), spawned);

        chat[1] = row("t1", SubagentStatus::Failed);
        let ended = state.key(&chat);
        assert_ne!(ended, spawned);

        state.apply(&report("t1", 1, None));
        assert_ne!(state.key(&chat), ended);
    }
}
