# One constructor for ExecutedTool and RunningTool

Neither has `new` or `Default`; ~55 literals spell out every field, mostly in tests.
`ExecutedTool::new(tool_name, summary)` would make a new field two lines, not 45.

- headway:dave/desert-summer-alley — adding `description` touched 45 literals in 13 files, scripted in from the compiler's errors, for a change whose real code was in seven
- headway:dave/taxi-hill-current — adding `is_error` touched 35 literals in 8 files the same way, scripted from E0063 locations, a day after `description` did
