# The review-queue snapshot tests pass on every machine

Two review-pane tests wait for a `jex0:/home/jb55/dev/notedeck` label that never
draws on monad, so each headway card sets its work aside to prove they aren't its.

- headway:notedeck/plug-dry-virus — 3 min: put the card's files aside and reran the review suite on the base commit to show both failures weren't mine
- headway:notedeck/payment-bounce-consider — 3 min: matched both failures to this file before trusting the rest of the run
- headway:headway/require-drill-room — the same two failed under `cargo test -p notedeck_headway`; cost a minute to match them to this file instead of a rebuild of the tip
