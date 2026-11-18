# Two edits of one field order within a second

Latest-wins edits are stamped in whole seconds; a same-author tie keeps the first.
A second edit of a field in one second can silently lose, so tests sleep it out.

- headway:notedeck/bundle-hood-stick — `next_second()` in `tools/tests/edits.rs` before every clear and the restore, and the test spreads its edits over six cards so no field is edited twice a second
- headway:notedeck/coyote-color-quantum — cards added in one second tie on creation time, so an epic's work-order isn't its add order: `tools/tests/structure.rs` reads the order back from `next --ready` instead of assuming it, and sleeps a second before each unblock, unrelate, terminal off and detach
- headway:notedeck/panther-gadget-toddler — `tools/tests/search.rs` sleeps a second between every add in the list test, since the rows come back in board order and same-second cards tie in it (~4s a run); `field_edits_round_trip` in `edits.rs` flakes the same way under a parallel `--lib tools` run ("row 0 is the top")
- headway:dave/dial-ride-spirit — `nd headway seq --first` over seven cards in one loop: every move acked, the order came back scrambled; three later single moves of one card, each in a fresh second, acked and changed nothing either (~5 min, the epic order left slightly off)
