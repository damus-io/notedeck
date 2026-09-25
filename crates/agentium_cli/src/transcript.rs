//! `agentium messages` — transcript filtering, rendering, and JSON view,
//! shared by `log` and `grep`.

use agentium_core::messages::{Message, PermissionResponseType, SubagentStatus};
use agentium_core::tools::{ToolCall, ToolResponse, ToolResponses};
use nostrdb_net::relay::sync::Result;

use crate::term::{SGR_NEEDS_INPUT, paint};

/// The transcript-shaping flags for `agentium log`: which roles to keep,
/// whether to fold tool-call/result noise, and how many trailing messages to
/// show. `jsonl` selects the reconstructed-archive path instead (see
/// [`cmd_log`](crate::log::cmd_log)); it's carried here so the whole command's shape lives in
/// one value.
pub(crate) struct MessageView {
    /// `--role <r>[,<r>…]`: keep only messages whose canonical role token (see
    /// [`message_role`]) matches one of these, case-insensitively. Accumulates
    /// across repeated flags and comma-separated lists; empty keeps all roles.
    pub(crate) roles: Vec<String>,
    /// `--last N` (`-n N`): after role/tool filtering, keep only the trailing `N`.
    pub(crate) last: Option<usize>,
    /// `--tools`/`--no-tools`: when `false`, drop `tool_call`/`tool_result`
    /// messages so the human turns read cleanly. Defaults to `false` (fold) —
    /// a transcript is mostly tool noise, and the conversation is what you open
    /// `log` for; `--tools` opts the tool traffic back in. `--no-tools` remains
    /// as the (now redundant) explicit form.
    pub(crate) show_tools: bool,
    /// `--jsonl`: emit reconstructed claude-code JSONL instead of the rendered
    /// transcript. Mutually exclusive in effect with the filters above.
    pub(crate) jsonl: bool,
    /// `--color <when>`: whether to ANSI-color the rendered transcript.
    pub(crate) color: ColorWhen,
    /// `--pager`/`--no-pager`: whether to route output through a pager, like
    /// `git log`.
    pub(crate) pager: PagerMode,
    /// `--follow`/`-f`: after printing the current tail, keep streaming each new
    /// message as it lands (`git log`'s transcript, followed like `tail -f`)
    /// until Ctrl-C. A live stream can't be paged or reconstructed as a
    /// point-in-time archive, so it conflicts with `--pager`/`--jsonl` (see
    /// [`MessageView::check_follow`]).
    pub(crate) follow: bool,
}

/// When to ANSI-color the rendered transcript (`--color`). `Auto` follows the
/// effective sink (a tty or a color-aware pager); `Always`/`Never` force it —
/// `Always` is how you keep color when piping into your own `less -R`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ColorWhen {
    Auto,
    Always,
    Never,
}

impl ColorWhen {
    /// Parse the `--color` value; anything else is an error naming the choices.
    pub(crate) fn parse(s: &str) -> Result<ColorWhen> {
        match s {
            "auto" => Ok(ColorWhen::Auto),
            "always" => Ok(ColorWhen::Always),
            "never" => Ok(ColorWhen::Never),
            other => Err(format!("--color must be auto|always|never, got '{other}'").into()),
        }
    }

    /// Resolve to on/off. `sink_supports_color` is whether the effective output
    /// (tty or color-aware pager) can render ANSI — the `Auto` signal.
    pub(crate) fn enabled(self, sink_supports_color: bool) -> bool {
        match self {
            ColorWhen::Auto => sink_supports_color,
            ColorWhen::Always => true,
            ColorWhen::Never => false,
        }
    }
}

/// Whether to page `log` output (`--pager`/`--no-pager`). `Auto` pages only when
/// stdout is a tty (so a pipe stays unpaged); the flags force it either way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PagerMode {
    Auto,
    Always,
    Never,
}

impl PagerMode {
    /// Resolve to on/off given whether stdout is a terminal.
    pub(crate) fn enabled(self, stdout_tty: bool) -> bool {
        match self {
            PagerMode::Auto => stdout_tty,
            PagerMode::Always => true,
            PagerMode::Never => false,
        }
    }
}

impl MessageView {
    /// Whether a single message survives the role/tool filters — the per-message
    /// predicate shared by [`select`](MessageView::select) (which then also
    /// tails with `--last`) and the live `--follow` stream (which filters each
    /// new message the same way but never tails it).
    pub(crate) fn keep(&self, m: &Message) -> bool {
        let role_ok = self.roles.is_empty()
            || self
                .roles
                .iter()
                .any(|r| message_role(m).eq_ignore_ascii_case(r));
        role_ok && (self.show_tools || !is_tool_message(m))
    }

    /// Apply the role/tool/last filters to an ordered message slice, returning
    /// borrowed references in the same order. Order of operations: role filter,
    /// then tool fold, then the `--last` tail — so `--last N` counts the
    /// messages actually shown, not ones a filter already dropped.
    pub(crate) fn select<'a>(&self, messages: &'a [Message]) -> Vec<&'a Message> {
        let mut kept: Vec<&Message> = messages.iter().filter(|m| self.keep(m)).collect();
        if let Some(n) = self.last {
            let start = kept.len().saturating_sub(n);
            kept.drain(..start);
        }
        kept
    }

    /// Reject the flag combinations that a live `--follow` can't honor. A live
    /// stream can't be paged (there is no end to page over), and the `--jsonl`
    /// reconstruction is a point-in-time dump of a finished archive, not a
    /// stream — so both conflict with `--follow`. Returns the reason on
    /// conflict; a no-op when `--follow` isn't set.
    pub(crate) fn check_follow(&self) -> Result<()> {
        if !self.follow {
            return Ok(());
        }
        if self.jsonl {
            return Err(
                "cannot --follow --jsonl: the reconstructed source archive is a \
                 point-in-time dump, not a live stream"
                    .into(),
            );
        }
        if self.pager == PagerMode::Always {
            return Err("cannot page a live follow: --follow and --pager conflict".into());
        }
        Ok(())
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
/// [`load_session_messages_for_author`]: agentium_core::session_loader::load_session_messages_for_author
/// [`EventOrder`]: agentium_core::session_loader::EventOrder
pub(crate) fn first_after<T: Ord + Copy>(orders: &[T], last: Option<T>) -> usize {
    match last {
        None => 0,
        Some(last) => orders.partition_point(|o| *o <= last),
    }
}

/// The canonical role token for a message — the axis `--role` filters on and the
/// `role` tag the `--json` view carries. Collapses each [`Message`] variant to
/// the kind-1988 role vocabulary a reader would type.
pub(crate) fn message_role(m: &Message) -> &'static str {
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

/// Whether a message is tool-call/result noise that the default view folds
/// away (and `--tools` keeps).
fn is_tool_message(m: &Message) -> bool {
    matches!(m, Message::ToolCalls(_) | Message::ToolResponse(_))
}

/// Terminal presentation for a message role: a human label and an SGR color.
/// Mirrors [`message_role`]'s vocabulary; used to head each rendered entry.
pub(crate) fn role_style(m: &Message) -> (&'static str, &'static str) {
    match m {
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
    }
}

/// Render a filtered message stream as plain text, one entry per message
/// separated by a blank line. Returns an owned `String` (like [`render_detail`](crate::render_detail))
/// so the layout is unit-testable; ANSI color is applied only via [`paint`] when
/// `color` (stdout is a tty).
pub(crate) fn render_messages(messages: &[&Message], color: bool) -> String {
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
pub(crate) fn render_message(m: &Message, color: bool) -> String {
    let (label, sgr) = role_style(m);
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
/// sees, and so also the text [`cmd_grep`](crate::grep::cmd_grep) searches. Every [`Message`] variant is
/// handled so neither the transcript nor a search silently drops an entry.
pub(crate) fn message_body(m: &Message) -> String {
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
        Message::System(s) => s.clone(),
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

/// Truncate to `max` chars with a trailing `…` when cut; unlike [`col`](crate::term::col), does
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
    use agentium_core::messages::{
        AssistantMessage, CompactionInfo, ExecutedTool, Message, PermissionRequest,
        PermissionResponseType, SubagentInfo, SubagentStatus,
    };
    use agentium_core::tools::{QueryCall, ToolCall, ToolCalls, ToolResponse};

    /// A `tool_result` message carrying an [`ExecutedTool`], as the loader builds.
    fn tool_result(name: &str, summary: &str) -> Message {
        Message::ToolResponse(ToolResponse::executed_tool(ExecutedTool {
            tool_name: name.into(),
            summary: summary.into(),
            output: None,
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
        }))
    }

    /// A show-everything view: no role filter, no tail, tools shown, auto
    /// color/pager. Note this is deliberately *not* the parsed default (which
    /// folds tools — see [`show_tools_defaults_off_and_tools_opts_back_in`]);
    /// it's the widest view, so a test can narrow one axis at a time.
    pub(crate) fn view_all() -> MessageView {
        MessageView {
            roles: vec![],
            last: None,
            show_tools: true,
            jsonl: false,
            color: ColorWhen::Auto,
            pager: PagerMode::Auto,
            follow: false,
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
        let view = MessageView {
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
        let view = MessageView {
            roles: vec!["user".into(), "assistant".into()],
            ..view_all()
        };
        let kept = view.select(&msgs);
        assert_eq!(kept.len(), 2);
        assert!(
            kept.iter()
                .all(|m| matches!(message_role(m), "user" | "assistant"))
        );
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
        let folded = MessageView {
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
        let view = MessageView {
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
            task_id: "t1".into(),
            description: "explore the tree".into(),
            subagent_type: "Explore".into(),
            status: SubagentStatus::Completed,
            output: String::new(),
            max_output_size: 0,
            tool_results: vec![],
            background: false,
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
    fn color_when_resolves_against_sink() {
        // auto follows the sink; always/never override it.
        assert!(ColorWhen::Auto.enabled(true));
        assert!(!ColorWhen::Auto.enabled(false));
        assert!(ColorWhen::Always.enabled(false));
        assert!(!ColorWhen::Never.enabled(true));
        assert_eq!(ColorWhen::parse("always").unwrap(), ColorWhen::Always);
        assert!(ColorWhen::parse("technicolor").is_err());
    }

    #[test]
    fn pager_mode_resolves_against_tty() {
        // auto pages only for a tty; the flags force it either way.
        assert!(PagerMode::Auto.enabled(true));
        assert!(!PagerMode::Auto.enabled(false));
        assert!(PagerMode::Always.enabled(false));
        assert!(!PagerMode::Never.enabled(true));
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

    /// A `MessageView` with only `--follow` and one conflicting flag set,
    /// otherwise default — for exercising [`MessageView::check_follow`].
    fn follow_view(jsonl: bool, pager: PagerMode) -> MessageView {
        MessageView {
            follow: true,
            jsonl,
            pager,
            ..view_all()
        }
    }

    #[test]
    fn check_follow_rejects_paging_and_jsonl() {
        // A plain follow is fine, as is follow with a non-forced pager mode.
        assert!(follow_view(false, PagerMode::Auto).check_follow().is_ok());
        assert!(follow_view(false, PagerMode::Never).check_follow().is_ok());
        // --follow --jsonl and --follow --pager both conflict.
        assert!(follow_view(true, PagerMode::Auto).check_follow().is_err());
        assert!(
            follow_view(false, PagerMode::Always)
                .check_follow()
                .is_err()
        );
        // Without --follow the checks are a no-op even with those flags set.
        let not_following = MessageView {
            follow: false,
            jsonl: true,
            pager: PagerMode::Always,
            ..view_all()
        };
        assert!(not_following.check_follow().is_ok());
    }

    #[test]
    fn keep_matches_role_and_tool_filters() {
        let user = Message::User("hi".into());
        let tool = tool_result("Bash", "ok");
        // Empty role filter keeps everything; view_all shows tools.
        assert!(view_all().keep(&user));
        assert!(view_all().keep(&tool));
        // --no-tools drops tool messages but keeps others.
        let no_tools = MessageView {
            show_tools: false,
            ..view_all()
        };
        assert!(no_tools.keep(&user));
        assert!(!no_tools.keep(&tool));
        // A role filter keeps only the named role (case-insensitively).
        let only_user = MessageView {
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
