# One BoardView builder for tests

Tests across three crates build `BoardView` as a struct literal, so a new field
breaks every one of them and each has to be patched by hand or by script.

- headway:headway/neutral-cruel-public — adding `BoardView::deleted` broke 8 hand-built test literals in 8 files (headway, headway_cli, notedeck_headway); wrote a throwaway script that read rustc's E0063 locations and inserted `deleted: false`
