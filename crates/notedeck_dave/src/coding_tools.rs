//! The Coding toolset's tools in the native loop: Read, Write, Edit, Grep,
//! Glob and Bash (declared in [`agentium_core::agent_spec::coding`]), run on
//! the host by [`LocalExecutor`] in the session's working tree.
//!
//! ```text
//! model ─ ToolCalls::App{Edit, …} ─▶ start_app_call ── approval? ──▶ job (off the frame)
//!                                                                      │
//!          CodingCall::parse ─▶ LocalExecutor::run(call, round's stop) ┤
//!                                                                      │
//!   row:    ExecutedTool (summary, Bash output, Edit/Write diff) ◀─────┤ reported as progress
//!   answer: the executor's text, or its error, for the model ◀─────────┘
//! ```
//!
//! A call shows the way a Claude Code session's does: an [`ExecutedTool`]
//! row built as a backend's own result would be ([`FinishedCall`]), keyed by
//! the call's id. The model gets the executor's text as is, not the row:
//! the row is for the user, the text is what the model reads.
//!
//! ## Only the host runner
//!
//! A spec whose coding toolset names the Microsandbox runner is refused when
//! its session starts ([`SpecToolsError`](crate::native_tools::SpecToolsError)):
//! running its calls here would put a session presented as sandboxed on the
//! host.
//!
//! [`ExecutedTool`]: crate::messages::ExecutedTool

use std::path::PathBuf;

use agentium_core::agent_spec::coding::CodingCall;
use agentium_core::agent_spec::local_executor::{ExecError, LocalExecutor};
use serde_json::Value;

use crate::backend::shared::FinishedCall;
use crate::messages::DaveApiResponse;
use crate::tool_round::{block_on_own_runtime, JobCtx, ToolJob, CANCELLED};
use crate::tools::{AppToolCall, AppToolResponse};
use crate::ToolResponses;

/// The job that runs the coding tool `call` off the frame, in the working
/// tree `root`; the round's stop kills a running Bash.
pub(crate) fn job(call: AppToolCall, root: PathBuf) -> ToolJob {
    Box::new(move |ctx| {
        let coding = CodingCall::parse(&call.name, &call.args).map_err(|err| err.to_string())?;
        let exec = LocalExecutor::new(root);
        let ran =
            block_on_own_runtime(async { Ok(exec.run(&coding, ctx.cancel().cancelled()).await) })?;
        let text = match ran {
            Ok(text) => text,
            Err(ExecError::Cancelled) => return Err(CANCELLED.to_owned()),
            Err(err) => {
                let err = err.to_string();
                report_row(ctx, &coding, &err, true);
                return Err(err);
            }
        };
        report_row(ctx, &coding, &text, false);
        Ok(ToolResponses::App(AppToolResponse {
            tool: call.name,
            value: Value::String(text),
        }))
    })
}

/// Show `coding`'s row in the chat: answered `text`, or failed with it.
/// A failed Edit or Write changed nothing, so its row carries no diff.
fn report_row(ctx: &JobCtx, coding: &CodingCall, text: &str, failed: bool) {
    let input = coding.input();
    let result = Value::String(text.to_owned());
    let row = FinishedCall {
        tool_name: coding.name(),
        tool_input: &input,
        result_value: &result,
        file_update: if failed { None } else { coding.file_update() },
        parent_task_id: None,
        tool_use_id: Some(ctx.call_id()),
        is_error: failed,
    }
    .executed_tool();
    ctx.report(DaveApiResponse::ToolResult(row));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::ExecutedTool;
    use crate::tool_round::ToolCancel;
    use serde_json::json;

    /// Run `name(args)` in `root` outside any round: its answer, and the
    /// rows it reported.
    fn run(root: &std::path::Path, name: &str, args: Value) -> RunOutcome {
        let (progress, rows) = std::sync::mpsc::channel();
        let ctx = JobCtx::reporting("c1", ToolCancel::default(), progress);
        let call = AppToolCall {
            name: name.into(),
            args,
        };
        let answer = job(call, root.to_owned())(&ctx);
        let rows = rows
            .try_iter()
            .map(|r| match r {
                DaveApiResponse::ToolResult(row) => row,
                _ => panic!("only rows are reported"),
            })
            .collect();
        RunOutcome { answer, rows }
    }

    struct RunOutcome {
        answer: Result<ToolResponses, String>,
        rows: Vec<ExecutedTool>,
    }

    /// An Edit changes the file, answers the executor's text as a plain
    /// string, and shows one row keyed by the call with its diff.
    #[test]
    fn an_edit_answers_text_and_shows_its_diff() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let out = run(
            dir.path(),
            "Edit",
            json!({ "file_path": "a.txt", "old_string": "hello", "new_string": "bye" }),
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "bye\n"
        );
        let Ok(ToolResponses::App(answer)) = out.answer else {
            panic!("an edit answers: {:?}", out.answer);
        };
        assert_eq!(answer.tool, "Edit");
        assert!(answer.value.is_string(), "{:?}", answer.value);
        let [row] = out.rows.as_slice() else {
            panic!("one row: {:?}", out.rows);
        };
        assert_eq!(row.tool_name, "Edit");
        assert_eq!(row.tool_use_id.as_deref(), Some("c1"));
        assert!(row.file_update.is_some());
        assert!(!row.is_error);
    }

    /// A Bash call's output is the answer and shows in its row.
    #[test]
    fn bash_output_is_the_answer_and_the_rows_body() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            dir.path(),
            "Bash",
            json!({ "command": "echo hi", "description": "Say hi" }),
        );
        let Ok(ToolResponses::App(answer)) = out.answer else {
            panic!("bash answers: {:?}", out.answer);
        };
        assert!(answer.value.as_str().unwrap().contains("hi"));
        let [row] = out.rows.as_slice() else {
            panic!("one row: {:?}", out.rows);
        };
        assert_eq!(row.description.as_deref(), Some("Say hi"));
        assert!(row.output.is_some());
    }

    /// A failed edit answers the reason, shows a failed row, and carries
    /// no diff since nothing changed.
    #[test]
    fn a_failed_edit_shows_a_failed_row_without_a_diff() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let out = run(
            dir.path(),
            "Edit",
            json!({ "file_path": "a.txt", "old_string": "absent", "new_string": "x" }),
        );
        let err = out.answer.expect_err("the edit fails");
        assert!(err.starts_with("old_string isn't in"), "{err}");
        let [row] = out.rows.as_slice() else {
            panic!("one row: {:?}", out.rows);
        };
        assert!(row.is_error);
        assert!(row.file_update.is_none());
    }

    /// A call the model got wrong answers why, before anything runs.
    #[test]
    fn a_malformed_call_answers_why_without_a_row() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(dir.path(), "Read", json!({}));
        assert!(out.answer.is_err());
        assert!(out.rows.is_empty());
    }
}
