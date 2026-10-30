# Every snapshot harness holds the style still

Suites stop animation by hand (14 copies) and two stop the caret blink, so a
focused field anywhere else draws by how much time a wait loop stepped.

- headway:notedeck/toilet-cause-wood — ~3 min: command_bar_rich_rows's 50 px flake was the query field's caret; its frame copied `animation_time = 0.0` but not notebook's `text_cursor.blink = false`, which one steady-style setup in `notedeck::test_harness` would have carried
- headway:notedeck/board-cousin-brick — ~10 min: `snapshot-test --changed --debug` failed headway_switcher_same_slug (the board menu popup drawn shifted) on a chrome-only change; it passed alone without `--debug`, a wall-clock frame no shared steady setup holds still
- headway:notedeck/west-invite-hill — ~10 min: the same headway_switcher_same_slug `--debug` failure on a second chrome-only change, again passing alone without `--debug`; two cards in a row re-ran it by hand to prove it was not theirs
- headway:notedeck/suspect-typical-exotic — ~3 min: `headway_switcher_same_slug` failed only under `--debug` (the switcher popup drawn at a different point of its open), and passed on a re-run without it; nothing in the diff reaches the switcher
- headway:headway/honey-motion-spoil — ~5 min: headway_switcher_same_slug failed (its board popup drawn narrower) on an explainer-only change, under `--debug` and then twice alone without it after passing the first `--changed` run; nothing in the diff reaches the switcher
- headway:cashflow/cattle-taxi-cheap — ~4 min: snapshot_detail_rule_preview failed 1 run in 3 with `Harness::run exceeded max_steps`, its repaints all from egui `theme/mod.rs:37`; a harness that held the theme still would not repaint past `run`, so the test now steps with `run_ok` and waits on the plan instead
