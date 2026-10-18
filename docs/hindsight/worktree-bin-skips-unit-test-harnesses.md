# worktree_bin never picks a bin crate's own unit-test harness

A bin's unit tests build to deps/<bin>-<hash> from the same sources, so the
crate_dir filter admits them and only the caller's probe stands in the way.

- headway:cashflow/practice-expose-vast — cashflow's e2e harness picked cashflow_cli's unit-test binary, which ran `help config` as test filters and exited 0, failing all seven e2e tests after a rebuild; fixed by probing for the page header in tests/harness/mod.rs, but bin_testing::worktree_bin could tell the two apart itself
- headway:personal/ribbon-dance-garbage — ~3 min: headway_cli's tests/progress.rs probes with `|_| true`, so after a `cargo test` it ran the headway bin's unit-test harness ("Unrecognized option: 'nsec'") and both its tests fail on autowork; ruled out as pre-existing rather than fixed
