# A kittest text lookup that matches values too and prints the tree on a miss

`wait_for_label` matches names only, and a missed query says nothing about what
was on screen, so value-bearing widgets cost an inline query and a guess.

- headway:headway/inside-cost-knock — no wait-for-text helper matching label values cost the most time; wrote an inline query_all
- headway:notedeck/ethics-fun-afraid — "no node has value == BODY" gave nothing to go on; dumping the accesskit tree found it in one run, killing the "macOS race" premise
- headway:dave/general-tilt-repeat — 33-min session, ~8 min of it here: an exact lookup missed "2 background tasks" once its FTL entry existed (Fluent wraps `{$count}` in U+2068/U+2069), and my hand-rolled `label().contains` predicate skipped Role::Label values; a tree dump on the miss would have shown both at once
