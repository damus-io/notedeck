# One public chrome frame that tests drive, instead of each suite's own copy

headless chrome tests hand-roll the chrome's frame (tool update, actions, nav,
render), so each copy drifts from the real one and misses what it skipped.

- headway:notedeck/favorite-please-side — three hand-rolled host frames (headway_nav_tests chrome_frame, tests/common Fixture::frame, headway_keymap Fixture::frame); the one that skipped the tool update cost a diagnosis
- headway:headway/identify-payment-segment — slide_tests::drag_rig_frame copies frame.rs's record-in_flight/collect-pop/drain sequence because global_nav borrows against apps
- headway:headway/owner-baby-mother — snapshot_tests' chrome_frame never lands a Back, so the review-pane archive race only showed once the test moved off the copy (8 tests still on it)
- headway:notedeck/pretty-truly-example — no public way to drive an AppAction through the chrome (chrome_handle_app_action is pub(super)), so the routing test had to live in-crate
- headway:headway/bottom-crack-kidney — no chrome seam for "resolve then route" an Open, so the reference-open path got no chrome-level test
- headway:notedeck/ginger-home-mirror — 46 min: chrome's new data-tool dispatch had to be copied into two hand-rolled key harnesses (keys.rs chrome_keys, snapshot_tests ChromeKeys) on top of chrome's own; plus a 10-min ci.py timeout on a cold workspace build
