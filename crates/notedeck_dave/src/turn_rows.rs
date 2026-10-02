//! Chat positions of the rows a live turn keeps writing to after inserting
//! them.
//!
//! A turn inserts a row and then comes back to it:
//! - the assistant segment still receiving tokens is extended and later
//!   closed,
//! - an in-flight tool row is upgraded in place when its result lands, or
//!   finalized at turn end,
//! - a subagent row takes output, a completion and the tool results its
//!   subagent ran. A background subagent outlives its turn.
//!
//! [`TurnRows`] holds all three positions. It is also the only way rows are
//! inserted into the middle of a chat ([`insert`](TurnRows::insert)), so it
//! can move every recorded position an insert shifts. A replaced chat is
//! re-indexed in one call ([`reindex`](TurnRows::reindex)), so a rebuild or a
//! restore can't leave one of the three pointing into the old chat.

use std::collections::HashMap;

use crate::Message;

/// Chat positions of the open assistant segment, the in-flight tool rows and
/// the subagent rows of one session.
///
/// Every position is kept correct by the two calls that change row
/// positions: [`insert`](Self::insert) and [`reindex`](Self::reindex). Rows
/// appended to the end of the chat shift nothing.
#[derive(Debug, Default)]
pub struct TurnRows {
    /// The assistant segment still receiving tokens, if any.
    open_assistant: Option<usize>,
    /// In-flight `Message::ToolRunning` rows, keyed by the `tool_use` id.
    running_tools: HashMap<String, usize>,
    /// `Message::Subagent` rows, keyed by task id.
    subagents: HashMap<String, usize>,
}

impl TurnRows {
    /// Insert `message` into `chat` at `pos` and return `pos`.
    ///
    /// Every recorded row at or after `pos` moves down by one, so it keeps
    /// pointing at its row. A streaming assistant, a running tool or a
    /// subagent row is then recorded at `pos`.
    pub fn insert(&mut self, chat: &mut Vec<Message>, pos: usize, message: Message) -> usize {
        chat.insert(pos, message);
        self.shift_from(pos);
        self.track(pos, &chat[pos]);
        pos
    }

    /// Forget every position and record the subagent rows of `chat`, a chat
    /// that just replaced the one these positions pointed into.
    ///
    /// Only subagent rows are recorded: a background subagent keeps running
    /// after its turn ends, and its completion finds its row here. A chat is
    /// replaced only while nothing streams into it (a local session's
    /// reconcile at rest, a remote session's rebuild, a restore), so no
    /// assistant segment is open. A `ToolRunning` row in a replaced chat
    /// belongs to a turn this host is no longer streaming, so no result will
    /// upgrade it.
    pub fn reindex(&mut self, chat: &[Message]) {
        self.open_assistant = None;
        self.running_tools.clear();
        self.subagents.clear();
        for (idx, message) in chat.iter().enumerate() {
            if let Message::Subagent(info) = message {
                self.subagents.insert(info.task_id.clone(), idx);
            }
        }
    }

    /// Take the open assistant segment's position, ending the segment.
    pub fn take_open_assistant(&mut self) -> Option<usize> {
        self.open_assistant.take()
    }

    /// Whether an assistant segment is still open.
    pub fn has_open_assistant(&self) -> bool {
        self.open_assistant.is_some()
    }

    /// Position of the in-flight tool row for `tool_use_id`.
    pub fn running_tool(&self, tool_use_id: &str) -> Option<usize> {
        self.running_tools.get(tool_use_id).copied()
    }

    /// Stop tracking the in-flight tool row for `tool_use_id` and return its
    /// position.
    pub fn take_running_tool(&mut self, tool_use_id: &str) -> Option<usize> {
        self.running_tools.remove(tool_use_id)
    }

    /// Whether any tool row is still in flight.
    pub fn has_running_tools(&self) -> bool {
        !self.running_tools.is_empty()
    }

    /// Stop tracking every in-flight tool row and return their positions in
    /// chat order.
    ///
    /// Allocates; it runs at a turn boundary, not per frame.
    pub fn drain_running_tools(&mut self) -> Vec<usize> {
        let mut rows: Vec<usize> = self.running_tools.drain().map(|(_, idx)| idx).collect();
        rows.sort_unstable();
        rows
    }

    /// Position of the subagent row for `task_id`.
    pub fn subagent(&self, task_id: &str) -> Option<usize> {
        self.subagents.get(task_id).copied()
    }

    /// Positions of every subagent row, in no particular order.
    pub fn subagent_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.subagents.values().copied()
    }

    /// Move every recorded position at or after `pos` down by one, for a row
    /// just inserted at `pos`.
    fn shift_from(&mut self, pos: usize) {
        let positions = self
            .open_assistant
            .iter_mut()
            .chain(self.running_tools.values_mut())
            .chain(self.subagents.values_mut());
        for idx in positions {
            if *idx >= pos {
                *idx += 1;
            }
        }
    }

    /// Record the row at `idx` if it is one a turn comes back to.
    fn track(&mut self, idx: usize, message: &Message) {
        match message {
            Message::Assistant(msg) if msg.is_streaming() => self.open_assistant = Some(idx),
            Message::ToolRunning(running) => {
                self.running_tools.insert(running.tool_use_id.clone(), idx);
            }
            Message::Subagent(info) => {
                self.subagents.insert(info.task_id.clone(), idx);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{AssistantMessage, RunningTool, SubagentInfo, SubagentStatus};

    fn streaming_assistant(text: &str) -> Message {
        let mut msg = AssistantMessage::new();
        msg.push_token(text);
        Message::Assistant(msg)
    }

    fn running(id: &str) -> Message {
        Message::ToolRunning(RunningTool {
            tool_use_id: id.to_string(),
            tool_name: "Bash".to_string(),
            summary: "ls".to_string(),
        })
    }

    fn subagent(task_id: &str) -> Message {
        Message::Subagent(SubagentInfo {
            task_id: task_id.to_string(),
            description: "explore".to_string(),
            subagent_type: "Explore".to_string(),
            status: SubagentStatus::Running,
            output: String::new(),
            max_output_size: 1000,
            tool_results: vec![],
            background: false,
        })
    }

    fn is_running(chat: &[Message], idx: Option<usize>, id: &str) -> bool {
        matches!(idx.and_then(|i| chat.get(i)), Some(Message::ToolRunning(r)) if r.tool_use_id == id)
    }

    fn is_subagent(chat: &[Message], idx: Option<usize>, task_id: &str) -> bool {
        matches!(idx.and_then(|i| chat.get(i)), Some(Message::Subagent(s)) if s.task_id == task_id)
    }

    /// A turn's rows are recorded where they land, ahead of a queued user
    /// message that stays trailing.
    #[test]
    fn insert_records_each_tracked_row() {
        let mut chat = vec![Message::User("go".into()), Message::User("queued".into())];
        let mut rows = TurnRows::default();

        let a = rows.insert(&mut chat, 1, streaming_assistant("thinking"));
        let t = rows.insert(&mut chat, 2, running("t1"));
        let s = rows.insert(&mut chat, 3, subagent("s1"));

        assert_eq!((a, t, s), (1, 2, 3));
        assert!(
            matches!(chat.last(), Some(Message::User(_))),
            "queued stays last"
        );
        assert_eq!(rows.take_open_assistant(), Some(1));
        assert!(is_running(&chat, rows.running_tool("t1"), "t1"));
        assert!(is_subagent(&chat, rows.subagent("s1"), "s1"));
    }

    /// An insert at or before a recorded row moves that row's position with it.
    #[test]
    fn insert_shifts_recorded_rows_at_or_after_it() {
        let mut chat = vec![Message::User("go".into())];
        let mut rows = TurnRows::default();
        rows.insert(&mut chat, 1, streaming_assistant("a"));
        rows.insert(&mut chat, 2, running("t1"));
        rows.insert(&mut chat, 3, subagent("s1"));

        // An untracked row lands where the tool row was.
        rows.insert(&mut chat, 2, Message::User("reply".into()));

        assert_eq!(rows.take_open_assistant(), Some(1), "rows before it stay");
        assert!(is_running(&chat, rows.running_tool("t1"), "t1"));
        assert!(is_subagent(&chat, rows.subagent("s1"), "s1"));
    }

    /// A finalized assistant row is not an open segment.
    #[test]
    fn finalized_assistant_is_not_recorded_open() {
        let mut chat = Vec::new();
        let mut rows = TurnRows::default();
        let Message::Assistant(mut msg) = streaming_assistant("done") else {
            unreachable!()
        };
        msg.finalize();
        rows.insert(&mut chat, 0, Message::Assistant(msg));
        assert!(!rows.has_open_assistant());
    }

    /// A replaced chat keeps only its subagent rows, at their new positions.
    #[test]
    fn reindex_forgets_streaming_rows_and_finds_subagents() {
        let mut chat = vec![Message::User("go".into())];
        let mut rows = TurnRows::default();
        rows.insert(&mut chat, 1, streaming_assistant("a"));
        rows.insert(&mut chat, 2, running("t1"));

        let rebuilt = vec![
            Message::User("go".into()),
            running("t1"),
            Message::User("more".into()),
            subagent("s1"),
        ];
        rows.reindex(&rebuilt);

        assert!(!rows.has_open_assistant());
        assert!(!rows.has_running_tools());
        assert!(is_subagent(&rebuilt, rows.subagent("s1"), "s1"));
    }

    /// Draining returns the in-flight rows in chat order.
    #[test]
    fn drain_running_tools_is_in_chat_order() {
        let mut chat = Vec::new();
        let mut rows = TurnRows::default();
        for (pos, id) in ["a", "b", "c"].into_iter().enumerate() {
            rows.insert(&mut chat, pos, running(id));
        }
        assert_eq!(rows.drain_running_tools(), vec![0, 1, 2]);
        assert!(!rows.has_running_tools());
    }
}
