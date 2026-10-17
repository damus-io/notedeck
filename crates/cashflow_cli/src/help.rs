//! `cashflow`'s help table, laid out by [`cli_term::help`] so its pages read
//! like `headway --help` and `nd --help`.
//!
//! [`CLI`] is also the list of command names: parsing looks a name up here
//! before dispatching, and holds the run to the flags the command's page lists,
//! so a command or flag can't ship undocumented.

use cli_term::help::{Cli, CommandHelp, GlobalOption};

const SETUP: &str = "SETUP";
const VAULT: &str = "VAULT";

/// The whole help surface.
pub const CLI: Cli = Cli {
    bin: "cashflow",
    about: "import, inspect and export a personal finance ledger",
    groups: &[SETUP, VAULT],
    commands: COMMANDS,
    global_hint: "--dir",
};

/// The options every run takes.
pub const GLOBALS: &[GlobalOption] = &[
    GlobalOption {
        flag: "--dir <path>",
        summary: "Data dir for this run [default: $CASHFLOW_DIR]",
        details: "The data directory for this run: config.toml, journal/, staging/ and secrets/ live under it. Overrides $CASHFLOW_DIR, which overrides the default <data-local-dir>/cashflow (~/.local/share/cashflow on Linux). The notedeck app reads the same directory",
    },
    GlobalOption {
        flag: "--relay <url>",
        summary: "Relay the vault syncs with [default: ws://127.0.0.1:6677]",
        details: "The relay the vault commands reconcile with before reading and publish to after writing. Overrides $CASHFLOW_RELAY, which overrides notedeck's embedded relay at ws://127.0.0.1:6677. Only sealed kind-1081 envelopes move; an unreachable relay leaves the command working offline against the local cache. Not persisted",
    },
    GlobalOption {
        flag: "--db <path>",
        summary: "The CLI's nostrdb cache [default: <data-dir>/cashflow-cli]",
        details: "Where the CLI keeps its own nostrdb, the local cache of the vault (~/.local/share/cashflow-cli on Linux). Not persisted",
    },
];

/// What `cashflow help options` prints after the flags: where secrets come
/// from, which holds for every command.
pub const OPTIONS_FOOTER: &str = "\
SECRETS:
    A secret <name> is read from $CASHFLOW_<NAME> (upper-cased, - and . as _),
    else from the file secrets/<name> under the data dir. Secrets never go in
    config.toml, and no command prints one. The vault's signing key is the
    secret `nsec` ($CASHFLOW_NSEC), which `cashflow login` stores.
";

const COMMANDS: &[CommandHelp] = &[
    CommandHelp {
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
    },
    CommandHelp {
        name: "login",
        group: SETUP,
        summary: "Store the nostr key that signs and opens the vault",
        usage: &["login [<nsec>]"],
        details: "\
Store <nsec> as the secret `nsec`: the file secrets/nsec under the data dir,
owner-only. With no <nsec> it is read from stdin, which keeps it out of shell
history. $CASHFLOW_NSEC, when set, still wins over the file.

The key signs every vault record and derives the vault itself, so every
machine logged in with it sees the same vault.",
        options: &[],
        examples: &["cashflow login < ~/keys/me.nsec", "cashflow login nsec1..."],
    },
    CommandHelp {
        name: "logout",
        group: SETUP,
        summary: "Forget the stored nostr key",
        usage: &["logout"],
        details: "\
Delete secrets/nsec. The local vault cache is kept, and a set $CASHFLOW_NSEC is
untouched.",
        options: &[],
        examples: &["cashflow logout"],
    },
    CommandHelp {
        name: "settings",
        group: VAULT,
        summary: "Show or change the settings every machine shares",
        usage: &[
            "settings [show] [--json]",
            "settings get <name>",
            "settings set <name> <value>",
        ],
        details: "\
Settings live in the vault, so they follow the key across machines; config.toml
is the per-machine file. Each run reconciles with the relay first, so a
setting changed elsewhere shows up here.

  home    the commodity reports convert into [default: CAD]

`show` lists each setting and its value, marking defaults. `get` prints one
value alone, for scripts. `set` writes it and publishes it to the relay.",
        options: &[("--json", "Machine-readable output")],
        examples: &[
            "cashflow settings",
            "cashflow settings set home CAD",
            "cashflow settings get home",
        ],
    },
];

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
        assert!(overview.contains("--relay"));
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
