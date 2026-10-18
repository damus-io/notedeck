# Headway's relay sync in the core crate

Headway's relay sync is private to `headway_cli`; other board readers can't sync.
It reads the `headway` CLI's cache instead: as fresh as the last `headway` run.

- headway:cashflow/reform-sun-harbor — `cashflow goals` reads `headway-cli`'s cache (`goals_cmd.rs`) instead of syncing its own, because pulling boards would have meant copying ~450 lines of `headway_cli` sync
- headway:cashflow/month-bronze-usual — goal write-back ships for the app only: a `cashflow goals sync` would plan against the stale `headway-cli` cache (repeating a comment the app already posted) and has no way to publish, so the CLI half became a follow-up
