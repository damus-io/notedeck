//! `nd mcp`: a stdio MCP server in front of the running notedeck's socket.
//!
//! An MCP client starts `nd mcp` and speaks newline-delimited JSON-RPC 2.0 on
//! its stdin and stdout. `tools/list` becomes the socket's `list_tools` and
//! `tools/call` its `call_tool`, each over a fresh connection, so the server
//! starts without a notedeck and picks one up whenever it appears. Nothing
//! but JSON-RPC goes to stdout; diagnostics go to stderr.
//!
//! By default only data tools are exposed: an agent shouldn't drive someone's
//! window unless they asked for that with `--ui`. `--read-only` further keeps
//! only the tools that change nothing. The same [`Exposure`] guards
//! `tools/call`, so a hidden tool can't be called by name either.

use crate::Failure;
use notedeck_ipc::{Reply, Request, ToolInfo, ToolKind};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

/// MCP protocol revisions this server speaks, newest first. A client asking
/// for one of these gets it back; any other gets the newest.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC: the line wasn't JSON.
const PARSE_ERROR: i64 = -32700;
/// JSON-RPC: JSON, but not a request.
const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC: no such method.
const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC: the method's params are wrong.
const INVALID_PARAMS: i64 = -32602;
/// JSON-RPC: the server couldn't answer, e.g. notedeck isn't running.
const INTERNAL_ERROR: i64 = -32603;

/// Which of notedeck's tools the server shows and lets a client call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exposure {
    /// Include tools that act on the window (`--ui`).
    pub ui: bool,
    /// Keep only tools that change nothing (`--read-only`).
    pub read_only: bool,
}

impl Exposure {
    /// Whether a client may see and call `tool`.
    fn allows(self, tool: &ToolInfo) -> bool {
        (self.ui || tool.kind == ToolKind::Data) && (!self.read_only || tool.read_only)
    }

    /// The flag that would expose `tool`, for the error a hidden call gets.
    fn hint(self, tool: &ToolInfo) -> &'static str {
        if tool.kind == ToolKind::Ui && !self.ui {
            "it acts on the window; start `nd mcp --ui` to allow that"
        } else {
            "it changes data, and this server was started with --read-only"
        }
    }
}

/// Serve MCP on stdin and stdout until stdin closes, asking notedeck through
/// `ask`.
pub fn serve(
    exposure: Exposure,
    ask: &mut dyn FnMut(Request) -> Result<Reply, Failure>,
) -> Result<(), Failure> {
    let stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lines() {
        let line = line.map_err(|e| Failure::new(format!("reading stdin: {e}")))?;
        let Some(response) = handle_line(exposure, &line, ask) else {
            continue;
        };
        writeln!(stdout, "{response}")
            .and_then(|()| stdout.flush())
            .map_err(|e| Failure::new(format!("writing stdout: {e}")))?;
    }
    Ok(())
}

/// The response to one line of input, or `None` for a notification or a
/// blank line.
pub fn handle_line(
    exposure: Exposure,
    line: &str,
    ask: &mut dyn FnMut(Request) -> Result<Reply, Failure>,
) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(e) => return Some(error(Value::Null, PARSE_ERROR, format!("not JSON: {e}"))),
    };
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        return Some(error(id, INVALID_REQUEST, "not a JSON-RPC request"));
    };
    // A notification has no id and gets no answer, whatever its method.
    let id = message.get("id")?.clone();
    let params = message.get("params").unwrap_or(&Value::Null);

    let result = match method {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok(json!({})),
        "tools/list" => tools_list(exposure, ask),
        "tools/call" => tools_call(exposure, params, ask),
        other => Err(RpcError {
            code: METHOD_NOT_FOUND,
            message: format!("nd mcp has no method '{other}'"),
        }),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(e) => error(id, e.code, e.message),
    })
}

/// A JSON-RPC error a method answers with.
struct RpcError {
    code: i64,
    message: String,
}

impl From<Failure> for RpcError {
    fn from(failure: Failure) -> Self {
        Self {
            code: INTERNAL_ERROR,
            message: failure.message,
        }
    }
}

/// A JSON-RPC error response.
fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message.into()},
    })
}

/// `initialize`: the protocol revision, and that this server has tools.
fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "nd", "version": env!("CARGO_PKG_VERSION")},
        "instructions": "Tools of the running notedeck, called over its local socket. \
            A call fails with 'notedeck isn't running' until notedeck starts.",
    })
}

/// `tools/list`: notedeck's tools, less the ones `exposure` hides.
fn tools_list(
    exposure: Exposure,
    ask: &mut dyn FnMut(Request) -> Result<Reply, Failure>,
) -> Result<Value, RpcError> {
    let tools = list_tools(ask)?;
    let listed = tools
        .iter()
        .filter(|tool| exposure.allows(tool))
        .map(|tool| {
            json!({
                "name": tool.name,
                "title": tool.title,
                "description": tool.description,
                "inputSchema": tool.input_schema,
                "annotations": {
                    "title": tool.title,
                    "readOnlyHint": tool.read_only,
                },
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "tools": listed }))
}

/// `tools/call`: the call over the socket, its answer as MCP content.
///
/// A tool that fails, or one `exposure` hides, is a result with `isError` so
/// the model sees why; only a malformed request is a JSON-RPC error.
fn tools_call(
    exposure: Exposure,
    params: &Value,
    ask: &mut dyn FnMut(Request) -> Result<Reply, Failure>,
) -> Result<Value, RpcError> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err(RpcError {
            code: INVALID_PARAMS,
            message: "tools/call needs a tool name".to_owned(),
        });
    };
    let args = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(args @ Value::Object(_)) => args.clone(),
        Some(_) => {
            return Err(RpcError {
                code: INVALID_PARAMS,
                message: "tools/call arguments must be an object".to_owned(),
            });
        }
    };

    let tools = match list_tools(ask) {
        Ok(tools) => tools,
        Err(failure) => return Ok(tool_error(failure.message)),
    };
    let Some(tool) = tools.iter().find(|t| t.name == name) else {
        return Ok(tool_error(format!(
            "the running notedeck has no tool '{name}'"
        )));
    };
    if !exposure.allows(tool) {
        return Ok(tool_error(format!(
            "{name} isn't exposed: {}",
            exposure.hint(tool)
        )));
    }

    let request = Request::CallTool {
        name: name.to_owned(),
        args,
    };
    Ok(match ask(request) {
        Ok(Reply::Result { result }) => data(result),
        Ok(Reply::Intent { .. }) => text(format!("done: {name} acted on the window"), false),
        Ok(Reply::Tools { .. }) => tool_error("notedeck answered a call with a tool list"),
        Err(failure) => tool_error(failure.message),
    })
}

/// The running notedeck's tools.
fn list_tools(
    ask: &mut dyn FnMut(Request) -> Result<Reply, Failure>,
) -> Result<Vec<ToolInfo>, Failure> {
    match ask(Request::ListTools)? {
        Reply::Tools { tools } => Ok(tools),
        _ => Err(Failure::new(
            "notedeck answered list_tools without a tool list",
        )),
    }
}

/// A tool's data: a JSON text block, and the value itself when it's an
/// object, which is what MCP's `structuredContent` holds.
fn data(result: Value) -> Value {
    let body = serde_json::to_string(&result).expect("a JSON value always serializes");
    let mut content = text(body, false);
    if result.is_object() {
        content["structuredContent"] = result;
    }
    content
}

/// A failed call, as the model should see it.
fn tool_error(message: impl Into<String>) -> Value {
    text(message.into(), true)
}

/// A call result holding one text block.
fn text(body: String, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": body}],
        "isError": is_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notedeck_ipc::IntentStatus;

    fn tool(name: &str, kind: ToolKind, read_only: bool) -> ToolInfo {
        ToolInfo {
            name: name.to_owned(),
            title: name.to_owned(),
            description: format!("{name} does a thing"),
            category: None,
            kind,
            read_only,
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }

    fn tools() -> Vec<ToolInfo> {
        vec![
            tool("read_board", ToolKind::Data, true),
            tool("move_card", ToolKind::Data, false),
            tool("chrome_open", ToolKind::Ui, false),
        ]
    }

    const DATA: Exposure = Exposure {
        ui: false,
        read_only: false,
    };

    /// What [`run`] saw: the answer to the line, and what was asked of notedeck.
    struct Ran {
        response: Option<Value>,
        seen: Vec<Request>,
    }

    /// Answer `line` against a fake notedeck that lists [`tools`] and answers
    /// every call with `call`.
    fn run(exposure: Exposure, line: &str, call: Result<Reply, String>) -> Ran {
        let mut seen = Vec::new();
        let mut ask = |request: Request| {
            seen.push(request.clone());
            match request {
                Request::ListTools => Ok(Reply::Tools { tools: tools() }),
                _ => call.clone().map_err(Failure::new),
            }
        };
        let response = handle_line(exposure, line, &mut ask);
        Ran { response, seen }
    }

    fn listed_names(exposure: Exposure) -> Vec<String> {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let Ran { response, .. } = run(exposure, line, Err("unused".to_owned()));
        response.unwrap()["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    }

    fn call_line(name: &str, args: Value) -> String {
        json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": name, "arguments": args},
        })
        .to_string()
    }

    #[test]
    fn initialize_echoes_a_known_revision_and_offers_tools() {
        let line = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
        let Ran { response, seen } = run(DATA, line, Err("unused".to_owned()));
        let result = &response.unwrap()["result"];
        assert_eq!(result["protocolVersion"], "2025-03-26");
        assert_eq!(result["serverInfo"]["name"], "nd");
        assert!(result["capabilities"]["tools"].is_object());
        assert!(seen.is_empty(), "initialize needs no notedeck");

        let line = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#;
        let Ran { response, .. } = run(DATA, line, Err("unused".to_owned()));
        assert_eq!(response.unwrap()["result"]["protocolVersion"], "2025-06-18");
    }

    #[test]
    fn notifications_and_blank_lines_get_no_answer() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(run(DATA, line, Err("unused".to_owned())).response, None);
        assert_eq!(run(DATA, "  ", Err("unused".to_owned())).response, None);
    }

    #[test]
    fn bad_lines_and_methods_are_jsonrpc_errors() {
        let Ran { response, .. } = run(DATA, "{oops", Err("unused".to_owned()));
        let response = response.unwrap();
        assert_eq!(response["id"], Value::Null);
        assert_eq!(response["error"]["code"], PARSE_ERROR);

        let line = r#"{"jsonrpc":"2.0","id":"a","method":"resources/list"}"#;
        let response = run(DATA, line, Err("unused".to_owned())).response.unwrap();
        assert_eq!(response["id"], "a");
        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);

        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_board","arguments":[1]}}"#;
        let response = run(DATA, line, Err("unused".to_owned())).response.unwrap();
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn exposure_picks_which_tools_are_listed() {
        assert_eq!(listed_names(DATA), ["read_board", "move_card"]);
        let all = Exposure {
            ui: true,
            read_only: false,
        };
        assert_eq!(
            listed_names(all),
            ["read_board", "move_card", "chrome_open"]
        );
        let read_only = Exposure {
            ui: true,
            read_only: true,
        };
        assert_eq!(listed_names(read_only), ["read_board"]);
    }

    #[test]
    fn listing_carries_the_schema_and_read_only_hint() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let response = run(DATA, line, Err("unused".to_owned())).response.unwrap();
        let first = &response["result"]["tools"][0];
        assert_eq!(
            first["inputSchema"],
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(first["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn data_comes_back_as_text_and_structured_content() {
        let reply = Ok(Reply::Result {
            result: json!({"cards": [1, 2]}),
        });
        let line = call_line("read_board", json!({"board": "notedeck"}));
        let Ran { response, seen } = run(DATA, &line, reply);
        let result = &response.unwrap()["result"];
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["text"], r#"{"cards":[1,2]}"#);
        assert_eq!(result["structuredContent"], json!({"cards": [1, 2]}));
        assert_eq!(
            seen[1],
            Request::CallTool {
                name: "read_board".to_owned(),
                args: json!({"board": "notedeck"}),
            }
        );
    }

    #[test]
    fn an_applied_intent_is_a_short_done() {
        let all = Exposure {
            ui: true,
            read_only: false,
        };
        let reply = Ok(Reply::Intent {
            intent: IntentStatus::Applied,
        });
        let line = call_line("chrome_open", json!({}));
        let result = run(all, &line, reply).response.unwrap()["result"].clone();
        assert_eq!(result["isError"], false);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("done")
        );
    }

    #[test]
    fn a_tool_error_is_an_error_result() {
        let line = call_line("move_card", json!({}));
        let response = run(DATA, &line, Err("no such card".to_owned()))
            .response
            .unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(response["result"]["content"][0]["text"], "no such card");
    }

    #[test]
    fn a_hidden_tool_is_refused_without_calling_it() {
        let line = call_line("chrome_open", json!({}));
        let Ran { response, seen } = run(DATA, &line, Err("unused".to_owned()));
        let result = &response.unwrap()["result"];
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("--ui"),
            "{result}"
        );
        assert_eq!(seen, [Request::ListTools]);

        let read_only = Exposure {
            ui: false,
            read_only: true,
        };
        let line = call_line("move_card", json!({}));
        let Ran { response, seen } = run(read_only, &line, Err("unused".to_owned()));
        let text = response.unwrap()["result"]["content"][0]["text"].clone();
        assert!(text.as_str().unwrap().contains("--read-only"), "{text}");
        assert_eq!(seen, [Request::ListTools]);
    }
}
