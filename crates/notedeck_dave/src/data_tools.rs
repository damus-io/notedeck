//! Data tools reading agentium sessions — the list, one session's detail, its
//! transcript, a search across transcripts, and a d-tag's sayable ref — over
//! the shared per-app tool interface ([`notedeck::AppTool`]), one per
//! `agentium` CLI read, so `nd agentium list|show|log|grep|id` is that CLI.
//!
//! Each tool answers from nostrdb alone, never from Dave's session list or
//! its [`session_cache`](crate::session_cache): the shell lists a stopped
//! app's tools and runs a call before the Dave it starts has updated once, so
//! the first `nd agentium list` in a window with Dave closed would otherwise
//! answer empty. Reads select, resolve and render with
//! [`agentium_core::cli`], the same code the CLI uses. None of them runs a
//! backend or publishes, so they work in a headless notedeck that is not a
//! Dave host.

use agentium_core::cli::grep::{self, SessionMatches};
use agentium_core::cli::list::{load_sessions, render_list, ListFilters, ListScope, SessionJson};
use agentium_core::cli::log::Transcript;
use agentium_core::cli::show::SessionDetail;
use agentium_core::cli::term::now_secs;
use agentium_core::cli::transcript::MessageFilter;
use agentium_core::session_loader::SessionState;
use cli_term::CaseMode;
use nostrdb::Transaction;
use notedeck::{AppTool, RegisteredTool, ToolArg, ToolArgType, ToolContext, ToolSpec};
use regex::Regex;
use serde::{Deserialize, Serialize, Serializer};

/// What the agentium tools are filed under in a command bar.
const TOOL_CATEGORY: &str = "Agentium";

/// The variable an agent's own session ref is in, which `show` and `log`
/// fall back to when no session is named.
const SESSION_ENV: &str = "AGENTIUM_SESSION";

/// Every data tool Dave contributes, ready to register.
pub fn tools() -> Vec<RegisteredTool> {
    vec![
        RegisteredTool::new(ListSessions),
        RegisteredTool::new(ShowSession),
        RegisteredTool::new(SessionLog),
        RegisteredTool::new(GrepSessions),
        RegisteredTool::new(SessionRef),
    ]
}

/// A read transaction, its error worded for the caller.
fn open_txn(cx: &ToolContext) -> Result<Transaction, String> {
    Transaction::new(cx.ndb).map_err(|e| format!("opening a db transaction: {e}"))
}

/// The session argument `show` and `log` take.
fn session_arg() -> ToolArg {
    ToolArg::new(
        "session",
        ToolArgType::String,
        "The session: an `agentium:<word-id>` ref, a word-id, a session id or a unique prefix. Deleted sessions resolve too. Defaults to `$AGENTIUM_SESSION`.",
    )
    .positional()
    .env(SESSION_ENV)
}

// ---------------------------------------------------------------------------
// session selection: the `list` filters, shared with `grep`
// ---------------------------------------------------------------------------

/// The `list` filters, as arguments.
#[derive(Deserialize)]
struct FilterArgs {
    /// Hostname substring.
    #[serde(default)]
    host: Option<String>,
    /// Exact status token.
    #[serde(default)]
    status: Option<String>,
    /// Working-directory substring.
    #[serde(default)]
    cwd: Option<String>,
    /// Backend substring.
    #[serde(default)]
    backend: Option<String>,
    /// Only deleted sessions.
    #[serde(default)]
    deleted: bool,
    /// Live and deleted sessions.
    #[serde(default)]
    all: bool,
}

impl FilterArgs {
    /// The row filters.
    fn filters(&self) -> ListFilters {
        ListFilters {
            host: self.host.clone(),
            status: self.status.clone(),
            cwd: self.cwd.clone(),
            backend: self.backend.clone(),
        }
    }
}

/// The `list` filter arguments, after any tool-specific ones.
fn filter_args(scope_default: &'static str) -> [ToolArg; 6] {
    [
        ToolArg::new(
            "host",
            ToolArgType::String,
            "Keep sessions whose hostname contains this (case-insensitive).",
        ),
        ToolArg::new(
            "status",
            ToolArgType::String,
            "Keep sessions with exactly this status: idle, working, needs_input, error, done, pending, waiting or deleted.",
        ),
        ToolArg::new(
            "cwd",
            ToolArgType::String,
            "Keep sessions whose working directory contains this (case-insensitive).",
        ),
        ToolArg::new(
            "backend",
            ToolArgType::String,
            "Keep sessions whose backend contains this (case-insensitive), e.g. `claude`.",
        ),
        ToolArg::new("deleted", ToolArgType::Boolean, "Only deleted sessions."),
        ToolArg::new("all", ToolArgType::Boolean, scope_default),
    ]
}

/// Sessions as `list --json` prints them: each state with its `agentium:`
/// URI beside it.
fn serialize_sessions<S: Serializer>(
    sessions: &[SessionState],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(sessions.iter().map(SessionJson::new))
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

/// `agentium_list_sessions`: this account's sessions.
struct ListSessions;

/// [`ListSessions`]' answer: the sessions, newest first.
struct Sessions(Vec<SessionState>);

impl Serialize for Sessions {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_sessions(&self.0, serializer)
    }
}

impl AppTool for ListSessions {
    type Args = FilterArgs;
    type Output = Sessions;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_list_sessions",
            "List this account's agentium (Dave) sessions, newest first, as `agentium list` does. Each is its kind-31988 state — `claude_session_id`, `title`, `status`, `hostname`, `cwd`, `backend`, `cli_session_id`, `issue_url`, … — plus `agentium_uri`, its sayable `agentium:<word-id>` ref. Deleted sessions are left out unless `deleted` or `all` is set.",
            filter_args("Live and deleted sessions together.").into(),
        )
        .titled("List agent sessions")
        .read_only()
        .command(&["agentium", "list"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let author = *cx.accounts.selected_account_pubkey();
        let txn = open_txn(cx)?;
        let scope = ListScope::from_flags(args.all, args.deleted);
        Ok(Sessions(load_sessions(
            cx.ndb,
            &txn,
            &author,
            &args.filters(),
            scope,
        )))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_list(&out.0, now_secs(), true))
    }
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

/// `agentium_show_session`: one session's detail.
struct ShowSession;

/// Arguments naming one session.
#[derive(Deserialize)]
struct SessionArgs {
    /// The selector; `None` when neither it nor `$AGENTIUM_SESSION` was given.
    #[serde(default)]
    session: Option<String>,
}

/// [`ShowSession`]'s answer.
struct Detail(SessionDetail);

impl Serialize for Detail {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.json().serialize(serializer)
    }
}

impl AppTool for ShowSession {
    type Args = SessionArgs;
    type Output = Detail;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_show_session",
            "Show one agentium session in detail, as `agentium show` does: `{session, run_configs, usage, conversation, subagents}`. `session` is the state `agentium_list_sessions` lists (with `cli_session_id`, the backend's own session id, and `agentium_uri`); `conversation` counts the messages and lists pending permission requests; `subagents` rolls up the subagents it spawned.",
            vec![session_arg()],
        )
        .titled("Show agent session")
        .read_only()
        .command(&["agentium", "show"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let author = *cx.accounts.selected_account_pubkey();
        let txn = open_txn(cx)?;
        SessionDetail::load(cx.ndb, &txn, &author, args.session.as_deref()).map(Detail)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.0.render(now_secs(), true))
    }
}

// ---------------------------------------------------------------------------
// log
// ---------------------------------------------------------------------------

/// The transcript filter, as arguments.
#[derive(Deserialize)]
struct ViewArgs {
    /// Roles to keep.
    #[serde(default)]
    role: Vec<String>,
    /// Keep only the trailing this-many.
    #[serde(default)]
    last: Option<usize>,
    /// Show tool calls and results.
    #[serde(default)]
    tools: bool,
    /// Fold tool calls and results (the default).
    #[serde(default)]
    no_tools: bool,
}

impl ViewArgs {
    /// The filter. `no_tools` wins over `tools`: it is the explicit default.
    fn filter(self) -> MessageFilter {
        MessageFilter {
            roles: self.role,
            last: self.last,
            show_tools: self.tools && !self.no_tools,
        }
    }
}

/// The transcript filter arguments.
fn view_args() -> [ToolArg; 4] {
    [
        ToolArg::new(
            "role",
            ToolArgType::List,
            "Keep only messages of these roles: user, assistant, tool_call, tool_running, tool_result, permission_request, compaction, subagent, system, error, todo.",
        ),
        ToolArg::new(
            "last",
            ToolArgType::Integer,
            "Keep only the last N messages, after the other filters.",
        )
        .short('n'),
        ToolArg::new(
            "tools",
            ToolArgType::Boolean,
            "Include tool calls and results, which are folded away by default.",
        ),
        ToolArg::new(
            "no_tools",
            ToolArgType::Boolean,
            "Fold tool calls and results away (the default).",
        ),
    ]
}

/// `agentium_session_log`: one session's transcript.
struct SessionLog;

/// Arguments for [`SessionLog`].
#[derive(Deserialize)]
struct LogArgs {
    /// The selector.
    #[serde(default)]
    session: Option<String>,
    /// The transcript filter.
    #[serde(flatten)]
    view: ViewArgs,
    /// The reconstructed claude-code JSONL instead.
    #[serde(default)]
    jsonl: bool,
}

/// [`SessionLog`]'s answer.
struct Log(Transcript);

impl Serialize for Log {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.json().serialize(serializer)
    }
}

impl AppTool for SessionLog {
    type Args = LogArgs;
    type Output = Log;

    fn spec(&self) -> ToolSpec {
        let mut arguments = vec![session_arg()];
        arguments.extend(view_args());
        arguments.push(ToolArg::new(
            "jsonl",
            ToolArgType::Boolean,
            "The claude-code JSONL reconstructed from the session's archive, one entry per line, instead of the conversation. The filters don't apply.",
        ));
        ToolSpec::new(
            "agentium_session_log",
            "Read one agentium session's conversation, in order, as `agentium log` does: one role-tagged object per message (`{role, text, …}`). Tool calls and results are folded away unless `tools` is set; `role` and `last` narrow it further. `jsonl` answers the reconstructed claude-code JSONL entries instead.",
            arguments,
        )
        .titled("Read agent session log")
        .read_only()
        .command(&["agentium", "log"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let author = *cx.accounts.selected_account_pubkey();
        let txn = open_txn(cx)?;
        let filter = args.view.filter();
        Transcript::load(
            cx.ndb,
            &txn,
            &author,
            args.session.as_deref(),
            &filter,
            args.jsonl,
        )
        .map(Log)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.0.render(true))
    }
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

/// `agentium_grep_sessions`: search every session's transcript.
struct GrepSessions;

/// Arguments for [`GrepSessions`].
#[derive(Deserialize)]
struct GrepArgs {
    /// The regex.
    pattern: String,
    /// Always fold case.
    #[serde(default)]
    ignore_case: bool,
    /// Never fold case.
    #[serde(default)]
    case_sensitive: bool,
    /// The transcript filter.
    #[serde(flatten)]
    view: ViewArgs,
    /// The session filters.
    #[serde(flatten)]
    filters: FilterArgs,
}

/// [`GrepSessions`]' answer: the sessions with a match, and the pattern that
/// highlights them.
struct Matches {
    hits: Vec<SessionMatches>,
    pattern: Regex,
}

impl Serialize for Matches {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        grep::json(&self.hits).serialize(serializer)
    }
}

impl AppTool for GrepSessions {
    type Args = GrepArgs;
    type Output = Matches;

    fn spec(&self) -> ToolSpec {
        let mut arguments = vec![
            ToolArg::new(
                "pattern",
                ToolArgType::String,
                "The regular expression, matched per line. Smart case: all-lowercase folds case, any capital matches exactly.",
            )
            .required(true)
            .positional(),
            ToolArg::new("ignore_case", ToolArgType::Boolean, "Always fold case.").short('i'),
            ToolArg::new("case_sensitive", ToolArgType::Boolean, "Never fold case.").short('s'),
        ];
        arguments.extend(view_args());
        arguments.extend(filter_args(
            "Accepted for the CLI's sake: deleted sessions are searched already unless `deleted` narrows to them.",
        ));
        ToolSpec::new(
            "agentium_grep_sessions",
            "Search the transcripts of this account's agentium sessions, as `agentium grep` does: every session `agentium_list_sessions` would list (deleted ones included) whose messages match `pattern` per line, each as its session state plus `matches: [{role, text}]`. Tool calls and results are folded away and not searched unless `tools` is set.",
            arguments,
        )
        .titled("Search agent sessions")
        .read_only()
        .command(&["agentium", "grep"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let case = match (args.ignore_case, args.case_sensitive) {
            (true, true) => return Err("pass one of ignore_case and case_sensitive".to_owned()),
            (true, false) => CaseMode::Insensitive,
            (false, true) => CaseMode::Sensitive,
            (false, false) => CaseMode::Smart,
        };
        let pattern = cli_term::compile_pattern(&args.pattern, case)?;
        // Every session, deleted ones included, unless `deleted` narrows to
        // them: the conversation you're hunting for is as likely to be in a
        // session you've since closed.
        let scope = if args.filters.deleted && !args.filters.all {
            ListScope::Deleted
        } else {
            ListScope::All
        };
        let author = *cx.accounts.selected_account_pubkey();
        let txn = open_txn(cx)?;
        let hits = grep::search(
            cx.ndb,
            &txn,
            &author,
            &args.filters.filters(),
            scope,
            &pattern,
            &args.view.filter(),
        );
        Ok(Matches { hits, pattern })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(grep::render(&out.hits, &out.pattern, true))
    }
}

// ---------------------------------------------------------------------------
// id
// ---------------------------------------------------------------------------

/// `agentium_session_ref`: the `agentium:` ref of a session id.
struct SessionRef;

/// Arguments for [`SessionRef`].
#[derive(Deserialize)]
struct RefArgs {
    /// Session ids (kind-31988 `d` tags).
    d_tags: Vec<String>,
}

impl AppTool for SessionRef {
    type Args = RefArgs;
    type Output = Vec<String>;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_session_ref",
            "The sayable `agentium:<word-id>` ref of each session id (a kind-31988 `d` tag, the `claude_session_id` field), as `agentium id` prints it. A pure computation: the sessions need not exist.",
            vec![ToolArg::new(
                "d_tags",
                ToolArgType::List,
                "Session ids to name.",
            )
            .required(true)
            .rest()],
        )
        .titled("Session ref")
        .read_only()
        .command(&["agentium", "id"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, _cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        if args.d_tags.is_empty() {
            return Err("pass at least one session id".to_owned());
        }
        Ok(args
            .d_tags
            .iter()
            .map(|d| agentium_core::wordid::session_ref(d))
            .collect())
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.iter().map(|r| format!("{r}\n")).collect())
    }
}

#[cfg(test)]
mod tests;
