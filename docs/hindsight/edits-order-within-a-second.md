# Two edits of one field order within a second

Latest-wins edits are stamped in whole seconds; a same-author tie keeps the first.
A second edit of a field in one second can silently lose, so tests sleep it out.

- headway:notedeck/bundle-hood-stick — `next_second()` in `tools/tests/edits.rs` before every clear and the restore, and the test spreads its edits over six cards so no field is edited twice a second
- headway:notedeck/coyote-color-quantum — cards added in one second tie on creation time, so an epic's work-order isn't its add order: `tools/tests/structure.rs` reads the order back from `next --ready` instead of assuming it, and sleeps a second before each unblock, unrelate, terminal off and detach
