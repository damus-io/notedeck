//! `nd <ns> <verb> …`: the commands the running notedeck's tools declare.
//!
//! A tool with a command path (`ToolInfo::command`, e.g. `["headway",
//! "show"]`) is a subcommand. [`resolve`] reads a command line against the
//! tool list fetched on this run, so nd never compiles in a tool name: a
//! tool the app adds is a command the next time nd runs.
//!
//! How a command line reads, all of it from the tool's schema:
//!
//! - **Positionals** fill the arguments marked `x-nd-positional`, in slot
//!   order; a `rest` argument takes every word left, joined with spaces.
//! - **Flags** are `--<flag> <value>` or `--<flag>=<value>`, where the flag is
//!   the argument's `x-nd-long`, else its name with `_` as `-`. `-<short>
//!   <value>` when it has an `x-nd-short`. A boolean is a bare switch.
//!   `--` ends the flags.
//! - **Env fallback**: an argument left out is read from its `x-nd-env`
//!   variable when that is set and not empty.
//! - **nd's own flags** go with every command and are read before the
//!   tool's: `--json` prints the tool's value rather than the text it
//!   rendered, `--pager`/`--no-pager` force paging the text either way, and
//!   `--color <auto|always|never>` its color (see [`Output`]).
//!
//! **Two tools may share a path.** They are tried in order of how many
//! positionals each *requires*, most first, and the first whose required
//! positionals the line supplies wins: `headway show` alone is
//! `headway_show_board`, and `headway show <card>` is `headway_show_card`.
//! Tools that tie keep the order notedeck listed them in.

use crate::help::CLI;
use crate::output::Output;
use crate::schema::{self, Arg, Slot};
use cli_term::help::{CommandHelp, NAME_COL};
use cli_term::{ColorWhen, PagerMode};
use notedeck_ipc::ToolInfo;
use serde_json::Value;

/// What a command line resolved to.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Call `tool` with `args`, printing its answer as `output` says. `ns`
    /// is the command's namespace, whose pager variable the text goes
    /// through.
    Call {
        tool: String,
        args: Value,
        output: Output,
        ns: String,
    },
    /// Print this help page.
    Help(String),
}

/// One tool that declares a command path, with its arguments read once.
struct Entry<'t> {
    tool: &'t ToolInfo,
    args: Vec<Arg<'t>>,
}

impl Entry<'_> {
    /// Its command path.
    fn path(&self) -> &[String] {
        &self.tool.command
    }

    /// How many positionals it can't run without.
    fn required_slots(&self) -> usize {
        self.positionals().filter(|arg| arg.required).count()
    }

    /// Its positional arguments, in slot order with any `rest` last.
    fn positionals(&self) -> impl Iterator<Item = &Arg<'_>> {
        let mut slots: Vec<&Arg> = self
            .args
            .iter()
            .filter(|arg| arg.positional.is_some())
            .collect();
        slots.sort_by_key(|arg| arg.positional);
        slots.into_iter()
    }

    /// The argument `--<flag>` (or `-<flag>` for one letter) names.
    fn flag(&self, flag: &str) -> Option<&Arg<'_>> {
        self.args.iter().find(|arg| {
            let short = flag.chars().count() == 1 && arg.short == flag.chars().next();
            arg.positional.is_none() && (short || arg.flag() == flag)
        })
    }

    /// Its usage line: the path, its positionals, and `[OPTIONS]` when it
    /// has flags.
    fn usage(&self) -> String {
        let mut line = self.path().join(" ");
        for arg in self.positionals() {
            let word = match arg.positional {
                Some(Slot::Rest) => format!("<{}>...", arg.name),
                _ => format!("<{}>", arg.name),
            };
            match arg.required {
                true => line.push_str(&format!(" {word}")),
                false => line.push_str(&format!(" [{word}]")),
            }
        }
        if self.args.iter().any(|arg| arg.positional.is_none()) {
            line.push_str(" [OPTIONS]");
        }
        line
    }
}

/// Every tool in `tools` that declares a command path.
fn entries(tools: &[ToolInfo]) -> Vec<Entry<'_>> {
    tools
        .iter()
        .filter(|tool| !tool.command.is_empty())
        .map(|tool| Entry {
            tool,
            args: schema::args(&tool.input_schema),
        })
        .collect()
}

/// Read `words` (the command line after `nd`) against `tools`. `help` is
/// set when the line was spelled `nd help …`; a `-h`/`--help` among the
/// words asks for the same page. `env` looks up an environment variable.
pub fn resolve(
    tools: &[ToolInfo],
    words: &[String],
    help: bool,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Action, String> {
    let entries = entries(tools);
    let want_help = help || words.iter().any(|w| w == "-h" || w == "--help");
    let leading: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .take_while(|w| !w.starts_with('-'))
        .collect();

    // The longest declared path the leading words spell.
    let path_len = entries
        .iter()
        .map(|e| e.path().len())
        .filter(|&len| len <= leading.len())
        .filter(|&len| entries.iter().any(|e| e.path() == &leading[..len]))
        .max();
    let Some(path_len) = path_len else {
        // `nd headway`, or `nd headway --help`: the namespace's page.
        let prefix = leading.as_slice();
        if !prefix.is_empty() && entries.iter().any(|e| e.path().starts_with(&owned(prefix))) {
            return Ok(Action::Help(namespace_page(&entries, prefix)));
        }
        return Err(no_command(&entries, &leading));
    };
    let path = &leading[..path_len];
    let candidates = candidates(&entries, path);

    if want_help {
        return Ok(Action::Help(command_page(path, &candidates)));
    }

    let line = Line::read(&candidates, &words[path_len..]).map_err(|e| with_help(e, path))?;
    let Some(entry) = candidates
        .iter()
        .find(|e| e.required_slots() <= line.positionals.len())
    else {
        // Nothing runs on this few words, so name what the least demanding
        // tool still needs.
        let entry = candidates.last().expect("a path has a tool");
        let missing = entry
            .positionals()
            .filter(|arg| arg.required)
            .nth(line.positionals.len())
            .map_or("an argument", |arg| arg.name);
        return Err(with_help(format!("missing <{missing}>"), path));
    };
    let pairs = line.pairs(entry, env).map_err(|e| with_help(e, path))?;
    let args = schema::assemble(&entry.tool.name, &entry.tool.input_schema, None, &pairs)
        .map_err(|e| with_help(e, path))?;
    Ok(Action::Call {
        tool: entry.tool.name.clone(),
        args,
        output: line.output,
        ns: path[0].to_owned(),
    })
}

/// The tools `path` runs, in the order a command line tries them: most
/// required positionals first, and ties in notedeck's order (the sort is
/// stable).
fn candidates<'e, 't, S: PartialEq<String>>(
    entries: &'e [Entry<'t>],
    path: &[S],
) -> Vec<&'e Entry<'t>> {
    let mut candidates: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.path().len() == path.len() && path.iter().zip(e.path()).all(|(a, b)| a == b))
        .collect();
    candidates.sort_by_key(|e| std::cmp::Reverse(e.required_slots()));
    candidates
}

/// `problem`, then where the command's page is.
fn with_help(problem: String, path: &[&str]) -> String {
    format!("{problem} (see `nd {} --help`)", path.join(" "))
}

/// A command line after its path, split into bare words and flags.
struct Line<'w> {
    positionals: Vec<&'w str>,
    /// Each flag as typed without its dashes, and its value: the text after
    /// it, or `true` for a switch.
    flags: Vec<(&'w str, String)>,
    /// What nd's own flags asked of the output.
    output: Output,
}

impl<'w> Line<'w> {
    /// Split `words`, reading each flag against every tool in `candidates`
    /// (the line's tool isn't known until its positionals are counted): a
    /// flag none of them takes is refused here, and one that takes a value
    /// in any of them takes the next word.
    fn read(candidates: &[&Entry], words: &'w [String]) -> Result<Self, String> {
        let mut line = Line {
            positionals: Vec::new(),
            flags: Vec::new(),
            output: Output::default(),
        };
        let mut words = words.iter().map(String::as_str);
        while let Some(word) = words.next() {
            if word == "--" {
                line.positionals.extend(words.by_ref());
                break;
            }
            let Some(flag) = flag_name(word) else {
                line.positionals.push(word);
                continue;
            };
            let (flag, inline) = match flag.split_once('=') {
                Some((flag, value)) => (flag, Some(value)),
                None => (flag, None),
            };
            match (flag, inline) {
                ("json", None) => {
                    line.output.json = true;
                    continue;
                }
                ("pager", None) => {
                    line.output.pager = PagerMode::Always;
                    continue;
                }
                ("no-pager", None) => {
                    line.output.pager = PagerMode::Never;
                    continue;
                }
                ("color", _) => {
                    let when = match inline {
                        Some(when) => when,
                        None => words.next().ok_or("--color needs a value")?,
                    };
                    line.output.color = ColorWhen::parse(when)?;
                    continue;
                }
                _ => {}
            }
            let args: Vec<&Arg> = candidates.iter().filter_map(|e| e.flag(flag)).collect();
            if args.is_empty() {
                return Err(format!("unknown flag '{word}'"));
            }
            let value = match inline {
                Some(value) => value.to_owned(),
                None if args.iter().all(|arg| arg.is_switch()) => "true".to_owned(),
                None => words
                    .next()
                    .ok_or_else(|| format!("{word} needs a value"))?
                    .to_owned(),
            };
            line.flags.push((flag, value));
        }
        Ok(line)
    }

    /// The `key=value` pairs for calling `entry`: its positionals in slot
    /// order, each flag under its argument's name, then the env fallback
    /// for any argument still unset.
    fn pairs(
        &self,
        entry: &Entry,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Vec<String>, String> {
        let mut pairs: Vec<(&str, String)> = Vec::new();
        let mut words = self.positionals.iter();
        for arg in entry.positionals() {
            let value = match arg.positional {
                Some(Slot::Rest) => {
                    let rest: Vec<&str> = words.by_ref().copied().collect();
                    (!rest.is_empty()).then(|| rest.join(" "))
                }
                _ => words.next().map(|w| (*w).to_owned()),
            };
            if let Some(value) = value {
                pairs.push((arg.name, value));
            }
        }
        if let Some(extra) = words.next() {
            return Err(format!("unexpected argument '{extra}'"));
        }

        for (flag, value) in &self.flags {
            let Some(arg) = entry.flag(flag) else {
                return Err(format!(
                    "--{flag} doesn't go with {}",
                    entry.usage().trim_end_matches(" [OPTIONS]")
                ));
            };
            pairs.push((arg.name, value.clone()));
        }

        for arg in &entry.args {
            if pairs.iter().any(|(name, _)| *name == arg.name) {
                continue;
            }
            let Some(value) = arg.env.and_then(env).filter(|v| !v.is_empty()) else {
                continue;
            };
            pairs.push((arg.name, value));
        }
        Ok(pairs
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect())
    }
}

/// The flag `word` spells, without its dashes, or `None` for a bare word:
/// `-` alone (stdin, by convention) and a negative number are values.
fn flag_name(word: &str) -> Option<&str> {
    if let Some(long) = word.strip_prefix("--") {
        return Some(long);
    }
    let short = word.strip_prefix('-')?;
    let first = short.chars().next()?;
    (!first.is_ascii_digit()).then_some(short)
}

/// Why `leading` names no command: an unknown namespace lists the ones
/// there are, and a namespace with no such verb prints its page.
fn no_command(entries: &[Entry], leading: &[&str]) -> String {
    // The longest run of leading words that is the start of some path; it
    // is shorter than `leading`, or `leading` would have been a page.
    let known = (1..=leading.len()).rev().find(|&len| {
        entries
            .iter()
            .any(|e| e.path().starts_with(&owned(&leading[..len])))
    });
    match (known, leading.len()) {
        (Some(len), _) => format!(
            "`nd {}` has no command '{}'\n\n{}",
            leading[..len].join(" "),
            leading[len],
            namespace_page(entries, &leading[..len])
        ),
        (None, 0) => "no command given (see `nd --help`)".to_owned(),
        (None, _) => {
            let mut namespaces: Vec<&str> = entries.iter().map(|e| e.path()[0].as_str()).collect();
            namespaces.sort_unstable();
            namespaces.dedup();
            let own: Vec<&str> = CLI.commands.iter().map(|c| c.name).collect();
            format!(
                "unknown command '{}': the running notedeck's are {}, and nd's own {} (see `nd --help`)",
                leading[0],
                if namespaces.is_empty() {
                    "none".to_owned()
                } else {
                    namespaces.join(", ")
                },
                own.join(", ")
            )
        }
    }
}

/// `words` as owned strings, to compare against a path.
fn owned(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// `nd <ns> --help`: every command under `prefix`, one line each, with the
/// titles of the tools it runs.
fn namespace_page(entries: &[Entry], prefix: &[&str]) -> String {
    let prefix_owned = owned(prefix);
    let mut paths: Vec<&[String]> = entries
        .iter()
        .map(Entry::path)
        .filter(|path| path.starts_with(&prefix_owned))
        .collect();
    paths.sort_unstable();
    paths.dedup();

    let ns = prefix.join(" ");
    let mut out = format!(
        "nd {ns} — the running notedeck's {ns} commands\n\nUSAGE: nd {ns} <COMMAND> [ARGS...]\n\nCOMMANDS\n"
    );
    for path in paths {
        let name = path[prefix.len()..].join(" ");
        let titles: Vec<&str> = candidates(entries, path)
            .iter()
            .map(|e| e.tool.title.as_str())
            .collect();
        out.push_str(&format!("    {name:<NAME_COL$}{}\n", titles.join(" / ")));
    }
    out.push_str(&format!("\nPer command: `nd {ns} <cmd> --help`.\n"));
    out
}

/// `nd <path> --help`: one page for every tool the path runs, laid out by
/// [`cli_term::help`] as nd's own commands are.
fn command_page(path: &[&str], candidates: &[&Entry]) -> String {
    let name = path.join(" ");
    let summary: Vec<&str> = candidates.iter().map(|e| e.tool.title.as_str()).collect();
    let summary = summary.join(" / ");
    let usage: Vec<String> = candidates.iter().map(|e| e.usage()).collect();

    // One paragraph a tool: its usage line when the path runs several, so
    // each description says which spelling it belongs to.
    let mut details = String::new();
    for (entry, usage) in candidates.iter().zip(&usage) {
        if !details.is_empty() {
            details.push('\n');
        }
        let indent = if candidates.len() > 1 {
            details.push_str(&format!("nd {usage}  ({})\n", entry.tool.name));
            4
        } else {
            0
        };
        details.push_str(&cli_term::help::wrapped(&entry.tool.description, indent));
    }
    if candidates.len() == 1 {
        details.push_str(&format!("\nRuns the tool {}.\n", candidates[0].tool.name));
    }

    // An argument two tools share is listed once, as the first describes
    // it; an env fallback only the other has is noted with the spelling it
    // belongs to.
    let mut options: Vec<(String, String)> = Vec::new();
    for (entry, usage) in candidates.iter().zip(&usage) {
        for arg in &entry.args {
            let key = option_key(arg);
            if let Some((_, text)) = options.iter_mut().find(|(k, _)| *k == key) {
                if let Some(var) = arg.env.filter(|var| !text.contains(&format!("${var}"))) {
                    let spelling = usage.trim_end_matches(" [OPTIONS]");
                    text.push_str(&format!(" Falls back to ${var} for `nd {spelling}`."));
                }
                continue;
            }
            let mut text = arg.description.to_owned();
            if arg.required && arg.positional.is_none() {
                text.push_str(" Required.");
            }
            if let Some(var) = arg.env {
                text.push_str(&format!(" Falls back to ${var}."));
            }
            options.push((key, text));
        }
    }
    for (key, text) in ND_OPTIONS {
        options.push(((*key).to_owned(), (*text).to_owned()));
    }

    let usage: Vec<&str> = usage.iter().map(String::as_str).collect();
    let options: Vec<(&str, &str)> = options
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    CLI.page(&CommandHelp {
        name: &name,
        group: "",
        summary: &summary,
        usage: &usage,
        details: &details,
        options: &options,
        examples: &[],
    })
}

/// The flags nd reads on every command, before the tool's, as
/// [`command_page`] lists them.
const ND_OPTIONS: &[(&str, &str)] = &[
    (
        "--json",
        "Print the tool's answer as JSON rather than the text it renders",
    ),
    (
        "--pager / --no-pager",
        "Page the text, or don't; by default it pages when stdout is a terminal, through $<NS>_PAGER, then $PAGER, then `less -R`",
    ),
    (
        "--color <when>",
        "Color the text: auto (a terminal or a pager), always or never",
    ),
];

/// How [`command_page`] lists `arg`: `<card>` for a positional, else its
/// flag spellings and a value placeholder unless it's a switch.
fn option_key(arg: &Arg) -> String {
    if arg.positional.is_some() {
        return format!("<{}>", arg.name);
    }
    let mut key = match arg.short {
        Some(short) => format!("-{short}, --{}", arg.flag()),
        None => format!("--{}", arg.flag()),
    };
    if !arg.is_switch() {
        key.push_str(&format!(" <{}>", arg.name));
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use notedeck_ipc::ToolKind;
    use serde_json::json;

    fn tool(name: &str, command: &[&str], schema: Value) -> ToolInfo {
        ToolInfo {
            name: name.to_owned(),
            title: format!("{name} title"),
            description: format!("What {name} does."),
            category: Some("Headway".to_owned()),
            kind: ToolKind::Data,
            read_only: true,
            input_schema: schema,
            command: command.iter().map(|w| (*w).to_owned()).collect(),
        }
    }

    /// headway's four read tools, shaped as `ToolSpec::json_schema` writes
    /// them, plus a tool with no command and one with a `rest` title.
    fn fixture() -> Vec<ToolInfo> {
        vec![
            tool(
                "headway_list_boards",
                &["headway", "board"],
                json!({"type": "object", "properties": {}, "required": [], "x-nd-order": []}),
            ),
            tool(
                "headway_show_board",
                &["headway", "show"],
                json!({
                    "type": "object",
                    "properties": {
                        "board": {"type": "string", "description": "The board.", "x-nd-env": "HEADWAY_BOARD"},
                    },
                    "required": [],
                    "x-nd-order": ["board"],
                }),
            ),
            tool(
                "headway_show_card",
                &["headway", "show"],
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
                &["headway", "next"],
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
                "headway_add_card",
                &["headway", "add"],
                json!({
                    "type": "object",
                    "properties": {
                        "title": {"type": "string", "description": "The title.", "x-nd-positional": "rest"},
                        "column": {"type": "string", "description": "The column.", "x-nd-positional": 0},
                        "urgent": {"type": "boolean", "description": "Flag it."},
                        "group_by": {"type": "string", "description": "Grouping."},
                    },
                    "required": ["title", "column"],
                    "x-nd-order": ["column", "title", "urgent", "group_by"],
                }),
            ),
            tool(
                "headway_open_review_queue",
                &[],
                json!({"type": "object", "properties": {}}),
            ),
        ]
    }

    fn run_env(line: &[&str], env: &[(&str, &str)]) -> Result<Action, String> {
        let words: Vec<String> = line.iter().map(|w| (*w).to_owned()).collect();
        let lookup = |var: &str| {
            env.iter()
                .find(|(k, _)| *k == var)
                .map(|(_, v)| (*v).to_owned())
        };
        resolve(&fixture(), &words, false, &lookup)
    }

    fn run(line: &[&str]) -> Result<Action, String> {
        run_env(line, &[])
    }

    fn call(tool: &str, args: Value) -> Result<Action, String> {
        call_with(tool, args, Output::default())
    }

    fn call_with(tool: &str, args: Value, output: Output) -> Result<Action, String> {
        Ok(Action::Call {
            tool: tool.to_owned(),
            args,
            output,
            ns: "headway".to_owned(),
        })
    }

    #[test]
    fn a_shared_path_picks_by_required_positionals() {
        assert_eq!(
            run(&["headway", "show"]),
            call("headway_show_board", json!({}))
        );
        assert_eq!(
            run(&["headway", "show", "headway:notedeck/a-b-c"]),
            call(
                "headway_show_card",
                json!({"card": "headway:notedeck/a-b-c"})
            )
        );
        assert_eq!(
            run(&["headway", "show", "--board", "notedeck"]),
            call("headway_show_board", json!({"board": "notedeck"}))
        );
        assert_eq!(
            run(&["headway", "show", "x", "--board=dave"]),
            call("headway_show_card", json!({"card": "x", "board": "dave"}))
        );
    }

    #[test]
    fn long_short_and_inline_flags_name_their_args() {
        let want = call(
            "headway_ready_items",
            json!({"container": "headway:notedeck/keen-ranch-hotel", "limit": 1}),
        );
        assert_eq!(
            run(&[
                "headway",
                "next",
                "--in",
                "headway:notedeck/keen-ranch-hotel",
                "-n",
                "1"
            ]),
            want
        );
        assert_eq!(
            run(&[
                "headway",
                "next",
                "--in=headway:notedeck/keen-ranch-hotel",
                "--limit",
                "1"
            ]),
            want
        );
        let err = run(&["headway", "next", "-n", "one"]).unwrap_err();
        assert!(err.starts_with("limit: 'one' is not an integer"), "{err}");
        assert!(err.ends_with("(see `nd headway next --help`)"), "{err}");
    }

    #[test]
    fn env_fills_what_the_line_leaves_out() {
        let env = [("HEADWAY_BOARD", "dave")];
        assert_eq!(
            run_env(&["headway", "next"], &env),
            call("headway_ready_items", json!({"board": "dave"}))
        );
        assert_eq!(
            run_env(&["headway", "next", "--board", "notedeck"], &env),
            call("headway_ready_items", json!({"board": "notedeck"}))
        );
        // An empty variable is unset, and show_card's board has no fallback.
        assert_eq!(
            run_env(&["headway", "next"], &[("HEADWAY_BOARD", "")]),
            call("headway_ready_items", json!({}))
        );
        assert_eq!(
            run_env(&["headway", "show", "x"], &env),
            call("headway_show_card", json!({"card": "x"}))
        );
    }

    #[test]
    fn rest_joins_the_words_left_and_switches_take_no_value() {
        assert_eq!(
            run(&[
                "headway",
                "add",
                "todo",
                "fix",
                "the",
                "--urgent",
                "thing",
                "--group-by",
                "col"
            ]),
            call(
                "headway_add_card",
                json!({"column": "todo", "title": "fix the thing", "urgent": true, "group_by": "col"})
            )
        );
        assert_eq!(
            run(&["headway", "add", "todo", "--", "--not-a-flag"]),
            call(
                "headway_add_card",
                json!({"column": "todo", "title": "--not-a-flag"})
            )
        );
        let err = run(&["headway", "add", "todo"]).unwrap_err();
        assert!(err.starts_with("missing <title>"), "{err}");
    }

    /// nd's own output flags go with every command, anywhere after its
    /// path, and never reach the tool.
    #[test]
    fn output_flags_are_nds() {
        let json = Output {
            json: true,
            ..Output::default()
        };
        assert_eq!(
            run(&["headway", "board", "--json"]),
            call_with("headway_list_boards", json!({}), json)
        );
        assert_eq!(
            run(&["headway", "show", "--json", "x"]),
            call_with("headway_show_card", json!({"card": "x"}), json)
        );
        assert_eq!(
            run(&["headway", "show", "--no-pager", "--color", "always"]),
            call_with(
                "headway_show_board",
                json!({}),
                Output {
                    pager: PagerMode::Never,
                    color: ColorWhen::Always,
                    ..Output::default()
                }
            )
        );
        assert_eq!(
            run(&["headway", "next", "--pager", "--color=never", "-n", "1"]),
            call_with(
                "headway_ready_items",
                json!({"limit": 1}),
                Output {
                    pager: PagerMode::Always,
                    color: ColorWhen::Never,
                    ..Output::default()
                }
            )
        );
        let err = run(&["headway", "show", "--color", "pink"]).unwrap_err();
        assert!(
            err.starts_with("--color must be auto|always|never"),
            "{err}"
        );
    }

    #[test]
    fn each_mistake_says_what_went_wrong() {
        let err = run(&["headway", "show", "--bogus"]).unwrap_err();
        assert_eq!(err, "unknown flag '--bogus' (see `nd headway show --help`)");

        let err = run(&["headway", "board", "extra"]).unwrap_err();
        assert!(err.starts_with("unexpected argument 'extra'"), "{err}");

        let err = run(&["headway", "next", "--in"]).unwrap_err();
        assert!(err.starts_with("--in needs a value"), "{err}");

        let err = run(&["headway", "frob"]).unwrap_err();
        assert!(
            err.starts_with("`nd headway` has no command 'frob'"),
            "{err}"
        );
        assert!(err.contains("    next"), "{err}");

        let err = run(&["notebook", "show"]).unwrap_err();
        assert!(
            err.starts_with("unknown command 'notebook': the running notedeck's are headway,"),
            "{err}"
        );
        assert!(err.contains("nd's own tools, call"), "{err}");
    }

    #[test]
    fn a_namespace_alone_prints_its_page() {
        for line in [&["headway"][..], &["headway", "--help"]] {
            let Ok(Action::Help(page)) = run(line) else {
                panic!("{line:?} is no page");
            };
            assert!(page.starts_with("nd headway — "), "{page}");
        }
        let Ok(Action::Help(page)) = run(&["headway"]) else {
            unreachable!()
        };
        assert!(
            page.contains(
                "    show              headway_show_card title / headway_show_board title\n"
            ),
            "{page}"
        );
        // A tool with no command isn't one.
        assert!(!page.contains("review"), "{page}");
    }

    #[test]
    fn help_pages_come_from_the_specs() {
        let words = |line: &[&str]| line.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        let page =
            |line: &[&str], help: bool| match resolve(&fixture(), &words(line), help, &|_| None) {
                Ok(Action::Help(page)) => page,
                other => panic!("{line:?}: {other:?}"),
            };

        let show = page(&["headway", "show", "--help"], false);
        assert!(show.starts_with("nd headway show — "), "{show}");
        assert!(show.contains("    nd headway show [OPTIONS]\n"), "{show}");
        assert!(
            show.contains("    nd headway show <card> [OPTIONS]\n"),
            "{show}"
        );
        assert!(show.contains("(headway_show_card)"), "{show}");
        let flat = show.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains("The board. Falls back to $HEADWAY_BOARD for `nd headway show`."),
            "{show}"
        );

        let next = page(&["headway", "next"], true);
        assert!(next.contains("-n, --limit <limit>"), "{next}");
        assert!(next.contains("--in <container>"), "{next}");
        assert!(
            next.contains("Runs the tool headway_ready_items."),
            "{next}"
        );
    }

    #[test]
    fn flag_names_leave_values_alone() {
        assert_eq!(flag_name("--in"), Some("in"));
        assert_eq!(flag_name("-n"), Some("n"));
        assert_eq!(flag_name("-"), None);
        assert_eq!(flag_name("-5"), None);
        assert_eq!(flag_name("word"), None);
    }
}
