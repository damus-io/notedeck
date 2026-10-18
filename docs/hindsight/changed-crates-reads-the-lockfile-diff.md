# scripts/changed-crates reads what a Cargo.lock change touched

Any edit to `Cargo.lock` makes `scripts/changed-crates` print every crate, so
adding one dependency to a non-UI crate runs every snapshot suite in the repo.

- headway:cashflow/oblige-regular-over — 13 min: `chrono` and `lnsocket` added to `cashflow`'s deps touched only `cashflow`'s entry in Cargo.lock, but `snapshot-test --changed --debug` ran all 41 crates' suites, and one of them (`notedeck_headway`'s `headway_switcher_same_slug`) failed only under `--debug`, which cost a release re-run to clear
