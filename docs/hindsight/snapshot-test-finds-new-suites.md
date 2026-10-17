# scripts/snapshot-test finds a crate's snapshot suites itself

`scripts/snapshot-test` runs a hand-kept list of `--test` targets per crate, so a
new test file's snapshots are silently skipped and the run still exits 0.

- headway:dave/position-illegal-spare — the throwaway slash-popup prototype in `tests/slash_prototype.rs` "passed" with 0 tests run; I only noticed because no png appeared, then had to sed the suite line in and revert it on every re-render
- headway:dave/student-local-solution — the new message-menu snapshots in `chip_click_tests` were silently skipped through a 25-minute full update; found only because their pngs never appeared
- headway:notedeck/fury-excuse-accident — the new `tests/command_bar.rs` snapshots ran 0 tests with exit 0; had to add `--test command_bar` to the chrome suite line by hand
- headway:notedeck/zebra-illness-salute — the new `tests/shot.rs` GPU test would have run 0 tests in CI; added `--test shot` to the chrome suite line by hand
- headway:cashflow/answer-vibrant-crush — a new crate's `tests/snapshot_tests.rs` ran nowhere until a `notedeck_cashflow` suite line was added to the script by hand
- headway:cashflow/elder-spirit-fly — the new `tests/chart_tests.rs` snapshots in notedeck_ui needed `--test chart_tests` added to the suite line by hand
