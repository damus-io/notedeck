# One constructor for ExecutedTool and RunningTool

Neither has `new` or `Default`; ~55 literals spell out every field, mostly in tests.
`ExecutedTool::new(tool_name, summary)` would make a new field two lines, not 45.

- headway:dave/desert-summer-alley — adding `description` touched 45 literals in 13 files, scripted in from the compiler's errors, for a change whose real code was in seven
