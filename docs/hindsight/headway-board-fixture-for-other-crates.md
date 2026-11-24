# A headway board fixture other crates can seed

Each crate reading boards hand-writes its seeding: ingest waits, cards, channels.
Two accounts in one nostrdb hide each other's key-share, and nothing warns.

- headway:cashflow/reform-sun-harbor — wrote `settle`/`add`/`add_alices` in `cashflow::goals` tests and again in `cashflow_cli/tests/goals.rs`; the shared-board case timed out for 30 s until I found that Alice's self-share and her share to me were one rumor
- headway:cashflow/right-gain-gym — ~4 min: a fourth hand-rolled seeding (`Writer::settle/apply/add/move_to` in `cashflow::board_progress` tests) to fold a board with real moves and an archive; first draft lost the measured move because headway drops activity in a card's creation second
- headway:cashflow/book-ignore-priority — a third copy, in notedeck_cashflow/tests/goals.rs (`add`/`landed`/`wait` closures), to seed a goal card into the host nostrdb for the GoalSource test
- headway:headway/require-drill-room — wrote a fifth seeding, `SharedBoard` in notedeck_headway's renderers tests (root, roster, sealed definition, member card, two ingest waits), to put a board another key owns on the cache's roster
