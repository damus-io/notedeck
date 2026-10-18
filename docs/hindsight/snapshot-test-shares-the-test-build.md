# scripts/snapshot-test reuses the build `cargo test` just made

Card: headway:notedeck/brass-suggest-satisfy
`--debug` builds no release now, but each suite's features rebuild the crates it
shares with the others (chrome, columns), so suites evict each other's builds.

- headway:cashflow/right-gain-gym — 33 min session, ~12 of it builds: two release snapshot builds (one lost to a `target/release` fingerprint race and retried) beside two debug `cargo test -p cashflow -p notedeck_cashflow` runs, before and after rebasing onto lane A
- headway:headway/will-print-climb — ~9 min: `--changed=autowork --debug` spent 7m16s on a cold release build of chrome's suite (release-only under --debug), then was killed while the dave suite rebuilt
