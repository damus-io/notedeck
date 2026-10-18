# A headway board fixture other crates can seed

Each crate reading boards hand-writes its seeding: ingest waits, cards, channels.
Two accounts in one nostrdb hide each other's key-share, and nothing warns.

- headway:cashflow/reform-sun-harbor — wrote `settle`/`add`/`add_alices` in `cashflow::goals` tests and again in `cashflow_cli/tests/goals.rs`; the shared-board case timed out for 30 s until I found that Alice's self-share and her share to me were one rumor
