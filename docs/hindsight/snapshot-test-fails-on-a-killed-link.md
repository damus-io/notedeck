# scripts/snapshot-test fails loudly when its container's linker is killed

`--changed` on a Mac links every suite at once in the lavapipe container, the
linker is OOM-killed (`ld terminated with signal 9`), and the run still exits
0, so a session reads "no png moved" from a run that rendered nothing.

- headway:headway/cry-puppy-refuse — a --changed --debug run killed every suite's link and exited 0; caught only by grepping its 66 KB log, then reran per crate through `docker exec -e CARGO_BUILD_JOBS=1|2` (notedeck_ui needed 1)
