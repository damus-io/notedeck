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
    /// `--relay <url>`: the relay the vault syncs with, overriding
    /// `$CASHFLOW_RELAY` for this run.
    pub relay: Option<String>,
    /// `--db <path>`: the CLI's own nostrdb for this run.
    pub db: Option<PathBuf>,
    pub command: Command,
}

/// A command to run.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// `cashflow config path [<name>] [--json]`.
    ConfigPath { name: Option<String>, json: bool },
    /// `cashflow config show [--json]`.
    ConfigShow { json: bool },
    /// `cashflow login [<nsec>]`: `None` reads it from stdin.
    Login { nsec: Option<String> },
    /// `cashflow logout`.
    Logout,
    /// `cashflow settings [show] [--json]`.
    SettingsShow { json: bool },
    /// `cashflow settings get <name>`.
    SettingsGet { name: String },
    /// `cashflow settings set <name> <value>`.
    SettingsSet { name: String, value: String },
}

/// The global flags, each taking a value, read before or after the command.
#[derive(Default)]
struct Globals {
    dir: Option<PathBuf>,
    relay: Option<String>,
    db: Option<PathBuf>,
}

impl Globals {
    /// Read `flag`'s value from `args` if it is a global flag. `Ok(false)`
    /// when it isn't one.
    fn take(
        &mut self,
        flag: &str,
        args: &mut impl Iterator<Item = String>,
    ) -> Result<bool, String> {
        match flag {
            "--dir" => self.dir = Some(value_of(flag, args.next())?.into()),
            "--relay" => self.relay = Some(value_of(flag, args.next())?),
            "--db" => self.db = Some(value_of(flag, args.next())?.into()),
            _ => return Ok(false),
        }
        Ok(true)
    }
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
    let mut globals = Globals::default();

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
            flag if globals.take(flag, &mut args)? => {}
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
        globals.take(&arg, &mut args)?;
        flags.push(arg);
    }

    if want_help {
        return Ok(Invocation::CommandHelp(cmd));
    }
    help::CLI.check_flags(cmd, help::GLOBALS, &flags)?;
    let json = flags.iter().any(|f| f == "--json");

    let command = match cmd.name {
        "config" => config_command(positionals, json)?,
        "login" => login_command(positionals)?,
        "logout" => no_positionals(positionals, Command::Logout, "logout")?,
        "settings" => settings_command(positionals, json)?,
        other => unreachable!("'{other}' has a help entry but no parser arm"),
    };
    Ok(Invocation::Run(Run {
        dir: globals.dir,
        relay: globals.relay,
        db: globals.db,
        command,
    }))
}

/// `login [<nsec>]`.
fn login_command(positionals: Vec<String>) -> Result<Command, String> {
    let mut positionals = positionals.into_iter();
    let nsec = positionals.next();
    if positionals.next().is_some() {
        return Err("`login` takes at most one <nsec> (see `cashflow login --help`)".to_owned());
    }
    Ok(Command::Login { nsec })
}

/// `command` when there are no positionals, else an error naming `name`.
fn no_positionals(
    positionals: Vec<String>,
    command: Command,
    name: &str,
) -> Result<Command, String> {
    if let Some(extra) = positionals.first() {
        return Err(format!("`{name}` takes no arguments, got '{extra}'"));
    }
    Ok(command)
}

/// `settings`'s subcommand, from its positional arguments.
fn settings_command(positionals: Vec<String>, json: bool) -> Result<Command, String> {
    let usage = "`settings` takes `show`, `get <name>` or `set <name> <value>` (see `cashflow settings --help`)";
    let mut positionals = positionals.into_iter();
    let command = match positionals.next().as_deref() {
        None | Some("show") => Command::SettingsShow { json },
        Some("get") => Command::SettingsGet {
            name: positionals.next().ok_or(usage)?,
        },
        Some("set") => Command::SettingsSet {
            name: positionals.next().ok_or(usage)?,
            value: positionals.next().ok_or(usage)?,
        },
        Some(_) => return Err(usage.to_owned()),
    };
    if positionals.next().is_some() {
        return Err(usage.to_owned());
    }
    Ok(command)
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
    fn relay_and_db_are_global() {
        let run = run_of(&["--relay", "ws://r", "settings", "--db", "/db"]);
        assert_eq!(run.relay.as_deref(), Some("ws://r"));
        assert_eq!(run.db, Some(PathBuf::from("/db")));
        let bare = run_of(&["settings"]);
        assert_eq!((bare.relay, bare.db), (None, None));
        assert!(err_of(&["settings", "--relay"]).contains("needs a value"));
    }

    #[test]
    fn login_logout_and_settings() {
        assert_eq!(run_of(&["login"]).command, Command::Login { nsec: None });
        assert_eq!(
            run_of(&["login", "nsec1x"]).command,
            Command::Login {
                nsec: Some("nsec1x".to_owned())
            }
        );
        assert!(err_of(&["login", "a", "b"]).contains("at most one"));
        assert_eq!(run_of(&["logout"]).command, Command::Logout);
        assert!(err_of(&["logout", "x"]).contains("no arguments"));

        assert_eq!(
            run_of(&["settings"]).command,
            Command::SettingsShow { json: false }
        );
        assert_eq!(
            run_of(&["settings", "show", "--json"]).command,
            Command::SettingsShow { json: true }
        );
        assert_eq!(
            run_of(&["settings", "get", "home"]).command,
            Command::SettingsGet {
                name: "home".to_owned()
            }
        );
        assert_eq!(
            run_of(&["settings", "set", "home", "CAD"]).command,
            Command::SettingsSet {
                name: "home".to_owned(),
                value: "CAD".to_owned()
            }
        );
        assert!(err_of(&["settings", "set", "home"]).contains("set <name> <value>"));
        assert!(err_of(&["settings", "frob"]).contains("set <name> <value>"));
        assert!(err_of(&["settings", "get", "home", "x"]).contains("get <name>"));
    }

    #[test]
    fn unknown_names_and_flags_are_rejected() {
        assert!(err_of(&["frob"]).contains("unknown command 'frob'"));
        assert!(err_of(&["--frob", "config"]).contains("unknown flag '--frob'"));
        assert!(err_of(&["config", "show", "--frob"]).contains("doesn't take --frob"));
    }
}
