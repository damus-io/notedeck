# A tool's spec derived from its typed input struct

Card: headway:notedeck/brass-bonus-vivid
A `ToolSpec`'s `ToolArg` list is hand-written, so a tool that parses into a
serde struct names every field twice, held together only by a test.

- headway:dave/explain-feel-blind — the six coding tools declare each argument as a `ToolArg` and again as a field of `ReadInput`…`BashInput`; `each_schema_names_exactly_its_inputs_fields` in agent_spec/coding.rs is what holds the two together
