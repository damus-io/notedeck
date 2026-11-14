//! `nd`: list and call the running notedeck's tools from a shell.
//!
//! Every run asks the running notedeck for its tool list over the local
//! socket ([`notedeck_ipc`]), so `nd` carries no copy of the tool names and
//! never goes stale against the app. `nd call` uses that list's schemas to
//! read `key=value` pairs as the right types and to reject a wrong key before
//! anything is sent. A tool that declares a command path is also a
//! subcommand, `nd headway show <card>`, built from that same list
//! ([`commands`]).
//!
//! When no notedeck is listening, nd starts a data-only one itself
//! ([`start`]), so a call works whether or not the app is open.
//!
//! Exit codes: 0 when it worked, 1 for a tool error or a bad command line, and
//! 2 when no notedeck is listening and none could be started (or `--no-start`
//! said not to), so a script can tell "not running" apart from "the tool said
//! no".

mod caller;
mod commands;
mod help;
mod listing;
mod mcp;
mod output;
mod schema;
mod start;

use cli_term::help::{CommandHelp, OPTIONS_PAGE, option_names};
use notedeck_ipc::{Outcome, PROTOCOL_VERSION, Reply, Request, ToolInfo};
use output::print_json;
use serde_json::Value;
use start::{Reach, Start};
use std::io::Read;
use std::process::ExitCode;

/// What a command line asks for.
enum Invocation {
    /// A command, and whether nd may start a notedeck to run it in.
    Run { command: Command, no_start: bool },
    /// `nd`, or `nd --help`: the command list.
    Usage,
    /// `nd <cmd> --help` or `nd help <cmd>`.
    CommandHelp(&'static CommandHelp<'static>),
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
    /// `nd spawn [--json] [<dir>]`.
    Spawn { json: bool, dir: Option<String> },
    /// `nd <ns> <verb> …`: a command a tool declares, read against the tool
    /// list once it's fetched. `help` is set for `nd help <ns> …`.
    App { words: Vec<String>, help: bool },
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
pub(crate) const NOT_RUNNING: u8 = 2;

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
            eprint!("{}{}", help::CLI.overview(help::GLOBALS), help::APPS_HINT);
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
            version(&Reach::new(Start::Never));
            Ok(())
        }
        Invocation::Run { command, no_start } => {
            let env = std::env::var(start::NO_START_ENV).ok();
            run(
                command,
                &Reach::new(Start::resolve(no_start, env.as_deref())),
            )
        }
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
/// Before the command only `--help`, `--version` and `--no-start` are read;
/// `--no-start` is also read after one of nd's own commands. After it, a
/// flag is the command's when its help page lists it, and takes a value when
/// that entry spells one (`--json <object>`), so the parser and the page
/// can't drift; [`check_flags`](cli_term::help::Cli::check_flags) rejects any
/// other.
fn parse(args: impl IntoIterator<Item = String>) -> Result<Invocation, String> {
    let mut args = args.into_iter();
    let mut want_help = false;
    let mut help_command = false;
    let mut no_start = false;

    let name = loop {
        let Some(arg) = args.next() else {
            return Ok(Invocation::Usage);
        };
        match arg.as_str() {
            "-h" | "--help" => want_help = true,
            "--version" => return Ok(Invocation::Version),
            "--no-start" => no_start = true,
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
        // Not one of nd's own, so maybe an app's: only the tool list can say.
        let words = std::iter::once(name).chain(args).collect();
        return Ok(Invocation::Run {
            command: Command::App {
                words,
                help: want_help,
            },
            no_start,
        });
    };

    let mut flags: Vec<String> = Vec::new();
    let mut values: Vec<(String, String)> = Vec::new();
    let mut positionals: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            want_help = true;
            continue;
        }
        if arg == "--no-start" {
            no_start = true;
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
        "spawn" => {
            if positionals.len() > 1 {
                return Err("`spawn` takes one directory".to_owned());
            }
            Command::Spawn {
                json: has("--json"),
                dir: positionals.pop(),
            }
        }
        other => unreachable!("'{other}' is in the help table but not dispatched"),
    };
    Ok(Invocation::Run { command, no_start })
}

/// Whether `flag` is one of `cmd`'s options and its entry names a value.
fn takes_value(cmd: &CommandHelp, flag: &str) -> bool {
    cmd.options
        .iter()
        .any(|(key, _)| key.contains('<') && option_names(key).any(|name| name == flag))
}

/// Run `command` against the socket `reach` names.
fn run(command: Command, reach: &Reach) -> Result<(), Failure> {
    match command {
        Command::Tools { json, query } => tools(reach, json, query.as_deref()),
        Command::Call { name, pairs, base } => call(reach, &name, &pairs, base),
        Command::Mcp { exposure } => mcp::serve(exposure, &mut |request| answer(reach, request)),
        Command::Spawn { json, dir } => spawn(reach, json, dir.as_deref()),
        Command::App { words, help } => app(reach, &words, help),
    }
}

/// `nd <ns> <verb> …`: resolve the line against the running notedeck's
/// tools, then call the one it names, or print the page it asks for.
fn app(reach: &Reach, words: &[String], help: bool) -> Result<(), Failure> {
    let tools = list_tools(reach)?;
    match commands::resolve(&tools, words, help, &caller::Process)? {
        commands::Action::Help(page) => {
            eprint!("{page}");
            Ok(())
        }
        commands::Action::Call {
            tool,
            args,
            output,
            ns,
            signer,
        } => send_call(
            reach,
            tool,
            args,
            signer,
            Print::Command { output, ns: &ns },
        ),
    }
}

/// `nd tools`.
fn tools(reach: &Reach, json: bool, query: Option<&str>) -> Result<(), Failure> {
    let tools = list_tools(reach)?;

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
fn call(
    reach: &Reach,
    name: &str,
    pairs: &[String],
    base: Option<JsonSource>,
) -> Result<(), Failure> {
    let base = base.map(read_json).transpose()?;
    let tools = list_tools(reach)?;
    let Some(tool) = tools.iter().find(|t| t.name == name) else {
        return Err(Failure::new(format!(
            "the running notedeck has no tool '{name}' (see `nd tools`)"
        )));
    };
    let args = schema::build(name, &tool.input_schema, base, pairs)?;
    let signer = caller::signer_for(tool, &caller::Process)?;
    send_call(reach, name.to_owned(), args, signer, Print::Value)
}

/// How [`send_call`] prints a tool's answer.
enum Print<'a> {
    /// The value, as `nd call` always has: its `--json` is the argument
    /// object, not an output mode.
    Value,
    /// As `nd <ns> <verb>`'s flags say: the text the tool rendered, else
    /// its value.
    Command { output: output::Output, ns: &'a str },
}

/// Call tool `name` with `args`, already checked against its schema, and
/// `signer` beside them, and print what it answers.
fn send_call(
    reach: &Reach,
    name: String,
    args: Value,
    signer: Option<notedeck_ipc::Signer>,
    print: Print,
) -> Result<(), Failure> {
    match answer(reach, Request::CallTool { name, args, signer })? {
        Reply::Result { result, text } => match print {
            Print::Value => print_json(&result),
            Print::Command { output, ns } => output.print(&result, text.as_deref(), ns),
        },
        Reply::Intent { .. } => {}
        Reply::Tools { .. } => {
            return Err(Failure::new("notedeck answered a call with a tool list"));
        }
    }
    Ok(())
}

/// Dave's tool that opens a session in a directory, which `nd spawn` calls.
const SPAWN_SESSION: &str = "dave_spawn_session";

/// `nd spawn`: open a Dave session in `dir` (the working directory when
/// absent) and print its id. The chrome lists the tool and starts Dave for
/// the call even while Dave isn't running (the spec's `starts_app`), so this
/// calls it straight away rather than looking it up first.
fn spawn(reach: &Reach, json: bool, dir: Option<&str>) -> Result<(), Failure> {
    let cwd = spawn_dir(dir)?;
    let request = Request::CallTool {
        name: SPAWN_SESSION.to_owned(),
        args: serde_json::json!({ "cwd": cwd }),
        signer: None,
    };
    let reply = answer(reach, request).map_err(|failure| match failure.code {
        NOT_RUNNING => failure,
        _ if failure.message.starts_with("unknown tool") => Failure::new(
            "the running notedeck has no Dave to spawn a session in (built without it?)",
        ),
        _ => Failure::new(format!(
            "spawning in {}: {}",
            cwd.display(),
            failure.message
        )),
    })?;
    let Reply::Result { result, .. } = reply else {
        return Err(Failure::new("notedeck answered a spawn without data"));
    };
    let Some(session_id) = result["session_id"].as_u64() else {
        return Err(Failure::new(format!(
            "notedeck answered a spawn without a session id: {result}"
        )));
    };
    if json {
        println!("{session_id}");
    } else {
        println!("Spawned session {session_id}");
    }
    Ok(())
}

/// The directory `nd spawn` names, made absolute and checked here, so a typo
/// fails in the shell rather than in the app.
fn spawn_dir(dir: Option<&str>) -> Result<std::path::PathBuf, Failure> {
    let dir = match dir {
        Some(dir) => std::path::PathBuf::from(dir),
        None => std::env::current_dir()
            .map_err(|e| Failure::new(format!("reading the working directory: {e}")))?,
    };
    let cwd = dir
        .canonicalize()
        .map_err(|e| Failure::new(format!("{}: {e}", dir.display())))?;
    if !cwd.is_dir() {
        return Err(Failure::new(format!(
            "{} is not a directory",
            cwd.display()
        )));
    }
    Ok(cwd)
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
/// one answers. It never starts one to ask.
fn version(reach: &Reach) {
    println!("nd {}", env!("CARGO_PKG_VERSION"));
    if let Ok(response) = reach.exchange(&Request::ListTools) {
        println!("notedeck {}", response.notedeck);
    }
}

/// The running notedeck's tools.
fn list_tools(reach: &Reach) -> Result<Vec<ToolInfo>, Failure> {
    match answer(reach, Request::ListTools)? {
        Reply::Tools { tools } => Ok(tools),
        _ => Err(Failure::new(
            "notedeck answered list_tools without a tool list",
        )),
    }
}

/// Send `request` and unwrap its outcome: a tool's error is a [`Failure`].
fn answer(reach: &Reach, request: Request) -> Result<Reply, Failure> {
    let response = reach.exchange(&request)?;
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

/// A tool as JSON.
fn to_value(tool: &ToolInfo) -> Value {
    serde_json::to_value(tool).expect("a tool always serializes")
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
            Ok(Invocation::Run { command, .. }) => command,
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

    /// `--no-start` reads before any command, and after one of nd's own.
    #[test]
    fn no_start_is_read_before_or_after_a_command() {
        let no_start_of = |args: &[&str]| match parse_args(args) {
            Ok(Invocation::Run { no_start, .. }) => no_start,
            _ => panic!("{args:?} is not a run"),
        };
        assert!(!no_start_of(&["tools"]));
        assert!(no_start_of(&["--no-start", "tools"]));
        assert!(no_start_of(&["tools", "--no-start"]));
        assert!(no_start_of(&["--no-start", "headway", "show"]));
        assert_eq!(
            command_of(&["--no-start", "headway", "show"]),
            Command::App {
                words: vec!["headway".to_owned(), "show".to_owned()],
                help: false,
            }
        );
    }

    #[test]
    fn unknown_flags_are_rejected() {
        let err = err_of(&["tools", "--json-file", "x"]);
        assert!(err.contains("`tools` doesn't take --json-file"), "{err}");
        let err = err_of(&["--bogus", "tools"]);
        assert!(err.contains("unknown flag '--bogus'"), "{err}");
    }

    /// A name that isn't one of nd's commands is kept whole for the tool
    /// list to read, `help` spelling and all.
    #[test]
    fn any_other_command_is_an_apps() {
        let words = |line: &[&str]| line.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            command_of(&["headway", "show", "--board", "x"]),
            Command::App {
                words: words(&["headway", "show", "--board", "x"]),
                help: false,
            }
        );
        assert_eq!(
            command_of(&["help", "headway", "next"]),
            Command::App {
                words: words(&["headway", "next"]),
                help: true,
            }
        );
        // `options` is a page only through `help`.
        assert_eq!(
            command_of(&["options", "--help"]),
            Command::App {
                words: words(&["options", "--help"]),
                help: false,
            }
        );
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
    fn spawn_takes_an_optional_dir() {
        assert_eq!(
            command_of(&["spawn"]),
            Command::Spawn {
                json: false,
                dir: None
            }
        );
        assert_eq!(
            command_of(&["spawn", "--json", "/tmp"]),
            Command::Spawn {
                json: true,
                dir: Some("/tmp".to_owned())
            }
        );
        let err = err_of(&["spawn", "a", "b"]);
        assert!(err.contains("one directory"), "{err}");
    }

    #[test]
    fn spawn_checks_its_dir_before_sending() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "").unwrap();

        let cwd = spawn_dir(Some(dir.path().to_str().unwrap())).ok().unwrap();
        assert_eq!(cwd, dir.path().canonicalize().unwrap());
        let err = spawn_dir(Some(file.to_str().unwrap())).err().unwrap();
        assert!(
            err.message.contains("is not a directory"),
            "{}",
            err.message
        );
        let missing = dir.path().join("missing");
        assert!(spawn_dir(Some(missing.to_str().unwrap())).is_err());
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
