# An importer sees the journal it imports into

`Importer::import` can't see the `Ledger` that `import::write` loads, so a
pull re-reads its whole source instead of resuming where the journal stops.

- headway:cashflow/pyramid-fluid-bomb — ~4 min: `btc` pages every wallet's whole `listtransactions` history each run instead of stopping below the journal's highest block, since the importer can't see the ledger `write` loads
- headway:cashflow/slow-humble-goat — ~2 min: the review asked `payroll` to import only pay periods after the journal's newest payroll record; it re-reads the whole file and leans on `write` dropping the unchanged rows, since the importer can't see the ledger
- headway:cashflow/brick-hospital-race — ~2 min: with no months named, `appstore` fetches a fixed twelve months back on every run rather than resuming after the journal's newest `appstore:` record, since the importer can't see the ledger
