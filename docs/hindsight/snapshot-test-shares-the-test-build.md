# scripts/snapshot-test reuses the build `cargo test` just made

`scripts/snapshot-test` builds in release, apart from `cargo test`'s debug build,
so a card running both pays two cold builds, and both again after every rebase.

- headway:cashflow/right-gain-gym — 33 min session, ~12 of it builds: two release snapshot builds (one lost to a `target/release` fingerprint race and retried) beside two debug `cargo test -p cashflow -p notedeck_cashflow` runs, before and after rebasing onto lane A
