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
```

Only the skeleton exists so far. The journal, importers, export, reports and
the nostr vault come in later cards; the design is on the root Headway epic,
`headway:cashflow/flame-vault-near`.

## Layout

- `src/help.rs`: the help table. It is also the parser's list of commands and
  flags, so a command can't ship without a help page.
- `src/args.rs`: parsing into a `Command`.
- `src/config_cmd.rs`: `config path` / `config show`.
- `tests/harness/mod.rs`: the e2e harness. It runs this worktree's build of
  the binary (`bin_testing::worktree_bin`) in a fresh temp `$CASHFLOW_DIR`,
  with the developer's own `CASHFLOW_*` variables removed. Add a command's
  cases with `Cashflow::knowing("<command>")`.
