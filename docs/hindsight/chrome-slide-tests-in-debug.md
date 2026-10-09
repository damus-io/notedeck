# Chrome-through tests that slide egui_nav run under a plain debug `cargo test`

egui's debug "widget changed layer_id" assert fires in any test that slides
egui_nav, so those tests run release-only and are found by tripping it.

- headway:notedeck/spoil-diary-squirrel — the Escape test could only live in the release-only chrome suite; wanted a helper that settles a NavStack without drawing two layers
- headway:notedeck/soup-unveil-decide — hit the debug-only egui_nav layer assert twice (Columns in slot 0, then dave/headway features shifting the slot) before building on new_headless_with_apps
