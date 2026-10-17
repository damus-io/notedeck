# cashflow

The non-UI core of cashflow, a personal finance dashboard and ledger engine for
notedeck. The `cashflow` CLI (`crates/cashflow_cli`) and the notedeck app
(`crates/notedeck_cashflow`) both build on it. It has no egui dependency.

So far it holds the data directory, its config and its secrets, and the
account's private nostr vault:

```text
$CASHFLOW_DIR/              default: <data-local-dir>/cashflow
    config.toml             settings; missing means all defaults, unknown keys are errors
    journal/                content-addressed ledger segments (the synced truth)
    staging/                where a sync transport fetches before the journal admits
    secrets/                one file per secret (owner-only)
```

- `CashflowDir` resolves the directory (`$CASHFLOW_DIR`, else the platform data
  dir) and names each path in it.
- `Config` is `config.toml`. Each importer adds its own table when it lands.
- `CashflowDir::secret(name)` reads `$CASHFLOW_<NAME>` or `secrets/<name>` into a
  `Secret`, whose `Debug` is redacted. Secrets never go in `config.toml`.
- `vault` is the team-of-one SNS vault: a root derived from the account
  secret (`vault_root`), and typed `Record`s, each one replaceable note
  `d = <prefix>:<id>` with JSON content `{"v": 1, ...}`, sealed into kind-1081
  envelopes. `Vault::write` seals and ingests one; `vault::latest` and
  `vault::latest_all` read the newest revisions back.
- `kinds` allocates cashflow's nostr kinds, 30700–30709.
- `settings` is the first record type: settings every machine shares, such as
  the home commodity (default CAD).

Importers, the journal store, the vault's other records, goals and the Adventure view's
XP all land here in later cards. The design they follow, including the three
storage layers and the rule that no personal data goes in the repo, is on the
root Headway epic, `headway:cashflow/flame-vault-near`.
