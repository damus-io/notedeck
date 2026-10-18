# worktree_bin's freshness probe outlives a help rewrite

E2e suites call a build current when its usage contains a command's text, so a
help rewrite makes the probe miss and they fail "build it first" on a fresh one.

- headway:notedeck/spell-kitten-day — ~3 min: 7 agentium_cli e2e tests (id_prints_ref, link_lands, spawn_lands) fail on autowork because dd940f7b885b's one-screen help dropped "id <d-tag" / "unlink <session>" / "--wait" from the usage; a probe on a build marker (e.g. `agentium --version` carrying the git hash) wouldn't break on a help edit
