//! Runs of back-to-back tool calls, folded into one quiet line.
//!
//! An agentic turn is mostly tool calls: a dozen Bash, Read and Grep rows
//! between two sentences of reply. Drawn one row each they bury the reply,
//! so `render_chat` draws each run as a single collapsible line instead —
//! `3 tools · List the crates, Read dave.rs, …` — and unfolding it shows the
//! rows as before.
//!
//! What stays out of a fold, splitting the run around it, is what the reader
//! has to see: a file edit with its diff, a permission prompt, a subagent, a
//! call that failed, and a call still running. In a live turn the newest call
//! also stays under the fold, so the row that is working is never hidden;
//! once the turn moves on (a reply, or the turn ends) it folds with the rest.
//!
//! This module only decides what folds and what the line says. Drawing is
//! `DaveUi::tool_fold_ui`.

use crate::messages::{ExecutedTool, Message};
use crate::tools::ToolResponses;
use std::ops::Range;

/// How long a fold's label grows before it stops naming calls. The header
/// truncates to its width anyway; this only bounds what is built each frame.
const LABEL_CAP: usize = 240;

/// How one chat row takes part in a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// A finished call that can hide inside a fold.
    Member,
    /// A row that draws nothing in an agentic session, so it neither joins
    /// nor ends a run.
    Invisible,
    /// Anything that draws on its own: it ends the run before it.
    Break,
}

/// The [`Role`] of `message` in an agentic session.
fn role(message: &Message) -> Role {
    match message {
        Message::ToolResponse(response) => match response.responses() {
            ToolResponses::ExecutedTool(tool) if foldable(tool) => Role::Member,
            ToolResponses::ExecutedTool(_) => Role::Break,
            // Other responses draw nothing in agentic mode.
            _ => Role::Invisible,
        },
        Message::TodoUpdate(_) => Role::Invisible,
        _ => Role::Break,
    }
}

/// Whether a finished call may hide in a fold: not an edit with a diff,
/// which is the only place an auto-approved edit shows, and not a failure.
fn foldable(tool: &ExecutedTool) -> bool {
    tool.file_update.is_none() && !tool.is_error
}

/// The rows a fold covers, from the row that starts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ToolRun {
    /// The chat rows the fold stands for: its first member through its last.
    /// Rows in between that draw nothing are inside it too.
    pub rows: Range<usize>,
    /// How many calls are folded.
    pub calls: usize,
}

/// The fold starting at chat row `start` of an agentic session, or `None`
/// when that row doesn't start one (it isn't a foldable call, or it is the
/// newest call of a live turn and so draws on its own).
///
/// `live` is whether the session is working: then a run that reaches the end
/// of the chat leaves its newest call out of the fold.
pub(super) fn tool_run(chat: &[Message], start: usize, live: bool) -> Option<ToolRun> {
    if role(chat.get(start)?) != Role::Member {
        return None;
    }
    let mut calls = 0;
    let mut last = start;
    let mut before_last = None;
    let mut end = start;
    while let Some(message) = chat.get(end) {
        match role(message) {
            Role::Member => {
                calls += 1;
                before_last = Some(last);
                last = end;
            }
            Role::Invisible => {}
            Role::Break => break,
        }
        end += 1;
    }
    if live && end == chat.len() {
        // The newest call draws under the fold, with whatever came before
        // it folded.
        calls -= 1;
        last = before_last.filter(|_| calls > 0)?;
    }
    Some(ToolRun {
        rows: start..last + 1,
        calls,
    })
}

/// The calls a fold over `rows` stands for, in order.
pub(super) fn members<'a>(
    chat: &'a [Message],
    rows: Range<usize>,
) -> impl Iterator<Item = &'a ExecutedTool> + 'a {
    chat[rows].iter().filter_map(|message| match message {
        Message::ToolResponse(response) => match response.responses() {
            ToolResponses::ExecutedTool(tool) => Some(tool),
            _ => None,
        },
        _ => None,
    })
}

/// Write the fold's label into `out`: each call's own account of itself,
/// comma separated, stopping with `…` once it is longer than any header.
pub(super) fn label<'a>(calls: impl Iterator<Item = &'a ExecutedTool>, out: &mut String) {
    for (n, tool) in calls.enumerate() {
        if n > 0 {
            out.push_str(", ");
        }
        if out.len() >= LABEL_CAP {
            out.push('…');
            return;
        }
        push_call_label(tool, out);
    }
}

/// One call's part of a fold's label: the model's description of it when it
/// gave one, else a Bash call's command, else the tool's name and summary.
fn push_call_label(tool: &ExecutedTool, out: &mut String) {
    if let Some(description) = &tool.description {
        out.push_str(description);
        return;
    }
    if tool.tool_name == "Bash" {
        out.push_str(bash_command(&tool.summary));
        return;
    }
    out.push_str(&tool.tool_name);
    if !tool.summary.is_empty() {
        out.push(' ');
        out.push_str(&tool.summary);
    }
}

/// A Bash summary's command, for a call recorded before descriptions were:
/// without the backticks `format_bash_summary` wraps it in, or a leading
/// `cd <dir> &&` that agents prefix most commands with and which would fill
/// the label with the same long path.
fn bash_command(summary: &str) -> &str {
    let command = summary
        .strip_prefix('`')
        .and_then(|s| s.strip_suffix('`'))
        .unwrap_or(summary);
    strip_leading_cd(command)
}

/// `command` without a leading `cd <dir> &&` or `cd <dir>;`, when what follows
/// is a further command. A bare `cd <dir>` is kept: it is the whole command.
fn strip_leading_cd(command: &str) -> &str {
    let Some(rest) = command.strip_prefix("cd ") else {
        return command;
    };
    let rest = rest.trim_start();
    let dir_end = rest
        .find(|c: char| c.is_whitespace() || c == ';')
        .unwrap_or(rest.len());
    let after = rest[dir_end..].trim_start();
    let next = after
        .strip_prefix("&&")
        .or_else(|| after.strip_prefix(';'))
        .map(str::trim_start);
    match next {
        Some(next) if !next.is_empty() => next,
        _ => command,
    }
}

/// The count half of a fold's line: `1 tool`, `3 tools`.
pub(super) fn count_text(calls: usize) -> String {
    if calls == 1 {
        "1 tool".to_owned()
    } else {
        format!("{calls} tools")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{AssistantMessage, RunningTool};
    use crate::tools::ToolResponse;

    fn call(tool_name: &str, summary: &str, description: Option<&str>) -> ExecutedTool {
        ExecutedTool {
            tool_name: tool_name.into(),
            summary: summary.into(),
            output: None,
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
            description: description.map(Into::into),
            is_error: false,
        }
    }

    fn row(tool: ExecutedTool) -> Message {
        Message::ToolResponse(ToolResponse::executed_tool(tool))
    }

    fn bash(command: &str) -> Message {
        row(call("Bash", &format!("`{command}`"), None))
    }

    fn failed(command: &str) -> Message {
        let mut tool = call("Bash", &format!("`{command}`"), None);
        tool.is_error = true;
        row(tool)
    }

    fn say(text: &str) -> Message {
        Message::Assistant(AssistantMessage::from_text(text.into()))
    }

    fn running() -> Message {
        Message::ToolRunning(RunningTool {
            tool_use_id: "t".into(),
            tool_name: "Bash".into(),
            summary: "`cargo test`".into(),
            description: None,
        })
    }

    fn run(rows: Range<usize>, calls: usize) -> Option<ToolRun> {
        Some(ToolRun { rows, calls })
    }

    /// A run folds up to the next row that draws on its own, and only from
    /// a call: prose never starts one.
    #[test]
    fn a_run_ends_at_the_next_row_that_draws() {
        let chat = [say("a"), bash("ls"), bash("pwd"), bash("id"), say("b")];
        assert_eq!(tool_run(&chat, 0, false), None);
        assert_eq!(tool_run(&chat, 1, false), run(1..4, 3));
        assert_eq!(tool_run(&chat, 4, false), None);
    }

    /// A failed call breaks out, splitting the run around it; so does a call
    /// still running. A row that draws nothing doesn't.
    #[test]
    fn failures_and_running_calls_split_a_run() {
        let chat = [bash("ls"), failed("false"), bash("pwd"), running()];
        assert_eq!(tool_run(&chat, 0, false), run(0..1, 1));
        assert_eq!(tool_run(&chat, 1, false), None);
        assert_eq!(tool_run(&chat, 2, false), run(2..3, 1));

        let todo = Message::TodoUpdate(Default::default());
        let chat = [bash("ls"), todo, bash("pwd"), say("done")];
        assert_eq!(tool_run(&chat, 0, false), run(0..3, 2));
    }

    /// In a live turn the newest call stays out of the fold, and a run of
    /// one is not folded at all; once the turn ends, the whole run folds.
    #[test]
    fn a_live_turn_keeps_its_newest_call_out() {
        let chat = [say("a"), bash("ls"), bash("pwd"), bash("id")];
        assert_eq!(tool_run(&chat, 1, true), run(1..3, 2));
        assert_eq!(tool_run(&chat, 3, true), None);
        assert_eq!(tool_run(&chat, 1, false), run(1..4, 3));

        let chat = [say("a"), bash("ls")];
        assert_eq!(tool_run(&chat, 1, true), None);

        // A run that a reply has already ended folds whole, live or not.
        let chat = [bash("ls"), bash("pwd"), say("b")];
        assert_eq!(tool_run(&chat, 0, true), run(0..2, 2));
    }

    /// The label prefers the model's description, then a Bash command
    /// without its `cd` prefix, then the tool and its summary.
    #[test]
    fn label_names_each_call() {
        let chat = [
            row(call("Bash", "`cd /a/b && grep -n x f`", Some("Find x"))),
            row(call("Read", "dave.rs", None)),
            bash("cd /a/b && sed -n 1,2p f"),
            row(call("ToolSearch", "", None)),
        ];
        let mut out = String::new();
        label(members(&chat, 0..4), &mut out);
        assert_eq!(out, "Find x, Read dave.rs, sed -n 1,2p f, ToolSearch");
    }

    /// A long run stops naming calls once its label outgrows any header.
    #[test]
    fn label_stops_past_the_cap() {
        let long = "x".repeat(LABEL_CAP);
        let chat = [bash(&long), bash("ls"), bash("pwd")];
        let mut out = String::new();
        label(members(&chat, 0..3), &mut out);
        assert_eq!(out, format!("{long}, …"));
    }

    #[test]
    fn bash_command_drops_backticks_and_a_leading_cd() {
        assert_eq!(bash_command("`git status`"), "git status");
        assert_eq!(bash_command("`cd /a/b; sed -n 1,2p f`"), "sed -n 1,2p f");
        // Only the first `cd` goes, and a bare one is the whole command.
        assert_eq!(bash_command("`cd /a && cd b && make`"), "cd b && make");
        assert_eq!(bash_command("`cd /a/b`"), "cd /a/b");
        assert_eq!(bash_command("`cdx && ls`"), "cdx && ls");
    }
}
