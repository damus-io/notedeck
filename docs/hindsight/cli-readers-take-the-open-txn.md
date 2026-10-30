# cashflow CLI readers that take the open transaction

`raw_ledger`, `retired` and `vault_edits` open their own txn, one per thread,
so a command holding its own txn must reorder its reads or fail opaquely.

- headway:cashflow/dream-accident-reveal — ~3 min: `account alias` held the chart's txn while calling `raw_ledger`/`vault_edits` and died with a bare "Transaction failed"; reordered so those run first
- headway:cashflow/scale-jazz-holiday — ~3 min: `triage::apply` opens its own txn, so `account set` and `categorize`/`split`/`uncategorize` (holding the chart/`VaultEdits` txn) died with "couldn't read the vault: Transaction failed"; moved their reads into helpers (`merge`, `replaced`) that end the txn first
