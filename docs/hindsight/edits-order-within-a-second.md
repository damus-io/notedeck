# Two edits of one field order within a second

Latest-wins edits are stamped in whole seconds; a same-author tie keeps the first.
A second edit of a field in one second can silently lose, so tests sleep it out.

- headway:notedeck/bundle-hood-stick — `next_second()` in `tools/tests/edits.rs` before every clear and the restore, and the test spreads its edits over six cards so no field is edited twice a second
