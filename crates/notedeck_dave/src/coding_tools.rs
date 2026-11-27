//! The Coding toolset's tools in the native loop: Read, Write, Edit, Grep,
//! Glob and Bash (declared in [`agentium_core::agent_spec::coding`]), run on
//! the host by [`Executor`] in the session's working tree ([`job`]), or in
//! the session's microVM guest when its spec says so ([`guest_job`]).
//!
//! ```text
//! model ─ ToolCalls::App{Edit, …} ─▶ start_app_call ── approval? ──▶ job (off the frame)
//!                                                                      │
//!          CodingCall::parse ─┬▶ host:  Executor::run(call, round's stop)
//!                             └▶ guest: GuestCalls::run(call, round's stop)
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
//! ## What a Bash child sees
//!
//! Bash inherits notedeck's environment, less what a session must never
//! see ([`STRIPPED_ENV`]: the key notedeck may have been launched with),
//! plus the session's own identity (`AGENTIUM_SESSION`,
//! `AGENTIUM_SESSION_ID`), as a Claude session's CLI gets it, so `nd` and
//! the skills that read it work from a spec session's shell too.
//!
//! ## The guest runner
//!
//! A spec whose coding toolset names the Microsandbox runner runs every
//! call in the session's own guest ([`crate::sandbox`]), never here: a
//! notedeck that can't run guests refuses the spec when its session starts
//! ([`SpecToolsError::Sandbox`](crate::native_tools::SpecToolsError::Sandbox)).
//! The guest runs the same executor, so a call's row and answer are the
//! ones the host runner gives. Its Bash gets the session's identity from the
//! guest's environment, and has no host environment to strip.
//!
//! [`ExecutedTool`]: crate::messages::ExecutedTool

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use agentium_core::agent_spec::coding::{file_update, CodingCall};
use agentium_core::agent_spec::local_executor::{ChildEnv, ExecError, Executor};
use serde_json::Value;

use crate::backend::shared::FinishedCall;
use crate::messages::DaveApiResponse;
use crate::sandbox::{GuestCallError, GuestCalls};
use crate::tool_round::{block_on_own_runtime, JobCtx, ToolJob, CANCELLED};
use crate::tools::{AppToolCall, AppToolResponse};
use crate::ToolResponses;

/// Variables a Bash child never inherits from notedeck: `DAVE_SEC` is a
/// secret key notedeck can be launched with, and `DAVE` rides beside it.
/// A session's shell has no use for either, and an `env` would print the
/// key into its transcript.
pub(crate) const STRIPPED_ENV: [&str; 2] = ["DAVE", "DAVE_SEC"];

/// What a session's Bash children get on top of notedeck's environment:
/// `identity` (the session's own variables) set, [`STRIPPED_ENV`] removed.
pub(crate) fn child_env(identity: BTreeMap<String, String>) -> ChildEnv {
    ChildEnv {
        set: identity,
        remove: STRIPPED_ENV.iter().map(|name| (*name).to_owned()).collect(),
    }
}

/// The job that runs the coding tool `call` off the frame, in the working
/// tree `root`, a Bash call with `env` ([`child_env`]); the round's stop
/// kills a running Bash.
pub(crate) fn job(call: AppToolCall, root: PathBuf, env: ChildEnv) -> ToolJob {
    Box::new(move |ctx| {
        let coding = CodingCall::parse(&call.name, &call.args).map_err(|err| err.to_string())?;
        let exec = Executor::new(root).with_env(env);
        let ran =
            block_on_own_runtime(async { Ok(exec.run(&coding, ctx.cancel().cancelled()).await) })?;
        let ran = match ran {
            Ok(text) => Ok(text),
            Err(ExecError::Cancelled) => return Err(CANCELLED.to_owned()),
            Err(err) => Err(err.to_string()),
        };
        answer(ctx, call.name, &coding, ran)
    })
}

/// The job that runs the coding tool `call` off the frame in the session's
/// guest, booting it on the session's first call; the round's stop cancels
/// it in the guest. A guest that can't boot fails the turn's view with why
/// (a [`DaveApiResponse::Failed`] row) as well as the call.
pub(crate) fn guest_job(call: AppToolCall, guest: Arc<dyn GuestCalls>) -> ToolJob {
    Box::new(move |ctx| {
        let coding = CodingCall::parse(&call.name, &call.args).map_err(|err| err.to_string())?;
        let ran = match guest.run(&coding, ctx.cancel()) {
            Ok(text) => Ok(text),
            Err(GuestCallError::Cancelled) => return Err(CANCELLED.to_owned()),
            Err(err @ GuestCallError::Boot(_)) => {
                let err = err.to_string();
                ctx.report(DaveApiResponse::Failed(err.clone()));
                return Err(err);
            }
            Err(GuestCallError::Call(err)) => Err(err),
        };
        answer(ctx, call.name, &coding, ran)
    })
}

/// A coding call's answer for the model, `ran`'s text either way, and its
/// row in the chat: the same whichever runner ran it.
fn answer(
    ctx: &JobCtx,
    tool: String,
    coding: &CodingCall,
    ran: Result<String, String>,
) -> Result<ToolResponses, String> {
    let text = match ran {
        Ok(text) => text,
        Err(err) => {
            report_row(ctx, coding, &err, true);
            return Err(err);
        }
    };
    report_row(ctx, coding, &text, false);
    Ok(ToolResponses::App(AppToolResponse {
        tool,
        value: Value::String(text),
    }))
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
        file_update: if failed { None } else { file_update(coding) },
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
        let answer = job(call, root.to_owned(), ChildEnv::default())(&ctx);
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
    ///
    /// Unix only, like the Bash tool, which runs `/bin/sh`.
    #[cfg(unix)]
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

    /// A guest that answers every call with `answer`.
    struct Answers(Result<String, GuestCallError>);

    impl GuestCalls for Answers {
        fn run(&self, _: &CodingCall, _: &ToolCancel) -> Result<String, GuestCallError> {
            self.0.clone()
        }
    }

    /// What a job run in a guest did.
    struct GuestRun {
        answer: Result<ToolResponses, String>,
        /// Everything it reported, in order.
        reported: Vec<DaveApiResponse>,
    }

    /// Run `name(args)` in a guest that answers `answer`.
    fn run_in_guest(answer: Result<String, GuestCallError>, name: &str, args: Value) -> GuestRun {
        let (progress, reported) = std::sync::mpsc::channel();
        let ctx = JobCtx::reporting("c1", ToolCancel::default(), progress);
        let call = AppToolCall {
            name: name.into(),
            args,
        };
        let answer = guest_job(call, Arc::new(Answers(answer)))(&ctx);
        GuestRun {
            answer,
            reported: reported.try_iter().collect(),
        }
    }

    /// A call the guest answered shows the row the host runner's would:
    /// keyed by the call, an Edit with its diff, and the guest's text is
    /// the model's answer.
    #[test]
    fn a_guest_answer_shows_the_host_runners_row() {
        let GuestRun { answer, reported } = run_in_guest(
            Ok("The file a.txt has been updated.".into()),
            "Edit",
            json!({ "file_path": "a.txt", "old_string": "hello", "new_string": "bye" }),
        );
        let Ok(ToolResponses::App(answer)) = answer else {
            panic!("an edit answers: {answer:?}");
        };
        assert_eq!(answer.value, "The file a.txt has been updated.");
        let [DaveApiResponse::ToolResult(row)] = reported.as_slice() else {
            panic!("one row");
        };
        assert_eq!(row.tool_name, "Edit");
        assert_eq!(row.tool_use_id.as_deref(), Some("c1"));
        assert!(row.file_update.is_some());
        assert!(!row.is_error);
    }

    /// A tool that failed in the guest shows a failed row, as on the host.
    #[test]
    fn a_guest_tool_error_shows_a_failed_row() {
        let GuestRun { answer, reported } = run_in_guest(
            Err(GuestCallError::Call("a.txt: no such file".into())),
            "Read",
            json!({ "file_path": "a.txt" }),
        );
        assert_eq!(answer.err().as_deref(), Some("a.txt: no such file"));
        let [DaveApiResponse::ToolResult(row)] = reported.as_slice() else {
            panic!("one row");
        };
        assert!(row.is_error);
    }

    /// A guest that can't boot fails the turn's view with why (no tool row,
    /// since nothing ran) and answers the model the same; a stop answers as
    /// a cancelled host call does.
    #[test]
    fn a_guest_that_cant_boot_shows_why_and_a_stop_is_cancelled() {
        let GuestRun { answer, reported } = run_in_guest(
            Err(GuestCallError::Boot("all 4 guests are in use".into())),
            "Bash",
            json!({ "command": "true" }),
        );
        let err = answer.expect_err("nothing ran");
        assert!(err.contains("all 4 guests are in use"), "{err}");
        let [DaveApiResponse::Failed(shown)] = reported.as_slice() else {
            panic!("only the failure shows");
        };
        assert_eq!(shown, &err);

        let GuestRun { answer, reported } = run_in_guest(
            Err(GuestCallError::Cancelled),
            "Bash",
            json!({ "command": "true" }),
        );
        assert_eq!(answer.err().as_deref(), Some(CANCELLED));
        assert!(reported.is_empty());
    }

    /// A session's Bash child gets the session's identity and never
    /// notedeck's key (headway:dave/talk-giraffe-south).
    #[test]
    fn a_bash_child_gets_the_identity_and_not_the_key() {
        let identity = crate::backend::shared::session_env(Some("d-tag"), &BTreeMap::new(), None);
        let env = child_env(identity);
        assert_eq!(env.set["AGENTIUM_SESSION_ID"], "d-tag");
        assert!(env.set["AGENTIUM_SESSION"].starts_with("agentium:"));
        for name in ["DAVE", "DAVE_SEC"] {
            assert!(env.remove.iter().any(|n| n == name), "{name} stripped");
        }
    }
}
