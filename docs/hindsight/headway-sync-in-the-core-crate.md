# Headway's relay sync in the core crate

Headway's relay sync is private to `headway_cli`; other board readers can't sync.
It reads the `headway` CLI's cache instead: as fresh as the last `headway` run.

- headway:cashflow/reform-sun-harbor — `cashflow goals` reads `headway-cli`'s cache (`goals_cmd.rs`) instead of syncing its own, because pulling boards would have meant copying ~450 lines of `headway_cli` sync
