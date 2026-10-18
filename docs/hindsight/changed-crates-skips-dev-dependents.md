# scripts/changed-crates follows only what a crate's code depends on

`snapshot-test --changed` runs suites that reach a diff only by dev-dependency,
so a cashflow diff reruns headway's snapshots and their `--debug`-only failure.

- headway:cashflow/help-lesson-season — 6 min: `snapshot-test --changed --debug` ran the six crates downstream of `cashflow`, notedeck_headway's suites among them, though nothing in the diff reaches a headway render; `-p=notedeck_cashflow` was the run that mattered
