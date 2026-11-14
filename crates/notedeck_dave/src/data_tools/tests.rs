//! The `agentium` CLI's read cases (`list`, `show`, `log`, `grep`, `id`),
//! ported to the tools that replace it: each runs through the registry, as
//! `nd agentium <verb>` runs it, against a live notedeck signed in as a fresh
//! account whose sessions are seeded straight into its nostrdb. No Dave is
//! ever built, which is the case that matters: the shell calls these tools
//! before a stopped Dave has updated once.

use super::*;
use agentium_core::config::{RunConfig, AI_RUN_CONFIG_KIND};
use agentium_core::messages::{SubagentInfo, SubagentStatus};
use agentium_core::session_events::{
    build_permission_request_event, build_run_config_event_at, build_subagent_event,
    ThreadingState, AI_CONVERSATION_KIND, AI_SESSION_COMMAND_KIND, AI_SESSION_STATE_KIND,
    AI_SOURCE_DATA_KIND,
};
use nostrdb::{Filter, NoteBuilder, SubscriptionStream};
use nostrdb_net::FullKeypair;
use serde_json::{json, Value};
use tempfile::TempDir;

/// The host and home every seeded session runs on.
const HOST: &str = "macbook";
const HOME: &str = "/home/u";

/// A notedeck signed in as a fresh account. Bind it for the whole test: the
/// database lives in `_dir`.
struct Live {
    _dir: TempDir,
    notedeck: notedeck::Notedeck,
    secret: [u8; 32],
    /// Notes seeded and not yet waited for, and the stream that sees them.
    pending: usize,
    stream: SubscriptionStream,
}

/// What a tool answered: its JSON and the text `nd` prints for it, with its
/// color stripped.
#[derive(Debug)]
struct Answer {
    json: Value,
    text: String,
}

fn live() -> Live {
    let dir = TempDir::new().expect("tmp dir");
    let account = FullKeypair::generate();
    let args: Vec<String> = [
        "notedeck-test",
        "--testrunner",
        "--nsec",
        &account.secret_key.to_secret_hex(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut notedeck = notedeck::Notedeck::init(&egui::Context::default(), dir.path(), &args);
    let ndb = notedeck.app_context().ndb.clone();
    let filter = Filter::new()
        .kinds([
            AI_SESSION_STATE_KIND as u64,
            AI_CONVERSATION_KIND as u64,
            AI_SOURCE_DATA_KIND as u64,
            AI_RUN_CONFIG_KIND as u64,
            AI_SESSION_COMMAND_KIND as u64,
        ])
        .build();
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .expect("subscribe");
    Live {
        _dir: dir,
        notedeck,
        secret: account.secret_key.secret_bytes(),
        pending: 0,
        stream: SubscriptionStream::new(ndb, sub),
    }
}

/// `s` without its SGR color escapes, as `nd` prints it to a pipe.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        for e in chars.by_ref() {
            if e.is_ascii_alphabetic() {
                break;
            }
        }
    }
    out
}

impl Live {
    /// Ingest one event's JSON as a client would publish it.
    fn ingest(&mut self, note_json: &str) {
        let ndb = self.notedeck.app_context().ndb.clone();
        ndb.process_client_event(&format!(r#"["EVENT",{note_json}]"#))
            .expect("ingest");
        self.pending += 1;
    }

    /// Seed a signed kind-31988 state for `d` with the tags
    /// `SessionState::from_note` reads.
    fn state(&mut self, d: &str, title: &str, status: &str, cwd: &str) {
        let note = NoteBuilder::new()
            .kind(AI_SESSION_STATE_KIND)
            .content("")
            .start_tag()
            .tag_str("d")
            .tag_str(d)
            .start_tag()
            .tag_str("title")
            .tag_str(title)
            .start_tag()
            .tag_str("status")
            .tag_str(status)
            .start_tag()
            .tag_str("hostname")
            .tag_str(HOST)
            .start_tag()
            .tag_str("cwd")
            .tag_str(cwd)
            .start_tag()
            .tag_str("home_dir")
            .tag_str(HOME)
            .start_tag()
            .tag_str("backend")
            .tag_str("claude")
            .start_tag()
            .tag_str("permission-mode")
            .tag_str("plan")
            .sign(&self.secret)
            .build()
            .expect("build state note");
        self.ingest(&note.json().expect("note json"));
    }

    /// Seed a signed kind-1988 conversation note on `d` with the
    /// `d`/`role`/`seq`/`ms` tags the loader reads.
    fn message(&mut self, d: &str, role: &str, content: &str, seq: u32, ms: u64) {
        let note = NoteBuilder::new()
            .kind(AI_CONVERSATION_KIND)
            .content(content)
            .start_tag()
            .tag_str("d")
            .tag_str(d)
            .start_tag()
            .tag_str("role")
            .tag_str(role)
            .start_tag()
            .tag_str("seq")
            .tag_str(&seq.to_string())
            .start_tag()
            .tag_str("ms")
            .tag_str(&ms.to_string())
            .sign(&self.secret)
            .build()
            .expect("build conversation note");
        self.ingest(&note.json().expect("note json"));
    }

    /// Seed a signed kind-1989 source-data note carrying one raw JSONL line,
    /// as the `jsonl` reconstructor reads it.
    fn source_line(&mut self, d: &str, seq: u32, line: &str) {
        let note = NoteBuilder::new()
            .kind(AI_SOURCE_DATA_KIND)
            .content("")
            .start_tag()
            .tag_str("d")
            .tag_str(d)
            .start_tag()
            .tag_str("seq")
            .tag_str(&seq.to_string())
            .start_tag()
            .tag_str("source-data")
            .tag_str(line)
            .sign(&self.secret)
            .build()
            .expect("build source note");
        self.ingest(&note.json().expect("note json"));
    }

    /// Wait for every note seeded so far to be queryable.
    fn settle(&mut self) {
        notedeck_testing::await_notes(&mut self.stream, self.pending);
        self.pending = 0;
    }

    /// Call the registered tool `name`, as `nd` would: its answer and text,
    /// or its error.
    fn try_call(&mut self, name: &str, args: Value) -> Result<Answer, String> {
        if self.pending > 0 {
            self.settle();
        }
        let tool = tools()
            .into_iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("{name} is registered"));
        let mut app = self.notedeck.app_context();
        let mut cx = app.tool_context();
        let rendered = tool.call_rendered(&mut cx, &args);
        match rendered.outcome {
            notedeck::ToolOutcome::Data(json) => Ok(Answer {
                json,
                text: plain(&rendered.text.expect("every agentium tool renders text")),
            }),
            notedeck::ToolOutcome::Error(err) => Err(err),
            notedeck::ToolOutcome::Intent(_) => panic!("{name} answered with an intent"),
        }
    }

    /// [`try_call`](Self::try_call), failing the test on an error.
    fn call(&mut self, name: &str, args: Value) -> Answer {
        self.try_call(name, args.clone())
            .unwrap_or_else(|err| panic!("{name} {args}: {err}"))
    }

    /// `agentium grep`'s text for `args`.
    fn grep(&mut self, args: Value) -> String {
        self.call("agentium_grep_sessions", args).text
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

/// The tombstone is hidden by default and surfaced by `deleted` (only it) and
/// `all` (both); the text leads each row with the pasteable ref under a host
/// header.
#[test]
fn list_renders_live_sessions_and_scopes_tombstones() {
    let mut live = live();
    live.state("sess-live", "Live work", "working", "/home/u/proj");
    live.state("sess-gone", "Closed spike", "deleted", "/home/u/spike");

    let default = live.call("agentium_list_sessions", json!({}));
    assert!(default.text.contains("Live work"), "{}", default.text);
    assert!(!default.text.contains("Closed spike"), "{}", default.text);
    assert!(
        default.text.starts_with(HOST),
        "host header: {}",
        default.text
    );
    assert!(
        default
            .text
            .contains(&agentium_core::wordid::session_ref("sess-live")),
        "{}",
        default.text
    );
    assert!(default.text.contains("● Working"), "{}", default.text);
    assert!(default.text.contains("~/proj"), "{}", default.text);

    let deleted = live.call("agentium_list_sessions", json!({ "deleted": true }));
    assert!(deleted.text.contains("Closed spike") && deleted.text.contains("Deleted"));
    assert!(!deleted.text.contains("Live work"), "{}", deleted.text);

    let all = live.call("agentium_list_sessions", json!({ "all": true }));
    assert!(all.text.contains("Live work") && all.text.contains("Closed spike"));
}

/// `list --json` keeps every state field beside the `agentium_uri` the skills
/// quote, and the row filters narrow it.
#[test]
fn list_json_includes_agentium_uri_and_filters() {
    let mut live = live();
    live.state(
        "sess-json",
        "Draft release notes",
        "working",
        "/home/u/proj",
    );
    live.state("sess-idle", "Idle elsewhere", "idle", "/home/u/other");

    let rows = live.call("agentium_list_sessions", json!({})).json;
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 2);
    let row = rows
        .iter()
        .find(|r| r["claude_session_id"] == "sess-json")
        .expect("the seeded row");
    assert_eq!(
        row["agentium_uri"],
        Value::String(agentium_core::wordid::session_ref("sess-json"))
    );
    for field in [
        "title",
        "status",
        "hostname",
        "cwd",
        "backend",
        "cli_session_id",
    ] {
        assert!(row.get(field).is_some(), "missing {field}: {row}");
    }

    let by_cwd = live
        .call("agentium_list_sessions", json!({ "cwd": "OTHER" }))
        .json;
    assert_eq!(by_cwd.as_array().unwrap().len(), 1, "{by_cwd}");
    assert_eq!(by_cwd[0]["title"], "Idle elsewhere");
    let by_status = live
        .call("agentium_list_sessions", json!({ "status": "working" }))
        .json;
    assert_eq!(by_status.as_array().unwrap().len(), 1, "{by_status}");
    assert_eq!(by_status[0]["title"], "Draft release notes");
    let by_host = live
        .call("agentium_list_sessions", json!({ "host": "linux" }))
        .json;
    assert!(by_host.as_array().unwrap().is_empty(), "{by_host}");
    let none = live.call("agentium_list_sessions", json!({ "host": "linux" }));
    assert_eq!(none.text, "no sessions\n");
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

/// The session `show` is seeded with.
const SESSION: &str = "sess-show";
/// Its working directory, where its run config is registered.
const CWD: &str = "/home/u/proj";

/// Publish one `role=subagent` lifecycle note on [`SESSION`] through the
/// host's own builder.
fn subagent(
    live: &mut Live,
    threading: &mut ThreadingState,
    task_id: &str,
    subagent_type: &str,
    status: SubagentStatus,
    background: bool,
) {
    let info = SubagentInfo {
        task_id: task_id.into(),
        description: format!("do {task_id}"),
        subagent_type: subagent_type.into(),
        status,
        output: String::new(),
        max_output_size: 4000,
        tool_results: Vec::new(),
        background,
        agent_id: None,
        transcript: Default::default(),
    };
    let secret = live.secret;
    let ev = build_subagent_event(&info, SESSION, threading, &secret).expect("subagent event");
    live.ingest(&ev.note_json);
}

/// [`SESSION`] with two chat messages, a pending `Bash` permission request,
/// two subagents (one completed, one running in the background) and a run
/// config on its host+cwd, plus a bare second session: the pending request's
/// id.
fn seed_show(live: &mut Live) -> uuid::Uuid {
    live.state(SESSION, "Ship the show tests", "needs_input", CWD);
    live.state("sess-other", "Some other session", "idle", CWD);
    live.message(SESSION, "user", "please run the tests", 0, 1_000);
    live.message(SESSION, "assistant", "on it", 1, 2_000);

    let secret = live.secret;
    let perm_id = uuid::Uuid::new_v4();
    let mut threading = ThreadingState::new();
    let req = build_permission_request_event(
        &perm_id,
        "Bash",
        &json!({ "command": "cargo test" }),
        SESSION,
        &mut threading,
        &secret,
    )
    .expect("request event");
    live.ingest(&req.note_json);

    let completed = SubagentStatus::Completed;
    subagent(
        live,
        &mut threading,
        "t1",
        "Explore",
        SubagentStatus::Running,
        false,
    );
    subagent(live, &mut threading, "t1", "Explore", completed, false);
    subagent(
        live,
        &mut threading,
        "t2",
        "Plan",
        SubagentStatus::Running,
        true,
    );

    let config = RunConfig {
        id: "cfg-build".into(),
        name: "build".into(),
        command: "cargo build --release".into(),
        updated_at: 0,
    };
    let rc = build_run_config_event_at(&config, CWD, HOST, Some(1_000), &secret).expect("config");
    live.ingest(&rc.note_json);
    perm_id
}

#[test]
fn show_renders_every_section() {
    let mut live = live();
    let perm_id = seed_show(&mut live);
    let sref = agentium_core::wordid::session_ref(SESSION);

    let shown = live.call("agentium_show_session", json!({ "session": sref }));
    let text = &shown.text;
    for needle in [
        sref.as_str(),
        "◆ Needs Input",
        "Ship the show tests",
        SESSION,
        "~/proj",
        "run configs",
        "cargo build --release",
        "conversation",
        "pending permissions",
        &perm_id.to_string()[..8],
        "subagents",
        "1 completed, 1 running (1 background), 0 failed",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }
    assert!(!text.contains("Some other session"), "{text}");
}

/// `show --json`'s contract: `.session` is the list row (the agentium skill
/// reads `.session.cli_session_id`), beside the run configs, conversation and
/// subagent rollup.
#[test]
fn show_json_shape() {
    let mut live = live();
    let perm_id = seed_show(&mut live);

    // A word-id prefix resolves as the CLI's does.
    let sref = agentium_core::wordid::session_ref(SESSION);
    let word_id = sref.strip_prefix("agentium:").unwrap().to_owned();
    let detail = live
        .call("agentium_show_session", json!({ "session": word_id }))
        .json;
    let session = &detail["session"];
    assert_eq!(session["claude_session_id"], SESSION);
    assert_eq!(session["agentium_uri"], Value::String(sref));
    assert_eq!(session["hostname"], HOST);
    assert!(
        session.get("cli_session_id").is_some(),
        "the skills read .session.cli_session_id: {session}"
    );
    assert_eq!(detail["run_configs"][0]["command"], "cargo build --release");

    let conv = &detail["conversation"];
    let pending = conv["pending_permissions"].as_array().expect("array");
    assert_eq!(pending.len(), 1, "{conv}");
    assert_eq!(pending[0]["perm_id"], perm_id.to_string());
    assert_eq!(pending[0]["tool_name"], "Bash");
    assert_eq!(conv["message_count"], 5, "{conv}");

    let subagents = &detail["subagents"];
    assert_eq!(subagents["completed"], 1, "{subagents}");
    assert_eq!(subagents["running"], 1, "{subagents}");
    assert_eq!(subagents["background"], 1, "{subagents}");
    assert_eq!(subagents["items"].as_array().unwrap().len(), 2);
}

/// With no session named (nd's `$AGENTIUM_SESSION` fallback left it out),
/// `show` and `log` say how to name one; an unknown one is an error too.
#[test]
fn show_and_log_without_a_session_say_so() {
    let mut live = live();
    seed_show(&mut live);
    for tool in ["agentium_show_session", "agentium_session_log"] {
        let err = live.try_call(tool, json!({})).err().expect("an error");
        assert_eq!(err, agentium_core::cli::NO_SESSION, "{tool}");
        assert!(
            live.try_call(tool, json!({ "session": "agentium:no-such-thing" }))
                .is_err(),
            "{tool}"
        );
    }
}

// ---------------------------------------------------------------------------
// log
// ---------------------------------------------------------------------------

/// A three-turn conversation whose `seq` disagrees with its `ms`, so a
/// transcript ordered by `seq` reads backwards, plus a tool session.
fn seed_log(live: &mut Live) {
    live.state("sess-log", "Ordered session", "idle", CWD);
    live.message("sess-log", "user", "first question", 3, 1_000);
    live.message("sess-log", "assistant", "the answer", 2, 2_000);
    live.message("sess-log", "user", "again please", 1, 3_000);

    live.state("sess-tools", "Tool session", "idle", CWD);
    live.message("sess-tools", "user", "run the thing", 1, 1_000);
    live.message("sess-tools", "tool_result", "ran the thing", 2, 2_000);
    live.message("sess-tools", "assistant", "it ran", 3, 3_000);
}

/// `text` has `first` before `second`.
fn assert_before(text: &str, first: &str, second: &str) {
    let a = text.find(first).unwrap_or_else(|| panic!("no {first:?}"));
    let b = text.find(second).unwrap_or_else(|| panic!("no {second:?}"));
    assert!(a < b, "{first:?} must come before {second:?} in:\n{text}");
}

#[test]
fn log_renders_in_event_order_and_filters() {
    let mut live = live();
    seed_log(&mut live);

    let text = live
        .call("agentium_session_log", json!({ "session": "sess-log" }))
        .text;
    assert_before(&text, "first question", "the answer");
    assert_before(&text, "the answer", "again please");
    assert!(text.contains("user\n  first question\n"), "{text}");

    let users = live
        .call(
            "agentium_session_log",
            json!({ "session": "sess-log", "role": ["user"] }),
        )
        .text;
    assert!(users.contains("first question") && users.contains("again please"));
    assert!(!users.contains("the answer"), "{users}");

    let last = live
        .call(
            "agentium_session_log",
            json!({ "session": "sess-log", "last": 1 }),
        )
        .text;
    assert!(last.contains("again please"), "{last}");
    assert!(!last.contains("first question"), "{last}");
}

#[test]
fn log_folds_tool_traffic_unless_tools_is_passed() {
    let mut live = live();
    seed_log(&mut live);
    let log = |live: &mut Live, extra: Value| {
        let mut args = json!({ "session": "sess-tools" });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        live.call("agentium_session_log", args).text
    };

    let plain = log(&mut live, json!({}));
    assert!(plain.contains("run the thing") && plain.contains("it ran"));
    assert!(!plain.contains("ran the thing"), "{plain}");
    let tools = log(&mut live, json!({ "tools": true }));
    assert!(tools.contains("ran the thing"), "{tools}");
    let explicit = log(&mut live, json!({ "tools": true, "no_tools": true }));
    assert!(!explicit.contains("ran the thing"), "{explicit}");
}

#[test]
fn log_json_emits_role_tagged_objects() {
    let mut live = live();
    seed_log(&mut live);
    let rows = live
        .call("agentium_session_log", json!({ "session": "sess-log" }))
        .json;
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["role"], "user");
    assert_eq!(rows[0]["text"], "first question");
    assert_eq!(rows[1]["role"], "assistant");
    assert_eq!(rows[1]["text"], "the answer");
    assert_eq!(rows[2]["text"], "again please");
}

/// `jsonl` replays the raw archive lines in `seq` order, tool lines and all.
#[test]
fn log_jsonl_reconstructs_source_archive_in_seq_order() {
    let mut live = live();
    live.state("sess-src", "Archived session", "idle", CWD);
    live.source_line("sess-src", 2, r#"{"type":"assistant","n":2}"#);
    live.source_line("sess-src", 1, r#"{"type":"user","n":1}"#);
    live.source_line("sess-src", 3, r#"{"type":"tool_result","n":3}"#);

    let answer = live.call(
        "agentium_session_log",
        json!({ "session": "sess-src", "jsonl": true }),
    );
    assert_eq!(
        answer.text.lines().collect::<Vec<_>>(),
        [
            r#"{"type":"user","n":1}"#,
            r#"{"type":"assistant","n":2}"#,
            r#"{"type":"tool_result","n":3}"#,
        ]
    );
    assert_eq!(answer.json[2], json!({ "type": "tool_result", "n": 3 }));
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

/// Four sessions: two that mention "terminal", one that doesn't, and a
/// deleted one.
fn seed_grep(live: &mut Live) {
    live.state("sess-soh", "Ship of Harkinian", "idle", "/home/u/soh");
    live.message("sess-soh", "user", "why does it crash?", 1, 1000);
    live.message(
        "sess-soh",
        "assistant",
        "the terminal resize hook is missing\nsecond line, no match",
        2,
        2000,
    );
    live.state("sess-proj", "Other project", "idle", "/home/u/proj");
    live.message("sess-proj", "user", "mind the TERMINAL width", 1, 1000);
    live.message("sess-proj", "tool_result", "terminal: ok", 2, 2000);
    live.state("sess-quiet", "Quiet session", "idle", "/home/u/quiet");
    live.message("sess-quiet", "user", "nothing to find here", 1, 1000);
    live.state("sess-gone", "Closed session", "deleted", "/home/u/gone");
    live.message("sess-gone", "user", "write the postmortem", 1, 1000);
}

#[test]
fn grep_spans_every_session_with_smart_case() {
    let mut live = live();
    seed_grep(&mut live);

    let out = live.grep(json!({ "pattern": "terminal" }));
    assert!(out.contains("Ship of Harkinian") && out.contains("Other project"));
    assert!(!out.contains("Quiet session"), "{out}");
    assert!(out.contains("agentium:"), "{out}");
    assert!(out.contains("the terminal resize hook is missing"));
    assert!(!out.contains("second line, no match"), "{out}");
    assert!(out.contains("mind the TERMINAL width"), "smart case: {out}");

    let out = live.grep(json!({ "pattern": "terminal", "case_sensitive": true }));
    assert!(!out.contains("mind the TERMINAL width"), "{out}");
    assert!(out.contains("the terminal resize hook is missing"), "{out}");
    let out = live.grep(json!({ "pattern": "Terminal" }));
    assert!(
        !out.contains("the terminal resize hook is missing"),
        "{out}"
    );
    let out = live.grep(json!({ "pattern": "Terminal", "ignore_case": true }));
    assert!(out.contains("the terminal resize hook is missing"), "{out}");

    let bad = live.try_call("agentium_grep_sessions", json!({ "pattern": "[unclosed" }));
    assert!(bad.is_err());
}

#[test]
fn grep_honors_the_session_and_message_filters() {
    let mut live = live();
    seed_grep(&mut live);

    let out = live.grep(json!({ "pattern": "terminal", "cwd": "soh" }));
    assert!(out.contains("Ship of Harkinian") && !out.contains("Other project"));
    let out = live.grep(json!({ "pattern": "terminal", "role": ["assistant"] }));
    assert!(!out.contains("mind the TERMINAL width"), "{out}");
    let out = live.grep(json!({ "pattern": "terminal" }));
    assert!(
        !out.contains("terminal: ok"),
        "tools fold by default: {out}"
    );
    let out = live.grep(json!({ "pattern": "terminal", "tools": true }));
    assert!(out.contains("terminal: ok"), "{out}");
    let out = live.grep(json!({ "pattern": "zzz-no-such-text" }));
    assert_eq!(out, "no matches\n");
}

#[test]
fn grep_json_groups_matches_under_each_session() {
    let mut live = live();
    seed_grep(&mut live);
    let rows = live
        .call(
            "agentium_grep_sessions",
            json!({ "pattern": "terminal", "ignore_case": true }),
        )
        .json;
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 2, "{rows:?}");
    let soh = rows
        .iter()
        .find(|r| r["title"] == "Ship of Harkinian")
        .expect("the soh row");
    assert!(soh["agentium_uri"]
        .as_str()
        .is_some_and(|u| u.starts_with("agentium:")));
    assert_eq!(soh["cwd"], "/home/u/soh");
    assert_eq!(
        soh["matches"],
        json!([{ "role": "assistant", "text": "the terminal resize hook is missing" }])
    );
}

/// Unlike `list`, `grep` searches deleted sessions by default; `deleted`
/// narrows to them, and `status` to one status.
#[test]
fn grep_searches_deleted_sessions_by_default() {
    let mut live = live();
    seed_grep(&mut live);

    let out = live.grep(json!({ "pattern": "postmortem" }));
    assert!(out.contains("  ⊘ Deleted  Closed session"), "{out}");
    let out = live.grep(json!({ "pattern": "resize hook" }));
    assert!(out.contains("  ○ Idle  Ship of Harkinian"), "{out}");
    let out = live.grep(json!({ "pattern": "terminal|postmortem", "deleted": true }));
    assert!(out.contains("Closed session") && !out.contains("Ship of Harkinian"));
    let out = live.grep(json!({ "pattern": "postmortem", "status": "idle" }));
    assert_eq!(out, "no matches\n");
}

// ---------------------------------------------------------------------------
// id
// ---------------------------------------------------------------------------

#[test]
fn id_prints_each_d_tags_ref() {
    let mut live = live();
    let answer = live.call(
        "agentium_session_ref",
        json!({ "d_tags": ["d-one", "d-two"] }),
    );
    let refs = [
        agentium_core::wordid::session_ref("d-one"),
        agentium_core::wordid::session_ref("d-two"),
    ];
    assert_eq!(answer.json, json!(refs));
    assert_eq!(answer.text, format!("{}\n{}\n", refs[0], refs[1]));
    assert!(live
        .try_call("agentium_session_ref", json!({ "d_tags": [] }))
        .is_err());
}

/// Every tool has an `agentium` command path, the five reads are read-only,
/// and `show`/`log` take their session from `$AGENTIUM_SESSION` when none is
/// named.
#[test]
fn every_tool_is_an_agentium_command() {
    for tool in tools() {
        let spec = tool.spec();
        assert_eq!(spec.command_path()[0], "agentium", "{}", spec.name());
    }
    let reads = [
        ListSessions.spec(),
        ShowSession.spec(),
        SessionLog.spec(),
        GrepSessions.spec(),
        SessionRef.spec(),
    ];
    for spec in &reads {
        assert!(spec.is_read_only(), "{}", spec.name());
    }
    for tool in ShowSession
        .spec()
        .arguments()
        .iter()
        .chain(SessionLog.spec().arguments())
    {
        if tool.name() == "session" {
            assert_eq!(tool.cli().env, Some(SESSION_ENV));
        }
    }
}

mod control;
