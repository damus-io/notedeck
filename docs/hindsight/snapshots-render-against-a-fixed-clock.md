# Snapshot tests render against a fixed clock

A snapshot whose text reads the wall clock ("updated 61s", "edited 2y 10mo
ago") matches only until the clock moves, then fails every commit after it.

- headway:cashflow/elder-spirit-fly — ~8 min: the "no visual change" check for moving the charts into notedeck_ui failed on the committed pngs; had to render the base to scratch and diff against it to see only the "updated 61s" label and the refresh button differ
- headway:notedeck/balcony-what-club — notebook's five vault pngs ("edited 2y 10mo ago" → "2y 11mo ago": 2023-fixed seeds against `unix_time_secs()` in `editor.rs` `vault_rows`) were re-recorded knowing they go red again in about a month
- headway:cashflow/crash-area-planet — ~3 min: the journal summary's "loaded in 412 ms" is measured, so its snapshot draws an app handed its books (no load time) rather than the loaded journal it describes
- headway:cashflow/boat-burst-capital — the Settings snapshot draws an app handed its books for the same "loaded in" reason, so the Sync section it shows is the no-journal line; the transport rows and Sync now are only label-checked (`tests/settings.rs`), never seen
- headway:headway/below-person-volcano — the review pane's new record picker has no snapshot: `store::apply` stamps each review record with the wall clock and the pane orders records by `(created_at, id)`, so two records added in one test land in either order from run to run; checked it with a throwaway render instead
- headway:cashflow/snake-chunk-february — the detail's override and split snapshots draw "Written just now" off `SystemTime::now` in `views/detail/mod.rs` `unix_now`; they hold only because `detail_harness` writes the vault notes seconds before the render, and a slow machine past a minute reads "1 min ago"
- headway:notedeck/thunder-stick-cabin — ~3 min: a notedeck core change sent `--changed --debug` through every suite; notebook_vault*/note_embed* failed on "days ago" text alone, beside 7 other unrelated pngs, and had to be read as diffs to rule out the Cmd-K change
- headway:ux-feedback/method-reflect-april — ~12 min: an image-cache change in notedeck core sent `--changed --debug` through every dependent suite (~10 min) and it flagged 13 pngs (notebook vault "days ago", headway review "time", cashflow journal, chrome update_install_problem) none of which draw an image; read the diffs to rule the change out
- headway:ux-feedback/torch-legend-session — ~2 min: `--changed --debug` flagged `headway_detail_review_sidebar` and `headway_review_comments` on the note renderer's comment age alone (wall-clock `created_at` against the frozen clock); read both diffs to rule this card out, left them unrecorded
