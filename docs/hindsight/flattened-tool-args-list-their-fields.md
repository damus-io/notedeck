# A tool's flattened args still list their fields

A `#[serde(flatten)]` args struct has no serde field list, so `arg_drift`
can't check its tool: 38 of the 78 built-in data tools go unchecked.

- headway:notedeck/brass-bonus-vivid — `every_data_tool_describes_exactly_its_args` could check only 40 of 78 tools and settled for a measured floor; a `ToolInput`-style `fields()` that a flattened part contributes to (or dropping flatten for the shared board arg) would cover the rest
