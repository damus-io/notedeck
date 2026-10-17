//! `cashflow`'s help table, laid out by [`cli_term::help`] so its pages read
//! like `headway --help` and `nd --help`.
//!
//! [`CLI`] is also the list of command names: parsing looks a name up here
//! before dispatching, and holds the run to the flags the command's page lists,
//! so a command or flag can't ship undocumented.

use cli_term::help::{Cli, CommandHelp, GlobalOption};

const SETUP: &str = "SETUP";
const VAULT: &str = "VAULT";
const JOURNAL: &str = "JOURNAL";
const REPORTS: &str = "REPORTS";

/// The layer, rules and home-commodity options every reading command takes.
macro_rules! books_options {
    ($($extra:expr),* $(,)?) => {
        &[
            $($extra,)*
            ("--layer <name>", "Only segments in this layer; repeat for more"),
            ("--except <name>", "Every layer but this one; repeat for more"),
            ("--rules <toml>", "Categorize with this local rules file"),
        ]
    };
}

/// The whole help surface.
pub const CLI: Cli = Cli {
    bin: "cashflow",
    about: "import, inspect and export a personal finance ledger",
    groups: &[SETUP, JOURNAL, REPORTS, VAULT],
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

/// How `--rules` files read, shared by the pages that take one (a macro so
/// `concat!` can append it to a page's details).
macro_rules! rules_details {
    () => {
        "\
A rules file is TOML, a superset of amex2ledger's amex.toml. Each [[rule]]
moves a transaction's counter leg (its posting to a default account such as
expenses:unknown) to `account`; the first matching rule wins:

  defaults = [\"expenses:unknown\", \"income:unknown\"]  # counter legs
  [[rule]]
  account = \"expenses:food\"
  match = [\"grocer\", \"market\"]      # any one, payee substring
  meta = { raw-city = \"montreal\" }    # all, metadata substring
  amount = { min = 100, max = 5000 }  # smallest units, unsigned
  source = \"amex\"                     # segment source
  layer = \"cards\"                     # segment layer

Matching ignores case."
    };
}

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
        name: "journal",
        group: JOURNAL,
        summary: "List, check or retire the journal's segments",
        usage: &[
            "journal ls [--json]",
            "journal verify [--json]",
            "journal rm <hash> [--json]",
        ],
        details: "\
The journal is journal/ under the data dir: one write-once segment per import,
named by the sha256 of its bytes, the synced truth everything else is derived
from.

`ls` prints each segment, oldest import first: its hash, import date (UTC),
layer, source, and how many transactions and prices it holds.

`verify` reads every segment, checking its bytes still hash to its name and
that it parses, and prints each one that doesn't. It exits non-zero when any
fails.

`rm` retires one segment, named by its hash or a prefix of it unique in `ls`,
by moving the file to journal/.trash/; its records drop out of every later
export and report. Use it to retire a legacy `import ledger` segment once a
real importer covers the same source. A delete does not sync: sync is a
union of files, so the segment comes back from any other copy of the journal
that still has it. Remove it there too, or delete it before it is synced.",
        options: &[("--json", "Machine-readable output")],
        examples: &[
            "cashflow journal ls",
            "cashflow journal verify",
            "cashflow journal rm 3fa9c2",
        ],
    },
    CommandHelp {
        name: "import",
        group: JOURNAL,
        summary: "Read a source into the journal as one new segment",
        usage: &["import <importer> [<path>...] [--source <name>] [--layer <name>] [--json]"],
        details: "\
Run <importer> over its input and write what it read to the journal as one
segment. Importing the same input again writes nothing: the segment is named
by its bytes and is already there. A record imported again by a later run
replaces the earlier one, keyed by its `; id:`.

Importers:

  amex <path>...  Amex activity CSV exports; a directory reads its *.csv
                  files in name order. Each row posts to the card
                  (liabilities:amex) and expenses:unknown, with its
                  merchant, city and description as raw-* metadata. Its id
                  is amex:<date>:<cents>:<merchant>:<n>, n counting the
                  identical rows in one export, so overlapping exports
                  keep each charge once. [amex] in config.toml sets
                  `card`, `currency` (CAD) and `title_case` (payees in
                  Title Case).
  btc [<file>...] on-chain bitcoin wallets, from bitcoind's JSON-RPC: every
                  wallet `listwallets` names (or [btc] wallets), each
                  read whole. Amounts are exact msat in assets:btc:<wallet>,
                  layer btc-onchain. A txid any wallet sent is one
                  transaction, btc:<txid>: an output another read wallet
                  received is a transfer between them, any other goes to
                  expenses:unknown, and the fee to expenses:fees:btc. A
                  payment in or a mined coin is btc:<txid>:<vout> against
                  income:unknown. A transaction not yet in a block is left
                  out until it is. [btc] in config.toml sets `url`
                  (http://127.0.0.1:8332), `user` (whose password is the
                  bitcoind-rpc-password secret) or `cookie`
                  (~/.bitcoin/.cookie), and `external_wallets` (watch-only
                  wallets read too). Given files, it reads the old
                  btc-txs-raw TSV listings instead.
  ledger <file>   a ledger-cli file, such as books kept by hand or a script.
                  A transaction with no `; id:` is given a legacy one,
                  <source>:<hash of its text>, plus :2, :3, … for identical
                  transactions in the file. A legacy id changes when its
                  transaction is edited, so edits keyed on it don't survive
                  that. Lines that don't parse are reported with their line
                  numbers and their transaction is left out; the rest is
                  imported.
  rbc <path>...   RBC online banking \"Download Transactions\" CSVs, any
                  mix of chequing, savings and card accounts; a directory
                  reads its *.csv files in name order. [rbc.accounts] in
                  config.toml maps each account's last four digits to its
                  ledger account (\"1234\" = \"assets:rbc:chequing\"); an
                  unmapped account is an error. Each row posts to that
                  account and expenses:unknown or income:unknown by sign,
                  in USD for a USD$ amount. Its id is
                  rbc:<last4>:<date>:<cents>:<description>:<n>, so
                  overlapping downloads keep each row once.

--source names the segment's source (default: the importer's name), the
prefix of the record ids it makes. --layer files it in a layer (default: the
importer's), which `export` and `report` can include or leave out.",
        options: &[
            (
                "--source <name>",
                "The segment's source [default: the importer]",
            ),
            (
                "--layer <name>",
                "The segment's layer [default: the importer's]",
            ),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "cashflow import amex ~/Downloads/activity.csv",
            "cashflow import btc",
            "cashflow import ledger books.ledger",
            "cashflow import rbc ~/Downloads/csv*.csv",
            "cashflow import ledger old.ledger --source bank --layer cards",
        ],
    },
    CommandHelp {
        name: "prices",
        group: JOURNAL,
        summary: "Fetch daily exchange rates into the journal",
        usage: &["prices fetch [--since <date>] [--source <name>]... [--json]"],
        details: "\
Fetch daily rates from public sources (no keys) and write them to the journal
as `P` directives in the `prices` layer, one segment per source and year:

  units      msat → sat → btc, exact, dated 1970-01-01
  boc        Bank of Canada USD/CAD, business days (before 2017-01-03, its
             legacy noon rate)
  bitstamp   Bitstamp BTC/USD daily close, finished days only

Together they price msat in CAD, chaining msat → sat → btc → USD → CAD at
each transaction's date, the nearest earlier rate covering weekends.

Without --since each source resumes the day after its newest rate in the
journal, or from the oldest transaction when it has none. A segment is
stamped with the day after its last rate rather than the clock, so fetching
a range again writes nothing, and a later fetch that revises a rate
replaces it. A source that fails is reported and the others still run.",
        options: &[
            ("--since <date>", "First day to fetch [default: resume]"),
            (
                "--source <name>",
                "Only this source; repeat for more [default: all]",
            ),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "cashflow prices fetch",
            "cashflow prices fetch --since 2016-01-01",
            "cashflow prices fetch --source boc",
        ],
    },
    CommandHelp {
        name: "export",
        group: JOURNAL,
        summary: "Print the categorized ledger as ledger-cli text",
        usage: &[
            "export [--layer <name>]... [--except <name>]... [--rules <toml>]",
            "export [--begin <date>] [--end <date>] [...]",
        ],
        details: concat!(
            "\
Load the journal, categorize it with --rules (none: as imported), and print
it to stdout in ledgerdb's canonical ledger-cli form, readable by ledger-cli
and by `cashflow import ledger`.

With --begin or --end only the transactions in the range are printed
(--begin inclusive, --end exclusive, both YYYY-MM-DD), after every price
dated before the end so the range still converts.

Segments that don't load and rules that don't apply are reported on stderr.

",
            rules_details!()
        ),
        options: books_options![
            ("--begin <date>", "First day to print"),
            ("--end <date>", "Day after the last to print"),
        ],
        examples: &[
            "cashflow export > all.ledger",
            "cashflow export --except btc --rules amex.toml --begin 2025-01-01",
        ],
    },
    CommandHelp {
        name: "report",
        group: REPORTS,
        summary: "Balances, a register or the top accounts, for a quick look",
        usage: &[
            "report bal [<account>] [--depth <n>] [--end <date>] [--native]",
            "report reg [<account>] [--begin/--end <date>] [--payee <text>]",
            "report top [<parent>] [--depth <n>] [--limit <n>] [--begin/--end <date>]",
        ],
        details: "\
Quick terminal checks over the categorized ledger, in --in's commodity
[default: CAD], each posting converted at its own date. A posting with no
rate is left out of a total and counted on stderr.

`bal` prints each account's balance at the end of the day before --end
(default: all time), from <account> (default: every top-level account) down
to --depth (default: one level below it). --native prints each commodity as
held instead of converting.

`reg` prints each posting under <account> (default: all), in date order and
in its own commodity, with --payee keeping transactions whose payee contains
the text (ignoring case).

`top` ranks the accounts at --depth (default: one below <parent>, which
defaults to expenses) by their total over the range, with each one's share.

Dates are YYYY-MM-DD; --begin is inclusive, --end exclusive. --layer,
--except and --rules choose the books as for `export`.",
        options: books_options![
            (
                "--in <commodity>",
                "Convert into this commodity [default: CAD]"
            ),
            ("--depth <n>", "Account depth to list (bal, top)"),
            ("--begin <date>", "First day (reg, top)"),
            ("--end <date>", "Day after the last"),
            ("--native", "Each commodity as held (bal)"),
            ("--payee <text>", "Payee contains this (reg)"),
            ("--limit <n>", "How many accounts [default: 10] (top)"),
        ],
        examples: &[
            "cashflow report bal",
            "cashflow report bal assets --native",
            "cashflow report reg expenses:food --begin 2025-01-01",
            "cashflow report top expenses --except btc --limit 5",
        ],
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
