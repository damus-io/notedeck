# A session knows its own agentium: ref

A session holds only its event id, so finding the one a `parent-session` ref
names formats every session's `agentium:` ref and compares strings.

- headway:dave/throw-draw-draw — wrote `lineage::SessionRefs`, a per-regroup cache of each session's ref keyed on its event id, so nesting children and chains under their spawner doesn't format N refs every time the groups rebuild; `child_report` still formats them all per report
