# A shot clicks a widget by its label

`notedeck --shot` clicks only at `--click x,y` pixels, so a script hard-codes
where a widget sat in some PNG and clicks the wrong thing when layout moves.

- headway:notedeck/zebra-illness-salute — the rig opens Headway by clicking (97, 539) in the app drawer, read off a shot; the keyboard route (Cmd-K) panics (headway:headway/fetch-mind-series), and a label lookup would need egui's accesskit tree, which the release binary doesn't build
