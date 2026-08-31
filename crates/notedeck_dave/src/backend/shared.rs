//! Shared utilities used by multiple AI backend implementations.

use crate::auto_accept::AutoAcceptRules;
use crate::backend::tool_summary::{
    extract_response_content, format_tool_summary, truncate_output,
};
use crate::file_update::FileUpdate;
use crate::messages::{
    DaveApiResponse, ExecutedTool, ImageAttachment, PendingPermission, PermissionRequest,
    PermissionView, UserMessage,
};
use crate::Message;
use agentium_core::Waker;
use claude_agent_sdk_rs::PermissionMode;
use std::sync::mpsc;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Commands sent to a session's actor task.
///
/// Used identically by the Claude and Codex backends.
pub(crate) enum SessionCommand {
    Query {
        prompt: String,
        images: Vec<crate::messages::ImageAttachment>,
        /// UI response channel for this turn.
        ///
        /// `Some` for backends that mint a fresh channel per query (Codex).
        /// `None` for backends whose session actor owns one long-lived channel
        /// for the session's whole lifetime (Claude) — the actor already holds
        /// the sender, so the command doesn't carry one.
        response_tx: Option<mpsc::Sender<DaveApiResponse>>,
        waker: Waker,
    },
    /// Interrupt the current query - stops the stream but preserves session
    Interrupt {
        waker: Waker,
    },
    /// Set the permission mode (Default or Plan)
    SetPermissionMode {
        mode: PermissionMode,
        waker: Waker,
    },
    /// Trigger manual context compaction
    Compact {
        response_tx: mpsc::Sender<DaveApiResponse>,
        waker: Waker,
    },
    Shutdown,
}

/// Handle kept by a backend to communicate with its session actor.
pub(crate) struct SessionHandle {
    pub command_tx: tokio_mpsc::Sender<SessionCommand>,
}

/// Convert our messages to a prompt string for the AI backend.
///
/// Includes the system message (if any) followed by the conversation
/// history formatted as `Human:` / `Assistant:` turns. Tool-related,
/// error, permission, compaction and subagent messages are skipped.
pub fn messages_to_prompt(messages: &[Message]) -> String {
    let mut prompt = String::new();

    // Include system message if present
    for msg in messages {
        if let Message::System(content) = msg {
            prompt.push_str(content);
            prompt.push_str("\n\n");
            break;
        }
    }

    // Format conversation history
    for msg in messages {
        match msg {
            Message::System(_) => {} // Already handled
            Message::User(msg) => {
                prompt.push_str("Human: ");
                prompt.push_str(&msg.text);
                prompt.push_str("\n\n");
            }
            Message::Assistant(content) => {
                prompt.push_str("Assistant: ");
                prompt.push_str(content.text());
                prompt.push_str("\n\n");
            }
            Message::ToolCalls(_)
            | Message::ToolResponse(_)
            | Message::Error(_)
            | Message::PermissionRequest(_)
            | Message::CompactionComplete(_)
            | Message::Subagent(_)
            | Message::TodoUpdate(_) => {}
        }
    }

    prompt
}

/// Collect all trailing user messages and join them.
///
/// When multiple messages are queued, they're all sent as one prompt
/// so the AI sees everything at once instead of one at a time.
#[cfg(test)]
pub fn get_pending_user_messages(messages: &[Message]) -> String {
    pending_user_prompt_and_images(messages).0
}

/// Collect trailing user messages from newest to oldest.
fn trailing_user_messages(messages: &[Message]) -> Vec<&UserMessage> {
    messages
        .iter()
        .rev()
        .take_while(|m| matches!(m, Message::User(_)))
        .filter_map(|m| match m {
            Message::User(msg) => Some(msg),
            _ => None,
        })
        .collect()
}

/// Build prompt and image list from trailing queued user messages in one pass.
///
/// Returns user text joined with `\n` and images in chronological order.
fn pending_user_prompt_and_images(messages: &[Message]) -> (String, Vec<ImageAttachment>) {
    let trailing = trailing_user_messages(messages);
    if trailing.is_empty() {
        return (String::new(), Vec::new());
    }

    // Iterate newest->oldest in reverse so output is chronological.
    let mut prompt = String::new();
    let mut images = Vec::new();
    for (idx, msg) in trailing.iter().rev().enumerate() {
        if idx > 0 {
            prompt.push('\n');
        }
        prompt.push_str(msg.as_str());
        images.extend(msg.images.iter().cloned());
    }

    (prompt, images)
}

/// Remove a completed subagent from the stack and notify the UI.
pub fn complete_subagent(
    task_id: &str,
    result_text: &str,
    subagent_stack: &mut Vec<String>,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    waker: &Waker,
) {
    subagent_stack.retain(|id| id != task_id);
    let _ = response_tx.send(DaveApiResponse::SubagentCompleted {
        task_id: task_id.to_string(),
        result: truncate_output(result_text, 2000),
    });
    waker.wake();
}

/// Build an [`ExecutedTool`] from a completed tool call and send it
/// to the UI along with a repaint request.
#[allow(clippy::too_many_arguments)]
pub fn send_tool_result(
    tool_name: &str,
    tool_input: &serde_json::Value,
    result_value: &serde_json::Value,
    file_update: Option<FileUpdate>,
    parent_override: Option<&str>,
    subagent_stack: &[String],
    response_tx: &mpsc::Sender<DaveApiResponse>,
    waker: &Waker,
) {
    let summary = format_tool_summary(tool_name, tool_input, result_value);
    // Some tools' result content *is* the payload worth reading inline, not just
    // a confirmation captured by the summary: Bash's stdout/stderr, and
    // WebSearch's returned results. Keep those around to render in the
    // collapsible body. Other tools are covered by their summary (or a file
    // diff); broadening this set further is a follow-up. Kept in full here:
    // truncation is a display concern the UI applies (see `tool_command_output_ui`),
    // and the wire keeps only a safety-capped copy (see `ToolResultContent::encode`).
    let output = matches!(tool_name, "Bash" | "WebSearch")
        .then(|| extract_response_content(result_value))
        .flatten()
        .filter(|s| !s.is_empty());
    // An explicit `parent_tool_use_id` (a subagent-internal result) wins over
    // the foreground nesting stack.
    let parent_task_id = parent_override
        .map(str::to_string)
        .or_else(|| subagent_stack.last().cloned());
    let tool_result = ExecutedTool {
        tool_name: tool_name.to_string(),
        summary,
        output,
        parent_task_id,
        file_update,
    };
    let _ = response_tx.send(DaveApiResponse::ToolResult(tool_result));
    waker.wake();
}

/// Check auto-accept rules for a tool invocation.
///
/// Returns `true` (and logs) when the tool should be silently
/// accepted without asking the user.
pub fn should_auto_accept(tool_name: &str, tool_input: &serde_json::Value) -> bool {
    should_auto_accept_with(&AutoAcceptRules::default(), tool_name, tool_input)
}

/// [`should_auto_accept`] against an explicit rules set.
///
/// `rules` is a parameter so the decision-tool gate below can be tested at all:
/// with the shipping [`AutoAcceptRules::default`] the gate is unobservable —
/// none of those rules match `AskUserQuestion` or `ExitPlanMode` anyway, so
/// removing it changes nothing. Pass a rules set that *would* accept them and
/// the gate becomes the only thing standing in the way.
fn should_auto_accept_with(
    rules: &AutoAcceptRules,
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> bool {
    // Decision-type prompts (AskUserQuestion / ExitPlanMode plan review) always
    // need a real user decision and must never be silently accepted. This gate
    // runs at the backend, before the request reaches dave's session-level
    // `should_runtime_allow`, so the same invariant is enforced here too — the
    // default rules don't list these tools today, but that must not be load-
    // bearing.
    if PermissionView::is_decision_tool(tool_name) {
        return false;
    }
    let accepted = rules.should_auto_accept(tool_name, tool_input);
    if accepted {
        tracing::debug!("Auto-accepting {}: matched auto-accept rule", tool_name);
    }
    accepted
}

/// Build a [`PermissionRequest`] + [`PendingPermission`], send it to
/// the UI via `response_tx`, and return the oneshot receiver the
/// caller can `await` to get the user's decision.
///
/// Returns `None` if the UI channel is closed (the request could not
/// be delivered).
pub fn forward_permission_to_ui(
    tool_name: &str,
    tool_input: serde_json::Value,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    waker: &Waker,
) -> Option<oneshot::Receiver<crate::messages::PermissionResponse>> {
    forward_permission_to_ui_with_view(tool_name, tool_input, None, response_tx, waker)
}

/// Variant of [`forward_permission_to_ui`] that allows a backend to provide an
/// explicit shared permission view instead of relying on inference.
pub fn forward_permission_to_ui_with_view(
    tool_name: &str,
    tool_input: serde_json::Value,
    view: Option<PermissionView>,
    response_tx: &mpsc::Sender<DaveApiResponse>,
    waker: &Waker,
) -> Option<oneshot::Receiver<crate::messages::PermissionResponse>> {
    let request_id = Uuid::new_v4();
    let (ui_resp_tx, ui_resp_rx) = oneshot::channel();

    let request = PermissionRequest::new(
        request_id,
        tool_name.to_string(),
        tool_input,
        view,
        None,
        None,
    );

    let pending = PendingPermission {
        request,
        response_tx: ui_resp_tx,
    };

    if response_tx
        .send(DaveApiResponse::PermissionRequest(pending))
        .is_err()
    {
        tracing::error!("Failed to send permission request to UI");
        return None;
    }

    waker.wake();
    Some(ui_resp_rx)
}

/// Prepare prompt text and image attachments from the same logical user turn(s).
///
/// This keeps text and image selection aligned:
/// - resumed sessions: trailing queued user messages
/// - first message in a new session: the single user message
/// - later turns in a new session: trailing queued user messages
pub fn prepare_prompt_and_images(
    messages: &[Message],
    resume_session_id: &Option<String>,
) -> (String, Vec<ImageAttachment>) {
    if resume_session_id.is_some() {
        return pending_user_prompt_and_images(messages);
    }

    let is_first_message = messages
        .iter()
        .filter(|m| matches!(m, Message::User(_)))
        .count()
        == 1;

    if is_first_message {
        let images = messages
            .iter()
            .find_map(|m| match m {
                Message::User(msg) => Some(msg.images.clone()),
                _ => None,
            })
            .unwrap_or_default();
        (messages_to_prompt(messages), images)
    } else {
        pending_user_prompt_and_images(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::prepare_prompt_and_images;
    use super::*;
    use crate::backend::CountingWaker;
    use crate::messages::{
        AssistantMessage, CompactionInfo, ImageAttachment, PermissionView, UserMessage,
    };
    use crate::Message;

    fn img(bytes: &[u8], mime: &str) -> ImageAttachment {
        ImageAttachment::new(bytes.to_vec(), mime)
    }

    #[test]
    fn decision_tools_never_backend_auto_accepted() {
        // A question set / plan review must reach the user for a real decision,
        // so the backend gate must never silently accept it — even if the rules
        // set says otherwise. Asserting this against the *default* rules proves
        // nothing: none of them match a decision tool, so the gate is
        // unobservable there. Use a rules set that would accept them.
        let permissive =
            AutoAcceptRules::from_rules(vec![crate::auto_accept::AutoAcceptRule::ReadOnlyTool {
                tools: vec![
                    "AskUserQuestion".to_string(),
                    "ExitPlanMode".to_string(),
                    "Read".to_string(),
                ],
            }]);

        // Control: these rules do accept, so a `false` below is the gate and not
        // a rules set that never matched anything.
        assert!(
            should_auto_accept_with(&permissive, "Read", &serde_json::json!({})),
            "the fixture rules must actually accept something"
        );

        assert!(!should_auto_accept_with(
            &permissive,
            "AskUserQuestion",
            &serde_json::json!({ "questions": [] }),
        ));
        assert!(!should_auto_accept_with(
            &permissive,
            "ExitPlanMode",
            &serde_json::json!({ "plan": "# Do the thing" }),
        ));

        // And the shipping path, which is what the backends actually call.
        assert!(!should_auto_accept(
            "AskUserQuestion",
            &serde_json::json!({ "questions": [] }),
        ));
        assert!(!should_auto_accept(
            "ExitPlanMode",
            &serde_json::json!({ "plan": "# Do the thing" }),
        ));
    }

    #[test]
    fn prepare_prompt_and_images_uses_all_trailing_user_images_in_order() {
        let first = img(&[1], "image/png");
        let second = img(&[2], "image/jpeg");
        let third = img(&[3], "image/gif");
        let old = img(&[9], "image/png");

        let messages = vec![
            Message::User(UserMessage::new("old", vec![old.clone()])),
            Message::Assistant(AssistantMessage::from_text("ack".to_string())),
            Message::User(UserMessage::new("queued one", vec![first.clone()])),
            Message::User(UserMessage::new(
                "queued two",
                vec![second.clone(), third.clone()],
            )),
        ];

        let (prompt, images) = prepare_prompt_and_images(&messages, &None);
        assert_eq!(prompt, "queued one\nqueued two");
        assert_eq!(images.len(), 3);
        assert_eq!(&*images[0].bytes, &*first.bytes);
        assert_eq!(&*images[1].bytes, &*second.bytes);
        assert_eq!(&*images[2].bytes, &*third.bytes);
    }

    #[test]
    fn prepare_prompt_and_images_first_turn_keeps_single_user_images() {
        let image = img(&[7, 8], "image/png");
        let messages = vec![
            Message::System("sys".to_string()),
            Message::User(UserMessage::new("hello", vec![image.clone()])),
        ];

        let (prompt, images) = prepare_prompt_and_images(&messages, &None);
        assert!(prompt.contains("Human: hello"));
        assert_eq!(images.len(), 1);
        assert_eq!(&*images[0].bytes, &*image.bytes);
    }

    #[test]
    fn prepare_prompt_and_images_resumed_session_uses_pending_images() {
        let first = img(&[11], "image/png");
        let second = img(&[12], "image/png");
        let messages = vec![
            Message::Assistant(AssistantMessage::from_text("prev".to_string())),
            Message::User(UserMessage::new("next", vec![first.clone()])),
            Message::User(UserMessage::new("last", vec![second.clone()])),
        ];

        let (prompt, images) = prepare_prompt_and_images(&messages, &Some("sid".to_string()));
        assert_eq!(prompt, "next\nlast");
        assert_eq!(images.len(), 2);
        assert_eq!(&*images[0].bytes, &*first.bytes);
        assert_eq!(&*images[1].bytes, &*second.bytes);
    }

    // ---- messages_to_prompt ----
    #[test]
    fn messages_to_prompt_empty() {
        assert_eq!(messages_to_prompt(&[]), "");
    }

    #[test]
    fn messages_to_prompt_system_first() {
        let msgs = vec![Message::System("You are helpful.".into())];
        assert_eq!(messages_to_prompt(&msgs), "You are helpful.\n\n");
    }

    #[test]
    fn messages_to_prompt_conversation() {
        let msgs = vec![
            Message::User("hello".into()),
            Message::Assistant(AssistantMessage::from_text("hi there".into())),
            Message::User("thanks".into()),
        ];
        let prompt = messages_to_prompt(&msgs);
        assert_eq!(
            prompt,
            "Human: hello\n\nAssistant: hi there\n\nHuman: thanks\n\n"
        );
    }

    #[test]
    fn messages_to_prompt_system_plus_conversation() {
        let msgs = vec![Message::System("system".into()), Message::User("hi".into())];
        let prompt = messages_to_prompt(&msgs);
        assert_eq!(prompt, "system\n\nHuman: hi\n\n");
    }

    #[test]
    fn messages_to_prompt_skips_non_conversation_types() {
        let msgs = vec![
            Message::User("hello".into()),
            Message::Error("oops".into()),
            Message::CompactionComplete(CompactionInfo { pre_tokens: 100 }),
            Message::User("world".into()),
        ];
        let prompt = messages_to_prompt(&msgs);
        assert_eq!(prompt, "Human: hello\n\nHuman: world\n\n");
    }

    // ---- get_pending_user_messages ----

    #[test]
    fn get_pending_empty() {
        assert_eq!(get_pending_user_messages(&[]), "");
    }

    #[test]
    fn get_pending_trailing_users() {
        let msgs = vec![
            Message::Assistant(AssistantMessage::from_text("ok".into())),
            Message::User("first".into()),
            Message::User("second".into()),
        ];
        assert_eq!(get_pending_user_messages(&msgs), "first\nsecond");
    }

    #[test]
    fn get_pending_stops_at_nonuser() {
        let msgs = vec![
            Message::User("old".into()),
            Message::Assistant(AssistantMessage::from_text("reply".into())),
            Message::User("new".into()),
        ];
        assert_eq!(get_pending_user_messages(&msgs), "new");
    }

    #[test]
    fn get_pending_no_trailing_users() {
        let msgs = vec![
            Message::User("hello".into()),
            Message::Assistant(AssistantMessage::from_text("hi".into())),
        ];
        assert_eq!(get_pending_user_messages(&msgs), "");
    }

    #[test]
    fn get_pending_single_user() {
        let msgs = vec![Message::User("hello".into())];
        assert_eq!(get_pending_user_messages(&msgs), "hello");
    }

    #[test]
    fn get_pending_stops_at_tool_response() {
        // A tool call/response pair breaks the trailing run just like an
        // assistant message does.
        let msgs = vec![
            Message::User("do something".into()),
            Message::Assistant(AssistantMessage::from_text("ok".into())),
            Message::ToolCalls(vec![crate::tools::ToolCall::invalid(
                "c1".into(),
                Some("Read".into()),
                None,
                "test".into(),
            )]),
            Message::ToolResponse(crate::tools::ToolResponse::error(
                "c1".into(),
                "result".into(),
            )),
            Message::User("queued 1".into()),
            Message::User("queued 2".into()),
        ];
        assert_eq!(get_pending_user_messages(&msgs), "queued 1\nqueued 2");
    }

    // ---- forward_permission_to_ui ----

    #[test]
    fn forward_permission_delivers() {
        let (tx, rx) = mpsc::channel();
        let waker = CountingWaker::new();
        let input = serde_json::json!({"command": "ls"});
        let result = forward_permission_to_ui("Bash", input.clone(), &tx, waker.waker());
        assert!(result.is_some());
        assert_eq!(
            waker.wakes(),
            1,
            "a permission prompt must repaint, or the user is never asked"
        );

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::PermissionRequest(pending) => {
                assert_eq!(pending.request.tool_name, "Bash");
                assert_eq!(pending.request.tool_input, input);
                assert!(matches!(pending.request.view, PermissionView::RawFallback));
                assert!(pending.request.response.is_none());
                assert!(pending.request.answer_summary.is_none());
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn forward_permission_exit_plan_caches() {
        let (tx, rx) = mpsc::channel();
        let waker = Waker::noop();
        let input = serde_json::json!({"plan": "# My Plan\n\nDo stuff"});
        let result = forward_permission_to_ui("ExitPlanMode", input, &tx, &waker);
        assert!(result.is_some());

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::PermissionRequest(pending) => {
                assert_eq!(pending.request.tool_name, "ExitPlanMode");
                assert!(matches!(
                    pending.request.view,
                    PermissionView::PlanReview(_)
                ));
                let plan = pending
                    .request
                    .view
                    .plan_markdown()
                    .expect("ExitPlanMode should cache plan");
                assert!(plan.source.contains("My Plan"));
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn forward_permission_closed_channel_returns_none() {
        let (tx, rx) = mpsc::channel::<DaveApiResponse>();
        drop(rx);
        let waker = Waker::noop();
        let result = forward_permission_to_ui("Bash", serde_json::json!({}), &tx, &waker);
        assert!(result.is_none());
    }

    // ---- send_tool_result ----

    #[test]
    fn send_tool_result_with_parent() {
        let (tx, rx) = mpsc::channel();
        let waker = CountingWaker::new();
        let stack = vec!["task-1".to_string()];
        send_tool_result(
            "Read",
            &serde_json::json!({"file_path": "/tmp/test"}),
            &serde_json::json!({"content": "hello"}),
            None,
            None,
            &stack,
            &tx,
            waker.waker(),
        );

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::ToolResult(tool) => {
                assert_eq!(tool.tool_name, "Read");
                assert_eq!(tool.parent_task_id, Some("task-1".to_string()));
                // Read's content is captured by its summary, not surfaced inline
                // (only Bash/WebSearch carry raw output today).
                assert!(tool.output.is_none(), "Read carries no inline output");
            }
            _ => panic!("expected ToolResult"),
        }
        assert_eq!(
            waker.wakes(),
            1,
            "a tool result must repaint, or it sits unseen until the next frame"
        );
    }

    #[test]
    fn send_tool_result_without_parent() {
        let (tx, rx) = mpsc::channel();
        let waker = Waker::noop();
        send_tool_result(
            "Bash",
            &serde_json::json!({"command": "ls"}),
            &serde_json::json!({"output": "file.txt"}),
            None,
            None,
            &[],
            &tx,
            &waker,
        );

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::ToolResult(tool) => {
                assert_eq!(tool.tool_name, "Bash");
                assert!(tool.parent_task_id.is_none());
                assert!(tool.file_update.is_none());
                // Summary should be non-empty (format_tool_summary produces something)
                assert!(!tool.summary.is_empty(), "summary should not be empty");
                // Bash stdout/stderr is surfaced inline for the transcript.
                assert_eq!(tool.output.as_deref(), Some("file.txt"));
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn send_tool_result_websearch_captures_results_inline() {
        let (tx, rx) = mpsc::channel();
        let waker = Waker::noop();
        send_tool_result(
            "WebSearch",
            &serde_json::json!({"query": "rust async"}),
            &serde_json::json!("Result 1\nResult 2"),
            None,
            None,
            &[],
            &tx,
            &waker,
        );

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::ToolResult(tool) => {
                assert_eq!(tool.tool_name, "WebSearch");
                assert_eq!(tool.summary, "'rust async'");
                // The returned results are surfaced inline for the transcript.
                assert_eq!(tool.output.as_deref(), Some("Result 1\nResult 2"));
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn send_tool_result_bash_empty_output_stays_none() {
        let (tx, rx) = mpsc::channel();
        let waker = Waker::noop();
        send_tool_result(
            "Bash",
            &serde_json::json!({"command": "true"}),
            &serde_json::json!(""),
            None,
            None,
            &[],
            &tx,
            &waker,
        );

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::ToolResult(tool) => {
                assert!(
                    tool.output.is_none(),
                    "empty output should not render a block"
                );
            }
            _ => panic!("expected ToolResult"),
        }
    }

    // ---- edge cases ----

    #[test]
    fn messages_to_prompt_multiple_systems_takes_first_only() {
        let msgs = vec![
            Message::System("First system".into()),
            Message::System("Second system".into()),
            Message::User("hello".into()),
        ];
        let prompt = messages_to_prompt(&msgs);
        assert!(prompt.contains("First system"));
        assert!(!prompt.contains("Second system"));
    }

    #[test]
    fn get_pending_all_users() {
        let msgs = vec![
            Message::User("a".into()),
            Message::User("b".into()),
            Message::User("c".into()),
        ];
        assert_eq!(get_pending_user_messages(&msgs), "a\nb\nc");
    }

    #[test]
    fn forward_permission_exit_plan_non_string_plan_gives_none() {
        let (tx, rx) = mpsc::channel();
        let waker = Waker::noop();
        // "plan" key is a number, not a string
        let input = serde_json::json!({"plan": 123});
        let result = forward_permission_to_ui("ExitPlanMode", input, &tx, &waker);
        assert!(result.is_some());

        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::PermissionRequest(pending) => {
                // Non-string "plan" should gracefully result in None
                assert!(pending.request.view.plan_markdown().is_none());
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    // ---- complete_subagent ----

    #[test]
    fn complete_subagent_removes_from_stack() {
        let (tx, rx) = mpsc::channel();
        let waker = CountingWaker::new();
        let mut stack = vec!["task-a".to_string(), "task-b".to_string()];
        complete_subagent("task-a", "done", &mut stack, &tx, waker.waker());

        assert_eq!(stack, vec!["task-b".to_string()]);
        assert_eq!(
            waker.wakes(),
            1,
            "a finished subagent must repaint, or the sidebar entry stays running"
        );
        let resp = rx.try_recv().unwrap();
        match resp {
            DaveApiResponse::SubagentCompleted { task_id, result } => {
                assert_eq!(task_id, "task-a");
                assert_eq!(result, "done");
            }
            _ => panic!("expected SubagentCompleted"),
        }
    }
}
