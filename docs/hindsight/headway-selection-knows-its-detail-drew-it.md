# Headway's selection knows whether its detail drew it

`detail_for == selected` is the only sign a selection's card drew and left, and
a seeded back never clears it, so each place that settles must avoid dropping.

- headway:notedeck/sick-tide-company — the issue list opens a card on another board mid-pass, so the end-of-pass latch had to skip `settle_view`'s drops (`end_view_pass` recomputes only) in case a stale `detail_for` dropped it
