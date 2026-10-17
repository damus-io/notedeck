//! `cashflow`'s help table, laid out by [`cli_term::help`] so its pages read
//! like `headway --help` and `nd --help`.
//!
//! [`CLI`] is also the list of command names: parsing looks a name up here
//! before dispatching, and holds the run to the flags the command's page lists,
//! so a command or flag can't ship undocumented.

use cli_term::help::{Cli, CommandHelp, GlobalOption};

const SETUP: &str = "SETUP";

/// The whole help surface.
pub const CLI: Cli = Cli {
    bin: "cashflow",
    about: "import, inspect and export a personal finance ledger",
    groups: &[SETUP],
    commands: COMMANDS,
    global_hint: "--dir",
};

/// The options every run takes.
pub const GLOBALS: &[GlobalOption] = &[GlobalOption {
    flag: "--dir <path>",
    summary: "Data dir for this run [default: $CASHFLOW_DIR]",
    details: "The data directory for this run: config.toml, journal/, staging/ and secrets/ live under it. Overrides $CASHFLOW_DIR, which overrides the default <data-local-dir>/cashflow (~/.local/share/cashflow on Linux). The notedeck app reads the same directory",
}];

/// What `cashflow help options` prints after the flags: where secrets come
/// from, which holds for every command.
pub const OPTIONS_FOOTER: &str = "\
SECRETS:
    A secret <name> is read from $CASHFLOW_<NAME> (upper-cased, - and . as _),
    else from the file secrets/<name> under the data dir. Secrets never go in
    config.toml, and no command prints one.
";

const COMMANDS: &[CommandHelp] = &[CommandHelp {
    name: "config",
    group: SETUP,
    summary: "Print the data dir's paths, or the effective config",
    usage: &["config path [<name>] [--json]", "config show [--json]"],
    details: "\
`config path` prints the data dir's layout, one name and path per line:
dir, config, journal, staging and secrets. With a <name>, just that path, so
a script can `cd \"$(cashflow config path journal)\"`. Nothing is created.

`config show` prints the effective config.toml (a missing file is all
defaults, an unknown key an error) and the secrets this run can see, by name
and source. A secret's value is never printed.",
    options: &[("--json", "Machine-readable output")],
    examples: &[
        "cashflow config path",
        "cashflow config path journal",
        "cashflow config show",
        "CASHFLOW_DIR=/tmp/cf cashflow config show --json",
    ],
}];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_holds_its_invariants() {
        CLI.check().unwrap();
    }

    #[test]
    fn every_command_is_on_the_overview() {
        let overview = CLI.overview(GLOBALS);
        for cmd in COMMANDS {
            assert!(overview.contains(cmd.name), "{}", cmd.name);
        }
        assert!(overview.contains("--dir"));
    }

    /// `cashflow --help` stays within 80 columns.
    #[test]
    fn overview_fits_80_columns() {
        let overview = CLI.overview(GLOBALS);
        for line in overview.lines() {
            let width = line.chars().count();
            assert!(width <= 80, "{width} columns: {line:?}");
        }
    }
}
