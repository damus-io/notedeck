# Snapshot tests render against a fixed clock

A snapshot whose text reads the wall clock ("updated 61s", "edited 2y 10mo
ago") matches only until the clock moves, then fails every commit after it.

- headway:cashflow/elder-spirit-fly — ~8 min: the "no visual change" check for moving the charts into notedeck_ui failed on the committed pngs; had to render the base to scratch and diff against it to see only the "updated 61s" label and the refresh button differ
- headway:notedeck/balcony-what-club — notebook's five vault pngs ("edited 2y 10mo ago" → "2y 11mo ago": 2023-fixed seeds against `unix_time_secs()` in `editor.rs` `vault_rows`) were re-recorded knowing they go red again in about a month
- headway:cashflow/crash-area-planet — ~3 min: the journal summary's "loaded in 412 ms" is measured, so its snapshot draws an app handed its books (no load time) rather than the loaded journal it describes
- headway:cashflow/boat-burst-capital — the Settings snapshot draws an app handed its books for the same "loaded in" reason, so the Sync section it shows is the no-journal line; the transport rows and Sync now are only label-checked (`tests/settings.rs`), never seen
- headway:headway/below-person-volcano — the review pane's new record picker has no snapshot: `store::apply` stamps each review record with the wall clock and the pane orders records by `(created_at, id)`, so two records added in one test land in either order from run to run; checked it with a throwaway render instead
