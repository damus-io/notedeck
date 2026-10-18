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
const CATEGORIZE: &str = "CATEGORIZE";

/// The layer, rules and home-commodity options every reading command takes.
macro_rules! books_options {
    ($($extra:expr),* $(,)?) => {
        &[
            $($extra,)*
            ("--layer <name>", "Only segments in this layer; repeat for more"),
            ("--except <name>", "Every layer but this one; repeat for more"),
            ("--rules <toml>", "Categorize with this file alone, not the vault"),
        ]
    };
}

/// The whole help surface.
pub const CLI: Cli = Cli {
    bin: "cashflow",
    about: "import, inspect and export a personal finance ledger",
    groups: &[SETUP, JOURNAL, REPORTS, CATEGORIZE, VAULT],
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
        summary: "List, check, publish or retire the journal's segments",
        usage: &[
            "journal ls [--json]",
            "journal status [--json]",
            "journal verify [--json]",
            "journal publish [--dry-run] [--json]",
            "journal rm <hash> [--json]",
        ],
        details: "\
The journal is journal/ under the data dir: one write-once segment per import,
named by the sha256 of its bytes, the synced truth everything else is derived
from. The vault keeps a manifest of each segment (its source, layer, dates,
size and the host that has it), so every machine knows which segments exist
even before their files arrive.

`ls` prints each segment, oldest import first: its hash, import date (UTC),
layer, source, and how many transactions and prices it holds.

`status` compares the journal with the vault's manifests: the segments this
machine is missing, grouped by the host that has them; local segments with
no manifest yet; retired segments whose file is still here; and a verify
summary. Until the missing segments arrive, every number is partial.

`verify` reads every segment, checking its bytes still hash to its name and
that it parses, and prints each one that doesn't. It exits non-zero when any
fails.

`publish` writes the manifest of every local segment the vault lacks one
for: a segment imported before manifests existed, or while logged out.
`import` publishes its segments' manifests itself.

`rm` retires one segment, named by its hash or a prefix of it unique among
the local segments and the vault's manifests: it moves the file to
journal/.trash/ if it is here, and marks its manifest retired, so every
machine leaves its records out of every later load, even where the file is
still there. Use it to retire a legacy `import ledger` segment once a real
importer covers the same source. Logged out, only the file moves, and the
segment comes back from any other copy of the journal that still has it.",
        options: &[
            (
                "--dry-run",
                "publish: list what would be published, write nothing",
            ),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "cashflow journal ls",
            "cashflow journal status",
            "cashflow journal verify",
            "cashflow journal publish",
            "cashflow journal rm 3fa9c2",
        ],
    },
    CommandHelp {
        name: "sync",
        group: JOURNAL,
        summary: "Fetch and push segments to the remotes in config.toml",
        usage: &["sync [--transport <name>] [--dry-run] [--json]"],
        details: "\
Each [[sync.transport]] in config.toml is a remote copy of the journal:

    [[sync.transport]]
    kind = \"rsync\"
    remote = \"host:/path/to/journal\"   # or a local dir, e.g. a mounted drive
    # name = \"backup\"                  # [default: the kind]

    [[sync.transport]]
    kind = \"git\"
    remote = \"host:cashflow.git\"       # any git URL; segments go on `main`

For each transport in order, sync pushes the remote every local segment it
lacks, then fetches the segments the vault's manifests say this machine
lacks (`journal status` lists them) into staging/<name>/, admitting each one
only if its bytes hash to its name. A segment that arrives from one remote
is pushed on to the ones after it. Retired segments are neither fetched nor
pushed. Nothing a remote sends is written into journal/ unchecked: a file
that fails the check is rejected and left in staging until the next sync.

Sync is a union of files named by their hashes, so it never conflicts and
is safe to run from any machine at any time. The rsync transport runs
`rsync -a --ignore-existing` (over ssh for host:path), which must be on
PATH. The git transport keeps its own clone in sync/git/<name>/ (never in
journal/) and makes one commit per push, `cashflow: add N segments from
<host>`, so the remote has the history; git must be on PATH and must reach
the remote without prompting. Segments are plaintext: a hosted git remote
should be private, and ideally encrypted (git-crypt) or self-hosted.
Needs `cashflow login`: the manifests say what to fetch.

Exits non-zero when a transport fails or serves a bad file; the others still
run.",
        options: &[
            ("--transport <name>", "Only the transport with this name"),
            (
                "--dry-run",
                "List what would be fetched and offered, move nothing",
            ),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "cashflow sync",
            "cashflow sync --dry-run",
            "cashflow sync --transport backup",
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
  appstore [<file>|<month>...]
                  App Store Connect financial reports: the proceeds Apple
                  pays out. Given report files (plain or .gz; a directory
                  reads its *.txt, *.tsv and *.gz), it reads those;
                  otherwise it fetches each named fiscal month (2026-03, or
                  2025-10..2026-03), or the 12 before this one, from the
                  App Store Connect API with regionCode ZZ. A report's rows
                  for one product, currency and product type are summed
                  into one transaction on the period's end date, posting
                  its Extended Partner Share to assets:apple and
                  income:unknown in the report's currency, with raw-app-id,
                  raw-sku, raw-title, raw-product-type, raw-country,
                  raw-units and raw-period. Its id is
                  appstore:<vendor>:<end>:<apple-id>:<currency>:<type>.
                  [appstore] in config.toml sets `vendor` (required),
                  `key_id` and `issuer` (the API key's ids; the key is the
                  appstore.p8 secret), `region` (ZZ) and `account`.
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
  cln [<file>...] Core Lightning bookkeeper events, bkpr-listaccountevents
                  over commando: [cln] in config.toml sets `node_id`,
                  `host` (host[:9735]) and `timezone`, and the
                  cln-rune secret is a rune that permits the call (mint
                  one with lightning-cli createrune
                  restrictions='[[\"method=bkpr-listaccountevents\"]]').
                  An event is filed under its day in the synced
                  `timezone` setting, so every machine dates it alike;
                  `timezone` under [cln] overrides that on one machine,
                  and with neither set it is this machine's own zone.
                  A node whose bookkeeper takes start_time is read from a
                  day before the newest payment the journal holds; one
                  that doesn't is read whole, and says so. Payments
                  (invoice, routed) go in layer cln, posting msat to
                  assets:cln against routed:in/out, income:zap (a zap
                  request; raw-zap-p is the recipient), income:lnurl
                  (raw-lnurl-address) or income/expenses:unknown
                  (raw-description); every other event goes in layer
                  cln-chain. Ids are cln:channel:<n>, cln:chain:<n> and
                  cln:fee:… from a patched bookkeeper, cln:h:<hash>
                  otherwise. A description edited later on an event older
                  than the cursor is not read again. Given files, it reads
                  saved bkpr-listaccountevents output instead.
  ledger <file>   a ledger-cli file, such as books kept by hand or a script.
                  A transaction with no `; id:` is given a legacy one,
                  <source>:<hash of its text>, plus :2, :3, … for identical
                  transactions in the file. A legacy id changes when its
                  transaction is edited, so edits keyed on it don't survive
                  that. Lines that don't parse are reported with their line
                  numbers and their transaction is left out; the rest is
                  imported.
  payroll <file>  a payroll file of bitcoin payouts, one `*` line each:
                  `* <invoice|??> <address> <date> <cad>CAD <sats> <txid>`;
                  other lines are passed over and a `*` line that doesn't
                  read is reported by line number. The txid is a send the
                  btc importer already files against expenses:unknown, so
                  each payout moves its msat from there to
                  expenses:payroll, as `<payee> invoice <n>`, id
                  payroll:<txid>, with raw-txid, raw-address and raw-cad
                  (the invoice's CAD). [payroll] in config.toml sets
                  `payee` (payroll), `account` (expenses:payroll) and
                  `counter` (expenses:unknown; a wallet account for
                  payouts from a wallet that isn't imported).
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
importer's), which `export` and `report` can include or leave out; an
importer's second layer, such as cln-chain, keeps its own name.",
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
            "cashflow import appstore 2025-10..2026-03",
            "cashflow import appstore ~/Downloads/80000001_0326_ZZ.txt.gz",
            "cashflow import btc",
            "cashflow import cln",
            "cashflow import ledger books.ledger",
            "cashflow import payroll payroll.txt",
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
Load the journal, categorize it with the vault (its rules from `cashflow
rule`, and the overrides, splits and transfer links from `categorize`,
`split` and `transfer`; with no stored key, as imported), or with --rules
alone instead, and print it to stdout in ledgerdb's canonical ledger-cli
form, readable by ledger-cli and by `cashflow import ledger`.

With --begin or --end only the transactions in the range are printed
(--begin inclusive, --end exclusive, both YYYY-MM-DD), after every price
dated before the end so the range still converts.

Segments that don't load and edits that don't apply are reported on stderr.

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
            "report projects [<parent>] [--begin/--end <date>]",
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

`projects` sums <parent>'s subtree (default: income) over the range by the
project each account is in (`cashflow account set --project`), with what no
project claims as (unattributed).

Dates are YYYY-MM-DD; --begin is inclusive, --end exclusive. --layer,
--except and --rules choose the books as for `export`: the vault's rules
and edits, unless --rules names a file.",
        options: books_options![
            (
                "--in <commodity>",
                "Convert into this commodity [default: CAD]"
            ),
            ("--depth <n>", "Account depth to list (bal, top)"),
            ("--begin <date>", "First day (reg, top, projects)"),
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
            "cashflow report projects --begin 2025-01-01",
        ],
    },
    CommandHelp {
        name: "rule",
        group: CATEGORIZE,
        summary: "List, add, move, remove or test the categorization rules",
        usage: &[
            "rule ls [--by <agentium-ref>] [--json]",
            "rule add <account> [--match <text>]... [--meta <key=value>]... [...]",
            "rule add ... [--amount <min..max>] [--source <s>] [--layer <l>]",
            "rule mv <rule> --first|--last|--before <rule>|--after <rule>",
            "rule rm <rule> [--dry-run]",
            "rule test <record-id> [--json]",
            "rule import <file.toml> [--dry-run]",
        ],
        details: concat!(
            "\
The vault's rules decide where each transaction's counter leg goes (its
posting to a default account such as expenses:unknown). They are tried in
order and the first match wins; `export` and `report` use them unless given
--rules. Each rule is its own vault note, so machines editing different rules
never collide, and a rule is named by its word-id or a unique prefix of it.

`ls` lists the rules in order, each with how many transactions it decides now
and the agentium session that added it; --by keeps one session's.

`add` puts a rule last, or where --first/--before/--after say. Its conditions:
--match is a payee substring (repeat: any one), --meta a metadata substring
(repeat: all), --amount the counter leg's size in smallest units, sign
ignored (`100..2000`, `5000..`, `1000..1000msat`), --source and --layer the
segment's. `mv` reorders one rule, `rm` deletes it.

`add`, `mv`, `rm` and `import` print how many transactions move, from which
account to which, a sample of them, and how many they take from another rule.
--dry-run prints that and writes nothing.

`test` says which rule decides one transaction, by its `; id:`, and why.

`import` appends a rules file's rules, in file order, skipping any the vault
already has, so it can be re-run. Its defaults/unknown/transfers are not
stored: if they differ from the built-in ones it says so.

The session that added a rule is $AGENTIUM_SESSION, when it is an
agentium:<word-id> ref.

",
            rules_details!()
        ),
        options: &[
            ("--json", "Machine-readable output"),
            ("--dry-run", "Show what would move; write nothing"),
            ("--by <agentium-ref>", "Only rules this session added (ls)"),
            (
                "--match <text>",
                "Payee contains this; repeat for any of (add)",
            ),
            ("--meta <key=value>", "Metadata contains this; repeat (add)"),
            (
                "--amount <min..max>",
                "Counter leg size, smallest units (add)",
            ),
            ("--source <name>", "Segment source (add)"),
            ("--layer <name>", "Segment layer (add)"),
            ("--first", "Try it before every rule (add, mv)"),
            ("--last", "Try it after every rule (add, mv)"),
            ("--before <rule>", "Try it just before this rule (add, mv)"),
            ("--after <rule>", "Try it just after this rule (add, mv)"),
        ],
        examples: &[
            "cashflow rule import ~/amex.toml --dry-run",
            "cashflow rule add expenses:food:coffee --match espresso --dry-run",
            "cashflow rule mv oak-flat-lamp --before hill-ripe-sock",
            "cashflow rule test amex:2024-01-02-0001",
            "cashflow rule ls --json",
        ],
    },
    CommandHelp {
        name: "categorize",
        group: CATEGORIZE,
        summary: "Send one transaction to an account",
        usage: &["categorize <record-id> <account> [--dry-run] [--json]"],
        details: "\
Override where one transaction's counter leg goes (its posting to a default
account such as expenses:unknown), by its `; id:`. An override beats every
rule, and replaces a split of the same transaction. It is its own vault note
(d = ov:<record-id>), so it follows the key to every machine.

It prints the transaction's legs before and after, the note it wrote, and
what that replaced; --dry-run prints the same and writes nothing. A
transaction linked as a transfer can't be categorized: `transfer unlink` it
first. The session that wrote it is $AGENTIUM_SESSION, when that is an
agentium:<word-id> ref (see `cashflow edits --by`).",
        options: &[
            ("--dry-run", "Show the change; write nothing"),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "cashflow categorize amex:2024-01-02-0001 expenses:food --dry-run",
            "cashflow categorize cln:inv:abc income:consulting",
        ],
    },
    CommandHelp {
        name: "uncategorize",
        group: CATEGORIZE,
        summary: "Clear one transaction's override or split",
        usage: &["uncategorize <record-id> [--dry-run] [--json]"],
        details: "\
Clear the override or split `categorize` or `split` wrote for a transaction,
so the rules decide it again. The clear is a newer revision of the same vault
note, so it syncs like the edit did. Prints the legs before and after;
--dry-run writes nothing.",
        options: &[
            ("--dry-run", "Show the change; write nothing"),
            ("--json", "Machine-readable output"),
        ],
        examples: &["cashflow uncategorize amex:2024-01-02-0001"],
    },
    CommandHelp {
        name: "split",
        group: CATEGORIZE,
        summary: "Share one transaction out over several accounts",
        usage: &["split <record-id> <account>=<amount>... [<account>] [--dry-run] [--json]"],
        details: "\
Replace a transaction's counter leg with several legs, by its `; id:`. Each
amount is in the counter leg's commodity (`12.50`, or `12.50 CAD` to say
so) and in its direction: give the parts of a 100.00 purchase as positive
numbers, and a negative one for a part going the other way. One leg may be a
bare <account>, taking whatever the others leave. The legs must add up to
the counter leg exactly.

A split beats every rule and replaces an override of the same transaction.
It is its own vault note (d = split:<record-id>); `uncategorize` clears it.
Prints the legs before and after; --dry-run writes nothing.",
        options: &[
            ("--dry-run", "Show the change; write nothing"),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "cashflow split amex:2024-01-02-0001 expenses:food=60 expenses:home",
            "cashflow split rbc:0042 expenses:rent=1200.00 expenses:utilities=80.50",
        ],
    },
    CommandHelp {
        name: "transfer",
        group: CATEGORIZE,
        summary: "Link the two sides of a move between your own accounts",
        usage: &[
            "transfer link <record-id> <record-id> [--dry-run] [--json]",
            "transfer unlink <record-id> <record-id> [--dry-run] [--json]",
            "transfer suggest [--accept-all [--dry-run]] [--json]",
        ],
        details: "\
Paying a card from chequing is recorded twice, once by each source, and until
the two are linked it counts as an expense on one side and income on the
other. A link sends both counter legs to the transfers account, where they
net out. Its two sides must be equal and opposite in one commodity.

`link` writes the link (d = xfer:<a>|<b>, the ids sorted, so either order is
the same link), `unlink` clears it. A transaction with an override or split,
or already in a link, can't be linked: `uncategorize` or `unlink` it first.

`suggest` lists the pairs that look like one transfer: an own account
(assets, liabilities) on each side, equal and opposite, at most 5 days apart,
neither decided by an edit yet. --accept-all links every one; with --dry-run
it lists them and writes nothing.",
        options: &[
            ("--accept-all", "Link every suggested pair (suggest)"),
            ("--dry-run", "Show the change; write nothing"),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "cashflow transfer suggest",
            "cashflow transfer suggest --accept-all --dry-run",
            "cashflow transfer link rbc:0042 amex:2024-01-07-0003",
        ],
    },
    CommandHelp {
        name: "edits",
        group: CATEGORIZE,
        summary: "List the overrides, splits and transfer links",
        usage: &["edits [--by <agentium-ref>] [--json]"],
        details: "\
List every live per-transaction edit in the vault: overrides (`categorize`),
splits and transfer links, each with the agentium session that wrote it.
--by keeps one session's, to review or undo what an agent did.",
        options: &[
            ("--by <agentium-ref>", "Only edits this session wrote"),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "cashflow edits",
            "cashflow edits --by agentium:oak-flat-lamp --json",
        ],
    },
    CommandHelp {
        name: "unknown",
        group: CATEGORIZE,
        summary: "The triage queue: what nothing has categorized, by payee",
        usage: &["unknown [--source <name>] [--limit <n>] [--in <commodity>] [--json]"],
        details: "\
List the transactions whose counter leg is still on an importer default
(no edit or rule decided it), grouped by payee (lower-cased, words with
digits dropped) and by that default, biggest total first, since one rule
there fixes the most. Totals convert into --in's commodity [default: CAD];
rows without a rate are counted apart.

Each group comes with the `cashflow rule add` that would take it: a --match
every row's payee contains, and --source when the rows share one. Replace
<account> and run it with --dry-run first.",
        options: &[
            ("--source <name>", "Only rows from this segment source"),
            ("--limit <n>", "How many groups [default: 20]"),
            (
                "--in <commodity>",
                "Convert totals into this [default: CAD]",
            ),
            ("--json", "Machine-readable output, all rows' ids included"),
        ],
        examples: &[
            "cashflow unknown",
            "cashflow unknown --source amex --limit 5",
            "cashflow unknown --json | jq '.groups[0].rule_add'",
        ],
    },
    CommandHelp {
        name: "account",
        group: CATEGORIZE,
        summary: "The chart of accounts: names, icons, projects and aliases",
        usage: &[
            "account ls [<account>] [--depth <n>] [--all] [--json]",
            "account set <account> [--name <text>] [--icon <emoji>] [--color <#rrggbb>]",
            "    [--project <name>] [--type <kind>] [--hide|--show] [--clear <field>]...",
            "account alias <from> <to> [--dry-run] [--json]",
            "account unalias <from> [--dry-run] [--json]",
        ],
        details: "\
Each account can carry metadata in the vault (d = acct:<account>): a
display name, an icon, a colour, the project it earns or spends for, a
hidden flag and a type (asset, liability, equity, income, expense) for an
account whose top-level name doesn't say. Every field but the name is
inherited: an account without its own takes its nearest ancestor's, so
`--project damus` on income:damus puts everything under it in Damus.

`ls` prints the tree under <account> (default: all) with each subtree's
balance in --in's commodity, its icon and name, and the project where one
starts. Hidden accounts and accounts with no postings are left out unless
--all. Then the metadata of accounts the ledger doesn't hold, and the
aliases.

`set` changes only the fields given; --clear <field> unsets one so it is
inherited again (name, icon, color, project, hidden, type), and --show
shows an account a hidden ancestor hides.

`alias` renames the subtree at <from> to <to> when the books are derived,
after every rule and override, so the chart can be restructured without
editing them (d = alias:<from>): expenses:food -> expenses:groceries files
expenses:food:produce as expenses:groceries:produce. An account takes its
nearest alias, a renamed account isn't renamed again, and one already under
<to> stays put. `unalias` clears it. Both print the accounts that move.",
        options: books_options![
            (
                "--in <commodity>",
                "Convert balances into this commodity [default: CAD] (ls)"
            ),
            ("--depth <n>", "Deepest account to list (ls)"),
            ("--all", "Include hidden and empty accounts (ls)"),
            ("--name <text>", "Display name (set)"),
            ("--icon <emoji>", "Icon (set)"),
            ("--color <#rrggbb>", "Colour (set)"),
            ("--project <name>", "Project the subtree belongs to (set)"),
            (
                "--type <kind>",
                "asset, liability, equity, income or expense (set)"
            ),
            ("--hide", "Hide the account and its subtree (set)"),
            ("--show", "Show it under a hidden ancestor (set)"),
            ("--clear <field>", "Unset a field; repeatable (set)"),
            ("--dry-run", "Show the change; write nothing"),
            ("--json", "Machine-readable output"),
        ],
        examples: &[
            "cashflow account ls income",
            "cashflow account set income:damus --project damus --icon 🦩",
            "cashflow account set expenses:food --clear icon",
            "cashflow account alias expenses:food expenses:groceries --dry-run",
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

  home      the commodity reports convert into [default: CAD]
  timezone  the day a CLN event is filed under: a zone like
            America/Vancouver, utc, or an offset like -07:00. Set it
            before importing from two machines, or near midnight each
            rewrites the other's events [default: local, each machine's
            own]

`show` lists each setting and its value, marking defaults. `get` prints one
value alone, for scripts. `set` writes it and publishes it to the relay.",
        options: &[("--json", "Machine-readable output")],
        examples: &[
            "cashflow settings",
            "cashflow settings set home CAD",
            "cashflow settings get home",
            "cashflow settings set timezone America/Vancouver",
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
