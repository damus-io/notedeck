//! Transcript filtering and rendering, shared by `log` and `grep` (and the nd
//! tools behind them).

use super::term::{paint, SGR_NEEDS_INPUT};
use crate::messages::{Message, PermissionResponseType, SubagentStatus};
use crate::tools::{ToolCall, ToolResponse, ToolResponses};

/// Which messages of a transcript to show: the roles to keep, whether to fold
/// tool-call/result noise, and how many trailing messages to show — the
/// `--role`/`--tools`/`--last` flags `log` and `grep` share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageFilter {
    /// `--role <r>[,<r>…]`: keep only messages whose canonical role token (see
    /// [`message_role`]) matches one of these, case-insensitively. Empty keeps
    /// all roles.
    pub roles: Vec<String>,
    /// `--last N` (`-n N`): after role/tool filtering, keep only the trailing `N`.
    pub last: Option<usize>,
    /// `--tools`/`--no-tools`: when `false`, drop `tool_call`/`tool_result`
    /// messages so the human turns read cleanly. Defaults to `false` (fold) —
    /// a transcript is mostly tool noise, and the conversation is what you open
    /// `log` for; `--tools` opts the tool traffic back in.
    pub show_tools: bool,
}

impl MessageFilter {
    /// Whether a single message survives the role/tool filters — the per-message
    /// predicate shared by [`select`](Self::select) (which then also
    /// tails with `--last`) and a live follow (which filters each new message
    /// the same way but never tails it).
    pub fn keep(&self, m: &Message) -> bool {
        self.keeps_role(message_role(m))
    }

    /// Whether a message whose canonical role token (see [`message_role`]) is
    /// `role` survives the role/tool filters: [`keep`](Self::keep) for when
    /// only the role is known, as it is to `grep`'s prefilter, which reads a
    /// note's `role` tag without rendering the note.
    pub fn keeps_role(&self, role: &str) -> bool {
        let role_ok =
            self.roles.is_empty() || self.roles.iter().any(|r| role.eq_ignore_ascii_case(r));
        role_ok && (self.show_tools || !TOOL_ROLES.contains(&role))
    }

    /// Apply the role/tool/last filters to an ordered message slice, returning
    /// borrowed references in the same order. Order of operations: role filter,
    /// then tool fold, then the `--last` tail — so `--last N` counts the
    /// messages actually shown, not ones a filter already dropped.
    pub fn select<'a>(&self, messages: &'a [Message]) -> Vec<&'a Message> {
        let mut kept: Vec<&Message> = messages.iter().filter(|m| self.keep(m)).collect();
        if let Some(n) = self.last {
            let start = kept.len().saturating_sub(n);
            kept.drain(..start);
        }
        kept
    }
}

/// Index of the first item whose order key is strictly greater than `last`
/// (`0` when `last` is `None`, i.e. nothing printed yet — take everything).
///
/// The input keys must be ascending (as [`load_session_messages_for_author`]
/// returns them); this then partitions them into "already printed" / "new" by
/// the order key *itself*, never by a message count. That is the out-of-order
/// guard the live follower needs: a message that arrives late and sorts before
/// the previous max is simply not in the `> last` suffix, so it can never shove
/// an already-printed message back into the tail (which a count-based cursor
/// would do). Live callers key off [`EventOrder`]; the generic bound keeps the
/// selection unit-testable with plain integers.
///
/// [`load_session_messages_for_author`]: crate::session_loader::load_session_messages_for_author
/// [`EventOrder`]: crate::session_loader::EventOrder
pub fn first_after<T: Ord + Copy>(orders: &[T], last: Option<T>) -> usize {
    match last {
        None => 0,
        Some(last) => orders.partition_point(|o| *o <= last),
    }
}

/// The canonical role token for a message — the axis `--role` filters on and the
/// `role` tag the `--json` view carries. Collapses each [`Message`] variant to
/// the kind-1988 role vocabulary a reader would type.
pub fn message_role(m: &Message) -> &'static str {
    match m {
        Message::User(_) => "user",
        Message::Assistant(_) => "assistant",
        Message::ToolCalls(_) => "tool_call",
        Message::ToolRunning(_) => "tool_running",
        Message::ToolResponse(_) => "tool_result",
        Message::PermissionRequest(_) => "permission_request",
        Message::CompactionComplete(_) => "compaction",
        Message::Subagent(_) => "subagent",
        Message::System(_) => "system",
        Message::Error(_) => "error",
        Message::TodoUpdate(_) => "todo",
    }
}

/// The [`message_role`] tokens of tool-call/result noise, which the default
/// view folds away (and `--tools` keeps).
const TOOL_ROLES: [&str; 3] = ["tool_call", "tool_running", "tool_result"];

/// Whether a message is tool-call/result noise (see [`TOOL_ROLES`]).
#[cfg(test)]
fn is_tool_message(m: &Message) -> bool {
    TOOL_ROLES.contains(&message_role(m))
}

/// How a message role is drawn in the terminal: see [`role_style`].
pub struct RoleStyle {
    /// The role in words.
    pub label: &'static str,
    /// The SGR color code it is painted in.
    pub sgr: &'static str,
}

/// Terminal presentation for a message role: a human label and an SGR color.
/// Mirrors [`message_role`]'s vocabulary; used to head each rendered entry.
pub fn role_style(m: &Message) -> RoleStyle {
    let (label, sgr) = match m {
        Message::User(_) => ("user", "36"),
        Message::Assistant(_) => ("assistant", "32"),
        Message::ToolCalls(_) => ("tool", "35"),
        Message::ToolRunning(_) => ("running", "36"),
        Message::ToolResponse(_) => ("result", "90"),
        Message::PermissionRequest(_) => ("permission", SGR_NEEDS_INPUT),
        Message::CompactionComplete(_) => ("compaction", "90"),
        Message::Subagent(_) => ("subagent", "34"),
        Message::System(_) => ("system", "90"),
        Message::Error(_) => ("error", "31"),
        Message::TodoUpdate(_) => ("todo", "90"),
    };
    RoleStyle { label, sgr }
}

/// Render a filtered message stream as plain text, one entry per message
/// separated by a blank line. Returns an owned `String` (like [`render_detail`](super::show))
/// so the layout is unit-testable; ANSI color is applied only via [`paint`] when
/// `color` (stdout is a tty).
pub fn render_messages(messages: &[&Message], color: bool) -> String {
    let mut out = String::new();
    for (i, m) in messages.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&render_message(m, color));
    }
    out
}

/// Render a single message: a colored role header line, then its body indented
/// two spaces. Every [`Message`] variant is handled so the transcript never
/// silently drops an entry.
pub fn render_message(m: &Message, color: bool) -> String {
    let RoleStyle { label, sgr } = role_style(m);
    let body = message_body(m);

    let mut out = paint(color, sgr, label);
    out.push('\n');
    for line in body.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// A message's rendered body text, without the role header — the text a reader
/// sees, and so also the text [`grep`](super::grep::search) searches. Every [`Message`] variant is
/// handled so neither the transcript nor a search silently drops an entry.
pub fn message_body(m: &Message) -> String {
    match m {
        Message::User(u) => {
            let mut b = u.text.clone();
            if !u.images.is_empty() {
                if !b.is_empty() {
                    b.push('\n');
                }
                b.push_str(&format!("[{} image(s)]", u.images.len()));
            }
            b
        }
        Message::Assistant(a) => a.text().to_string(),
        Message::ToolCalls(calls) => calls
            .iter()
            .map(render_tool_call)
            .collect::<Vec<_>>()
            .join("\n"),
        Message::ToolRunning(rt) => {
            if rt.summary.is_empty() {
                format!("{} (running)", rt.tool_name)
            } else {
                format!("{} {} (running)", rt.tool_name, rt.summary)
            }
        }
        Message::ToolResponse(tr) => render_tool_response(tr),
        Message::PermissionRequest(p) => {
            format!("{}  [{}]", p.tool_name, decision_label(p.response))
        }
        Message::CompactionComplete(c) => format!("{} tokens before compaction", c.pre_tokens),
        Message::Subagent(s) => format!(
            "{}: {}  [{}]",
            s.subagent_type,
            s.description,
            subagent_status_label(s.status)
        ),
        Message::System(s) => s.text().to_owned(),
        Message::Error(e) => e.clone(),
        Message::TodoUpdate(v) => todo_summary(v),
    }
}

/// One line for a tool call: its registry name plus a one-line summary of the
/// arguments JSON (whitespace-flattened and truncated).
fn render_tool_call(call: &ToolCall) -> String {
    let name = call.calls().tool_name();
    let args = one_line(&call.calls().arguments(), 100);
    if args.is_empty() {
        name.to_string()
    } else {
        format!("{name}  {args}")
    }
}

/// One line for a tool response, keyed on the underlying [`ToolResponses`]
/// variant. The loader's message stream only ever builds `ExecutedTool`, but the
/// other variants are handled so a directly-rendered `ToolResponse` never
/// silently vanishes.
fn render_tool_response(tr: &ToolResponse) -> String {
    match tr.responses() {
        ToolResponses::ExecutedTool(e) => {
            if e.summary.is_empty() {
                e.tool_name.clone()
            } else {
                format!("{}: {}", e.tool_name, one_line(&e.summary, 200))
            }
        }
        ToolResponses::Error(msg) => format!("error: {}", one_line(msg, 200)),
        ToolResponses::Query(q) => format!("query: {} notes", q.notes.len()),
        ToolResponses::PresentNotes(n) => format!("present: {n} notes"),
        ToolResponses::App(r) => format!("{}: {}", r.tool, one_line(&r.value.to_string(), 200)),
    }
}

/// The bracketed decision shown on a permission request row.
fn decision_label(response: Option<PermissionResponseType>) -> &'static str {
    match response {
        Some(PermissionResponseType::Allowed) => "allowed",
        Some(PermissionResponseType::Denied) => "denied",
        None => "pending",
    }
}

/// A subagent's status as a lowercase word.
fn subagent_status_label(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Running => "running",
        SubagentStatus::Completed => "completed",
        SubagentStatus::Failed => "failed",
    }
}

/// A one-line summary of a `TodoWrite` payload: the number of items, plus the
/// content of the in-progress one when the payload has the expected shape.
/// Falls back to a flattened one-line dump for anything unexpected.
fn todo_summary(v: &serde_json::Value) -> String {
    let Some(todos) = v.get("todos").and_then(|t| t.as_array()) else {
        return one_line(&v.to_string(), 120);
    };
    let active = todos
        .iter()
        .find(|t| t.get("status").and_then(|s| s.as_str()) == Some("in_progress"))
        .and_then(|t| t.get("content").and_then(|c| c.as_str()));
    match active {
        Some(content) => format!("{} item(s), doing: {}", todos.len(), one_line(content, 80)),
        None => format!("{} item(s)", todos.len()),
    }
}

/// Flatten `s` to a single line (runs of whitespace, including newlines,
/// collapse to one space) and truncate to `max` display chars with an ellipsis.
/// Keeps a summary scannable on one row regardless of the source's line breaks.
fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    col_ellipsis(&flat, max)
}

/// Truncate to `max` chars with a trailing `…` when cut; unlike [`col`](super::term::col), does
/// not pad (transcript bodies aren't columnar).
fn col_ellipsis(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let mut t: String = chars[..max.saturating_sub(1)].iter().collect();
    t.push('…');
    t
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::messages::{
        AssistantMessage, CompactionInfo, ExecutedTool, Message, PermissionRequest,
        PermissionResponseType, SubagentInfo, SubagentStatus,
    };
    use crate::tools::{QueryCall, ToolCall, ToolCalls, ToolResponse};

    /// A `tool_result` message carrying an [`ExecutedTool`], as the loader builds.
    fn tool_result(name: &str, summary: &str) -> Message {
        Message::ToolResponse(ToolResponse::executed_tool(ExecutedTool::new(
            name, summary,
        )))
    }

    /// A show-everything filter: no role filter, no tail, tools shown. Note this is deliberately *not* the parsed default (which
    /// folds tools — see [`show_tools_defaults_off_and_tools_opts_back_in`]);
    /// it's the widest view, so a test can narrow one axis at a time.
    pub(crate) fn view_all() -> MessageFilter {
        MessageFilter {
            roles: vec![],
            last: None,
            show_tools: true,
        }
    }

    #[test]
    fn message_role_tokens_cover_every_variant() {
        assert_eq!(message_role(&Message::User("x".into())), "user");
        assert_eq!(
            message_role(&Message::Assistant(AssistantMessage::from_text("x".into()))),
            "assistant"
        );
        assert_eq!(message_role(&tool_result("Bash", "ok")), "tool_result");
        assert_eq!(message_role(&Message::System("x".into())), "system");
        assert_eq!(message_role(&Message::Error("x".into())), "error");
        assert_eq!(
            message_role(&Message::CompactionComplete(CompactionInfo {
                pre_tokens: 1
            })),
            "compaction"
        );
        assert_eq!(
            message_role(&Message::TodoUpdate(serde_json::json!({}))),
            "todo"
        );
    }

    #[test]
    fn select_filters_by_role_case_insensitively() {
        let msgs = vec![
            Message::User("hello".into()),
            Message::Assistant(AssistantMessage::from_text("hi".into())),
            Message::User("again".into()),
        ];
        let view = MessageFilter {
            roles: vec!["USER".into()],
            ..view_all()
        };
        let kept = view.select(&msgs);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|m| message_role(m) == "user"));
    }

    #[test]
    fn select_keeps_any_of_multiple_roles() {
        let msgs = vec![
            Message::User("hello".into()),
            Message::Assistant(AssistantMessage::from_text("hi".into())),
            Message::System("boot".into()),
        ];
        // `--role user,assistant` (or repeated flags) keeps both, drops system.
        let view = MessageFilter {
            roles: vec!["user".into(), "assistant".into()],
            ..view_all()
        };
        let kept = view.select(&msgs);
        assert_eq!(kept.len(), 2);
        assert!(kept
            .iter()
            .all(|m| matches!(message_role(m), "user" | "assistant")));
    }

    #[test]
    fn select_folds_tool_noise_with_no_tools() {
        let msgs = vec![
            Message::User("hello".into()),
            tool_result("Bash", "exit 0"),
            Message::Assistant(AssistantMessage::from_text("done".into())),
        ];
        // The show-everything view keeps the tool_result; folding drops it.
        assert_eq!(view_all().select(&msgs).len(), 3);
        let folded = MessageFilter {
            show_tools: false,
            ..view_all()
        };
        let kept = folded.select(&msgs);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|m| !is_tool_message(m)));
    }

    #[test]
    fn select_last_tails_after_other_filters() {
        let msgs = vec![
            Message::User("1".into()),
            tool_result("Bash", "ok"),
            Message::User("2".into()),
            Message::User("3".into()),
        ];
        // --no-tools leaves 3 user messages; --last 2 keeps the final two.
        let view = MessageFilter {
            last: Some(2),
            show_tools: false,
            ..view_all()
        };
        let kept = view.select(&msgs);
        let texts: Vec<&str> = kept
            .iter()
            .filter_map(|m| match m {
                Message::User(u) => Some(u.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["2", "3"]);
    }

    #[test]
    fn render_messages_plain_handles_every_variant() {
        let subagent = SubagentInfo {
            status: SubagentStatus::Completed,
            max_output_size: 0,
            ..SubagentInfo::running("t1", "explore the tree", "Explore")
        };
        let msgs = vec![
            Message::System("session started".into()),
            Message::User("hi dave".into()),
            Message::Assistant(AssistantMessage::from_text("**hello**".into())),
            Message::ToolCalls(vec![ToolCall::new(
                "id1".into(),
                ToolCalls::Query(QueryCall::default()),
            )]),
            tool_result("Bash", "exit 0"),
            Message::PermissionRequest(PermissionRequest::new(
                uuid::Uuid::nil(),
                "Write".into(),
                serde_json::Value::Null,
                None,
                Some(PermissionResponseType::Denied),
                None,
            )),
            Message::CompactionComplete(CompactionInfo { pre_tokens: 12000 }),
            Message::Subagent(subagent),
            Message::TodoUpdate(serde_json::json!({
                "todos": [
                    {"content": "first", "status": "completed"},
                    {"content": "second", "status": "in_progress"},
                ]
            })),
            Message::Error("kaboom".into()),
        ];
        let refs: Vec<&Message> = msgs.iter().collect();
        let out = render_messages(&refs, false);

        assert!(!out.contains('\x1b'), "no ANSI when color=false: {out:?}");
        // Each role header renders.
        for label in [
            "system",
            "user",
            "assistant",
            "tool",
            "result",
            "permission",
            "compaction",
            "subagent",
            "todo",
            "error",
        ] {
            assert!(out.contains(label), "missing {label:?} header in:\n{out}");
        }
        // Bodies render (indented) — assistant markdown kept raw.
        assert!(out.contains("  hi dave"));
        assert!(out.contains("  **hello**"));
        assert!(out.contains("Write  [denied]"));
        assert!(out.contains("12000 tokens before compaction"));
        assert!(out.contains("Explore: explore the tree  [completed]"));
        assert!(out.contains("2 item(s), doing: second"));
        assert!(out.contains("  kaboom"));
    }

    #[test]
    fn render_message_colored_wraps_only_the_header() {
        let out = render_message(&Message::User("body".into()), true);
        // The header is colored; the indented body is not repainted.
        assert!(out.starts_with("\x1b[36muser\x1b[0m\n"));
        assert!(out.contains("\n  body\n"));
    }

    #[test]
    fn first_after_selects_the_strictly_greater_suffix() {
        let orders = [10u64, 20, 30];
        // Nothing printed yet: take everything.
        assert_eq!(first_after(&orders, None), 0);
        // Past a middle key: only the strictly-greater tail.
        assert_eq!(first_after(&orders, Some(10)), 1);
        assert_eq!(first_after(&orders, Some(20)), 2);
        // Past the max: nothing new.
        assert_eq!(first_after(&orders, Some(30)), 3);
        // A cursor between keys still splits by value, not position.
        assert_eq!(first_after(&orders, Some(15)), 1);
    }

    #[test]
    fn first_after_guards_out_of_order_inserts() {
        // We followed up to the max (30) of [10, 20, 30]. A later re-read shows a
        // late-arriving event (15) that sorts *before* that max, plus a genuinely
        // newer one (40). Selecting by the order key past the previous max yields
        // only the suffix (40) — the out-of-order 15 is neither re-emitted nor
        // does it drag an already-printed message (10/20/30) back into view, the
        // corruption a count-based cursor would cause.
        let reread = [10u64, 15, 20, 30, 40];
        let start = first_after(&reread, Some(30));
        assert_eq!(&reread[start..], &[40]);
    }

    #[test]
    fn keep_matches_role_and_tool_filters() {
        let user = Message::User("hi".into());
        let tool = tool_result("Bash", "ok");
        // Empty role filter keeps everything; view_all shows tools.
        assert!(view_all().keep(&user));
        assert!(view_all().keep(&tool));
        // --no-tools drops tool messages but keeps others.
        let no_tools = MessageFilter {
            show_tools: false,
            ..view_all()
        };
        assert!(no_tools.keep(&user));
        assert!(!no_tools.keep(&tool));
        // A role filter keeps only the named role (case-insensitively).
        let only_user = MessageFilter {
            roles: vec!["USER".into()],
            ..view_all()
        };
        assert!(only_user.keep(&user));
        assert!(!only_user.keep(&tool));
    }

    #[test]
    fn one_line_flattens_whitespace_and_truncates() {
        assert_eq!(one_line("a\n  b\tc", 100), "a b c");
        assert_eq!(one_line("abcdef", 4), "abc…");
        assert_eq!(one_line("abc", 3), "abc");
    }

    #[test]
    fn todo_summary_reports_count_and_active() {
        let none_active = serde_json::json!({"todos": [{"content": "x", "status": "pending"}]});
        assert_eq!(todo_summary(&none_active), "1 item(s)");
        let no_shape = serde_json::json!({"unexpected": true});
        assert!(todo_summary(&no_shape).contains("unexpected"));
    }

    #[test]
    fn message_to_json_is_tagged_by_role() {
        // The CLI's `--json` output is `Message::to_json` per message; check the
        // role-tagged shape the integration test relies on.
        let v = Message::User("hi".into()).to_json();
        assert_eq!(v["role"], "user");
        assert_eq!(v["text"], "hi");
        // A text-only user message omits the images field.
        assert!(v.get("images").is_none());

        let v = Message::PermissionRequest(PermissionRequest::new(
            uuid::Uuid::nil(),
            "Bash".into(),
            serde_json::Value::Null,
            None,
            Some(PermissionResponseType::Allowed),
            None,
        ))
        .to_json();
        assert_eq!(v["role"], "permission_request");
        assert_eq!(v["tool"], "Bash");
        assert_eq!(v["decision"], "allowed");

        let v = tool_result("Read", "154 lines").to_json();
        assert_eq!(v["role"], "tool_result");
        assert_eq!(v["tool"], "Read");
        assert_eq!(v["summary"], "154 lines");
    }
}
