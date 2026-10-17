# export_source_strings.py adds only the strings the diff added

Card: headway:notedeck/climb-gesture-physical
The exporter rewrites both FTL files whole, committing the tree's existing drift
too, so every card strips that out by hand to keep its commit standalone.

- headway:dave/general-tilt-repeat — ~3 min: wrote a scratch script to merge only this card's 24 keys into HEAD's en-US/en-XA files, leaving 7 unrelated relative-time and 3D-model keys and a reordered block run out
- headway:dave/question-flush-address — ~5 min: wrote another scratch merge for 13 keys, and found the exporter silently skips any message containing "/" (it reads it as a path), so `{used} / {window}` never reached the FTL and had to become a format!
- headway:dave/tray-ghost-view — ~6 min: a third scratch merge for 16 keys; and before exporting, the new card's labels were missing from the kittest tree entirely, so its tests failed for a reason that looked like layout
- headway:dave/adapt-toss-tattoo — ~4 min: a fourth scratch merge for 22 keys, whose first cut rejoined the blocks wrong and dropped ~700 lines of both files until the diffstat gave it away
- headway:dave/illness-morning-auto — ~3 min: a fifth scratch merge (now a reusable block-merge script in the scratchpad) for 13 keys, then hand-dropping 8 relative-time/no-entry drift keys it carried in
- headway:notedeck/bleak-tornado-rose — ~2 min: the exporter rewrote both files with 200 lines of drift for one new key; reverted and spliced the one entry in by hand
- headway:dave/tape-swim-army — ~4 min: yet another hand merge (one key), after the exporter silently skipped "{done}/{total} done" for its "/"; the header became a bare format! fraction instead
- headway:dave/raise-cube-used — ~3 min: an eighth hand splice (8 keys) out of a 234-line rewrite of both files, by the same block-merge script rewritten again in a fresh scratchpad
- headway:dave/matrix-message-outdoor — ~3 min: a ninth splice (5 keys, 1 dropped) out of a 274-line rewrite, by yet another scratchpad block-merge script
- headway:dave/gentle-target-sauce — ~2 min: a tenth splice (11 keys) out of a 310-line rewrite, by copying the last session's splice.py out of its scratchpad and editing its key list
- headway:dave/shrimp-guide-poet — ~2 min: an eleventh splice (10 keys, 1 dropped) out of a 296-line rewrite, by copying the last session's splice.py again and editing its key list
- headway:dave/general-tool-lecture — ~3 min: a sixth scratch merge for 5 keys (Stop, Stop?, Yes, No, stopping…); the exporter rewrote ~230 lines of each file around them
- headway:dave/large-denial-card — ~2 min: a twelfth splice (1 key) out of a ~300-line rewrite of both files, by a fresh scratch script that inserts the entry into HEAD's file in key order
- headway:dave/among-unusual-hazard — ~2 min: a thirteenth splice (4 keys) out of a ~230-line rewrite of both files, by another scratchpad splice.py
- headway:notedeck/fury-excuse-accident — ~3 min: a 225-line rewrite of both files for 2 keys, which also picked up the test-only `"no ftl entry exists for this"` from `i18n/manager.rs` tests and so failed two i18n tests; spliced the 2 entries in by script
- headway:notedeck/save-jump-disease — the export rewrote 237 lines of each FTL for 4 new strings; a script spliced just those 4 back into the committed files
- headway:notedeck/pave-round-marriage — ~3 min: the export rewrote ~234 lines of each FTL for 3 new keys (Apps, Open, Switch to app); spliced them into HEAD's files by a scratchpad script, again
- headway:dave/job-torch-blush — ~2 min: a 238-line rewrite of both files for 5 keys; spliced them into HEAD's files by a fresh scratchpad splice.py keyed on the new comments
- headway:notedeck/jump-bike-wife — ~4 min: the export rewrote ~230 lines of each FTL for 1 key, twice (the string changed mid-card); spliced it into HEAD's files by script both times
- headway:dave/shallow-upgrade-pet — ~2 min: a 233-line rewrite of both files for 1 plural key; spliced it into HEAD's by another scratch block-merge, whose first cut left a stray blank line
- headway:dave/decide-drip-huge — ~3 min: a 244-line rewrite of both files for 1 plural key; spliced it into HEAD's by another scratch block-merge
- headway:cashflow/answer-vibrant-crush — ~2 min: the export rewrote ~250 lines of each FTL for 1 key (the Cashflow sidebar label); reverted and spliced the one entry in by script
