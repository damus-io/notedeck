# An importer sees the journal it imports into

`Importer::import` can't see the `Ledger` that `import::write` loads, so a
pull re-reads its whole source instead of resuming where the journal stops.

- headway:cashflow/pyramid-fluid-bomb — ~4 min: `btc` pages every wallet's whole `listtransactions` history each run instead of stopping below the journal's highest block, since the importer can't see the ledger `write` loads
