# One public chrome frame that tests drive, instead of each suite's own copy

Card: headway:notedeck/shallow-obvious-nerve
headless chrome tests hand-roll the chrome's frame (tool update, actions, nav,
render), so each copy drifts from the real one and misses what it skipped.

- headway:notedeck/favorite-please-side — three hand-rolled host frames (headway_nav_tests chrome_frame, tests/common Fixture::frame, headway_keymap Fixture::frame); the one that skipped the tool update cost a diagnosis
- headway:headway/identify-payment-segment — slide_tests::drag_rig_frame copies frame.rs's record-in_flight/collect-pop/drain sequence because global_nav borrows against apps
- headway:headway/owner-baby-mother — snapshot_tests' chrome_frame never lands a Back, so the review-pane archive race only showed once the test moved off the copy (8 tests still on it)
- headway:notedeck/pretty-truly-example — no public way to drive an AppAction through the chrome (chrome_handle_app_action is pub(super)), so the routing test had to live in-crate
- headway:headway/bottom-crack-kidney — no chrome seam for "resolve then route" an Open, so the reference-open path got no chrome-level test
- headway:notedeck/ginger-home-mirror — 46 min: chrome's new data-tool dispatch had to be copied into two hand-rolled key harnesses (keys.rs chrome_keys, snapshot_tests ChromeKeys) on top of chrome's own; plus a 10-min ci.py timeout on a cold workspace build
- headway:notedeck/proof-below-physical — the strip's drawing lives in chrome and in two Headway harness copies (keys.rs chrome_strip, snapshot_tests ChromeKeys::strip_ui); moved the shared part into notedeck_ui key_hints_overlay so all three changed in one place, but each copy still had to move where it draws
- headway:notedeck/juice-crystal-stumble — a fourth copy of chrome's key step, ui_tools::chrome_keys in notedeck_dave, so Dave's key tests resolve through the real engine and keymap ahead of Dave
- headway:notedeck/write-stock-illness — a fifth hand-rolled frame (keymap_macros Session::frame), plus no public read of the chrome's active app, so "Headway is in front" is inferred from a stand-in not rendering and the text drawn
- headway:notedeck/bleak-tornado-rose — no public read of which app is in front or whether Notebook's editor is open through the chrome, so the keymap-editor tests infer both from the text the frame drew
- headway:dave/enact-evolve-glove — a third hand-rolled Dave key frame (scene.rs KeyedScene, beside ui/mod.rs normal_mode_frame and keybindings Keys::frame); the first copy skipped settle_normal_mode_focus, so Enter's focus handoff failed until it was copied in too
- headway:headway/album-trouble-treat — ~15 min: an archived-epic queue test went 3→1 entries in one `chrome_frame`; the helpers that pump frames without it (`archive_demo_cards`, `wait_for_label`) left a Back and two prunes queued under a stale token. Also, in the chrome nav harness a "Restore" click landed nowhere, so the restore test moved to the headway harness
- headway:dave/saddle-method-runway — a fourth hand-rolled Dave key frame (ui/mod.rs scene_rename_frame) copying view.rs's KeyContext, its `renaming` focus check and scene_ui's toolbar rename; Dave has no headless frame that draws scene_ui, so the Delete double-delete was found by reading, not a test
- headway:dave/wage-inform-install — chrome's jump recording and Ctrl-O / Ctrl-I had to be copied into snapshot_tests ChromeKeys (a JumpList, fired_jump, the two tools) for one Headway test to go through them
- headway:notedeck/exile-finger-birth — keys.rs chrome_keys folds a data tool into its one `h.view`, so a list `a` on another board's row could only be tested through snapshot_tests' real app; each harness copy of chrome's data-tool dispatch knows one board
- headway:notedeck/until-object-oak — keys.rs fold applies only MoveCard/ArchiveCard and never reaches the list's sources, so J/K's edit was checked as a Resequence value there and the row actually moving only through snapshot_tests' real app
- headway:notedeck/pave-round-marriage — still no public read of the active app, so the Cmd-K app-switch test infers "fake is in front" from the fake app recording a render area; and no public read of an AppAction the chrome routed, so Enter-opens-the-hit is checked through a new Chrome::pending_open accessor (an unresolvable uri is held there)
- headway:notedeck/lizard-advice-alert — the Notebook harness has no chrome, so Cmd-K Enter on a node hit was checked by resolving its `notebook:` uri through the registry by hand and calling `Notebook::open`, not through the Open branch that routes it
