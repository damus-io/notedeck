# scripts/snapshot-test says what each suite compiled and how long it took

The suites' builds scroll past inside `cargo test` output, so a suite that
rebuilds what the last one built goes unseen until a run is slow enough to kill.

- headway:notedeck/brass-suggest-satisfy — 27 min: a hand-written replay of the suites' `cargo test --no-run`s, run before and after at JOBS=2, to see which crates each one recompiled
