# export_source_strings.py adds only the strings the diff added

The exporter rewrites both FTL files whole, committing the tree's existing drift
too, so every card strips that out by hand to keep its commit standalone.

- headway:dave/general-tilt-repeat — ~3 min: wrote a scratch script to merge only this card's 24 keys into HEAD's en-US/en-XA files, leaving 7 unrelated relative-time and 3D-model keys and a reordered block run out
