# Every snapshot a test takes has its png committed

A test's `harness.snapshot("name")` with no `tests/snapshots/name.png` lands
silently, and the next session's `--update` records it as if it were its own.

- headway:notedeck/road-ritual-copper — a whole card to record headway_review_raw_message, which headway:headway/will-print-climb landed without; the suite check then found headway_review_explainer_figures(_dark) landed the same way in 5622a203c128
