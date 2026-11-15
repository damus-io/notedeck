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
//!   `--` ends the flags. A positional that declares an `x-nd-short` or
//!   `x-nd-long` takes its flag as well as bare words: `label <card> a -l b`.
//! - **Env fallback**: an argument left out is read from its `x-nd-env`
//!   variable when that is set and not empty.
//! - **Caller-side kinds**, since the tool runs in notedeck and can't see
//!   this shell: a `path` (`x-nd-kind`) is made absolute against nd's
//!   working directory and canonicalized, so a missing one fails here, and
//!   one marked `x-nd-default-cwd` is that directory when left out. A
//!   `list` flag repeats and each value may be a comma list: `-l a -l b,c`
//!   is `["a","b","c"]`. A `paths` flag repeats too, one path each, and
//!   each is resolved as a `path` is. An argument with an `x-nd-from-file` flag also
//!   takes `--<that flag> <path|->`, which sends the file's text, or
//!   stdin's for `-`, trimmed at the end; giving both spellings fails.
//! - **nd's own flags** go with every command and are read before the
//!   tool's: `--json` prints the tool's value rather than the text it
//!   rendered, `--pager`/`--no-pager` force paging the text either way, and
//!   `--color <auto|always|never>` its color (see [`Output`]). A tool that
//!   [streams](ToolInfo::streams) is followed rather than paged, so
//!   `--pager` is refused for it.
//!
//! **Two tools may share a path.** They are tried in order of how many
//! flags each *requires*, then how many positionals, most first, and the
//! first whose required flags and positionals the line supplies wins:
//! `headway show` alone is `headway_show_board`, `headway show <card>` is
//! `headway_show_card`, and `headway list --save-view <name>` is the tool
//! that requires `--save-view` rather than `headway_list`. Tools that tie
//! keep the order notedeck listed them in.

use crate::caller::{self, Caller};
use crate::help::CLI;
use crate::output::Output;
use crate::schema::{self, Arg, ClientKind, Slot};
use cli_term::help::{CommandHelp, NAME_COL};
use cli_term::{ColorWhen, PagerMode};
use notedeck_ipc::{Signer, StreamMode, ToolInfo};
use serde_json::Value;

/// What a command line resolved to.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Call `tool` with `args`, printing its answer as `output` says. `ns`
    /// is the command's namespace, whose pager variable the text goes
    /// through. `signer` is the caller's comment key, set only for a tool
    /// that accepts one (see [`caller::signer_for`]). `streams` is set for
    /// a tool that streams: nd subscribes to it and shows its events as the
    /// mode says, rather than calling it.
    Call {
        tool: String,
        args: Value,
        output: Output,
        ns: String,
        signer: Option<Signer>,
        streams: Option<StreamMode>,
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

    /// Its required flags: arguments that are neither positional nor read
    /// from the environment, so only a flag on the line can give them.
    /// A tool is picked only when the line gives every one, which is how
    /// one path holds a read and a write: `headway list` lists, and
    /// `headway list --save-view <name>` is the tool that requires
    /// `--save-view`.
    fn required_flags(&self) -> impl Iterator<Item = &Arg<'_>> {
        self.args
            .iter()
            .filter(|arg| arg.required && arg.positional.is_none() && arg.env.is_none())
    }

    /// Whether `line` gives every one of its [required
    /// flags](Self::required_flags).
    fn flags_given(&self, line: &Line) -> bool {
        self.required_flags().all(|arg| {
            line.flags
                .iter()
                .any(|(flag, _)| self.flag(flag).is_some_and(|hit| hit.arg.name == arg.name))
        })
    }

    /// The required positionals `line` must still give as bare words: those
    /// it doesn't already give by a flag, such as a comment body read with
    /// `--body-file`.
    fn words_needed<'e>(&'e self, line: &'e Line) -> impl Iterator<Item = &'e Arg<'e>> {
        self.positionals().filter(move |arg| {
            arg.required
                && !line
                    .flags
                    .iter()
                    .any(|(flag, _)| self.flag(flag).is_some_and(|hit| hit.arg.name == arg.name))
        })
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

    /// The argument `--<flag>` (or `-<flag>` for one letter) names, and
    /// whether it names it by its file form.
    fn flag(&self, flag: &str) -> Option<FlagHit<'_, '_>> {
        let by_value = self.args.iter().find(|arg| {
            let short = flag.chars().count() == 1 && arg.short == flag.chars().next();
            (arg.positional.is_none() || arg.has_flag()) && (short || arg.flag() == flag)
        });
        if let Some(arg) = by_value {
            return Some(FlagHit { arg, file: false });
        }
        // A positional can have a file form too: a comment body typed as
        // bare words, or read with `--body-file`.
        let arg = self.args.iter().find(|arg| arg.from_file == Some(flag))?;
        Some(FlagHit { arg, file: true })
    }

    /// Its usage line: the path, its required flags, its positionals, and
    /// `[OPTIONS]` when it has flags.
    fn usage(&self) -> String {
        let mut line = self.path().join(" ");
        for arg in self.required_flags() {
            match arg.is_switch() {
                true => line.push_str(&format!(" --{}", arg.flag())),
                false => line.push_str(&format!(" --{} <{}>", arg.flag(), arg.name)),
            }
        }
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

/// The argument a flag names.
struct FlagHit<'e, 'a> {
    arg: &'e Arg<'a>,
    /// Whether the flag is the argument's file form (`x-nd-from-file`),
    /// whose value is a path to read rather than the argument itself.
    file: bool,
}

/// One value a command line gave an argument, before nd resolves it.
struct Given<'a> {
    name: &'a str,
    value: String,
    /// Whether it came through the argument's file form.
    file: bool,
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
/// words asks for the same page. `caller` is the shell nd runs in, whose
/// environment, working directory and stdin fill the arguments that name
/// them.
pub fn resolve(
    tools: &[ToolInfo],
    words: &[String],
    help: bool,
    caller: &dyn Caller,
) -> Result<Action, String> {
    let entries = entries(tools);
    let want_help = help || words.iter().any(|w| w == "-h" || w == "--help");
    let words = &hoist_flags(&entries, words);
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
        .find(|e| e.words_needed(&line).count() <= line.positionals.len() && e.flags_given(&line))
    else {
        // Nothing runs on this few words, so name what the least demanding
        // tool still needs.
        let entry = candidates.last().expect("a path has a tool");
        let missing = entry
            .words_needed(&line)
            .nth(line.positionals.len())
            .map_or("an argument", |arg| arg.name);
        return Err(with_help(format!("missing <{missing}>"), path));
    };
    if entry.tool.streams.is_some() && line.output.pager == PagerMode::Always {
        return Err(with_help(
            "--pager doesn't go with a stream, which is followed as it comes".to_owned(),
            path,
        ));
    }
    let pairs = line.pairs(entry, caller).map_err(|e| with_help(e, path))?;
    let args = schema::assemble(&entry.tool.name, &entry.tool.input_schema, None, &pairs)
        .map_err(|e| with_help(e, path))?;
    Ok(Action::Call {
        tool: entry.tool.name.clone(),
        args,
        output: line.output,
        ns: path[0].to_owned(),
        signer: caller::signer_for(entry.tool, caller)?,
        streams: entry.tool.streams,
    })
}

/// `words` with any flag typed among the command path's words moved after
/// them, so `headway --board b show c` reads as `headway show --board b c`,
/// as the old per-app CLIs, which took their globals before the verb, did.
/// A flag takes the next word with it when a tool in the namespace reads a
/// value for it (or it is `--color`); one no tool there knows is moved
/// alone, for [`Line::read`] to refuse. Words after `--` stay put.
fn hoist_flags(entries: &[Entry], words: &[String]) -> Vec<String> {
    let Some(ns) = words.first() else {
        return Vec::new();
    };
    let in_ns: Vec<&Entry> = entries.iter().filter(|e| e.path()[0] == *ns).collect();
    let mut path: Vec<String> = vec![ns.clone()];
    let mut hoisted: Vec<String> = Vec::new();
    let mut rest = words[1..].iter();
    let mut tail: Vec<String> = Vec::new();
    while let Some(word) = rest.next() {
        if word == "--" {
            tail.push(word.clone());
            break;
        }
        if let Some(flag) = flag_name(word) {
            hoisted.push(word.clone());
            if !flag.contains('=') && takes_value(&in_ns, flag) {
                hoisted.extend(rest.next().cloned());
            }
            continue;
        }
        let deeper = in_ns.iter().any(|e| {
            e.path().len() > path.len()
                && e.path()[..path.len()] == path[..]
                && e.path()[path.len()] == *word
        });
        if !deeper {
            tail.push(word.clone());
            break;
        }
        path.push(word.clone());
    }
    path.extend(hoisted);
    path.extend(tail);
    path.extend(rest.cloned());
    path
}

/// Whether `--<flag>` takes the next word as its value on some tool in
/// `entries`: nd's own `--color`, or a tool argument that isn't a switch.
fn takes_value(entries: &[&Entry], flag: &str) -> bool {
    if flag == "color" {
        return true;
    }
    entries
        .iter()
        .filter_map(|e| e.flag(flag))
        .any(|hit| !hit.arg.is_switch() || hit.file)
}

/// The tools `path` runs, in the order a command line tries them: most
/// required flags first, then most required positionals, and ties in
/// notedeck's order (the sort is stable).
fn candidates<'e, 't, S: PartialEq<String>>(
    entries: &'e [Entry<'t>],
    path: &[S],
) -> Vec<&'e Entry<'t>> {
    let mut candidates: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.path().len() == path.len() && path.iter().zip(e.path()).all(|(a, b)| a == b))
        .collect();
    candidates.sort_by_key(|e| std::cmp::Reverse((e.required_flags().count(), e.required_slots())));
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
            let hits: Vec<FlagHit> = candidates.iter().filter_map(|e| e.flag(flag)).collect();
            if hits.is_empty() {
                return Err(format!("unknown flag '{word}'"));
            }
            let value = match inline {
                Some(value) => value.to_owned(),
                None if hits.iter().all(|hit| hit.arg.is_switch() && !hit.file) => {
                    "true".to_owned()
                }
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
    /// order, each flag under its argument's name, the env fallback for
    /// any argument still unset, then the working directory for a path
    /// that defaults to it. Each value is resolved by its argument's
    /// [`ClientKind`] and file form on the way (see the module docs).
    fn pairs(&self, entry: &Entry, caller: &dyn Caller) -> Result<Vec<String>, String> {
        let mut given: Vec<Given> = Vec::new();
        let mut words = self.positionals.iter();
        for arg in entry.positionals() {
            let rest = arg.positional == Some(Slot::Rest);
            if rest && arg.client == Some(ClientKind::List) {
                given.extend(words.by_ref().map(|w| Given::typed(arg, w)));
                continue;
            }
            let value = match rest {
                true => {
                    let rest: Vec<&str> = words.by_ref().copied().collect();
                    (!rest.is_empty()).then(|| rest.join(" "))
                }
                false => words.next().map(|w| (*w).to_owned()),
            };
            if let Some(value) = value {
                given.push(Given::typed(arg, &value));
            }
        }
        if let Some(extra) = words.next() {
            return Err(format!("unexpected argument '{extra}'"));
        }

        for (flag, value) in &self.flags {
            let Some(hit) = entry.flag(flag) else {
                return Err(format!(
                    "--{flag} doesn't go with {}",
                    entry.usage().trim_end_matches(" [OPTIONS]")
                ));
            };
            given.push(Given {
                name: hit.arg.name,
                value: value.clone(),
                file: hit.file,
            });
        }

        let mut pairs = Vec::new();
        let mut stdin_read = false;
        for arg in &entry.args {
            let mut values = given.iter().filter(|g| g.name == arg.name).peekable();
            let value = match values.peek() {
                Some(_) => given_value(arg, values, caller, &mut stdin_read)?,
                None => fallback(arg, caller)?,
            };
            if let Some(value) = value {
                pairs.push(format!("{}={value}", arg.name));
            }
        }
        Ok(pairs)
    }
}

impl<'a> Given<'a> {
    /// A value typed for `arg`, not read from a file.
    fn typed(arg: &Arg<'a>, value: &str) -> Self {
        Given {
            name: arg.name,
            value: value.to_owned(),
            file: false,
        }
    }
}

/// The value the command line gave `arg`, from its `given` words, as the
/// text [`schema::assemble`] coerces: a list's values comma-split into a
/// JSON array, a path resolved, a file form read, else the last one typed.
/// `stdin_read` is set once a `-` has read stdin, so a second fails rather
/// than reading nothing.
fn given_value<'g, 'a: 'g>(
    arg: &Arg,
    given: impl Iterator<Item = &'g Given<'a>>,
    caller: &dyn Caller,
    stdin_read: &mut bool,
) -> Result<Option<String>, String> {
    let mut typed = Vec::new();
    let mut file = None;
    for g in given {
        match g.file {
            true => file = Some(g.value.as_str()),
            false => typed.push(g.value.as_str()),
        }
    }
    if let Some(path) = file {
        if !typed.is_empty() {
            let flag = arg.from_file.unwrap_or_default();
            return Err(format!("give {} or --{flag}, not both", spelling(arg)));
        }
        let text = read_file(path, caller, stdin_read)
            .map_err(|e| format!("--{}: {e}", arg.from_file.unwrap_or_default()))?;
        return Ok(Some(text));
    }
    if arg.client == Some(ClientKind::List) {
        return Ok(Some(list(&typed)));
    }
    if arg.client == Some(ClientKind::Paths) {
        return paths(arg, &typed, caller).map(Some);
    }
    if arg.client == Some(ClientKind::Strings) {
        return Ok(Some(strings(&typed)));
    }
    let Some(last) = typed.last() else {
        return Ok(None);
    };
    resolved(arg, last, caller).map(Some)
}

/// The value of `arg` when the command line leaves it out: its env
/// variable when set and not empty, else the working directory for a path
/// that defaults to it.
fn fallback(arg: &Arg, caller: &dyn Caller) -> Result<Option<String>, String> {
    let from_env = arg.env.and_then(|var| caller.env(var));
    if let Some(value) = from_env.filter(|value| !value.is_empty()) {
        return match arg.client {
            Some(ClientKind::List) => Ok(Some(list(&[value.as_str()]))),
            Some(ClientKind::Paths) => paths(arg, &[value.as_str()], caller).map(Some),
            Some(ClientKind::Strings) => Ok(Some(strings(&[value.as_str()]))),
            _ => resolved(arg, &value, caller).map(Some),
        };
    }
    if arg.default_cwd && arg.client == Some(ClientKind::Path) {
        return resolved(arg, ".", caller).map(Some);
    }
    Ok(None)
}

/// `raw` as `arg` sends it: a path made absolute and canonicalized, any
/// other text as it is.
fn resolved(arg: &Arg, raw: &str, caller: &dyn Caller) -> Result<String, String> {
    if arg.client != Some(ClientKind::Path) {
        return Ok(raw.to_owned());
    }
    canonical(arg, raw, caller)
}

/// `values` as a JSON array of paths, each resolved as [`canonical`] does.
fn paths(arg: &Arg, values: &[&str], caller: &dyn Caller) -> Result<String, String> {
    let paths = values
        .iter()
        .map(|raw| canonical(arg, raw, caller))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::to_string(&paths).expect("strings serialize"))
}

/// `raw` made absolute against the caller's cwd and canonicalized, so a
/// missing path fails here, naming `arg`.
fn canonical(arg: &Arg, raw: &str, caller: &dyn Caller) -> Result<String, String> {
    let path = caller.cwd()?.join(raw);
    let path = path
        .canonicalize()
        .map_err(|e| format!("{}: '{raw}': {e}", arg.name))?;
    Ok(path.to_string_lossy().into_owned())
}

/// `values` as a JSON array of strings, each split on commas, with blanks
/// dropped: `["a", "b,c"]` is `["a","b","c"]`.
fn list(values: &[&str]) -> String {
    let items: Vec<&str> = values
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .collect();
    serde_json::to_string(&items).expect("strings serialize")
}

/// `values` as a JSON array of strings, each kept as it was typed.
fn strings(values: &[&str]) -> String {
    serde_json::to_string(values).expect("strings serialize")
}

/// The text a file form names: stdin for `-`, else the file at `path`
/// against the caller's working directory, with trailing whitespace
/// trimmed so a heredoc's closing newline doesn't ride along.
fn read_file(path: &str, caller: &dyn Caller, stdin_read: &mut bool) -> Result<String, String> {
    let text = if path == "-" {
        if std::mem::replace(stdin_read, true) {
            return Err("stdin is already read by another argument".to_owned());
        }
        caller.stdin()?
    } else {
        let full = caller.cwd()?.join(path);
        std::fs::read_to_string(&full).map_err(|e| format!("can't read '{path}': {e}"))?
    };
    Ok(text.trim_end().to_owned())
}

/// How a command line spells `arg` when typed: `<title>` for a
/// positional, else its `--flag`.
fn spelling(arg: &Arg) -> String {
    match arg.positional {
        Some(_) => format!("<{}>", arg.name),
        None => format!("--{}", arg.flag()),
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
            if arg.client == Some(ClientKind::List) {
                text.push_str(" Repeat it, or give a comma list.");
            }
            if matches!(arg.client, Some(ClientKind::Paths | ClientKind::Strings)) {
                text.push_str(" Repeat it for more than one.");
            }
            if let Some(var) = arg.env {
                text.push_str(&format!(" Falls back to ${var}."));
            }
            if arg.default_cwd && arg.client == Some(ClientKind::Path) {
                text.push_str(" Defaults to the current directory.");
            }
            options.push((key, text));
            if let Some(flag) = arg.from_file {
                options.push((
                    format!("--{flag} <path|->"),
                    format!("Read {} from a file, or stdin for -.", spelling(arg)),
                ));
            }
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
/// flag spellings and a value placeholder unless it's a switch; both for a
/// positional that also takes a flag (`<labels>, -l, --label <labels>`).
fn option_key(arg: &Arg) -> String {
    if arg.positional.is_some() && !arg.has_flag() {
        return format!("<{}>", arg.name);
    }
    let mut key = match arg.positional {
        Some(_) => format!("<{}>, ", arg.name),
        None => String::new(),
    };
    key.push_str(&match arg.short {
        Some(short) => format!("-{short}, --{}", arg.flag()),
        None => format!("--{}", arg.flag()),
    });
    if !arg.is_switch() {
        key.push_str(&format!(" <{}>", arg.name));
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caller::Process;
    use notedeck_ipc::ToolKind;
    use serde_json::json;
    use std::path::{Path, PathBuf};

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
            accepts_signer: false,
            streams: None,
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
            ToolInfo {
                accepts_signer: true,
                ..tool(
                    "headway_edit",
                    &["headway", "edit"],
                    json!({
                        "type": "object",
                        "properties": {
                            "card": {"type": "string", "description": "The card.", "x-nd-positional": 0},
                            "body": {"type": "string", "description": "A comment.", "x-nd-positional": "rest", "x-nd-from-file": "body-file"},
                            "labels": {"type": "array", "items": {"type": "string"}, "description": "Labels.", "x-nd-short": "l", "x-nd-kind": "list"},
                            "desc": {"type": "string", "description": "The text.", "x-nd-from-file": "desc-file"},
                            "repo_dir": {"type": "string", "description": "The checkout.", "x-nd-kind": "path", "x-nd-default-cwd": true},
                            "file": {"type": "string", "description": "A file.", "x-nd-kind": "path"},
                            "sources": {"type": "array", "items": {"type": "string"}, "description": "Files.", "x-nd-long": "source", "x-nd-kind": "paths"},
                            "session": {"type": "string", "description": "Who.", "x-nd-env": "AGENTIUM_SESSION"},
                            "env": {"type": "array", "items": {"type": "string"}, "description": "Vars.", "x-nd-kind": "strings"},
                        },
                        "required": ["card"],
                        "x-nd-order": ["card", "body", "labels", "desc", "repo_dir", "file", "sources", "session", "env"],
                    }),
                )
            },
            tool(
                "headway_list",
                &["headway", "list"],
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Terms.", "x-nd-positional": "rest"},
                        "group": {"type": "string", "description": "Grouping."},
                    },
                    "required": [],
                    "x-nd-order": ["query", "group"],
                }),
            ),
            tool(
                "headway_save_view",
                &["headway", "list"],
                json!({
                    "type": "object",
                    "properties": {
                        "save_view": {"type": "string", "description": "The name."},
                        "query": {"type": "string", "description": "Terms.", "x-nd-positional": "rest"},
                        "group": {"type": "string", "description": "Grouping."},
                    },
                    "required": ["save_view"],
                    "x-nd-order": ["save_view", "query", "group"],
                }),
            ),
            tool(
                "headway_open_review_queue",
                &[],
                json!({"type": "object", "properties": {}}),
            ),
        ]
    }

    /// A shell for [`resolve`] to read: its variables, working directory
    /// and stdin.
    struct Fake<'f> {
        env: &'f [(&'f str, &'f str)],
        cwd: PathBuf,
        stdin: &'f str,
    }

    impl Caller for Fake<'_> {
        fn env(&self, var: &str) -> Option<String> {
            self.env
                .iter()
                .find(|(k, _)| *k == var)
                .map(|(_, v)| (*v).to_owned())
        }

        fn cwd(&self) -> Result<PathBuf, String> {
            Ok(self.cwd.clone())
        }

        fn stdin(&self) -> Result<String, String> {
            Ok(self.stdin.to_owned())
        }
    }

    fn resolve_in(line: &[&str], caller: &Fake) -> Result<Action, String> {
        let words: Vec<String> = line.iter().map(|w| (*w).to_owned()).collect();
        resolve(&fixture(), &words, false, caller)
    }

    fn run_env(line: &[&str], env: &[(&str, &str)]) -> Result<Action, String> {
        let caller = Fake {
            env,
            cwd: PathBuf::from("/"),
            stdin: "",
        };
        resolve_in(line, &caller)
    }

    /// The args `line` calls its tool with, run in `dir` with `stdin`.
    fn args_in(line: &[&str], dir: &Path, stdin: &str) -> Result<Value, String> {
        let caller = Fake {
            env: &[],
            cwd: dir.to_owned(),
            stdin,
        };
        match resolve_in(line, &caller)? {
            Action::Call { args, .. } => Ok(args),
            other => panic!("{line:?}: {other:?}"),
        }
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
            signer: None,
            streams: None,
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

    /// A required flag picks its tool over one that doesn't require it,
    /// listed earlier or not, and only when the line gives it.
    #[test]
    fn a_shared_path_picks_by_required_flags() {
        assert_eq!(
            run(&["headway", "list", "p:high", "--group", "board"]),
            call("headway_list", json!({"query": "p:high", "group": "board"}))
        );
        assert_eq!(
            run(&["headway", "list", "p:high", "--save-view", "Hot"]),
            call(
                "headway_save_view",
                json!({"query": "p:high", "save_view": "Hot"})
            )
        );
        assert_eq!(
            run(&["headway", "list", "--save-view=Hot"]),
            call("headway_save_view", json!({"save_view": "Hot"}))
        );
        // The usage line names the flag that picks the tool.
        let Ok(Action::Help(page)) = run(&["headway", "list", "--help"]) else {
            panic!("a help page");
        };
        assert!(
            page.contains("nd headway list --save-view <save_view> [<query>...] [OPTIONS]"),
            "{page}"
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
            |line: &[&str], help: bool| match resolve(&fixture(), &words(line), help, &Process) {
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

    /// A relative path is made absolute against the caller's cwd and
    /// canonicalized; a default-cwd path left out is that cwd; a path that
    /// isn't there fails here, before the call.
    #[test]
    fn a_path_is_resolved_in_the_callers_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/notes.md"), "x").unwrap();
        let path = |p: &Path| p.to_string_lossy().into_owned();

        let args = args_in(
            &[
                "headway",
                "edit",
                "c",
                "--repo-dir",
                "sub/../sub",
                "--file",
                "sub/notes.md",
            ],
            &root,
            "",
        )
        .unwrap();
        assert_eq!(args["repo_dir"], json!(path(&root.join("sub"))));
        assert_eq!(args["file"], json!(path(&root.join("sub/notes.md"))));

        let args = args_in(&["headway", "edit", "c"], &root.join("sub"), "").unwrap();
        assert_eq!(args["repo_dir"], json!(path(&root.join("sub"))));
        assert!(args.get("file").is_none(), "{args}");

        let err = args_in(&["headway", "edit", "c", "--file", "gone.md"], &root, "").unwrap_err();
        assert!(err.starts_with("file: 'gone.md': "), "{err}");
        assert!(err.ends_with("(see `nd headway edit --help`)"), "{err}");
    }

    /// A paths flag repeats, one path each (a comma stays in its path),
    /// and each is resolved as a path is; one that isn't there fails here.
    #[test]
    fn a_paths_flag_repeats_and_resolves_each() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        std::fs::write(root.join("b,c.md"), "b").unwrap();
        let path = |p: &Path| p.to_string_lossy().into_owned();

        let args = args_in(
            &[
                "headway",
                "edit",
                "c",
                "--source",
                "a.md",
                "--source=b,c.md",
            ],
            &root,
            "",
        )
        .unwrap();
        assert_eq!(
            args["sources"],
            json!([path(&root.join("a.md")), path(&root.join("b,c.md"))])
        );
        let args = args_in(&["headway", "edit", "c"], &root, "").unwrap();
        assert!(args.get("sources").is_none(), "{args}");

        let err = args_in(&["headway", "edit", "c", "--source", "gone.md"], &root, "").unwrap_err();
        assert!(err.starts_with("sources: 'gone.md': "), "{err}");
    }

    /// A flag typed before the verb, as the old CLIs took their globals,
    /// reads as if typed after it, value and all; `--` still ends flags.
    #[test]
    fn a_flag_before_the_verb_moves_after_it() {
        let Ok(Action::Call {
            tool, args, output, ..
        }) = run(&["headway", "--json", "--labels", "a", "edit", "c", "-l", "b"])
        else {
            panic!("a leading flag runs");
        };
        assert_eq!(tool, "headway_edit");
        assert_eq!(args["labels"], json!(["a", "b"]));
        assert!(output.json);
        let Ok(Action::Call { args, .. }) = run(&["headway", "edit", "--", "--labels", "x"]) else {
            panic!("-- runs");
        };
        assert_eq!(args["card"], "--labels", "a word after -- is not a flag");
    }

    /// A strings flag repeats, one value each, kept whole: a comma or a
    /// space stays in its value.
    #[test]
    fn a_strings_flag_repeats_and_keeps_each_whole() {
        let Ok(Action::Call { args, .. }) =
            run(&["headway", "edit", "c", "--env", "A=x,y", "--env= B=z "])
        else {
            panic!("a strings flag runs");
        };
        assert_eq!(args["env"], json!(["A=x,y", " B=z "]));
    }

    /// A list flag repeats, each value may be a comma list, and blanks
    /// drop out.
    #[test]
    fn a_list_flag_repeats_and_splits_on_commas() {
        let dir = tempfile::tempdir().unwrap();
        let args = args_in(
            &[
                "headway", "edit", "c", "-l", "a", "--labels", "b, c,", "-l=d",
            ],
            dir.path(),
            "",
        )
        .unwrap();
        assert_eq!(args["labels"], json!(["a", "b", "c", "d"]));
    }

    /// A required positional given by its file form needs no bare word: `comment
    /// <card> --body-file -` runs, and the slots still fill in order.
    #[test]
    fn a_file_form_stands_in_for_a_required_positional() {
        let tools = vec![tool(
            "headway_comment_card",
            &["headway", "comment"],
            json!({
                "type": "object",
                "properties": {
                    "card": {"type": "string", "description": "The card.", "x-nd-positional": 0},
                    "body": {"type": "string", "description": "The text.",
                             "x-nd-positional": "rest", "x-nd-from-file": "body-file"},
                },
                "required": ["card", "body"],
                "x-nd-order": ["card", "body"],
            }),
        )];
        let caller = Fake {
            env: &[],
            cwd: PathBuf::from("/"),
            stdin: "from stdin\n",
        };
        let resolve_line = |line: &[&str]| {
            let words: Vec<String> = line.iter().map(|w| (*w).to_owned()).collect();
            resolve(&tools, &words, false, &caller)
        };
        let Action::Call { args, .. } =
            resolve_line(&["headway", "comment", "c", "--body-file", "-"]).unwrap()
        else {
            panic!("a call");
        };
        assert_eq!(args, json!({"card": "c", "body": "from stdin"}));

        let err = resolve_line(&["headway", "comment", "--body-file", "-"]).unwrap_err();
        assert!(err.starts_with("missing <card>"), "{err}");
        let err = resolve_line(&["headway", "comment", "c"]).unwrap_err();
        assert!(err.starts_with("missing <body>"), "{err}");
    }

    /// A positional that declares a flag answers to it as well as to bare
    /// words, the two spellings adding up, and its page lists both; one
    /// that declares none stays bare-words only.
    #[test]
    fn a_positional_with_a_flag_takes_either_spelling() {
        let tools = vec![tool(
            "headway_set_labels",
            &["headway", "label"],
            json!({
                "type": "object",
                "properties": {
                    "card": {"type": "string", "description": "The card.", "x-nd-positional": 0},
                    "labels": {"type": "array", "items": {"type": "string"}, "description": "Labels.",
                               "x-nd-positional": "rest", "x-nd-kind": "list",
                               "x-nd-short": "l", "x-nd-long": "label"},
                },
                "required": ["card"],
                "x-nd-order": ["card", "labels"],
            }),
        )];
        let caller = Fake {
            env: &[],
            cwd: PathBuf::from("/"),
            stdin: "",
        };
        let resolve_line = |line: &[&str]| {
            let words: Vec<String> = line.iter().map(|w| (*w).to_owned()).collect();
            resolve(&tools, &words, false, &caller)
        };

        let Action::Call { args, .. } =
            resolve_line(&["headway", "label", "c", "a", "b,c", "-l", "d", "--label=e"]).unwrap()
        else {
            panic!("a call");
        };
        assert_eq!(args["card"], json!("c"));
        assert_eq!(args["labels"], json!(["a", "b", "c", "d", "e"]));

        let err = resolve_line(&["headway", "label", "--card", "c"]).unwrap_err();
        assert!(err.starts_with("unknown flag '--card'"), "{err}");

        let Action::Help(page) = resolve_line(&["headway", "label", "--help"]).unwrap() else {
            panic!("a page");
        };
        assert!(page.contains("<labels>, -l, --label <labels>"), "{page}");
    }

    /// A file form reads its file against the caller's cwd, or stdin for
    /// `-`, trimmed at the end; it fills a positional too, and stdin is
    /// read only once.
    #[test]
    fn a_file_form_reads_the_file_or_stdin() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("desc.md"), "## Why\n\nbody\n\n").unwrap();

        let args = args_in(
            &["headway", "edit", "c", "--desc-file", "desc.md"],
            dir.path(),
            "",
        )
        .unwrap();
        assert_eq!(args["desc"], json!("## Why\n\nbody"));

        let args = args_in(
            &["headway", "edit", "c", "--body-file", "-"],
            dir.path(),
            "from stdin\n",
        )
        .unwrap();
        assert_eq!(args["body"], json!("from stdin"));

        let err = args_in(
            &[
                "headway",
                "edit",
                "c",
                "--desc-file",
                "-",
                "--body-file",
                "-",
            ],
            dir.path(),
            "x",
        )
        .unwrap_err();
        assert!(err.contains("stdin is already read"), "{err}");

        let err = args_in(
            &["headway", "edit", "c", "--desc-file", "nope.md"],
            dir.path(),
            "",
        )
        .unwrap_err();
        assert!(
            err.starts_with("--desc-file: can't read 'nope.md'"),
            "{err}"
        );
    }

    /// An argument and its file form are one value, so giving both fails
    /// rather than letting one win.
    #[test]
    fn an_arg_and_its_file_form_conflict() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("d.md"), "file").unwrap();
        let err = args_in(
            &[
                "headway",
                "edit",
                "c",
                "--desc",
                "inline",
                "--desc-file",
                "d.md",
            ],
            dir.path(),
            "",
        )
        .unwrap_err();
        assert!(
            err.starts_with("give --desc or --desc-file, not both"),
            "{err}"
        );

        let err = args_in(
            &["headway", "edit", "c", "typed", "body", "--body-file", "-"],
            dir.path(),
            "piped",
        )
        .unwrap_err();
        assert!(
            err.starts_with("give <body> or --body-file, not both"),
            "{err}"
        );
    }

    /// The caller's session fills a tool's `x-nd-env` argument the way
    /// `$HEADWAY_BOARD` does, and a typed value beats it.
    /// The signer `line` sends, in `cwd` with `env`.
    fn signer_of(
        line: &[&str],
        env: &[(&str, &str)],
        cwd: &Path,
    ) -> Result<Option<String>, String> {
        let caller = Fake {
            env,
            cwd: cwd.to_owned(),
            stdin: "",
        };
        match resolve_in(line, &caller)? {
            Action::Call { signer, .. } => Ok(signer.map(|s| s.comment_nsec)),
            other => panic!("{line:?}: {other:?}"),
        }
    }

    /// The comment key goes with a call of a tool that accepts one, from
    /// the variable before the file, trimmed; never with any other tool.
    #[test]
    fn the_comment_key_goes_only_to_a_tool_that_accepts_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("key"), "nsec1fromfile\n").unwrap();
        let edit = ["headway", "edit", "c"];
        let inline = ("HEADWAY_COMMENT_NSEC", " nsec1inline ");
        let file = ("HEADWAY_COMMENT_NSEC_FILE", "key");

        assert_eq!(signer_of(&edit, &[], dir.path()), Ok(None));
        assert_eq!(
            signer_of(&edit, &[file], dir.path()),
            Ok(Some("nsec1fromfile".to_owned()))
        );
        assert_eq!(
            signer_of(&edit, &[inline, file], dir.path()),
            Ok(Some("nsec1inline".to_owned()))
        );
        assert_eq!(
            signer_of(&["headway", "show"], &[inline, file], dir.path()),
            Ok(None),
            "headway_show_board doesn't accept one"
        );

        let missing = ("HEADWAY_COMMENT_NSEC_FILE", "absent");
        let err = signer_of(&edit, &[missing], dir.path()).unwrap_err();
        assert!(err.contains("can't read"), "{err}");
        let err = signer_of(&edit, &[("HEADWAY_COMMENT_NSEC", " ")], dir.path()).unwrap_err();
        assert!(err.contains("holds no key"), "{err}");
    }

    #[test]
    fn agentium_session_falls_back_from_env() {
        let env = [("AGENTIUM_SESSION", "agentium:gaze-cross-blossom")];
        let Ok(Action::Call { args, .. }) = run_env(&["headway", "edit", "c"], &env) else {
            panic!("no call");
        };
        assert_eq!(args["session"], json!("agentium:gaze-cross-blossom"));
        let Ok(Action::Call { args, .. }) =
            run_env(&["headway", "edit", "c", "--session", "agentium:x"], &env)
        else {
            panic!("no call");
        };
        assert_eq!(args["session"], json!("agentium:x"));
    }

    /// The page says how each caller-side kind reads.
    #[test]
    fn the_page_explains_the_caller_side_kinds() {
        let Ok(Action::Help(page)) = run(&["headway", "edit", "--help"]) else {
            panic!("no page");
        };
        let flat = page.split_whitespace().collect::<Vec<_>>().join(" ");
        for want in [
            "-l, --labels <labels> Labels. Repeat it, or give a comma list.",
            "--desc-file <path|-> Read --desc from a file, or stdin for -.",
            "--body-file <path|-> Read <body> from a file, or stdin for -.",
            "The checkout. Defaults to the current directory.",
            "Who. Falls back to $AGENTIUM_SESSION.",
        ] {
            assert!(flat.contains(want), "{want}\n{page}");
        }
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
