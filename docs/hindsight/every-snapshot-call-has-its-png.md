# Every snapshot a test takes has its png committed

A test's `harness.snapshot("name")` with no `tests/snapshots/name.png` lands
silently, and the next session's `--update` records it as if it were its own.

- headway:notedeck/road-ritual-copper — a whole card to record headway_review_raw_message, which headway:headway/will-print-climb landed without; the suite check then found headway_review_explainer_figures(_dark) landed the same way in 5622a203c128
- headway:headway/cry-puppy-refuse — the per-crate headway run flagged headway_review_explainer_figures(_dark) again, as .new.png with no png to diff against; had to check git history to tell they weren't this card's and delete the renders
- headway:notedeck/brass-suggest-satisfy — the run that proved suites stop rebuilding each other still exited 1 on headway_review_explainer_figures(_dark), so I had to tell their missing pngs apart from a pixel change before trusting it
