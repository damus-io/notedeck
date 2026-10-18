# One BoardView builder for tests

Tests across three crates build `BoardView` as a struct literal, so a new field
breaks every one of them and each has to be patched by hand or by script.

- headway:headway/neutral-cruel-public — adding `BoardView::deleted` broke 8 hand-built test literals in 8 files (headway, headway_cli, notedeck_headway); wrote a throwaway script that read rustc's E0063 locations and inserted `deleted: false`
- headway:headway/pilot-mind-secret — wrote two more 20-field `CardView` literals plus `BoardView`/`ColumnView` fixtures (query.rs, list.rs) copied from grep.rs/traversal.rs; a shared `CardView`/`BoardView` test builder in headway would have been one line each
- headway:cashflow/right-gain-gym — ~2 min: a 21-field `CardView` literal in `cashflow::board_progress` tests just to hand `completed_at` an activity list
