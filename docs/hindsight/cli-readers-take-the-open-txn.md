# cashflow CLI readers that take the open transaction

`raw_ledger`, `retired` and `vault_edits` open their own txn, one per thread,
so a command holding its own txn must reorder its reads or fail opaquely.

- headway:cashflow/dream-accident-reveal — ~3 min: `account alias` held the chart's txn while calling `raw_ledger`/`vault_edits` and died with a bare "Transaction failed"; reordered so those run first
