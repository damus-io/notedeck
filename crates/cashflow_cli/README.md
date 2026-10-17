# cashflow_cli

The `cashflow` binary: import, inspect and export a personal finance ledger.
It reads and writes the same `$CASHFLOW_DIR` as the notedeck app; the layout
is documented in `crates/cashflow`.

```sh
cashflow --help                 # the command list
cashflow config path            # the data dir's paths
cashflow config path journal    # one of them, for scripts
cashflow config show            # effective config.toml + secrets by name only
cashflow --dir /tmp/cf config show --json
cashflow login < ~/keys/me.nsec # store the vault key as secrets/nsec
cashflow settings set home CAD  # a setting every machine shares
cashflow settings               # each setting, defaults marked
```

The vault commands keep their own nostrdb (`--db`, default
`<data-dir>/cashflow-cli`) and reconcile its sealed envelopes with a relay
(`--relay`, `$CASHFLOW_RELAY`, default notedeck's `ws://127.0.0.1:6677`)
before reading, then publish what they write. An unreachable relay leaves
them working offline.

The journal, importers, export and reports come in later cards; the design is
on the root Headway epic, `headway:cashflow/flame-vault-near`.

## Layout

- `src/help.rs`: the help table. It is also the parser's list of commands and
  flags, so a command can't ship without a help page.
- `src/args.rs`: parsing into a `Command`.
- `src/config_cmd.rs`: `config path` / `config show`.
- `src/vault_cmd.rs`: `login`, `logout`, `settings`, and the `Session` every
  vault command opens (key, cache, root, relay sync). A later vault command
  opens one the same way.
- `tests/harness/mod.rs`: the e2e harness. It runs this worktree's build of
  the binary (`bin_testing::worktree_bin`) in a fresh temp `$CASHFLOW_DIR`,
  with the developer's own `CASHFLOW_*` variables removed, a dead relay and a
  temp `$XDG_DATA_HOME`. Add a command's cases with
  `Cashflow::knowing("<command>")`.
- `tests/vault.rs`: login, settings across processes, and through a real
  relay that only ever sees envelopes.
