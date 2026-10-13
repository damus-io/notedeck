# A test fixture for a subagent's virtual session

Tests fake a subagent as an agentic session with `subagent_of`; a real one
has no `AgenticSessionData`, so `agentic` filters pass tests, drop subagents.

- headway:dave/render-resemble-artwork — the scene's satellites (b49a78223da8) only ever drew in tests: `scene_layout::on_map` filters on `agentic`, which real subagent sessions lack; a `SessionManager::add_test_subagent(parent, info)` going through `sync_subagent_sessions` would have shown it
