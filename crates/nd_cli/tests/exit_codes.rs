//! `nd` against a fake notedeck: a socket in a temp dir, named by
//! `NOTEDECK_SOCKET`, that answers each request from a script. Checks what a
//! script can rely on — the exit code, and what lands on stdout and stderr —
//! without the real notedeck or the user's real socket.
#![cfg(unix)]

use notedeck_ipc::{IntentStatus, Reply, Request, Response, ToolInfo, ToolKind};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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
    }]
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
    let path = dir.path().join("nd.sock");
    let listener = UnixListener::bind(&path).expect("bind the fake socket");
    let server = thread::spawn(move || {
        let mut seen = Vec::new();
        for answer in answers {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read the request");
            seen.push(Request::from_line(&line).expect("a request nd sent"));
            writeln!(reader.get_mut(), "{}", answer.to_line()).expect("answer");
        }
        seen
    });
    Fake {
        socket: path,
        server,
    }
}

fn nd(socket: &Path, args: &[&str]) -> Output {
    Command::new(nd_bin())
        .args(args)
        .env("NOTEDECK_SOCKET", socket)
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
        }
    );
}

#[test]
fn data_prints_as_json_and_exits_0() {
    let dir = socket_dir();
    let result = Response::ok(Reply::Result {
        result: json!({"cards": [1, 2]}),
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
    assert!(err.contains("this nd speaks 1"), "{err}");
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
