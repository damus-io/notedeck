# One formatter per display property, shared by the CLI and the GUI

`headway list` and the GUI list each format a row's subissue rollup, estimate,
due date and relative times themselves, so the two can drift on what a cell says.

- headway:headway/end-kick-draft — wrote the same cell text twice: `list_line` in crates/headway_cli/src/list.rs and `LineText::of` in crates/notedeck_headway/src/ui/list.rs (`3pt` vs `3 pt`, ISO vs `Oct 20` due dates); a `DisplayProp::text(&ListRow) -> Option<String>` in headway::query would have been one place
