//! Parsing a command line into an [`Invocation`].
//!
//! The help table in [`crate::help`] is the parser's source of truth: a
//! command name must have an entry there, a flag must be on the command's page
//! (or be a global one). So a command or flag can't ship undocumented, and the
//! page and the parser can't drift.

use std::path::PathBuf;

use cli_term::help::{CommandHelp, OPTIONS_PAGE, option_names};
use ledgerdb::{Date, LayerFilter};

use crate::help;

/// What a command line asks for.
pub enum Invocation {
    /// A command to run, boxed: it is far larger than the help variants.
    Run(Box<Run>),
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
    /// `cashflow journal ls [--json]`.
    JournalLs { json: bool },
    /// `cashflow journal verify [--json]`.
    JournalVerify { json: bool },
    /// `cashflow journal rm <hash> [--json]`: `hash` is the whole hash or a
    /// unique prefix.
    JournalRm { hash: String, json: bool },
    /// `cashflow import <importer> [<path>...]`.
    Import(ImportArgs),
    /// `cashflow export`.
    Export { books: BooksArgs, range: Range },
    /// `cashflow report bal|reg|top`.
    Report { books: BooksArgs, report: Report },
}

/// `cashflow import`'s arguments.
#[derive(Debug, PartialEq)]
pub struct ImportArgs {
    /// The importer's name.
    pub importer: String,
    /// What it reads.
    pub paths: Vec<PathBuf>,
    /// `--source`: the segment's source, else the importer's name.
    pub source: Option<String>,
    /// `--layer`: the segment's layer, else the importer's.
    pub layer: Option<String>,
    pub json: bool,
}

/// Which books a reading command loads: the segments' layers, the rules
/// they are derived with, and the home commodity.
#[derive(Debug, PartialEq)]
pub struct BooksArgs {
    /// `--layer` (only these) or `--except` (all but these).
    pub filter: LayerFilter,
    /// `--rules <toml>`.
    pub rules: Option<PathBuf>,
    /// `--in <commodity>`: what amounts convert into.
    pub home: String,
}

/// `--begin <date>` (inclusive) and `--end <date>` (exclusive).
#[derive(Debug, Default, PartialEq, Clone, Copy)]
pub struct Range {
    pub begin: Option<Date>,
    pub end: Option<Date>,
}

/// One `cashflow report` subcommand.
#[derive(Debug, PartialEq)]
pub enum Report {
    /// `report bal [<account>] [--depth <n>] [--end <date>] [--native]`.
    Bal {
        account: Option<String>,
        depth: Option<u16>,
        end: Option<Date>,
        native: bool,
    },
    /// `report reg [<account>] [--begin] [--end] [--payee <text>]`.
    Reg {
        account: Option<String>,
        range: Range,
        payee: Option<String>,
    },
    /// `report top [<parent>] [--depth <n>] [--begin] [--end] [--limit <n>]`.
    Top {
        parent: Option<String>,
        depth: Option<u16>,
        range: Range,
        limit: usize,
    },
}

/// The commodity reports convert into when `--in` doesn't say.
pub const DEFAULT_HOME: &str = "CAD";

/// How many accounts `report top` lists when `--limit` doesn't say.
pub const DEFAULT_TOP: usize = 10;

/// A command flag as given, with its value when its page says it takes one.
struct Flag {
    name: String,
    value: Option<String>,
}

/// The command flags of one run, in order.
#[derive(Default)]
struct Flags(Vec<Flag>);

impl Flags {
    /// Whether `name` was given.
    fn has(&self, name: &str) -> bool {
        self.0.iter().any(|f| f.name == name)
    }

    /// The last value given for `name`.
    fn value(&self, name: &str) -> Option<&str> {
        self.values(name).last()
    }

    /// Every value given for `name`, in order.
    fn values(&self, name: &str) -> impl Iterator<Item = &str> {
        self.0
            .iter()
            .filter(move |f| f.name == name)
            .filter_map(|f| f.value.as_deref())
    }

    /// The first of `names` given, for a subcommand that doesn't take it.
    fn first_of<'a>(&self, names: &[&'a str]) -> Option<&'a str> {
        names.iter().copied().find(|name| self.has(name))
    }

    /// `name`'s value parsed as a date.
    fn date(&self, name: &str) -> Result<Option<Date>, String> {
        self.value(name)
            .map(|text| Date::parse(text).map_err(|e| format!("{name} {text}: {e}")))
            .transpose()
    }

    /// `name`'s value parsed as a number.
    fn number<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>, String> {
        self.value(name)
            .map(|text| {
                text.parse()
                    .map_err(|_| format!("{name} needs a number, got '{text}'"))
            })
            .transpose()
    }
}

/// Whether `cmd`'s page lists `flag` with a value placeholder.
fn takes_value(cmd: &CommandHelp, flag: &str) -> bool {
    cmd.options
        .iter()
        .any(|(key, _)| key.contains('<') && option_names(key).any(|name| name == flag))
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

    let mut flags = Flags::default();
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
        if globals.take(&arg, &mut args)? {
            continue;
        }
        // A flag whose page key has a `<placeholder>` reads the next word.
        let value = if takes_value(cmd, &arg) {
            Some(value_of(&arg, args.next())?)
        } else {
            None
        };
        flags.0.push(Flag { name: arg, value });
    }

    if want_help {
        return Ok(Invocation::CommandHelp(cmd));
    }
    let names: Vec<&str> = flags.0.iter().map(|f| f.name.as_str()).collect();
    help::CLI.check_flags(cmd, help::GLOBALS, &names)?;
    let json = flags.has("--json");

    let command = match cmd.name {
        "config" => config_command(positionals, json)?,
        "login" => login_command(positionals)?,
        "logout" => no_positionals(positionals, Command::Logout, "logout")?,
        "settings" => settings_command(positionals, json)?,
        "journal" => journal_command(positionals, json)?,
        "import" => import_command(positionals, &flags, json)?,
        "export" => no_positionals(
            positionals,
            Command::Export {
                books: books_args(&flags)?,
                range: range(&flags)?,
            },
            "export",
        )?,
        "report" => report_command(positionals, &flags)?,
        other => unreachable!("'{other}' has a help entry but no parser arm"),
    };
    Ok(Invocation::Run(Box::new(Run {
        dir: globals.dir,
        relay: globals.relay,
        db: globals.db,
        command,
    })))
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

/// `journal`'s subcommand.
fn journal_command(positionals: Vec<String>, json: bool) -> Result<Command, String> {
    let usage = "`journal` takes `ls`, `verify` or `rm <hash>` (see `cashflow journal --help`)";
    let mut positionals = positionals.into_iter();
    let command = match positionals.next().as_deref() {
        Some("ls") => Command::JournalLs { json },
        Some("verify") => Command::JournalVerify { json },
        Some("rm") => Command::JournalRm {
            hash: positionals.next().ok_or(usage)?,
            json,
        },
        _ => return Err(usage.to_owned()),
    };
    if positionals.next().is_some() {
        return Err(usage.to_owned());
    }
    Ok(command)
}

/// `import <importer> [<path>...]`.
fn import_command(positionals: Vec<String>, flags: &Flags, json: bool) -> Result<Command, String> {
    let mut positionals = positionals.into_iter();
    let importer = positionals.next().ok_or_else(|| {
        format!(
            "`import` needs an importer: {} (see `cashflow import --help`)",
            cashflow::import::NAMES.join(", ")
        )
    })?;
    Ok(Command::Import(ImportArgs {
        importer,
        paths: positionals.map(PathBuf::from).collect(),
        source: flags.value("--source").map(str::to_owned),
        layer: flags.value("--layer").map(str::to_owned),
        json,
    }))
}

/// The `--layer`/`--except`/`--rules`/`--in` a reading command shares.
fn books_args(flags: &Flags) -> Result<BooksArgs, String> {
    let only: Vec<String> = flags.values("--layer").map(str::to_owned).collect();
    let except: Vec<String> = flags.values("--except").map(str::to_owned).collect();
    let filter = match (only.is_empty(), except.is_empty()) {
        (true, true) => LayerFilter::All,
        (false, true) => LayerFilter::Only(only),
        (true, false) => LayerFilter::Except(except),
        (false, false) => return Err("--layer and --except can't be used together".to_owned()),
    };
    Ok(BooksArgs {
        filter,
        rules: flags.value("--rules").map(PathBuf::from),
        home: flags.value("--in").unwrap_or(DEFAULT_HOME).to_owned(),
    })
}

/// `--begin` and `--end`.
fn range(flags: &Flags) -> Result<Range, String> {
    let range = Range {
        begin: flags.date("--begin")?,
        end: flags.date("--end")?,
    };
    if let Range {
        begin: Some(begin),
        end: Some(end),
    } = range
        && end <= begin
    {
        return Err(format!("--end {end} is not after --begin {begin}"));
    }
    Ok(range)
}

/// `report`'s subcommand.
fn report_command(positionals: Vec<String>, flags: &Flags) -> Result<Command, String> {
    let usage = "`report` takes `bal`, `reg` or `top` (see `cashflow report --help`)";
    let mut positionals = positionals.into_iter();
    let sub = positionals.next().ok_or(usage)?;
    let account = positionals.next();
    if positionals.next().is_some() {
        return Err(format!("`report {sub}` takes at most one account"));
    }
    // The flags only some subcommands read: the rest refuse them.
    let refuse = |names: &[&str]| match flags.first_of(names) {
        Some(flag) => Err(format!("`report {sub}` doesn't take {flag}")),
        None => Ok(()),
    };
    let report = match sub.as_str() {
        "bal" => {
            refuse(&["--begin", "--payee", "--limit"])?;
            Report::Bal {
                account,
                depth: flags.number("--depth")?,
                end: flags.date("--end")?,
                native: flags.has("--native"),
            }
        }
        "reg" => {
            refuse(&["--depth", "--native", "--limit"])?;
            Report::Reg {
                account,
                range: range(flags)?,
                payee: flags.value("--payee").map(str::to_owned),
            }
        }
        "top" => {
            refuse(&["--native", "--payee"])?;
            Report::Top {
                parent: account,
                depth: flags.number("--depth")?,
                range: range(flags)?,
                limit: flags.number("--limit")?.unwrap_or(DEFAULT_TOP),
            }
        }
        _ => return Err(usage.to_owned()),
    };
    Ok(Command::Report {
        books: books_args(flags)?,
        report,
    })
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
            Ok(Invocation::Run(run)) => *run,
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

    fn date(text: &str) -> Option<Date> {
        Some(Date::parse(text).unwrap())
    }

    #[test]
    fn journal_and_import() {
        assert_eq!(
            run_of(&["journal", "ls", "--json"]).command,
            Command::JournalLs { json: true }
        );
        assert_eq!(
            run_of(&["journal", "verify"]).command,
            Command::JournalVerify { json: false }
        );
        assert_eq!(
            run_of(&["journal", "rm", "3fa9", "--json"]).command,
            Command::JournalRm {
                hash: "3fa9".to_owned(),
                json: true
            }
        );
        let usage = "`ls`, `verify` or `rm <hash>`";
        assert!(err_of(&["journal"]).contains(usage));
        assert!(err_of(&["journal", "ls", "x"]).contains(usage));
        assert!(err_of(&["journal", "rm"]).contains(usage));
        assert!(err_of(&["journal", "rm", "a", "b"]).contains(usage));

        assert_eq!(
            run_of(&[
                "import", "ledger", "a.ledger", "--source", "bank", "--layer", "cards"
            ])
            .command,
            Command::Import(ImportArgs {
                importer: "ledger".to_owned(),
                paths: vec![PathBuf::from("a.ledger")],
                source: Some("bank".to_owned()),
                layer: Some("cards".to_owned()),
                json: false,
            })
        );
        assert!(err_of(&["import"]).contains("needs an importer: ledger"));
        assert!(err_of(&["import", "ledger", "--source"]).contains("--source needs a value"));
    }

    #[test]
    fn export_reads_layers_rules_and_range() {
        let run = run_of(&[
            "export",
            "--layer",
            "a",
            "--layer",
            "b",
            "--rules",
            "r.toml",
            "--begin",
            "2024-01-01",
            "--end",
            "2024-02-01",
        ]);
        assert_eq!(
            run.command,
            Command::Export {
                books: BooksArgs {
                    filter: LayerFilter::Only(vec!["a".to_owned(), "b".to_owned()]),
                    rules: Some(PathBuf::from("r.toml")),
                    home: DEFAULT_HOME.to_owned(),
                },
                range: Range {
                    begin: date("2024-01-01"),
                    end: date("2024-02-01"),
                },
            }
        );
        let except = run_of(&["export", "--except", "btc"]);
        let Command::Export { books, .. } = except.command else {
            panic!("not an export");
        };
        assert_eq!(books.filter, LayerFilter::Except(vec!["btc".to_owned()]));
        assert!(err_of(&["export", "--layer", "a", "--except", "b"]).contains("together"));
        assert!(err_of(&["export", "--begin", "2024-13-01"]).contains("--begin"));
        assert!(
            err_of(&["export", "--begin", "2024-02-01", "--end", "2024-01-01"])
                .contains("not after")
        );
        assert!(err_of(&["export", "x"]).contains("no arguments"));
    }

    #[test]
    fn report_subcommands() {
        assert_eq!(
            run_of(&[
                "report", "bal", "assets", "--depth", "3", "--native", "--in", "USD"
            ])
            .command,
            Command::Report {
                books: BooksArgs {
                    filter: LayerFilter::All,
                    rules: None,
                    home: "USD".to_owned(),
                },
                report: Report::Bal {
                    account: Some("assets".to_owned()),
                    depth: Some(3),
                    end: None,
                    native: true,
                },
            }
        );
        let Command::Report { report, .. } =
            run_of(&["report", "top", "--limit", "3", "--end", "2025-01-01"]).command
        else {
            panic!("not a report");
        };
        assert_eq!(
            report,
            Report::Top {
                parent: None,
                depth: None,
                range: Range {
                    begin: None,
                    end: date("2025-01-01"),
                },
                limit: 3,
            }
        );
        let Command::Report { report, .. } = run_of(&["report", "reg", "--payee", "shop"]).command
        else {
            panic!("not a report");
        };
        assert!(matches!(report, Report::Reg { payee: Some(p), .. } if p == "shop"));

        assert!(err_of(&["report"]).contains("`bal`, `reg` or `top`"));
        assert!(err_of(&["report", "bal", "--payee", "x"]).contains("doesn't take --payee"));
        assert!(err_of(&["report", "top", "--limit", "many"]).contains("needs a number"));
        assert!(err_of(&["report", "reg", "a", "b"]).contains("at most one"));
    }

    #[test]
    fn unknown_names_and_flags_are_rejected() {
        assert!(err_of(&["frob"]).contains("unknown command 'frob'"));
        assert!(err_of(&["--frob", "config"]).contains("unknown flag '--frob'"));
        assert!(err_of(&["config", "show", "--frob"]).contains("doesn't take --frob"));
    }
}
