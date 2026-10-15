//! `nd`: list and call the running notedeck's tools from a shell.
//!
//! Every run asks the running notedeck for its tool list over the local
//! socket ([`notedeck_ipc`]), so `nd` carries no copy of the tool names and
//! never goes stale against the app. `nd call` uses that list's schemas to
//! read `key=value` pairs as the right types and to reject a wrong key before
//! anything is sent.
//!
//! Exit codes: 0 when it worked, 1 for a tool error or a bad command line, and
//! 2 when no notedeck is listening, so a script can tell "not running" apart
//! from "the tool said no".

mod help;
mod listing;
mod mcp;
mod schema;

use cli_term::help::{CommandHelp, OPTIONS_PAGE, option_names};
use notedeck_ipc::{Outcome, PROTOCOL_VERSION, Reply, Request, Response, ToolInfo};
use serde_json::Value;
use std::io::{IsTerminal, Read};
use std::process::ExitCode;

/// What a command line asks for.
enum Invocation {
    Run(Command),
    /// `nd`, or `nd --help`: the command list.
    Usage,
    /// `nd <cmd> --help` or `nd help <cmd>`.
    CommandHelp(&'static CommandHelp),
    /// `nd help options`.
    Options,
    /// `nd --version`.
    Version,
}

/// A command to run against the socket.
#[derive(Debug, PartialEq)]
enum Command {
    /// `nd tools [--json] [<name-or-pattern>]`.
    Tools { json: bool, query: Option<String> },
    /// `nd call <name> [k=v…] [--json … | --json-file …]`.
    Call {
        name: String,
        pairs: Vec<String>,
        base: Option<JsonSource>,
    },
    /// `nd mcp [--ui] [--read-only]`.
    Mcp { exposure: mcp::Exposure },
}

/// Where `nd call`'s argument object comes from, besides the pairs.
#[derive(Debug, PartialEq)]
enum JsonSource {
    /// `--json <object>`.
    Inline(String),
    /// `--json-file <path>`, or stdin for `-`.
    File(String),
}

/// Why a run failed, and the exit code that says so.
struct Failure {
    code: u8,
    message: String,
}

impl Failure {
    /// A tool error or a bad command line: exit 1.
    fn new(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

/// Exit code for "nothing is listening on the socket".
const NOT_RUNNING: u8 = 2;

fn main() -> ExitCode {
    let invocation = match parse(std::env::args().skip(1)) {
        Ok(invocation) => invocation,
        Err(message) => {
            eprintln!("nd: {message}");
            return ExitCode::from(1);
        }
    };
    let result = match invocation {
        Invocation::Usage => {
            eprint!("{}", help::CLI.overview(help::GLOBALS));
            Ok(())
        }
        Invocation::CommandHelp(cmd) => {
            eprint!("{}", help::CLI.page(cmd));
            Ok(())
        }
        Invocation::Options => {
            eprint!(
                "{}",
                help::CLI.options_page(help::GLOBALS, help::OPTIONS_FOOTER)
            );
            Ok(())
        }
        Invocation::Version => {
            version();
            Ok(())
        }
        Invocation::Run(command) => run(command),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("nd: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

/// Parse the arguments after the program name.
///
/// Before the command only `--help` and `--version` are read. After it, a
/// flag is the command's when its help page lists it, and takes a value when
/// that entry spells one (`--json <object>`), so the parser and the page
/// can't drift; [`check_flags`](cli_term::help::Cli::check_flags) rejects any
/// other.
fn parse(args: impl IntoIterator<Item = String>) -> Result<Invocation, String> {
    let mut args = args.into_iter();
    let mut want_help = false;
    let mut help_command = false;

    let name = loop {
        let Some(arg) = args.next() else {
            return Ok(Invocation::Usage);
        };
        match arg.as_str() {
            "-h" | "--help" => want_help = true,
            "--version" => return Ok(Invocation::Version),
            "help" if !help_command => {
                help_command = true;
                want_help = true;
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag '{flag}' (try `nd --help`)"));
            }
            _ => break arg,
        }
    };

    if help_command && name == OPTIONS_PAGE {
        return Ok(Invocation::Options);
    }
    let Some(cmd) = help::CLI.lookup(&name) else {
        return Err(format!("unknown command '{name}' (try `nd --help`)"));
    };

    let mut flags: Vec<String> = Vec::new();
    let mut values: Vec<(String, String)> = Vec::new();
    let mut positionals: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            want_help = true;
            continue;
        }
        if !arg.starts_with('-') || arg == "-" {
            positionals.push(arg);
            continue;
        }
        if takes_value(cmd, &arg) {
            let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
            values.push((arg.clone(), value));
        }
        flags.push(arg);
    }

    if want_help {
        return Ok(Invocation::CommandHelp(cmd));
    }
    help::CLI.check_flags(cmd, help::GLOBALS, &flags)?;
    let has = |flag: &str| flags.iter().any(|f| f == flag);
    let value = |flag: &str| values.iter().find(|(f, _)| f == flag).map(|(_, v)| v);

    let command = match cmd.name {
        "tools" => {
            if positionals.len() > 1 {
                return Err("`tools` takes one name or pattern".to_owned());
            }
            Command::Tools {
                json: has("--json"),
                query: positionals.pop(),
            }
        }
        "call" => {
            let mut positionals = positionals.into_iter();
            let name = positionals
                .next()
                .ok_or("`call` needs a tool name (see `nd tools`)")?;
            let base = match (value("--json"), value("--json-file")) {
                (Some(_), Some(_)) => {
                    return Err("pass either --json or --json-file, not both".to_owned());
                }
                (Some(json), None) => Some(JsonSource::Inline(json.clone())),
                (None, Some(path)) => Some(JsonSource::File(path.clone())),
                (None, None) => None,
            };
            Command::Call {
                name,
                pairs: positionals.collect(),
                base,
            }
        }
        "mcp" => {
            if !positionals.is_empty() {
                return Err("`mcp` takes no arguments".to_owned());
            }
            Command::Mcp {
                exposure: mcp::Exposure {
                    ui: has("--ui"),
                    read_only: has("--read-only"),
                },
            }
        }
        other => unreachable!("'{other}' is in the help table but not dispatched"),
    };
    Ok(Invocation::Run(command))
}

/// Whether `flag` is one of `cmd`'s options and its entry names a value.
fn takes_value(cmd: &CommandHelp, flag: &str) -> bool {
    cmd.options
        .iter()
        .any(|(key, _)| key.contains('<') && option_names(key).any(|name| name == flag))
}

/// Run `command` against the socket.
fn run(command: Command) -> Result<(), Failure> {
    match command {
        Command::Tools { json, query } => tools(json, query.as_deref()),
        Command::Call { name, pairs, base } => call(&name, &pairs, base),
        Command::Mcp { exposure } => mcp::serve(exposure, &mut answer),
    }
}

/// `nd tools`.
fn tools(json: bool, query: Option<&str>) -> Result<(), Failure> {
    let tools = list_tools()?;

    if let Some(tool) = query.and_then(|q| tools.iter().find(|t| t.name == q)) {
        if json {
            print_json(&to_value(tool));
        } else {
            print!("{}", listing::page(tool));
        }
        return Ok(());
    }

    let pattern = query
        .map(|q| cli_term::compile_pattern(q, cli_term::CaseMode::Smart))
        .transpose()?;
    let hits = listing::matching(&tools, pattern.as_ref());
    if hits.is_empty() {
        let query = query.unwrap_or_default();
        return Err(Failure::new(format!("no tool matches '{query}'")));
    }
    if json {
        print_json(&Value::Array(hits.into_iter().map(to_value).collect()));
    } else {
        print!("{}", listing::overview(&hits));
    }
    Ok(())
}

/// `nd call`.
fn call(name: &str, pairs: &[String], base: Option<JsonSource>) -> Result<(), Failure> {
    let base = base.map(read_json).transpose()?;
    let tools = list_tools()?;
    let Some(tool) = tools.iter().find(|t| t.name == name) else {
        return Err(Failure::new(format!(
            "the running notedeck has no tool '{name}' (see `nd tools`)"
        )));
    };
    let args = schema::build(name, &tool.input_schema, base, pairs)?;

    let request = Request::CallTool {
        name: name.to_owned(),
        args,
    };
    match answer(request)? {
        Reply::Result { result } => print_json(&result),
        Reply::Intent { .. } => {}
        Reply::Spawned { session_id } => {
            print_json(&serde_json::json!({ "session_id": session_id }))
        }
        Reply::Tools { .. } => {
            return Err(Failure::new("notedeck answered a call with a tool list"));
        }
    }
    Ok(())
}

/// The argument object a `--json` or `--json-file` names.
fn read_json(source: JsonSource) -> Result<Value, Failure> {
    let (text, from) = match source {
        JsonSource::Inline(text) => (text, "--json".to_owned()),
        JsonSource::File(path) if path == "-" => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| Failure::new(format!("reading stdin: {e}")))?;
            (text, "stdin".to_owned())
        }
        JsonSource::File(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| Failure::new(format!("reading {path}: {e}")))?;
            (text, path)
        }
    };
    serde_json::from_str(&text).map_err(|e| Failure::new(format!("{from} is not JSON: {e}")))
}

/// `nd --version`: this build's version, then the running notedeck's when
/// one answers.
fn version() {
    println!("nd {}", env!("CARGO_PKG_VERSION"));
    if let Ok(response) = exchange(&Request::ListTools) {
        println!("notedeck {}", response.notedeck);
    }
}

/// The running notedeck's tools.
fn list_tools() -> Result<Vec<ToolInfo>, Failure> {
    match answer(Request::ListTools)? {
        Reply::Tools { tools } => Ok(tools),
        _ => Err(Failure::new(
            "notedeck answered list_tools without a tool list",
        )),
    }
}

/// Send `request` and unwrap its outcome: a tool's error is a [`Failure`].
fn answer(request: Request) -> Result<Reply, Failure> {
    let response = exchange(&request)?;
    if response.v != PROTOCOL_VERSION {
        return Err(Failure::new(format!(
            "notedeck {} speaks protocol version {}, and this nd speaks {PROTOCOL_VERSION}",
            response.notedeck, response.v
        )));
    }
    match response.outcome {
        Outcome::Ok(reply) => Ok(reply),
        Outcome::Error { message } => Err(Failure::new(message)),
    }
}

/// One request and its response over a fresh connection.
#[cfg(unix)]
fn exchange(request: &Request) -> Result<Response, Failure> {
    use std::io::ErrorKind;

    let path = notedeck_ipc::socket_path();
    let mut client = match notedeck_ipc::Client::connect(&path) {
        Ok(client) => client,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            return Err(Failure {
                code: NOT_RUNNING,
                message: format!("notedeck isn't running (no socket at {})", path.display()),
            });
        }
        Err(e) => {
            return Err(Failure::new(format!(
                "connecting to {}: {e}",
                path.display()
            )));
        }
    };
    client
        .request(request)
        .map_err(|e| Failure::new(format!("talking to notedeck: {e}")))
}

/// There is no socket off Unix, so no notedeck to reach.
#[cfg(not(unix))]
fn exchange(_request: &Request) -> Result<Response, Failure> {
    Err(Failure {
        code: NOT_RUNNING,
        message: "nd needs notedeck's Unix socket, which this platform doesn't have".to_owned(),
    })
}

/// A tool as JSON.
fn to_value(tool: &ToolInfo) -> Value {
    serde_json::to_value(tool).expect("a tool always serializes")
}

/// `value` on stdout: indented for a person at a terminal, one line for a
/// pipe.
fn print_json(value: &Value) {
    let text = if std::io::stdout().is_terminal() {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    };
    println!("{}", text.expect("a JSON value always serializes"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Invocation, String> {
        parse(args.iter().map(|a| a.to_string()))
    }

    fn err_of(args: &[&str]) -> String {
        match parse_args(args) {
            Err(err) => err,
            Ok(_) => panic!("{args:?} parsed"),
        }
    }

    fn command_of(args: &[&str]) -> Command {
        match parse_args(args) {
            Ok(Invocation::Run(command)) => command,
            _ => panic!("{args:?} is not a run"),
        }
    }

    fn page_of(args: &[&str]) -> Option<&'static str> {
        match parse_args(args) {
            Ok(Invocation::CommandHelp(cmd)) => Some(cmd.name),
            _ => None,
        }
    }

    #[test]
    fn help_picks_the_overview_or_a_page() {
        assert!(matches!(parse_args(&[]), Ok(Invocation::Usage)));
        assert!(matches!(parse_args(&["--help"]), Ok(Invocation::Usage)));
        assert!(matches!(parse_args(&["help"]), Ok(Invocation::Usage)));
        assert_eq!(page_of(&["call", "--help"]), Some("call"));
        assert_eq!(page_of(&["help", "tools"]), Some("tools"));
        assert_eq!(page_of(&["--help", "call"]), Some("call"));
        assert_eq!(page_of(&["call", "x", "--bogus", "--help"]), Some("call"));
        assert!(matches!(
            parse_args(&["help", "options"]),
            Ok(Invocation::Options)
        ));
        assert!(matches!(
            parse_args(&["--version"]),
            Ok(Invocation::Version)
        ));
    }

    #[test]
    fn unknown_commands_and_flags_are_rejected() {
        let err = err_of(&["frob"]);
        assert!(err.contains("unknown command 'frob'"), "{err}");
        let err = err_of(&["tools", "--json-file", "x"]);
        assert!(err.contains("`tools` doesn't take --json-file"), "{err}");
        let err = err_of(&["--bogus", "tools"]);
        assert!(err.contains("unknown flag '--bogus'"), "{err}");
        let err = err_of(&["options", "--help"]);
        assert!(err.contains("unknown command 'options'"), "{err}");
    }

    #[test]
    fn json_is_a_switch_for_tools_and_a_value_for_call() {
        assert_eq!(
            command_of(&["tools", "--json", "headway"]),
            Command::Tools {
                json: true,
                query: Some("headway".to_owned())
            }
        );
        assert_eq!(
            command_of(&["call", "t", "--json", "{}", "a=1"]),
            Command::Call {
                name: "t".to_owned(),
                pairs: vec!["a=1".to_owned()],
                base: Some(JsonSource::Inline("{}".to_owned())),
            }
        );
        assert_eq!(
            command_of(&["call", "t", "--json-file", "-"]),
            Command::Call {
                name: "t".to_owned(),
                pairs: vec![],
                base: Some(JsonSource::File("-".to_owned())),
            }
        );
    }

    #[test]
    fn mcp_reads_its_exposure_flags() {
        assert_eq!(
            command_of(&["mcp"]),
            Command::Mcp {
                exposure: mcp::Exposure {
                    ui: false,
                    read_only: false
                }
            }
        );
        assert_eq!(
            command_of(&["mcp", "--ui", "--read-only"]),
            Command::Mcp {
                exposure: mcp::Exposure {
                    ui: true,
                    read_only: true
                }
            }
        );
        let err = err_of(&["mcp", "extra"]);
        assert!(err.contains("takes no arguments"), "{err}");
    }

    #[test]
    fn call_needs_a_name_and_one_json_source() {
        let err = err_of(&["call"]);
        assert!(err.contains("needs a tool name"), "{err}");
        let err = err_of(&["call", "t", "--json", "{}", "--json-file", "x"]);
        assert!(err.contains("not both"), "{err}");
        let err = err_of(&["call", "t", "--json"]);
        assert!(err.contains("--json needs a value"), "{err}");
    }
}
