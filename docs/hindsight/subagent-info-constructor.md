# One constructor for SubagentInfo

Card: headway:notedeck/copy-like-run
`SubagentInfo` has no `new` or `Default`, so ~28 struct literals across five
crates (mostly tests) spell out every field, and each new field edits them all.

- headway:dave/dance-venture-sure — adding `agent_id` and `transcript` touched 28 literals in 20 files, scripted in, for a change whose real code was in four; a `SubagentInfo::running(task_id, description, subagent_type)` would have made it two
- headway:dave/rude-food-bar — wrote two more ten-field literals (a test helper in `session/subagent.rs` and a kittest seed in `ui/mod.rs`) just to get a running Explore row
- headway:dave/chief-chimney-oppose — two more ten-field literals (the native researcher's `ChildRow::start` and its convergence test) to get a running row; `SubagentInfo::running(task_id, description, subagent_type)` would have been one line each
