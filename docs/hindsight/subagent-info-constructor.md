# One constructor for SubagentInfo

`SubagentInfo` has no `new` or `Default`, so ~28 struct literals across five
crates (mostly tests) spell out every field, and each new field edits them all.

- headway:dave/dance-venture-sure — adding `agent_id` and `transcript` touched 28 literals in 20 files, scripted in, for a change whose real code was in four; a `SubagentInfo::running(task_id, description, subagent_type)` would have made it two
- headway:dave/rude-food-bar — wrote two more ten-field literals (a test helper in `session/subagent.rs` and a kittest seed in `ui/mod.rs`) just to get a running Explore row
