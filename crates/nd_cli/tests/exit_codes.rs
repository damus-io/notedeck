//! `nd` against a fake notedeck: a socket in a temp dir, named by
//! `NOTEDECK_SOCKET`, that answers each request from a script. Checks what a
//! script can rely on — the exit code, and what lands on stdout and stderr —
//! without the real notedeck or the user's real socket.
#![cfg(unix)]

use notedeck_ipc::{
    IntentStatus, PROTOCOL_VERSION, Reply, Request, Response, StreamMode, ToolInfo, ToolKind,
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use tempfile::TempDir;

/// *This* worktree's build of `nd` (see [`bin_testing::worktree_bin`]).
fn nd_bin() -> PathBuf {
    bin_testing::worktree_bin(Path::new(env!("CARGO_BIN_EXE_nd")), "nd_cli", |bin| {
        Command::new(bin)
            .arg("--help")
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stderr).contains("call"))
    })
}

/// A short temp dir for the socket: a Unix socket path has to fit in about
/// 100 bytes, which the default temp dir under a sandbox may not leave room for.
fn socket_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("nd")
        .tempdir_in("/tmp")
        .or_else(|_| tempfile::tempdir())
        .expect("temp dir")
}

/// The tools the fake notedeck lists.
fn tools() -> Vec<ToolInfo> {
    vec![ToolInfo {
        name: "open".to_owned(),
        title: "Open reference".to_owned(),
        description: "Open a reference.".to_owned(),
        category: Some("Chrome".to_owned()),
        kind: ToolKind::Ui,
        read_only: false,
        input_schema: json!({
            "type": "object",
            "properties": {
                "reference": {"type": "string", "description": "What to open"},
                "limit": {"type": "integer", "description": "How many"},
            },
            "required": ["reference"],
        }),
        command: Vec::new(),
        accepts_signer: false,
        streams: None,
    }]
}

/// Tools that declare command lines, shaped as headway's read tools write
/// them: two sharing `headway show`, one with a long, a short and an env
/// flag.
fn app_tools() -> Vec<ToolInfo> {
    let tool = |name: &str, title: &str, verb: &str, schema: Value| ToolInfo {
        name: name.to_owned(),
        title: title.to_owned(),
        description: format!("{title}."),
        category: Some("Headway".to_owned()),
        kind: ToolKind::Data,
        read_only: true,
        input_schema: schema,
        command: vec!["headway".to_owned(), verb.to_owned()],
        accepts_signer: false,
        streams: None,
    };
    vec![
        tool(
            "headway_show_board",
            "Show board",
            "show",
            json!({
                "type": "object",
                "properties": {"board": {"type": "string", "description": "The board."}},
                "required": [],
                "x-nd-order": ["board"],
            }),
        ),
        tool(
            "headway_show_card",
            "Show card",
            "show",
            json!({
                "type": "object",
                "properties": {
                    "card": {"type": "string", "description": "The card.", "x-nd-positional": 0},
                    "board": {"type": "string", "description": "The board."},
                },
                "required": ["card"],
                "x-nd-order": ["card", "board"],
            }),
        ),
        tool(
            "headway_ready_items",
            "List ready cards",
            "next",
            json!({
                "type": "object",
                "properties": {
                    "container": {"type": "string", "description": "Where.", "x-nd-long": "in"},
                    "limit": {"type": "integer", "description": "How many.", "x-nd-short": "n"},
                    "board": {"type": "string", "description": "The board.", "x-nd-env": "HEADWAY_BOARD"},
                },
                "required": [],
                "x-nd-order": ["container", "limit", "board"],
            }),
        ),
        tool(
            "headway_edit",
            "Edit card",
            "edit",
            json!({
                "type": "object",
                "properties": {
                    "card": {"type": "string", "description": "The card.", "x-nd-positional": 0},
                    "labels": {"type": "array", "items": {"type": "string"}, "description": "Labels.", "x-nd-short": "l", "x-nd-kind": "list"},
                    "desc": {"type": "string", "description": "The text.", "x-nd-from-file": "desc-file"},
                    "repo_dir": {"type": "string", "description": "The checkout.", "x-nd-kind": "path", "x-nd-default-cwd": true},
                    "session": {"type": "string", "description": "Who.", "x-nd-env": "AGENTIUM_SESSION"},
                },
                "required": ["card"],
                "x-nd-order": ["card", "labels", "desc", "repo_dir", "session"],
            }),
        ),
        ToolInfo {
            accepts_signer: true,
            ..tool(
                "headway_comment_card",
                "Post comment",
                "comment",
                json!({
                    "type": "object",
                    "properties": {
                        "card": {"type": "string", "description": "The card.", "x-nd-positional": 0},
                        "body": {"type": "string", "description": "The text.", "x-nd-positional": "rest"},
                    },
                    "required": ["card", "body"],
                    "x-nd-order": ["card", "body"],
                }),
            )
        },
    ]
}

fn app_list() -> Response {
    Response::ok(Reply::Tools { tools: app_tools() })
}

/// A fake notedeck serving scripted answers.
struct Fake {
    /// Where it listens.
    socket: PathBuf,
    /// Ends after the last answer, with the requests it answered.
    server: thread::JoinHandle<Vec<Request>>,
}

/// Serve `answers` in order, one connection each, on a socket in `dir`.
fn serve(dir: &TempDir, answers: Vec<Response>) -> Fake {
    serve_lines(
        dir,
        answers.into_iter().map(|answer| vec![answer]).collect(),
    )
}

/// Serve `connections` in order, one connection each, writing each
/// connection's lines and hanging up: one line answers a call, several
/// stream. A line is written in the version its request came with, as
/// notedeck answers, unless a test set another to see nd refuse it.
fn serve_lines(dir: &TempDir, connections: Vec<Vec<Response>>) -> Fake {
    let path = dir.path().join("nd.sock");
    let listener = UnixListener::bind(&path).expect("bind the fake socket");
    let server = thread::spawn(move || {
        let mut seen = Vec::new();
        for lines in connections {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read the request");
            let request = Request::versioned(&line).expect("a request nd sent");
            for answer in lines {
                let v = match answer.v {
                    PROTOCOL_VERSION => request.v,
                    v => v,
                };
                // A stream's reader may have gone; the rest goes nowhere.
                let _ = writeln!(reader.get_mut(), "{}", answer.line_as(v));
            }
            seen.push(request.request);
        }
        seen
    });
    Fake {
        socket: path,
        server,
    }
}

fn nd(socket: &Path, args: &[&str]) -> Output {
    nd_env(socket, args, &[])
}

/// [`nd`] with `env` set, in an environment otherwise clear of the comment
/// key an agent session exports, and with `$ND_NO_START` set, so a test
/// never starts the real notedeck.
fn nd_env(socket: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(nd_bin())
        .args(args)
        .env("NOTEDECK_SOCKET", socket)
        .env("ND_NO_START", "1")
        .env_remove("HEADWAY_COMMENT_NSEC")
        .env_remove("HEADWAY_COMMENT_NSEC_FILE")
        .envs(env.iter().copied())
        .output()
        .expect("run nd")
}

fn list() -> Response {
    Response::ok(Reply::Tools { tools: tools() })
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn no_socket_exits_2_naming_the_path() {
    let dir = socket_dir();
    let missing = dir.path().join("absent.sock");
    let out = nd(&missing, &["tools"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!(
            "notedeck isn't running (no socket at {})",
            missing.display()
        )),
        "{}",
        stderr(&out)
    );
}

#[test]
fn an_applied_intent_prints_nothing_and_exits_0() {
    let dir = socket_dir();
    let applied = Response::ok(Reply::Intent {
        intent: IntentStatus::Applied,
    });
    let Fake { socket, server } = serve(&dir, vec![list(), applied]);
    let out = nd(
        &socket,
        &["call", "open", "reference=headway:notedeck/a-b", "limit=3"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    let seen = server.join().unwrap();
    assert_eq!(
        seen[1],
        Request::CallTool {
            name: "open".to_owned(),
            args: json!({"reference": "headway:notedeck/a-b", "limit": 3}),
            signer: None,
        }
    );
}

#[test]
fn a_bare_value_fills_the_one_required_string() {
    let dir = socket_dir();
    let applied = Response::ok(Reply::Intent {
        intent: IntentStatus::Applied,
    });
    let Fake { socket, server } = serve(&dir, vec![list(), applied]);
    let out = nd(
        &socket,
        &["call", "open", "agentium:possible-video-science"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let seen = server.join().unwrap();
    assert_eq!(
        seen[1],
        Request::CallTool {
            name: "open".to_owned(),
            args: json!({"reference": "agentium:possible-video-science"}),
            signer: None,
        }
    );
}

#[test]
fn data_prints_as_json_and_exits_0() {
    let dir = socket_dir();
    let result = Response::ok(Reply::Result {
        result: json!({"cards": [1, 2]}),
        text: None,
        notice: None,
    });
    let Fake { socket, server } = serve(&dir, vec![list(), result]);
    let out = nd(&socket, &["call", "open", "--json", r#"{"reference":"x"}"#]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let printed: Value = serde_json::from_str(&stdout(&out)).expect("JSON on stdout");
    assert_eq!(printed, json!({"cards": [1, 2]}));
    server.join().unwrap();
}

#[test]
fn a_tool_error_goes_to_stderr_with_exit_1() {
    let dir = socket_dir();
    let error = Response::error("open: 'x' is not a reference notedeck can open");
    let Fake { socket, server } = serve(&dir, vec![list(), error]);
    let out = nd(&socket, &["call", "open", "reference=x"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("is not a reference"),
        "{}",
        stderr(&out)
    );
    server.join().unwrap();
}

#[test]
fn a_wrong_key_fails_before_the_call_is_sent() {
    let dir = socket_dir();
    // Only the tool list is served: a second connection would hang the test.
    let Fake { socket, server } = serve(&dir, vec![list()]);
    let out = nd(&socket, &["call", "open", "refrence=x"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("open doesn't take 'refrence'"), "{err}");
    assert!(err.contains("reference  string, required"), "{err}");
    assert_eq!(server.join().unwrap(), [Request::ListTools]);
}

#[test]
fn another_protocol_version_names_both() {
    let dir = socket_dir();
    let mut future = list();
    future.v = 9;
    let Fake { socket, server } = serve(&dir, vec![future]);
    let out = nd(&socket, &["tools"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("protocol version 9"), "{err}");
    assert!(err.contains("this nd asked in 1"), "{err}");
    server.join().unwrap();
}

#[test]
fn tools_lists_and_shows_from_the_socket() {
    let dir = socket_dir();
    let Fake { socket, server } = serve(&dir, vec![list(), list()]);
    let out = nd(&socket, &["tools"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).starts_with("Chrome\n    open  ui       Open reference"),
        "{}",
        stdout(&out)
    );
    let out = nd(&socket, &["tools", "open"]);
    assert!(stdout(&out).contains("ARGS:\n"), "{}", stdout(&out));
    server.join().unwrap();
}

#[test]
fn version_names_the_running_notedeck() {
    let dir = socket_dir();
    let Fake { socket, server } = serve(&dir, vec![list()]);
    let out = nd(&socket, &["--version"]);
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(
        text.starts_with(&format!("nd {}\n", env!("CARGO_PKG_VERSION"))),
        "{text}"
    );
    assert!(text.contains("\nnotedeck "), "{text}");
    server.join().unwrap();
}

/// Run `nd mcp` with `args`, feed it `lines` on stdin, and return its
/// stdout parsed one JSON-RPC message per line.
fn mcp(socket: &Path, args: &[&str], lines: &[Value]) -> Vec<Value> {
    use std::process::Stdio;

    let mut child = Command::new(nd_bin())
        .arg("mcp")
        .args(args)
        .env("NOTEDECK_SOCKET", socket)
        .env("ND_NO_START", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run nd mcp");
    let mut stdin = child.stdin.take().unwrap();
    for line in lines {
        writeln!(stdin, "{line}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    stdout(&out)
        .lines()
        .map(|l| serde_json::from_str(l).expect("only JSON-RPC on stdout"))
        .collect()
}

#[test]
fn mcp_serves_a_session_over_stdio() {
    let dir = socket_dir();
    let applied = Response::ok(Reply::Intent {
        intent: IntentStatus::Applied,
    });
    let Fake { socket, server } = serve(&dir, vec![list(), list(), applied]);
    let replies = mcp(
        &socket,
        &["--ui"],
        &[
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                   "params": {"protocolVersion": "2025-06-18"}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "open", "arguments": {"reference": "x"}}}),
        ],
    );
    assert_eq!(replies.len(), 3, "{replies:?}");
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(replies[1]["result"]["tools"][0]["name"], "open");
    assert_eq!(replies[2]["id"], 2);
    assert_eq!(replies[2]["result"]["isError"], false);
    let seen = server.join().unwrap();
    assert_eq!(
        seen[2],
        Request::CallTool {
            name: "open".to_owned(),
            args: json!({"reference": "x"}),
            signer: None,
        }
    );
}

#[test]
fn mcp_without_notedeck_keeps_serving() {
    let dir = socket_dir();
    let missing = dir.path().join("absent.sock");
    let replies = mcp(
        &missing,
        &[],
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "open"}}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}),
        ],
    );
    let list_error = replies[0]["error"]["message"].as_str().unwrap();
    assert!(
        list_error.contains("notedeck isn't running"),
        "{list_error}"
    );
    assert_eq!(replies[1]["result"]["isError"], true);
    assert_eq!(replies[2]["result"], json!({}));
}

#[test]
fn spawn_sends_the_resolved_dir_and_prints_the_session() {
    let dir = socket_dir();
    let work = TempDir::new().unwrap();
    let spawned = || {
        Response::ok(Reply::Result {
            result: json!({"session_id": 7}),
            text: None,
            notice: None,
        })
    };
    // No tool list first: the chrome starts Dave for a spawn, so nd doesn't
    // look the tool up.
    let Fake { socket, server } = serve(&dir, vec![spawned(), spawned()]);
    let out = nd(&socket, &["spawn", work.path().to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Spawned session 7\n");
    let out = nd(&socket, &["spawn", "--json", work.path().to_str().unwrap()]);
    assert_eq!(stdout(&out), "7\n");
    let seen = server.join().unwrap();
    assert_eq!(
        seen[0],
        Request::CallTool {
            name: "dave_spawn_session".to_owned(),
            args: json!({"cwd": work.path().canonicalize().unwrap()}),
            signer: None,
        }
    );
}

#[test]
fn spawn_without_dave_says_so() {
    let dir = socket_dir();
    let work = TempDir::new().unwrap();
    let unknown = Response::error("unknown tool 'dave_spawn_session'");
    let Fake { socket, server } = serve(&dir, vec![unknown]);
    let out = nd(&socket, &["spawn", work.path().to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("no Dave"), "{}", stderr(&out));
    server.join().unwrap();
}

#[test]
fn spawn_rejects_a_missing_dir_without_connecting() {
    let dir = socket_dir();
    let missing = dir.path().join("absent.sock");
    let out = nd(&missing, &["spawn", "/no/such/dir/here"]);
    assert_eq!(out.status.code(), Some(1), "not 2: nothing was sent");
    assert!(
        stderr(&out).contains("/no/such/dir/here"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn an_app_command_calls_the_tool_it_names() {
    let dir = socket_dir();
    let result = Response::ok(Reply::Result {
        result: json!({"ref": "headway:notedeck/a-b-c"}),
        text: None,
        notice: None,
    });
    let Fake { socket, server } = serve(&dir, vec![app_list(), result]);
    let out = nd(
        &socket,
        &["headway", "show", "headway:notedeck/a-b-c", "--json"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let printed: Value = serde_json::from_str(&stdout(&out)).expect("JSON on stdout");
    assert_eq!(printed, json!({"ref": "headway:notedeck/a-b-c"}));
    let seen = server.join().unwrap();
    assert_eq!(
        seen[1],
        Request::CallTool {
            name: "headway_show_card".to_owned(),
            args: json!({"card": "headway:notedeck/a-b-c"}),
            signer: None,
        }
    );
}

/// A tool that renders answers with text, which a command prints in place
/// of the value: without its color into a pipe, with it under `--color
/// always`. `--json` prints the value, and so does `nd call`, whose `--json`
/// is its argument object.
#[test]
fn an_app_command_prints_the_tools_text() {
    let dir = socket_dir();
    let rendered = || {
        Response::ok(Reply::Result {
            result: json!({"id": "notedeck"}),
            text: Some("Todo (1)\n  One  \x1b[90mheadway:notedeck/a-b-c\x1b[0m\n".to_owned()),
            notice: None,
        })
    };
    let lines = [
        &["headway", "show"][..],
        &["headway", "show", "--color", "always"],
        &["headway", "show", "--json"],
        &["call", "headway_show_board"],
    ];
    let mut replies = Vec::new();
    for _ in lines {
        replies.push(app_list());
        replies.push(rendered());
    }
    let Fake { socket, server } = serve(&dir, replies);
    let outs: Vec<_> = lines.iter().map(|line| nd(&socket, line)).collect();
    for (line, out) in lines.iter().zip(&outs) {
        assert_eq!(out.status.code(), Some(0), "{line:?}: {}", stderr(out));
    }
    assert_eq!(
        stdout(&outs[0]),
        "Todo (1)\n  One  headway:notedeck/a-b-c\n"
    );
    assert_eq!(
        stdout(&outs[1]),
        "Todo (1)\n  One  \x1b[90mheadway:notedeck/a-b-c\x1b[0m\n"
    );
    for out in &outs[2..] {
        let printed: Value = serde_json::from_str(&stdout(out)).expect("JSON on stdout");
        assert_eq!(printed, json!({"id": "notedeck"}));
    }
    server.join().unwrap();
}

#[test]
fn an_app_command_reads_flags_and_the_callers_env() {
    let dir = socket_dir();
    let result = Response::ok(Reply::Result {
        result: json!([]),
        text: None,
        notice: None,
    });
    let Fake { socket, server } = serve(&dir, vec![app_list(), result]);
    let out = Command::new(nd_bin())
        .args([
            "headway",
            "next",
            "--in",
            "headway:notedeck/keen-ranch-hotel",
            "-n",
            "1",
        ])
        .env("NOTEDECK_SOCKET", &socket)
        .env("ND_NO_START", "1")
        .env("HEADWAY_BOARD", "notedeck")
        .output()
        .expect("run nd");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let seen = server.join().unwrap();
    assert_eq!(
        seen[1],
        Request::CallTool {
            name: "headway_ready_items".to_owned(),
            args: json!({
                "container": "headway:notedeck/keen-ranch-hotel",
                "limit": 1,
                "board": "notedeck",
            }),
            signer: None,
        }
    );
}

/// The caller-side kinds resolve in nd's own process: the path is nd's
/// cwd, the list flag is split, stdin is read for `--desc-file -`, and the
/// session comes from the caller's env, all before the call is sent.
#[test]
fn an_app_command_resolves_the_callers_side() {
    let dir = socket_dir();
    let cwd = tempfile::tempdir().expect("a cwd");
    let result = Response::ok(Reply::Result {
        result: json!({}),
        text: None,
        notice: None,
    });
    let Fake { socket, server } = serve(&dir, vec![app_list(), result]);
    let mut child = Command::new(nd_bin())
        .args([
            "headway",
            "edit",
            "c",
            "-l",
            "a",
            "-l",
            "b,c",
            "--desc-file",
            "-",
        ])
        .current_dir(cwd.path())
        .env("NOTEDECK_SOCKET", &socket)
        .env("ND_NO_START", "1")
        .env("AGENTIUM_SESSION", "agentium:gaze-cross-blossom")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run nd");
    child
        .stdin
        .take()
        .expect("nd's stdin")
        .write_all(b"## Why\n\npiped in\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("nd exits");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let seen = server.join().unwrap();
    let repo_dir = cwd.path().canonicalize().unwrap();
    assert_eq!(
        seen[1],
        Request::CallTool {
            name: "headway_edit".to_owned(),
            args: json!({
                "card": "c",
                "labels": ["a", "b", "c"],
                "desc": "## Why\n\npiped in",
                "repo_dir": repo_dir.to_string_lossy(),
                "session": "agentium:gaze-cross-blossom",
            }),
            signer: None,
        }
    );
}

/// Tools shaped as agentium's streams write them: `log` read once or, with
/// `--follow`, followed; `watch` a redrawn dashboard.
fn stream_tools() -> Vec<ToolInfo> {
    let tool = |name: &str, verb: &str, streams: Option<StreamMode>, schema: Value| ToolInfo {
        name: name.to_owned(),
        title: name.to_owned(),
        description: format!("{name}."),
        category: Some("Agentium".to_owned()),
        kind: ToolKind::Data,
        read_only: true,
        input_schema: schema,
        command: vec!["agentium".to_owned(), verb.to_owned()],
        accepts_signer: false,
        streams,
    };
    let session = json!({"type": "string", "description": "The session.", "x-nd-positional": 0});
    vec![
        tool(
            "agentium_session_log",
            "log",
            None,
            json!({
                "type": "object",
                "properties": {
                    "session": session,
                    "jsonl": {"type": "boolean", "description": "Raw."},
                },
                "required": [],
                "x-nd-order": ["session", "jsonl"],
            }),
        ),
        tool(
            "agentium_follow_log",
            "log",
            Some(StreamMode::Append),
            json!({
                "type": "object",
                "properties": {
                    "session": session,
                    "follow": {"type": "boolean", "description": "Follow.", "x-nd-short": "f"},
                },
                "required": ["follow"],
                "x-nd-order": ["session", "follow"],
            }),
        ),
        tool(
            "agentium_watch",
            "watch",
            Some(StreamMode::Redraw),
            json!({"type": "object", "properties": {}, "required": []}),
        ),
    ]
}

fn stream_list() -> Response {
    Response::ok(Reply::Tools {
        tools: stream_tools(),
    })
}

/// A stream event, with its text.
fn event(n: u32, text: &str) -> Response {
    Response::ok(Reply::Event {
        event: json!({ "n": n }),
        text: Some(text.to_owned()),
    })
}

/// `-f` picks the streaming tool on `log`'s line and subscribes to it in
/// v2: each event's text is appended as it comes, color stripped into a
/// pipe, until notedeck ends the stream. `--json` prints each event's value
/// as a line of its own. Without `-f` the same line is a plain call.
#[test]
fn a_stream_appends_each_events_text_until_it_ends() {
    let dir = socket_dir();
    let events = || {
        vec![
            event(0, "\x1b[1myou\x1b[0m\n  hi\n"),
            event(1, "\nassistant\n  hello\n"),
        ]
    };
    let Fake { socket, server } = serve_lines(
        &dir,
        vec![
            vec![stream_list()],
            events(),
            vec![stream_list()],
            events(),
            vec![stream_list()],
            vec![Response::ok(Reply::Result {
                result: json!([]),
                text: Some("once\n".to_owned()),
                notice: None,
            })],
        ],
    );
    let out = nd(&socket, &["agentium", "log", "s", "-f"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "you\n  hi\n\nassistant\n  hello\n");
    let out = nd(&socket, &["agentium", "log", "--follow", "s", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "{\"n\":0}\n{\"n\":1}\n");
    let out = nd(&socket, &["agentium", "log", "s"]);
    assert_eq!(stdout(&out), "once\n");

    let seen = server.join().unwrap();
    let subscribe = Request::Subscribe {
        name: "agentium_follow_log".to_owned(),
        args: json!({"session": "s", "follow": true}),
    };
    assert_eq!(seen[1], subscribe);
    assert_eq!(seen[3], subscribe);
    assert!(
        matches!(&seen[5], Request::CallTool { name, .. } if name == "agentium_session_log"),
        "{:?}",
        seen[5]
    );
}

/// A redrawn stream into a pipe prints each frame whole, set off from the
/// last by a blank line.
#[test]
fn a_redrawn_stream_into_a_pipe_prints_each_frame() {
    let dir = socket_dir();
    let Fake { socket, server } = serve_lines(
        &dir,
        vec![
            vec![stream_list()],
            vec![event(0, "2 sessions\n"), event(1, "3 sessions\n")],
        ],
    );
    let out = nd(&socket, &["agentium", "watch"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "2 sessions\n\n3 sessions\n");
    server.join().unwrap();
}

/// A stream that fails mid-way exits 1 with its error after what it
/// showed; one that can't start answers its error at once; and `--pager`
/// is refused before anything is subscribed to.
#[test]
fn a_streams_error_and_pager_exit_1() {
    let dir = socket_dir();
    let Fake { socket, server } = serve_lines(
        &dir,
        vec![
            vec![stream_list()],
            vec![
                event(0, "first\n"),
                Response::error("the database went away"),
            ],
            vec![stream_list()],
            vec![Response::error("no session matches 'nope'")],
            vec![stream_list()],
        ],
    );
    let out = nd(&socket, &["agentium", "log", "s", "-f"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "first\n");
    assert!(
        stderr(&out).contains("the database went away"),
        "{}",
        stderr(&out)
    );

    let out = nd(&socket, &["agentium", "log", "nope", "-f"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("no session matches"),
        "{}",
        stderr(&out)
    );

    let out = nd(&socket, &["agentium", "log", "s", "-f", "--pager"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("--pager"), "{}", stderr(&out));
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 5, "nothing was subscribed to for --pager");
}

#[test]
fn an_app_namespace_lists_its_commands() {
    let dir = socket_dir();
    let Fake { socket, server } = serve(&dir, vec![app_list(), app_list()]);
    let out = nd(&socket, &["headway", "--help"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let page = stderr(&out);
    assert!(
        page.contains("    next              List ready cards\n"),
        "{page}"
    );
    assert!(
        page.contains("    show              Show card / Show board\n"),
        "{page}"
    );

    let out = nd(&socket, &["help", "headway", "show"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("    nd headway show <card> [OPTIONS]\n"),
        "{}",
        stderr(&out)
    );
    server.join().unwrap();
}

#[test]
fn a_wrong_app_command_fails_before_the_call_is_sent() {
    let dir = socket_dir();
    let Fake { socket, server } = serve(&dir, vec![app_list(), app_list()]);
    let out = nd(&socket, &["notebook", "show"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("unknown command 'notebook': the running notedeck's are headway"),
        "{}",
        stderr(&out)
    );
    let out = nd(&socket, &["headway", "show", "--bogus"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("unknown flag '--bogus' (see `nd headway show --help`)"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        server.join().unwrap(),
        [Request::ListTools, Request::ListTools]
    );
}

#[test]
fn an_app_command_without_notedeck_exits_2() {
    let dir = socket_dir();
    let out = nd(&dir.path().join("absent.sock"), &["headway", "show"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

/// The comment key in nd's environment goes on the wire beside a call of a
/// tool that accepts one, by command or by `nd call`, and with no other.
#[test]
fn the_comment_key_rides_only_with_a_tool_that_accepts_one() {
    let dir = socket_dir();
    let done = || {
        Response::ok(Reply::Result {
            result: json!({"ok": true}),
            text: None,
            notice: None,
        })
    };
    let Fake { socket, server } = serve(
        &dir,
        vec![app_list(), done(), app_list(), done(), app_list(), done()],
    );
    let key = [("HEADWAY_COMMENT_NSEC", "nsec1agent")];
    for line in [
        &["headway", "comment", "c", "looks", "good"][..],
        &["call", "headway_comment_card", "card=c", "body=looks good"],
        &["headway", "show"],
    ] {
        let out = nd_env(&socket, line, &key);
        assert_eq!(out.status.code(), Some(0), "{line:?}: {}", stderr(&out));
    }
    let seen = server.join().unwrap();
    let signed = Some(notedeck_ipc::Signer {
        comment_nsec: "nsec1agent".to_owned(),
    });
    let comment = Request::CallTool {
        name: "headway_comment_card".to_owned(),
        args: json!({"card": "c", "body": "looks good"}),
        signer: signed,
    };
    assert_eq!(seen[1], comment, "by command");
    assert_eq!(seen[3], comment, "by nd call");
    assert_eq!(
        seen[5],
        Request::CallTool {
            name: "headway_show_board".to_owned(),
            args: json!({}),
            signer: None,
        },
        "a tool that doesn't accept one never gets it"
    );
}

/// Set in the environment of the fake notedeck a test's `NOTEDECK_BIN`
/// starts, so [`fake_notedeck`] serves instead of returning.
const FAKE_SERVE_ENV: &str = "ND_TEST_FAKE_NOTEDECK";

/// How long the fake notedeck waits for a connection before it exits.
const FAKE_IDLE: std::time::Duration = std::time::Duration::from_secs(3);

/// The notedeck a started test runs: not a test of its own (it returns at
/// once) unless [`FAKE_SERVE_ENV`] is set. Then it binds `NOTEDECK_SOCKET`
/// after a pause, answers its first `list_tools` with no tools as a notedeck
/// does before its first tick, then with [`app_tools`], answers every call
/// with a line of text, and exits once [`FAKE_IDLE`] passes with no
/// connection.
#[test]
#[ignore = "run by a test's NOTEDECK_BIN, not on its own"]
fn fake_notedeck() {
    use std::time::{Duration, Instant};

    if std::env::var_os(FAKE_SERVE_ENV).is_none() {
        return;
    }
    thread::sleep(Duration::from_millis(200));
    let path = notedeck_ipc::socket_path();
    let listener = UnixListener::bind(&path).expect("bind the fake socket");
    listener.set_nonblocking(true).unwrap();
    let mut listed = 0;
    let mut last = Instant::now();
    while last.elapsed() < FAKE_IDLE {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(_) => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        last = Instant::now();
        stream.set_nonblocking(false).unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            continue;
        }
        let answer = match Request::from_line(&line) {
            Ok(Request::ListTools) => {
                listed += 1;
                let tools = if listed == 1 { Vec::new() } else { app_tools() };
                Response::ok(Reply::Tools { tools })
            }
            Ok(Request::CallTool { name, .. }) => Response::ok(Reply::Result {
                result: json!({}),
                text: Some(format!("the started notedeck ran {name}\n")),
                notice: None,
            }),
            Ok(Request::Subscribe { .. }) => Response::error("the fake notedeck doesn't stream"),
            Err(e) => Response::error(e),
        };
        let _ = writeln!(reader.get_mut(), "{}", answer.line_as(1));
    }
}

/// A `NOTEDECK_BIN` in `dir`: a script that writes its arguments to
/// `args`, then runs `body`.
fn notedeck_script(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = dir.join("notedeck");
    let args = dir.join("args");
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho \"$@\" > '{}'\n{body}\n", args.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// A `NOTEDECK_BIN` that runs [`fake_notedeck`].
fn fake_notedeck_bin(dir: &Path) -> PathBuf {
    let test = std::env::current_exe().unwrap();
    notedeck_script(
        dir,
        &format!(
            "exec '{}' --exact fake_notedeck --ignored --nocapture --test-threads 1",
            test.display()
        ),
    )
}

/// nd with nothing on the socket and `bin` as the notedeck to start.
fn nd_starting(socket: &Path, bin: &Path, args: &[&str]) -> Output {
    Command::new(nd_bin())
        .args(args)
        .env("NOTEDECK_SOCKET", socket)
        .env("NOTEDECK_BIN", bin)
        .env(FAKE_SERVE_ENV, "1")
        .env_remove("ND_NO_START")
        .env_remove("HEADWAY_COMMENT_NSEC")
        .env_remove("HEADWAY_COMMENT_NSEC_FILE")
        .output()
        .expect("run nd")
}

/// With no notedeck on the socket, nd starts one as `--headless --serve`,
/// waits past its empty first tool list, and runs the command in it.
#[test]
fn with_no_notedeck_nd_starts_one_and_calls_it() {
    let dir = socket_dir();
    let socket = dir.path().join("nd.sock");
    let bin = fake_notedeck_bin(dir.path());
    let out = nd_starting(&socket, &bin, &["headway", "show"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "the started notedeck ran headway_show_board\n"
    );
    let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
    assert_eq!(args.trim(), "--headless --serve");
    assert!(
        dir.path().join("nd.log").exists(),
        "its output goes beside the socket"
    );

    // The next run finds it running and starts nothing.
    std::fs::remove_file(dir.path().join("args")).unwrap();
    let out = nd_starting(&socket, &bin, &["headway", "show"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!dir.path().join("args").exists(), "started a second one");
}

/// `--no-start` and `$ND_NO_START` keep the old exit 2, starting nothing.
#[test]
fn no_start_exits_2_without_starting_one() {
    let dir = socket_dir();
    let socket = dir.path().join("nd.sock");
    let bin = notedeck_script(dir.path(), "exit 0");
    let out = nd_starting(&socket, &bin, &["--no-start", "headway", "show"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--no-start"), "{}", stderr(&out));

    let out = Command::new(nd_bin())
        .args(["tools"])
        .env("NOTEDECK_SOCKET", &socket)
        .env("NOTEDECK_BIN", &bin)
        .env("ND_NO_START", "1")
        .output()
        .expect("run nd");
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(!dir.path().join("args").exists(), "started a notedeck");
}

/// A notedeck that can't be found, or exits without serving, is exit 2 at
/// once, naming why, not a 15 second wait.
#[test]
fn a_notedeck_that_wont_start_exits_2() {
    let dir = socket_dir();
    let socket = dir.path().join("nd.sock");

    let missing = dir.path().join("no-such-notedeck");
    let out = nd_starting(&socket, &missing, &["tools"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("there is no"), "{}", stderr(&out));

    let bin = notedeck_script(dir.path(), "echo boom >&2; exit 3");
    let started = std::time::Instant::now();
    let out = nd_starting(&socket, &bin, &["tools"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("exited"), "{}", stderr(&out));
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let log = std::fs::read_to_string(dir.path().join("nd.log")).unwrap();
    assert!(log.contains("boom"), "{log}");
}

/// `nd --help` lists the running notedeck's app commands, each namespace's
/// verbs under it, between nd's own groups and the options.
#[test]
fn help_lists_the_running_notedecks_app_commands() {
    let dir = socket_dir();
    let Fake { socket, server } = serve(&dir, vec![app_list()]);
    let out = nd(&socket, &["--help"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let page = stderr(&out);
    let apps = page.find("APPS (from the running notedeck)\n    headway\n");
    let serve = page.find("SERVE\n").unwrap();
    let options = page.find("OPTIONS (every command):").unwrap();
    assert!(
        apps.is_some_and(|apps| serve < apps && apps < options),
        "{page}"
    );
    assert!(
        page.contains("      show            Show card / Show board\n"),
        "{page}"
    );
    assert!(
        page.contains("App commands: `nd <app> <cmd> --help`."),
        "{page}"
    );
    assert!(!page.contains("no notedeck running"), "{page}");
    assert_eq!(server.join().unwrap(), vec![Request::ListTools]);
}

/// With nothing on the socket, `nd --help` prints the static page at once and
/// exits 0, without starting a notedeck to ask.
#[test]
fn help_without_notedeck_starts_none() {
    let dir = socket_dir();
    let socket = dir.path().join("nd.sock");
    let bin = notedeck_script(dir.path(), "exit 0");
    let started = std::time::Instant::now();
    for args in [&[][..], &["--help"], &["help"]] {
        let out = nd_starting(&socket, &bin, args);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        let page = stderr(&out);
        assert!(
            page.contains("App commands: `nd <app> <command>`"),
            "{page}"
        );
        assert!(
            page.contains("(no notedeck running: start one, or run `nd <app> --help`"),
            "{page}"
        );
        assert!(!page.contains("APPS"), "{page}");
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(!dir.path().join("args").exists(), "help started a notedeck");

    let out = nd_starting(&socket, &bin, &["--no-start", "--help"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("--no-start / $ND_NO_START keeps nd from starting one"),
        "{}",
        stderr(&out)
    );
}

/// A socket that takes the connection and never answers costs `nd --help`
/// its short wait, not a hang: it prints the page without the apps.
#[test]
fn help_doesnt_wait_on_a_socket_that_never_answers() {
    let dir = socket_dir();
    let socket = dir.path().join("nd.sock");
    let _listener = UnixListener::bind(&socket).expect("bind");
    let started = std::time::Instant::now();
    let out = nd(&socket, &["--help"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("no notedeck running"),
        "{}",
        stderr(&out)
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
}

/// A shim's command line, and the dir holding the `nd` it finds first on
/// PATH, which lives as long as the run needs it.
struct Shim {
    cmd: Command,
    _bin: TempDir,
}

/// `scripts/shims/<name>`: the bash shim installed as the old `<name>` CLI,
/// with [`nd_bin`] first on its PATH as `nd`.
fn shim(name: &str) -> Shim {
    let shim = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/shims")
        .join(name);
    let bin = tempfile::tempdir().expect("a bin dir");
    std::os::unix::fs::symlink(nd_bin(), bin.path().join("nd")).expect("link nd");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(bin.path().to_owned()).chain(std::env::split_paths(&path));
    let mut cmd = Command::new(shim);
    cmd.env("PATH", std::env::join_paths(dirs).expect("a PATH"))
        .env("ND_NO_START", "1")
        .env_remove("HEADWAY_COMMENT_NSEC")
        .env_remove("HEADWAY_COMMENT_NSEC_FILE")
        .env_remove("HEADWAY_BOARD");
    Shim { cmd, _bin: bin }
}

/// The `headway` shim is `nd headway`: an old-style `headway --board b show
/// --json`, its global before the verb, reaches the tool and prints its JSON.
#[test]
fn the_headway_shim_runs_nd_headway() {
    let dir = socket_dir();
    let result = Response::ok(Reply::Result {
        result: json!({"id": "b", "columns": []}),
        text: Some("board b\n".to_owned()),
        notice: None,
    });
    let Fake { socket, server } = serve(&dir, vec![app_list(), result]);
    let Shim { mut cmd, _bin } = shim("headway");
    let out = cmd
        .args(["--board", "b", "show", "--json"])
        .env("NOTEDECK_SOCKET", &socket)
        .output()
        .expect("run the shim");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let printed: Value = serde_json::from_str(&stdout(&out)).expect("JSON on stdout");
    assert_eq!(printed, json!({"id": "b", "columns": []}));
    let seen = server.join().unwrap();
    assert_eq!(
        seen[1],
        Request::CallTool {
            name: "headway_show_board".to_owned(),
            args: json!({"board": "b"}),
            signer: None,
        }
    );
}

/// A flag of the old CLIs' that nd has no counterpart for fails with one
/// line saying why, before anything reaches the socket; `agentium
/// --no-sync`, which nd always honours, is dropped instead.
#[test]
fn a_shim_refuses_the_old_identity_flags() {
    let dir = socket_dir();
    let socket = dir.path().join("none.sock");
    for (name, args) in [
        ("headway", &["--nsec", "nsec1x", "show"][..]),
        ("headway", &["show", "--relay=ws://x"][..]),
        ("agentium", &["--db", "/tmp/db", "list"][..]),
        ("notebook", &["--author", "abc", "show"][..]),
        ("headway", &["login"][..]),
    ] {
        let Shim { mut cmd, _bin } = shim(name);
        let out = cmd
            .args(args)
            .env("NOTEDECK_SOCKET", &socket)
            .output()
            .expect("run the shim");
        assert_eq!(out.status.code(), Some(1), "{name} {args:?}");
        let err = stderr(&out);
        assert_eq!(err.lines().count(), 1, "{err}");
        assert!(err.starts_with(&format!("{name}: ")), "{err}");
        assert!(err.contains("NOTEDECK_SOCKET"), "{err}");
    }

    let result = Response::ok(Reply::Result {
        result: json!([]),
        text: Some(String::new()),
        notice: None,
    });
    let list = Response::ok(Reply::Tools {
        tools: vec![ToolInfo {
            command: vec!["agentium".to_owned(), "list".to_owned()],
            name: "agentium_list_sessions".to_owned(),
            input_schema: json!({"type": "object", "properties": {}, "required": []}),
            ..tools().remove(0)
        }],
    });
    let Fake { socket, server } = serve(&dir, vec![list, result]);
    let Shim { mut cmd, _bin } = shim("agentium");
    let out = cmd
        .args(["--no-sync", "list"])
        .env("NOTEDECK_SOCKET", &socket)
        .output()
        .expect("run the shim");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let seen = server.join().unwrap();
    assert!(
        matches!(&seen[1], Request::CallTool { name, .. } if name == "agentium_list_sessions"),
        "{seen:?}"
    );
}
