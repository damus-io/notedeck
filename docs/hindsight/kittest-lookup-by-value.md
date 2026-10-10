# A kittest text lookup that matches values too and prints the tree on a miss

Card: headway:notedeck/invite-rubber-mind
`wait_for_label` matches names only, and a missed query says nothing about what
was on screen, so value-bearing widgets cost an inline query and a guess.

- headway:headway/inside-cost-knock — no wait-for-text helper matching label values cost the most time; wrote an inline query_all
- headway:notedeck/ethics-fun-afraid — "no node has value == BODY" gave nothing to go on; dumping the accesskit tree found it in one run, killing the "macOS race" premise
- headway:dave/general-tilt-repeat — 33-min session, ~8 min of it here: an exact lookup missed "2 background tasks" once its FTL entry existed (Fluent wraps `{$count}` in U+2068/U+2069), and my hand-rolled `label().contains` predicate skipped Role::Label values; a tree dump on the miss would have shown both at once
- headway:dave/question-flush-address — ~2 min: three Usage asserts passed until the FTL export landed, then missed on U+2068/U+2069 around `{$count}`; spelled the isolates into the needles
- headway:dave/illness-morning-auto — ~4 min: the Subagents card's meta and summary needles missed with the U+2068/U+2069 spelled in (this time they weren't there), so added a temporary tree dump to see the real values
- headway:dave/tape-swim-army — ~5 min: a Label's text sits in the accesskit `value`, not `label`, so `.accesskit_node().label()` came back None for "2/5 done"; went through NodeT imports and a debug eprintln before seeing it
