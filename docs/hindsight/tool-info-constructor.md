# One constructor for notedeck_ipc's ToolInfo

`ToolInfo` has no `new` or `Default`: each new wire field edits every literal.
`ToolInfo::new(name, kind, schema)` would make the next field a one-line change.

- headway:notedeck/rain-giant-charge — adding `streams` touched seven `ToolInfo` literals in six files, none of which cared about streams
