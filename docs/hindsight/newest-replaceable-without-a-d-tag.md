# Newest replaceable without a d tag

nostrdb_net's query_replaceable skips kinds with no `d` (0, 3, 10000-19999), so
each reader hand-rolls a limit(1) query; one shared newest fold would cover them.

- headway:ux-feedback/margin-trigger-student — the card's spec said to read kind 10063 through `query_replaceable`; it returned nothing for a list ingested twice, and the reason was only in the fork's source (`tag_value(&note, "d")` returns early)
