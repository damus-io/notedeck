# Apps claim the notes they open, instead of the chrome matching app variants

The chrome routes an opened note by `NotedeckApp` variant (`is_headway_note`
→ `headway_slot`), so an app wrapped as `Other` in a test never gets an open.

- headway:notedeck/tuition-raw-benefit — headway_keymap's Fixture runs Headway as `Other`, so Cmd-K Enter on a Headway hit routes nowhere there; the open was tested only from the Headway side (resolve the uri by hand, call `open_note_route`)
