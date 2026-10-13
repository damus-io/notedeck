# Chrome-through tests that slide egui_nav run under a plain debug `cargo test`

Card: headway:notedeck/mansion-behave-sustain
egui's debug "widget changed layer_id" assert fires in any test that slides
egui_nav, so those tests run release-only and are found by tripping it.

- headway:notedeck/spoil-diary-squirrel — the Escape test could only live in the release-only chrome suite; wanted a helper that settles a NavStack without drawing two layers
- headway:notedeck/soup-unveil-decide — hit the debug-only egui_nav layer assert twice (Columns in slot 0, then dave/headway features shifting the slot) before building on new_headless_with_apps
- headway:notedeck/write-stock-illness — 8 min: the `E` macro switches Dave→Headway, so its end-to-end test tripped the debug assert, moved release-only and cost two release builds of the chrome test, plus its own target on CI's release step
- headway:notedeck/bleak-tornado-rose — ~8 min: the keymap editor opens by switching to Notebook, so its debug test tripped the assert; reshaped the fixture to put Notebook in front and drive it with global-scope keys so nothing slides
- headway:notedeck/shoulder-wedding-penalty — the card-done test never switches apps, yet it sits in the release-only module because its fixture warms Headway up with a slide; another release build of the chrome test
- headway:notedeck/age-empower-gun — the `g p` then Back test through headway_keymap's debug fixture tripped the assert on the push itself (plain `Enter` does too), so the end-to-end check became the release-only epic-review macro test; another release build of the chrome test
- headway:dave/fresh-champion-crucial — nav_scenarios.rs wrote its own `land()` (reconcile Navigated / Returned by hand, then sync_active_from_nav), a third copy beside nav.rs's land_slide and the inline reconciles; the scenarios could only stay debug-runnable by never drawing
- headway:notedeck/save-jump-disease — the Cmd-K "issue list" test tripped the assert on the list's push (so does plain `g i` in headway_keymap's debug fixture); both list tests went release-only, one more `--test` on CI's release step and a release build of the chrome test
