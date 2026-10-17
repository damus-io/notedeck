//! Parsing a command line into an [`Invocation`].
//!
//! The help table in [`crate::help`] is the parser's source of truth: a
//! command name must have an entry there, a flag must be on the command's page
//! (or be a global one). So a command or flag can't ship undocumented, and the
//! page and the parser can't drift.

use std::path::PathBuf;

use cli_term::help::{CommandHelp, OPTIONS_PAGE};

use crate::help;

/// What a command line asks for.
pub enum Invocation {
    /// A command to run.
    Run(Run),
    /// `cashflow`, or `cashflow --help`: the command list.
    Usage,
    /// `cashflow <cmd> --help` or `cashflow help <cmd>`.
    CommandHelp(&'static CommandHelp),
    /// `cashflow help options`.
    Options,
}

/// A command and the global options it runs with.
#[derive(Debug, PartialEq)]
pub struct Run {
    /// `--dir <path>`: overrides `$CASHFLOW_DIR` for this run.
    pub dir: Option<PathBuf>,
    pub command: Command,
}

/// A command to run.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// `cashflow config path [<name>] [--json]`.
    ConfigPath { name: Option<String>, json: bool },
    /// `cashflow config show [--json]`.
    ConfigShow { json: bool },
}

/// Parse the arguments after the program name.
///
/// Before the command, only `--help`, `help` and the global flags are read.
/// After it, a flag is the command's when its page lists it; `--help` anywhere
/// turns the run into that command's page.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Invocation, String> {
    let mut args = args.into_iter();
    let mut want_help = false;
    let mut help_command = false;
    let mut dir: Option<PathBuf> = None;

    let name = loop {
        let Some(arg) = args.next() else {
            return Ok(Invocation::Usage);
        };
        match arg.as_str() {
            "-h" | "--help" => want_help = true,
            "help" if !help_command => {
                help_command = true;
                want_help = true;
            }
            "--dir" => dir = Some(value_of(&arg, args.next())?.into()),
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag '{flag}' (try `cashflow --help`)"));
            }
            _ => break arg,
        }
    };

    if help_command && name == OPTIONS_PAGE {
        return Ok(Invocation::Options);
    }
    let Some(cmd) = help::CLI.lookup(&name) else {
        return Err(format!("unknown command '{name}' (try `cashflow --help`)"));
    };

    let mut flags: Vec<String> = Vec::new();
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
        // No command flag takes a value yet. The first that does reads it
        // here, keyed off its page the way `nd_cli`'s `takes_value` does.
        if arg == "--dir" {
            dir = Some(value_of(&arg, args.next())?.into());
        }
        flags.push(arg);
    }

    if want_help {
        return Ok(Invocation::CommandHelp(cmd));
    }
    help::CLI.check_flags(cmd, help::GLOBALS, &flags)?;
    let json = flags.iter().any(|f| f == "--json");

    let command = match cmd.name {
        "config" => config_command(positionals, json)?,
        other => unreachable!("'{other}' has a help entry but no parser arm"),
    };
    Ok(Invocation::Run(Run { dir, command }))
}

/// `config`'s subcommand, from its positional arguments.
fn config_command(positionals: Vec<String>, json: bool) -> Result<Command, String> {
    let mut positionals = positionals.into_iter();
    let usage = "`config` takes `path [<name>]` or `show` (see `cashflow config --help`)";
    let command = match positionals.next().as_deref() {
        Some("path") => Command::ConfigPath {
            name: positionals.next(),
            json,
        },
        Some("show") => Command::ConfigShow { json },
        _ => return Err(usage.to_owned()),
    };
    if positionals.next().is_some() {
        return Err(usage.to_owned());
    }
    Ok(command)
}

/// The value after a valued flag, or an error naming the flag.
fn value_of(flag: &str, value: Option<String>) -> Result<String, String> {
    value.ok_or_else(|| format!("{flag} needs a value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Invocation, String> {
        parse(args.iter().map(|a| a.to_string()))
    }

    fn run_of(args: &[&str]) -> Run {
        match parse_args(args) {
            Ok(Invocation::Run(run)) => run,
            Ok(_) => panic!("{args:?} is a help page, not a run"),
            Err(err) => panic!("{args:?} failed: {err}"),
        }
    }

    fn err_of(args: &[&str]) -> String {
        match parse_args(args) {
            Err(err) => err,
            Ok(_) => panic!("{args:?} parsed"),
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
        assert!(matches!(
            parse_args(&["help", "options"]),
            Ok(Invocation::Options)
        ));
        assert_eq!(page_of(&["config", "--help"]), Some("config"));
        assert_eq!(page_of(&["help", "config"]), Some("config"));
        assert_eq!(
            page_of(&["config", "show", "--bogus", "-h"]),
            Some("config")
        );
    }

    #[test]
    fn config_subcommands() {
        assert_eq!(
            run_of(&["config", "path"]).command,
            Command::ConfigPath {
                name: None,
                json: false
            }
        );
        assert_eq!(
            run_of(&["config", "path", "journal", "--json"]).command,
            Command::ConfigPath {
                name: Some("journal".to_owned()),
                json: true
            }
        );
        assert_eq!(
            run_of(&["config", "show"]).command,
            Command::ConfigShow { json: false }
        );
        assert!(err_of(&["config"]).contains("path"));
        assert!(err_of(&["config", "frob"]).contains("path"));
        assert!(err_of(&["config", "show", "extra"]).contains("path"));
    }

    #[test]
    fn dir_is_global_before_or_after_the_command() {
        let before = run_of(&["--dir", "/a", "config", "show"]);
        assert_eq!(before.dir, Some(PathBuf::from("/a")));
        let after = run_of(&["config", "show", "--dir", "/b"]);
        assert_eq!(after.dir, Some(PathBuf::from("/b")));
        assert_eq!(run_of(&["config", "show"]).dir, None);
        assert!(err_of(&["--dir"]).contains("needs a value"));
    }

    #[test]
    fn unknown_names_and_flags_are_rejected() {
        assert!(err_of(&["frob"]).contains("unknown command 'frob'"));
        assert!(err_of(&["--frob", "config"]).contains("unknown flag '--frob'"));
        assert!(err_of(&["config", "show", "--frob"]).contains("doesn't take --frob"));
    }
}
