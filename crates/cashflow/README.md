# cashflow

The non-UI core of cashflow, a personal finance dashboard and ledger engine for
notedeck. The `cashflow` CLI (`crates/cashflow_cli`) and the notedeck app
(`crates/notedeck_cashflow`) both build on it. It has no egui dependency.

It holds the data directory, its config and its secrets, the account's
private nostr vault, and the journal with its importers, rules and loaded
books:

```text
$CASHFLOW_DIR/              default: <data-local-dir>/cashflow
    config.toml             settings; missing means all defaults, unknown keys are errors
    journal/                content-addressed ledger segments (the synced truth)
    staging/                where a sync transport fetches before the journal admits
    sync/                   a transport's own state, e.g. the git transport's clone
    secrets/                one file per secret (owner-only)
```

- `CashflowDir` resolves the directory (`$CASHFLOW_DIR`, else the platform data
  dir) and names each path in it.
- `Config` is `config.toml`. Each importer adds its own table when it lands.
- `CashflowDir::secret(name)` reads `$CASHFLOW_<NAME>` or `secrets/<name>` into a
  `Secret`, whose `Debug` is redacted. Secrets never go in `config.toml`.
- `vault` is the team-of-one SNS vault: a root derived from the account
  secret (`vault_root`), and typed `Record`s, each one replaceable note
  `d = <prefix>:<id>` whose fields are tags (plus `["v", "1"]`, content
  empty), sealed into kind-1081 envelopes. `Vault::write` seals and ingests
  one; `vault::latest` and `vault::latest_all` read the newest revisions back
  borrowed from the transaction, zero-copy.
- `kinds` allocates cashflow's nostr kinds, 30700–30709.
- `settings` is the first record type: settings every machine shares, such as
  the home commodity (default CAD).

The journal and what reads it:

- `journal` lists the segments under `journal/` (`list`) and checks each
  one's hash, header and body (`verify`); the store itself is ledgerdb's
  `JournalStore`.
- `import` holds the `Importer` trait (`name`, `layer`, `import(&ImportInput)
  -> Batch`) and its registry, a plain match on the name (`importer`,
  `NAMES`). `import::write` files a batch as one segment. A structured
  importer builds `RawTxn`/`RawPrice` records and `Batch::from_raw` turns
  them into a journal through ledgerdb's own parser, so balance and
  precision are checked in one place. Only the `ledger` importer exists so
  far: a ledger-cli file whose transactions already carry `; id:`.
- `rules` reads a local rules TOML (a superset of amex2ledger's `amex.toml`)
  into ledgerdb `Edits`; the format is in its module docs.
- `Books::load` is the pipeline every reader runs: load the segments a
  `LayerFilter` allows, derive them with the edits, build the price db and
  index the result in the home commodity for queries.
- `sync` moves segments between machines through a `SegmentTransport` per
  `[[sync.transport]]`: `rsync` (any rsync destination) or `git` (any git
  URL; its own clone under `sync/git/<name>/`, one commit per push). A
  transport only ever fetches into `staging/`, and the journal admits a file
  only if it hashes to its name. Segments are plaintext, so a **hosted** git
  remote should be private, and ideally encrypted (git-crypt) or
  self-hosted.

The real importers, the vault's other records, goals and the Adventure
view's XP all land here in later cards. The design they follow, including the three
storage layers and the rule that no personal data goes in the repo, is on the
root Headway epic, `headway:cashflow/flame-vault-near`.
