# Snapshot tests render against a fixed clock

A snapshot whose text reads the wall clock ("updated 61s", "edited 2y 10mo
ago") matches only until the clock moves, then fails every commit after it.

- headway:cashflow/elder-spirit-fly — ~8 min: the "no visual change" check for moving the charts into notedeck_ui failed on the committed pngs; had to render the base to scratch and diff against it to see only the "updated 61s" label and the refresh button differ
- headway:notedeck/balcony-what-club — notebook's five vault pngs ("edited 2y 10mo ago" → "2y 11mo ago": 2023-fixed seeds against `unix_time_secs()` in `editor.rs` `vault_rows`) were re-recorded knowing they go red again in about a month
