//! Parsing a command line into an [`Invocation`].
//!
//! The help table in [`crate::help`] is the parser's source of truth: a
//! command name must have an entry there, a flag must be on the command's page
//! (or be a global one). So a command or flag can't ship undocumented, and the
//! page and the parser can't drift.

use std::path::PathBuf;

use cashflow::chart::{AccountKind, MetaField};
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
    /// `cashflow journal status [--json]`.
    JournalStatus { json: bool },
    /// `cashflow journal publish [--dry-run] [--json]`.
    JournalPublish { write: Write },
    /// `cashflow sync [--transport <name>] [--dry-run] [--json]`.
    Sync {
        transport: Option<String>,
        write: Write,
    },
    /// `cashflow import <importer> [<path>...]`.
    Import(ImportArgs),
    /// `cashflow prices fetch`.
    PricesFetch(PricesArgs),
    /// `cashflow export`.
    Export { books: BooksArgs, range: Range },
    /// `cashflow report bal|reg|top`.
    Report { books: BooksArgs, report: Report },
    /// `cashflow rule ...`.
    Rule(RuleCommand),
    /// `cashflow categorize <record-id> <account>`.
    Categorize {
        record_id: String,
        account: String,
        write: Write,
    },
    /// `cashflow uncategorize <record-id>`.
    Uncategorize { record_id: String, write: Write },
    /// `cashflow split <record-id> <account>[=<amount>]...`.
    Split {
        record_id: String,
        legs: Vec<LegArg>,
        write: Write,
    },
    /// `cashflow transfer ...`.
    Transfer(TransferCommand),
    /// `cashflow edits [--by <ref>] [--json]`.
    Edits { by: Option<String>, json: bool },
    /// `cashflow unknown [--source <s>] [--limit <n>] [--in <c>] [--json]`.
    Unknown(UnknownArgs),
    /// `cashflow account ...`.
    Account(AccountCommand),
    /// `cashflow goals [--board <slug>]... [--author <key>] [--headway-db <path>] [--json]`.
    Goals(GoalsArgs),
    /// `cashflow skills [books options] [--json]`.
    Skills { books: BooksArgs, json: bool },
}

/// One `cashflow account` subcommand.
#[derive(Debug, PartialEq)]
pub enum AccountCommand {
    /// `account ls [<account>] [--depth <n>] [--all] [books options] [--json]`.
    Ls {
        books: BooksArgs,
        account: Option<String>,
        depth: Option<u16>,
        all: bool,
        json: bool,
    },
    /// `account set <account> [fields] [--clear <field>]... [--dry-run] [--json]`.
    Set {
        account: String,
        set: MetaArgs,
        write: Write,
    },
    /// `account alias <from> <to> [--dry-run] [--json]`.
    Alias {
        from: String,
        to: String,
        write: Write,
    },
    /// `account unalias <from> [--dry-run] [--json]`.
    Unalias { from: String, write: Write },
}

/// `account set`'s fields, as given: each `None` leaves the stored value,
/// and `clear` unsets fields so they are inherited again.
#[derive(Debug, PartialEq, Default)]
pub struct MetaArgs {
    /// `--name <text>`.
    pub name: Option<String>,
    /// `--icon <emoji>`.
    pub icon: Option<String>,
    /// `--color <#rrggbb>`.
    pub color: Option<String>,
    /// `--project <name>`.
    pub project: Option<String>,
    /// `--hide` (`Some(true)`) or `--show` (`Some(false)`).
    pub hidden: Option<bool>,
    /// `--type <kind>`.
    pub kind: Option<AccountKind>,
    /// `--clear <field>`, repeatable.
    pub clear: Vec<MetaField>,
}

/// One `split` leg as given: `<account>=<amount>`, or `<account>` alone
/// for the remainder.
#[derive(Debug, PartialEq)]
pub struct LegArg {
    pub account: String,
    /// The amount as written (`12.50`, `-3`, `12.50 CAD`); `None` takes
    /// what the other legs leave.
    pub amount: Option<String>,
}

/// One `cashflow transfer` subcommand.
#[derive(Debug, PartialEq)]
pub enum TransferCommand {
    /// `transfer link <a> <b> [--dry-run] [--json]`.
    Link { a: String, b: String, write: Write },
    /// `transfer unlink <a> <b> [--dry-run] [--json]`.
    Unlink { a: String, b: String, write: Write },
    /// `transfer suggest [--accept-all] [--dry-run] [--json]`.
    Suggest { accept_all: bool, write: Write },
}

/// `cashflow unknown`'s arguments.
#[derive(Debug, PartialEq)]
pub struct UnknownArgs {
    /// `--source <name>`: only rows from this source.
    pub source: Option<String>,
    /// `--limit <n>`: how many groups to print.
    pub limit: usize,
    /// `--in <commodity>`: what totals convert into.
    pub home: String,
    pub json: bool,
}

/// `cashflow goals`'s arguments.
#[derive(Debug, PartialEq)]
pub struct GoalsArgs {
    /// `--board <slug>`, repeatable: the boards to look on; empty is
    /// `config.toml`'s `[goals] boards`.
    pub boards: Vec<String>,
    /// `--author <npub|hex>`: whose boards, instead of the stored signing
    /// key's account.
    pub author: Option<String>,
    /// `--headway-db <path>`: the headway cache to read, instead of the
    /// `headway` CLI's.
    pub headway_db: Option<PathBuf>,
    pub json: bool,
}

/// One `cashflow rule` subcommand.
#[derive(Debug, PartialEq)]
pub enum RuleCommand {
    /// `rule ls [--by <ref>] [--json]`.
    Ls { by: Option<String>, json: bool },
    /// `rule add <account> [conditions] [placement] [--dry-run] [--json]`.
    Add {
        rule: NewRule,
        place: Place,
        write: Write,
    },
    /// `rule rm <rule> [--dry-run] [--json]`.
    Rm { rule: String, write: Write },
    /// `rule mv <rule> <placement> [--dry-run] [--json]`.
    Mv {
        rule: String,
        place: Place,
        write: Write,
    },
    /// `rule test <record-id> [--json]`.
    Test { record_id: String, json: bool },
    /// `rule import <file.toml> [--dry-run] [--json]`.
    Import { path: PathBuf, write: Write },
}

/// How a rule command that writes runs: `--dry-run` and `--json`.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Write {
    /// `--dry-run`: report what would move, write nothing.
    pub dry_run: bool,
    pub json: bool,
}

/// `rule add`'s rule, as given.
#[derive(Debug, PartialEq, Default)]
pub struct NewRule {
    pub account: String,
    /// `--match <text>`, repeatable: payee substrings, any one.
    pub patterns: Vec<String>,
    /// `--meta <key=value>`, repeatable: metadata substrings, every one.
    pub meta: Vec<MetaArg>,
    /// `--amount <min..max>`.
    pub amount: Option<AmountArg>,
    /// `--source <name>`.
    pub source: Option<String>,
    /// `--layer <name>`.
    pub layer: Option<String>,
}

/// One `--meta key=value`.
#[derive(Debug, PartialEq)]
pub struct MetaArg {
    pub key: String,
    pub needle: String,
}

/// `--amount <min>..<max>[<commodity>]`, either end open.
#[derive(Debug, PartialEq)]
pub struct AmountArg {
    pub min: i64,
    pub max: i64,
    pub commodity: Option<String>,
}

/// Where `rule add` or `rule mv` puts the rule.
#[derive(Debug, PartialEq)]
pub enum Place {
    /// `--first`.
    First,
    /// `--last`, `add`'s default.
    Last,
    /// `--before <rule>`.
    Before(String),
    /// `--after <rule>`.
    After(String),
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

/// `cashflow prices fetch`'s arguments.
#[derive(Debug, PartialEq)]
pub struct PricesArgs {
    /// `--since <date>`: the first day to fetch, else each source resumes
    /// after the newest rate the journal has from it.
    pub since: Option<Date>,
    /// `--source <name>`, repeatable: only these; empty is every source.
    pub sources: Vec<cashflow::prices::Source>,
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
    /// `report projects [<parent>] [--begin] [--end]`.
    Projects {
        parent: Option<String>,
        range: Range,
    },
}

/// The commodity reports convert into when `--in` doesn't say.
pub const DEFAULT_HOME: &str = "CAD";

/// How many accounts `report top` lists when `--limit` doesn't say.
pub const DEFAULT_TOP: usize = 10;

/// How many groups `unknown` lists when `--limit` doesn't say.
pub const DEFAULT_UNKNOWN: usize = 20;

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
        "journal" => journal_command(positionals, &flags, json)?,
        "sync" => no_positionals(
            positionals,
            Command::Sync {
                transport: flags.value("--transport").map(str::to_owned),
                write: write_of(&flags, json),
            },
            "sync",
        )?,
        "import" => import_command(positionals, &flags, json)?,
        "prices" => prices_command(positionals, &flags, json)?,
        "export" => no_positionals(
            positionals,
            Command::Export {
                books: books_args(&flags)?,
                range: range(&flags)?,
            },
            "export",
        )?,
        "report" => report_command(positionals, &flags)?,
        "rule" => rule_command(positionals, &flags, json)?,
        "categorize" => categorize_command(positionals, &flags, json)?,
        "uncategorize" => uncategorize_command(positionals, &flags, json)?,
        "split" => split_command(positionals, &flags, json)?,
        "transfer" => transfer_command(positionals, &flags, json)?,
        "edits" => no_positionals(
            positionals,
            Command::Edits {
                by: flags.value("--by").map(str::to_owned),
                json,
            },
            "edits",
        )?,
        "unknown" => no_positionals(
            positionals,
            Command::Unknown(UnknownArgs {
                source: flags.value("--source").map(str::to_owned),
                limit: flags.number("--limit")?.unwrap_or(DEFAULT_UNKNOWN),
                home: flags.value("--in").unwrap_or(DEFAULT_HOME).to_owned(),
                json,
            }),
            "unknown",
        )?,
        "account" => account_command(positionals, &flags, json)?,
        "goals" => no_positionals(
            positionals,
            Command::Goals(GoalsArgs {
                boards: flags.values("--board").map(str::to_owned).collect(),
                author: flags.value("--author").map(str::to_owned),
                headway_db: flags.value("--headway-db").map(PathBuf::from),
                json,
            }),
            "goals",
        )?,
        "skills" => no_positionals(
            positionals,
            Command::Skills {
                books: books_args(&flags)?,
                json,
            },
            "skills",
        )?,
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
fn journal_command(positionals: Vec<String>, flags: &Flags, json: bool) -> Result<Command, String> {
    let usage = "`journal` takes `ls`, `status`, `verify`, `publish` or `rm <hash>` (see `cashflow journal --help`)";
    let mut positionals = positionals.into_iter();
    let sub = positionals.next();
    if sub.as_deref() != Some("publish") && flags.has("--dry-run") {
        return Err("only `journal publish` takes --dry-run".to_owned());
    }
    let command = match sub.as_deref() {
        Some("ls") => Command::JournalLs { json },
        Some("status") => Command::JournalStatus { json },
        Some("verify") => Command::JournalVerify { json },
        Some("publish") => Command::JournalPublish {
            write: write_of(flags, json),
        },
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

/// `prices fetch [--since <date>] [--source <name>]...`.
fn prices_command(positionals: Vec<String>, flags: &Flags, json: bool) -> Result<Command, String> {
    let usage = "`prices` takes `fetch` (see `cashflow prices --help`)";
    let mut positionals = positionals.into_iter();
    if positionals.next().as_deref() != Some("fetch") || positionals.next().is_some() {
        return Err(usage.to_owned());
    }
    let sources = flags
        .values("--source")
        .map(|name| {
            cashflow::prices::Source::from_name(name).ok_or_else(|| {
                let names: Vec<_> = cashflow::prices::SOURCES.iter().map(|s| s.name()).collect();
                format!("no price source '{name}' (sources: {})", names.join(", "))
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(Command::PricesFetch(PricesArgs {
        since: flags.date("--since")?,
        sources,
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
    let usage = "`report` takes `bal`, `reg`, `top` or `projects` (see `cashflow report --help`)";
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
        "projects" => {
            refuse(&["--depth", "--native", "--payee", "--limit"])?;
            Report::Projects {
                parent: account,
                range: range(flags)?,
            }
        }
        _ => return Err(usage.to_owned()),
    };
    Ok(Command::Report {
        books: books_args(flags)?,
        report,
    })
}

/// `rule`'s subcommand.
fn rule_command(positionals: Vec<String>, flags: &Flags, json: bool) -> Result<Command, String> {
    let usage = "`rule` takes `ls`, `add <account>`, `rm <rule>`, `mv <rule>`, `test <record-id>` or `import <file.toml>` (see `cashflow rule --help`)";
    let mut positionals = positionals.into_iter();
    let sub = positionals.next().ok_or(usage)?;
    let mut operand = || positionals.next().ok_or(usage);
    let refuse = |names: &[&str]| match flags.first_of(names) {
        Some(flag) => Err(format!("`rule {sub}` doesn't take {flag}")),
        None => Ok(()),
    };
    const CONDITIONS: &[&str] = &["--match", "--meta", "--amount", "--source", "--layer"];
    const PLACES: &[&str] = &["--first", "--last", "--before", "--after"];
    let write = Write {
        dry_run: flags.has("--dry-run"),
        json,
    };
    let command = match sub.as_str() {
        "ls" => {
            refuse(CONDITIONS)?;
            refuse(PLACES)?;
            refuse(&["--dry-run"])?;
            RuleCommand::Ls {
                by: flags.value("--by").map(str::to_owned),
                json,
            }
        }
        "add" => {
            refuse(&["--by"])?;
            let account = operand()?;
            RuleCommand::Add {
                rule: new_rule(account, flags)?,
                place: place(flags)?.unwrap_or(Place::Last),
                write,
            }
        }
        "rm" => {
            refuse(CONDITIONS)?;
            refuse(PLACES)?;
            refuse(&["--by"])?;
            RuleCommand::Rm {
                rule: operand()?,
                write,
            }
        }
        "mv" => {
            refuse(CONDITIONS)?;
            refuse(&["--by"])?;
            let rule = operand()?;
            let place = place(flags)?
                .ok_or("`rule mv` needs --first, --last, --before <rule> or --after <rule>")?;
            RuleCommand::Mv { rule, place, write }
        }
        "test" => {
            refuse(CONDITIONS)?;
            refuse(PLACES)?;
            refuse(&["--by", "--dry-run"])?;
            RuleCommand::Test {
                record_id: operand()?,
                json,
            }
        }
        "import" => {
            refuse(CONDITIONS)?;
            refuse(PLACES)?;
            refuse(&["--by"])?;
            RuleCommand::Import {
                path: PathBuf::from(operand()?),
                write,
            }
        }
        _ => return Err(usage.to_owned()),
    };
    if positionals.next().is_some() {
        return Err(usage.to_owned());
    }
    Ok(Command::Rule(command))
}

/// `--dry-run` and `--json`, for a command that writes.
fn write_of(flags: &Flags, json: bool) -> Write {
    Write {
        dry_run: flags.has("--dry-run"),
        json,
    }
}

/// Exactly `N` operands, or `usage`.
fn operands<const N: usize>(positionals: Vec<String>, usage: &str) -> Result<[String; N], String> {
    <[String; N]>::try_from(positionals).map_err(|_| usage.to_owned())
}

/// `categorize <record-id> <account>`.
fn categorize_command(
    positionals: Vec<String>,
    flags: &Flags,
    json: bool,
) -> Result<Command, String> {
    let usage = "`categorize` takes <record-id> <account> (see `cashflow categorize --help`)";
    let [record_id, account] = operands(positionals, usage)?;
    Ok(Command::Categorize {
        record_id,
        account,
        write: write_of(flags, json),
    })
}

/// `uncategorize <record-id>`.
fn uncategorize_command(
    positionals: Vec<String>,
    flags: &Flags,
    json: bool,
) -> Result<Command, String> {
    let usage = "`uncategorize` takes <record-id> (see `cashflow uncategorize --help`)";
    let [record_id] = operands(positionals, usage)?;
    Ok(Command::Uncategorize {
        record_id,
        write: write_of(flags, json),
    })
}

/// `split <record-id> <account>[=<amount>]...`.
fn split_command(positionals: Vec<String>, flags: &Flags, json: bool) -> Result<Command, String> {
    let usage = "`split` takes <record-id> and one or more <account>=<amount> legs, one of which may be a bare <account> taking the rest (see `cashflow split --help`)";
    let mut positionals = positionals.into_iter();
    let record_id = positionals.next().ok_or(usage)?;
    let legs: Vec<LegArg> = positionals
        .map(|leg| match leg.split_once('=') {
            Some((account, amount)) => LegArg {
                account: account.to_owned(),
                amount: Some(amount.to_owned()),
            },
            None => LegArg {
                account: leg,
                amount: None,
            },
        })
        .collect();
    if legs.is_empty() || legs.iter().filter(|l| l.amount.is_none()).count() > 1 {
        return Err(usage.to_owned());
    }
    Ok(Command::Split {
        record_id,
        legs,
        write: write_of(flags, json),
    })
}

/// `transfer link|unlink|suggest`.
fn transfer_command(
    positionals: Vec<String>,
    flags: &Flags,
    json: bool,
) -> Result<Command, String> {
    let usage = "`transfer` takes `link <a> <b>`, `unlink <a> <b>` or `suggest` (see `cashflow transfer --help`)";
    let mut positionals = positionals.into_iter();
    let sub = positionals.next().ok_or(usage)?;
    let rest: Vec<String> = positionals.collect();
    let write = write_of(flags, json);
    if sub != "suggest" && flags.has("--accept-all") {
        return Err(format!("`transfer {sub}` doesn't take --accept-all"));
    }
    let command = match sub.as_str() {
        "link" => {
            let [a, b] = operands(rest, usage)?;
            TransferCommand::Link { a, b, write }
        }
        "unlink" => {
            let [a, b] = operands(rest, usage)?;
            TransferCommand::Unlink { a, b, write }
        }
        "suggest" => {
            let [] = operands(rest, usage)?;
            let accept_all = flags.has("--accept-all");
            if write.dry_run && !accept_all {
                return Err(
                    "`transfer suggest` writes only with --accept-all, so --dry-run needs it"
                        .to_owned(),
                );
            }
            TransferCommand::Suggest { accept_all, write }
        }
        _ => return Err(usage.to_owned()),
    };
    Ok(Command::Transfer(command))
}

/// `account ls|set|alias|unalias`.
fn account_command(positionals: Vec<String>, flags: &Flags, json: bool) -> Result<Command, String> {
    let usage = "`account` takes `ls [<account>]`, `set <account>`, `alias <from> <to>` or `unalias <from>` (see `cashflow account --help`)";
    let mut positionals = positionals.into_iter();
    let sub = positionals.next().ok_or(usage)?;
    let rest: Vec<String> = positionals.collect();
    let refuse = |names: &[&str]| match flags.first_of(names) {
        Some(flag) => Err(format!("`account {sub}` doesn't take {flag}")),
        None => Ok(()),
    };
    const BOOKS: [&str; 5] = ["--layer", "--except", "--rules", "--in", "--depth"];
    const LS: [&str; 1] = ["--all"];
    const SET: [&str; 8] = [
        "--name",
        "--icon",
        "--color",
        "--project",
        "--type",
        "--hide",
        "--show",
        "--clear",
    ];
    let write = write_of(flags, json);
    let command = match sub.as_str() {
        "ls" => {
            refuse(&SET)?;
            refuse(&["--dry-run"])?;
            if rest.len() > 1 {
                return Err(usage.to_owned());
            }
            AccountCommand::Ls {
                books: books_args(flags)?,
                account: rest.into_iter().next(),
                depth: flags.number("--depth")?,
                all: flags.has("--all"),
                json,
            }
        }
        "set" => {
            refuse(&BOOKS)?;
            refuse(&LS)?;
            let [account] = operands(rest, usage)?;
            AccountCommand::Set {
                account,
                set: meta_args(flags)?,
                write,
            }
        }
        "alias" => {
            refuse(&BOOKS)?;
            refuse(&LS)?;
            refuse(&SET)?;
            let [from, to] = operands(rest, usage)?;
            AccountCommand::Alias { from, to, write }
        }
        "unalias" => {
            refuse(&BOOKS)?;
            refuse(&LS)?;
            refuse(&SET)?;
            let [from] = operands(rest, usage)?;
            AccountCommand::Unalias { from, write }
        }
        _ => return Err(usage.to_owned()),
    };
    Ok(Command::Account(command))
}

/// `account set`'s field flags.
fn meta_args(flags: &Flags) -> Result<MetaArgs, String> {
    let text = |name: &str| flags.value(name).map(str::to_owned);
    let hidden = match (flags.has("--hide"), flags.has("--show")) {
        (true, true) => return Err("--hide and --show can't be used together".to_owned()),
        (true, false) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
    };
    let kinds = || {
        let names: Vec<&str> = AccountKind::ALL.iter().map(|k| k.as_str()).collect();
        names.join(", ")
    };
    let kind = flags
        .value("--type")
        .map(|text| {
            AccountKind::parse(text).ok_or_else(|| format!("--type {text}: not one of {}", kinds()))
        })
        .transpose()?;
    let clear = flags
        .values("--clear")
        .map(|text| {
            MetaField::parse(text).ok_or_else(|| {
                let names: Vec<&str> = MetaField::ALL.iter().map(|f| f.as_str()).collect();
                format!("--clear {text}: not one of {}", names.join(", "))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let set = MetaArgs {
        name: text("--name"),
        icon: text("--icon"),
        color: text("--color"),
        project: text("--project"),
        hidden,
        kind,
        clear,
    };
    if set == MetaArgs::default() {
        return Err(
            "`account set` needs a field to set or --clear (see `cashflow account --help`)"
                .to_owned(),
        );
    }
    Ok(set)
}

/// `rule add`'s conditions.
fn new_rule(account: String, flags: &Flags) -> Result<NewRule, String> {
    let meta = flags
        .values("--meta")
        .map(|pair| match pair.split_once('=') {
            Some((key, needle)) if !key.is_empty() => Ok(MetaArg {
                key: key.to_owned(),
                needle: needle.to_owned(),
            }),
            _ => Err(format!("--meta wants key=value, got '{pair}'")),
        })
        .collect::<Result<_, _>>()?;
    Ok(NewRule {
        account,
        patterns: flags.values("--match").map(str::to_owned).collect(),
        meta,
        amount: flags.value("--amount").map(amount_arg).transpose()?,
        source: flags.value("--source").map(str::to_owned),
        layer: flags.value("--layer").map(str::to_owned),
    })
}

/// `<min>..<max>[<commodity>]`: `100..2000`, `1000..1000msat`, `..500`,
/// `5000..`.
fn amount_arg(text: &str) -> Result<AmountArg, String> {
    let bad = || format!("--amount wants <min>..<max>[<commodity>], like 100..2000, got '{text}'");
    let (min, rest) = text.split_once("..").ok_or_else(bad)?;
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (max, commodity) = rest.split_at(digits);
    let number = |text: &str, open: i64| match text {
        "" => Ok(open),
        text => text.parse().map_err(|_| bad()),
    };
    Ok(AmountArg {
        min: number(min, 0)?,
        max: number(max, i64::MAX)?,
        commodity: (!commodity.is_empty()).then(|| commodity.to_owned()),
    })
}

/// The one placement flag given, if any.
fn place(flags: &Flags) -> Result<Option<Place>, String> {
    let mut given = Vec::new();
    if flags.has("--first") {
        given.push(Place::First);
    }
    if flags.has("--last") {
        given.push(Place::Last);
    }
    if let Some(rule) = flags.value("--before") {
        given.push(Place::Before(rule.to_owned()));
    }
    if let Some(rule) = flags.value("--after") {
        given.push(Place::After(rule.to_owned()));
    }
    if given.len() > 1 {
        return Err("give one of --first, --last, --before and --after".to_owned());
    }
    Ok(given.pop())
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
        assert_eq!(
            run_of(&["journal", "status"]).command,
            Command::JournalStatus { json: false }
        );
        assert_eq!(
            run_of(&["journal", "publish", "--dry-run", "--json"]).command,
            Command::JournalPublish {
                write: Write {
                    dry_run: true,
                    json: true
                }
            }
        );
        assert!(err_of(&["journal", "ls", "--dry-run"]).contains("only `journal publish`"));
        assert_eq!(
            run_of(&["sync", "--transport", "usb", "--dry-run"]).command,
            Command::Sync {
                transport: Some("usb".to_owned()),
                write: Write {
                    dry_run: true,
                    json: false,
                },
            }
        );
        assert_eq!(
            run_of(&["sync", "--json"]).command,
            Command::Sync {
                transport: None,
                write: Write {
                    dry_run: false,
                    json: true,
                },
            }
        );
        assert!(err_of(&["sync", "now"]).contains("takes no arguments"));
        let usage = "`ls`, `status`, `verify`, `publish` or `rm <hash>`";
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
        assert!(
            err_of(&["import"])
                .contains("needs an importer: amex, appstore, btc, cln, ledger, payroll, rbc")
        );
        assert!(err_of(&["import", "ledger", "--source"]).contains("--source needs a value"));
    }

    #[test]
    fn prices_fetch() {
        use cashflow::prices::Source;
        assert_eq!(
            run_of(&["prices", "fetch"]).command,
            Command::PricesFetch(PricesArgs {
                since: None,
                sources: vec![],
                json: false,
            })
        );
        assert_eq!(
            run_of(&[
                "prices",
                "fetch",
                "--since",
                "2016-01-01",
                "--source",
                "boc",
                "--source",
                "bitstamp",
                "--json"
            ])
            .command,
            Command::PricesFetch(PricesArgs {
                since: date("2016-01-01"),
                sources: vec![Source::Boc, Source::Bitstamp],
                json: true,
            })
        );
        let usage = "`prices` takes `fetch`";
        assert!(err_of(&["prices"]).contains(usage));
        assert!(err_of(&["prices", "fetch", "x"]).contains(usage));
        assert!(err_of(&["prices", "fetch", "--source", "x"]).contains("no price source 'x'"));
        assert!(err_of(&["prices", "fetch", "--since", "soon"]).contains("--since soon"));
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

        assert!(err_of(&["report"]).contains("`bal`, `reg`, `top` or `projects`"));
        assert!(err_of(&["report", "bal", "--payee", "x"]).contains("doesn't take --payee"));
        assert!(err_of(&["report", "top", "--limit", "many"]).contains("needs a number"));
        assert!(err_of(&["report", "reg", "a", "b"]).contains("at most one"));
    }

    #[test]
    fn rule_subcommands() {
        let write = |dry_run, json| Write { dry_run, json };
        assert_eq!(
            run_of(&["rule", "ls", "--by", "agentium:a-b-c", "--json"]).command,
            Command::Rule(RuleCommand::Ls {
                by: Some("agentium:a-b-c".to_owned()),
                json: true
            })
        );
        assert_eq!(
            run_of(&[
                "rule",
                "add",
                "expenses:food",
                "--match",
                "grocer",
                "--match",
                "market",
                "--meta",
                "raw-city=montreal",
                "--amount",
                "100..2000msat",
                "--source",
                "amex",
                "--layer",
                "cards",
                "--before",
                "oak",
                "--dry-run",
            ])
            .command,
            Command::Rule(RuleCommand::Add {
                rule: NewRule {
                    account: "expenses:food".to_owned(),
                    patterns: vec!["grocer".to_owned(), "market".to_owned()],
                    meta: vec![MetaArg {
                        key: "raw-city".to_owned(),
                        needle: "montreal".to_owned()
                    }],
                    amount: Some(AmountArg {
                        min: 100,
                        max: 2000,
                        commodity: Some("msat".to_owned())
                    }),
                    source: Some("amex".to_owned()),
                    layer: Some("cards".to_owned()),
                },
                place: Place::Before("oak".to_owned()),
                write: write(true, false),
            })
        );
        let Command::Rule(RuleCommand::Add { rule, place, .. }) =
            run_of(&["rule", "add", "a", "--amount", "5000.."]).command
        else {
            panic!("not an add");
        };
        assert_eq!(place, Place::Last);
        assert_eq!(
            rule.amount,
            Some(AmountArg {
                min: 5000,
                max: i64::MAX,
                commodity: None
            })
        );
        assert_eq!(
            run_of(&["rule", "mv", "oak", "--first", "--json"]).command,
            Command::Rule(RuleCommand::Mv {
                rule: "oak".to_owned(),
                place: Place::First,
                write: write(false, true),
            })
        );
        assert_eq!(
            run_of(&["rule", "rm", "oak"]).command,
            Command::Rule(RuleCommand::Rm {
                rule: "oak".to_owned(),
                write: write(false, false),
            })
        );
        assert_eq!(
            run_of(&["rule", "test", "amex:1"]).command,
            Command::Rule(RuleCommand::Test {
                record_id: "amex:1".to_owned(),
                json: false
            })
        );
        assert_eq!(
            run_of(&["rule", "import", "amex.toml", "--dry-run"]).command,
            Command::Rule(RuleCommand::Import {
                path: PathBuf::from("amex.toml"),
                write: write(true, false),
            })
        );

        assert!(err_of(&["rule"]).contains("`ls`, `add <account>`"));
        assert!(err_of(&["rule", "add"]).contains("`ls`, `add <account>`"));
        assert!(err_of(&["rule", "mv", "oak"]).contains("needs --first"));
        assert!(err_of(&["rule", "mv", "oak", "--first", "--last"]).contains("one of"));
        assert!(err_of(&["rule", "ls", "--match", "x"]).contains("doesn't take --match"));
        assert!(err_of(&["rule", "test", "x", "--dry-run"]).contains("doesn't take --dry-run"));
        assert!(err_of(&["rule", "add", "a", "--meta", "nokey"]).contains("key=value"));
        assert!(err_of(&["rule", "add", "a", "--amount", "lots"]).contains("--amount"));
        assert!(err_of(&["rule", "rm", "a", "b"]).contains("`ls`, `add <account>`"));
    }

    #[test]
    fn account_subcommands() {
        let write = |dry_run, json| Write { dry_run, json };
        assert_eq!(
            run_of(&[
                "account", "ls", "income", "--depth", "2", "--all", "--in", "USD"
            ])
            .command,
            Command::Account(AccountCommand::Ls {
                books: BooksArgs {
                    filter: LayerFilter::All,
                    rules: None,
                    home: "USD".to_owned(),
                },
                account: Some("income".to_owned()),
                depth: Some(2),
                all: true,
                json: false,
            })
        );
        assert_eq!(
            run_of(&[
                "account",
                "set",
                "income:a",
                "--project",
                "a",
                "--type",
                "income",
                "--show",
                "--clear",
                "icon",
                "--clear",
                "name",
                "--dry-run",
            ])
            .command,
            Command::Account(AccountCommand::Set {
                account: "income:a".to_owned(),
                set: MetaArgs {
                    project: Some("a".to_owned()),
                    kind: Some(AccountKind::Income),
                    hidden: Some(false),
                    clear: vec![MetaField::Icon, MetaField::Name],
                    ..MetaArgs::default()
                },
                write: write(true, false),
            })
        );
        assert_eq!(
            run_of(&["account", "alias", "a:b", "a:c", "--json"]).command,
            Command::Account(AccountCommand::Alias {
                from: "a:b".to_owned(),
                to: "a:c".to_owned(),
                write: write(false, true),
            })
        );
        assert_eq!(
            run_of(&["account", "unalias", "a:b"]).command,
            Command::Account(AccountCommand::Unalias {
                from: "a:b".to_owned(),
                write: write(false, false),
            })
        );
        assert_eq!(
            run_of(&["report", "projects", "income:x", "--begin", "2024-01-01"]).command,
            Command::Report {
                books: BooksArgs {
                    filter: LayerFilter::All,
                    rules: None,
                    home: DEFAULT_HOME.to_owned(),
                },
                report: Report::Projects {
                    parent: Some("income:x".to_owned()),
                    range: Range {
                        begin: date("2024-01-01"),
                        end: None,
                    },
                },
            }
        );
        assert!(err_of(&["account", "set", "a"]).contains("needs a field"));
        assert!(err_of(&["account", "set", "a", "--hide", "--show"]).contains("together"));
        assert!(err_of(&["account", "set", "a", "--type", "cash"]).contains("not one of"));
        assert!(err_of(&["account", "set", "a", "--clear", "x"]).contains("not one of"));
        assert!(err_of(&["account", "set", "a", "--all"]).contains("doesn't take --all"));
        assert!(err_of(&["account", "ls", "--project", "p"]).contains("doesn't take"));
        assert!(err_of(&["account", "alias", "a"]).contains("`account` takes"));
        assert!(err_of(&["report", "projects", "--limit", "3"]).contains("doesn't take"));
    }

    #[test]
    fn edit_subcommands() {
        let write = |dry_run, json| Write { dry_run, json };
        assert_eq!(
            run_of(&["categorize", "a:1", "expenses:food", "--dry-run"]).command,
            Command::Categorize {
                record_id: "a:1".to_owned(),
                account: "expenses:food".to_owned(),
                write: write(true, false),
            }
        );
        assert_eq!(
            run_of(&["uncategorize", "a:1", "--json"]).command,
            Command::Uncategorize {
                record_id: "a:1".to_owned(),
                write: write(false, true),
            }
        );
        assert_eq!(
            run_of(&["split", "a:1", "x:y=12.50", "x:z"]).command,
            Command::Split {
                record_id: "a:1".to_owned(),
                legs: vec![
                    LegArg {
                        account: "x:y".to_owned(),
                        amount: Some("12.50".to_owned()),
                    },
                    LegArg {
                        account: "x:z".to_owned(),
                        amount: None,
                    },
                ],
                write: write(false, false),
            }
        );
        assert!(err_of(&["split", "a:1"]).contains("one or more"));
        assert!(err_of(&["split", "a:1", "x", "y"]).contains("bare <account>"));
        assert!(err_of(&["categorize", "a:1"]).contains("<record-id> <account>"));

        assert_eq!(
            run_of(&["transfer", "link", "a:1", "b:2"]).command,
            Command::Transfer(TransferCommand::Link {
                a: "a:1".to_owned(),
                b: "b:2".to_owned(),
                write: write(false, false),
            })
        );
        assert_eq!(
            run_of(&["transfer", "suggest", "--accept-all", "--dry-run"]).command,
            Command::Transfer(TransferCommand::Suggest {
                accept_all: true,
                write: write(true, false),
            })
        );
        assert!(err_of(&["transfer", "suggest", "--dry-run"]).contains("--accept-all"));
        assert!(
            err_of(&["transfer", "link", "a:1", "b:2", "--accept-all"]).contains("doesn't take")
        );
        assert!(err_of(&["transfer", "link", "a:1"]).contains("`transfer` takes"));

        assert_eq!(
            run_of(&["edits", "--by", "agentium:a-b-c"]).command,
            Command::Edits {
                by: Some("agentium:a-b-c".to_owned()),
                json: false,
            }
        );
        assert_eq!(
            run_of(&["unknown", "--source", "amex", "--limit", "3", "--json"]).command,
            Command::Unknown(UnknownArgs {
                source: Some("amex".to_owned()),
                limit: 3,
                home: DEFAULT_HOME.to_owned(),
                json: true,
            })
        );
        assert!(err_of(&["unknown", "--limit", "x"]).contains("needs a number"));

        assert_eq!(
            run_of(&["goals", "--board", "a", "--board", "b", "--json"]).command,
            Command::Goals(GoalsArgs {
                boards: vec!["a".to_owned(), "b".to_owned()],
                author: None,
                headway_db: None,
                json: true,
            })
        );
        assert_eq!(
            run_of(&["goals", "--author", "npub1x", "--headway-db", "/tmp/hw"]).command,
            Command::Goals(GoalsArgs {
                boards: Vec::new(),
                author: Some("npub1x".to_owned()),
                headway_db: Some(PathBuf::from("/tmp/hw")),
                json: false,
            })
        );
        assert!(err_of(&["goals", "extra"]).contains("goals"));
        assert_eq!(
            run_of(&["skills", "--layer", "cards", "--json"]).command,
            Command::Skills {
                books: BooksArgs {
                    filter: LayerFilter::Only(vec!["cards".to_owned()]),
                    rules: None,
                    home: DEFAULT_HOME.to_owned(),
                },
                json: true,
            }
        );
        assert!(err_of(&["skills", "extra"]).contains("skills"));
    }

    #[test]
    fn unknown_names_and_flags_are_rejected() {
        assert!(err_of(&["frob"]).contains("unknown command 'frob'"));
        assert!(err_of(&["--frob", "config"]).contains("unknown flag '--frob'"));
        assert!(err_of(&["config", "show", "--frob"]).contains("doesn't take --frob"));
    }
}
