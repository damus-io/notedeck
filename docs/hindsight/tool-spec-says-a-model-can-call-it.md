# A ToolSpec says whether a model can call it

Card: headway:notedeck/debris-during-retreat
`nd mcp`'s `Exposure::allows` and the agent catalog's `offers` each work out
"a data tool that doesn't stream" from the spec's fields on their own.

- headway:dave/evoke-blanket-little — ~5 min: first read `needs_current()` as "an agent can't call it" and dropped `headway_comment_card` from the toolset, caught only by building the catalog from headway's real tools; a `ToolSpec::model_callable()` beside `capability_class()` would have been the one rule both callers use
- headway:dave/fix-armed-only — ~2 min: admitting command-line UI tools to `Catalog`'s `offers` meant checking `Exposure::allows` by hand to see the two rules now differ on purpose (mcp still needs `--ui`)
