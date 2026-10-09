# export_source_strings.py adds only the strings the diff added

The exporter rewrites both FTL files whole, committing the tree's existing drift
too, so every card strips that out by hand to keep its commit standalone.

- headway:dave/general-tilt-repeat — ~3 min: wrote a scratch script to merge only this card's 24 keys into HEAD's en-US/en-XA files, leaving 7 unrelated relative-time and 3D-model keys and a reordered block run out
- headway:dave/question-flush-address — ~5 min: wrote another scratch merge for 13 keys, and found the exporter silently skips any message containing "/" (it reads it as a path), so `{used} / {window}` never reached the FTL and had to become a format!
- headway:dave/tray-ghost-view — ~6 min: a third scratch merge for 16 keys; and before exporting, the new card's labels were missing from the kittest tree entirely, so its tests failed for a reason that looked like layout
- headway:dave/adapt-toss-tattoo — ~4 min: a fourth scratch merge for 22 keys, whose first cut rejoined the blocks wrong and dropped ~700 lines of both files until the diffstat gave it away
