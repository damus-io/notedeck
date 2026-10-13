# scripts/snapshot-test finds a crate's snapshot suites itself

`scripts/snapshot-test` runs a hand-kept list of `--test` targets per crate, so a
new test file's snapshots are silently skipped and the run still exits 0.

- headway:dave/position-illegal-spare — the throwaway slash-popup prototype in `tests/slash_prototype.rs` "passed" with 0 tests run; I only noticed because no png appeared, then had to sed the suite line in and revert it on every re-render
