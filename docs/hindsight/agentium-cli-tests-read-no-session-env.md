# agentium_cli's integration tests read no session environment

Spawned `agentium` binaries inherit the runner's `$AGENTIUM_SESSION`, so inside
an agent session only the tests that remember `env_remove` are hermetic.

- headway:dave/name-tree-bulk — spawn_lands' spawns now record `$AGENTIUM_SESSION` as their parent; the lineage case had to set it explicitly, and the other spawn tests silently stamp the runner's own ref on their commands
