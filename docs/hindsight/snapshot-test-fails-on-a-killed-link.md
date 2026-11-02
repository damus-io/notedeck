# scripts/snapshot-test fails loudly when its container's linker is killed

`--changed` on a Mac OOM-kills the lavapipe container's linker (signal 9) and
still exits 0, so a session reads "no png moved" from a run that rendered nothing.

- headway:headway/cry-puppy-refuse — a --changed --debug run killed every suite's link and exited 0; caught only by grepping its 66 KB log, then reran per crate through `docker exec -e CARGO_BUILD_JOBS=1|2` (notedeck_ui needed 1)
