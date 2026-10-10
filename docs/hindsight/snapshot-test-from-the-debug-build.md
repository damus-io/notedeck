# scripts/snapshot-test can render from the debug build a session already has

Card: headway:notedeck/song-suggest-decorate
`scripts/snapshot-test` always runs `cargo test --release`, so a fresh worktree
pays a cold release build of the crate's whole dependency tree to update one png.

- headway:dave/okay-scale-wrap — 60-min session: ~10 min of a cold --release snapshot build, killed; `VK_ICD_FILENAMES=$LAVAPIPE_ICD UPDATE_SNAPSHOTS=1 cargo test -p notedeck_dave --lib -- --ignored <name>` off the cached debug build rendered in 1s
- headway:dave/adapt-toss-tattoo — 47-min session: ~8 min across two --release snapshot-test runs to render one png (and re-render it after a layout fix)
- headway:dave/matrix-message-outdoor — 64-min session: ~4 min across two --release snapshot-test runs for one png (and its re-render after hiding zero counts); most of the rest was the full `cargo test -p notedeck_dave` linking ~20 integration binaries at load 97
- headway:dave/february-album-decrease — ~6 min of a cold --release build to re-record six session-list pngs after a 29-line row change
- headway:dave/width-useless-choose — 33-min session: ~12 min of a cold --release snapshot-test build for one png, killed; the debug-build render took 0.7s
- headway:dave/trade-thought-leisure — ~6 min: the --release snapshot build in the container was OOM-killed mid-rustc, and the retry then failed to link on the half-written objects until `cargo clean -p notedeck_dave --release` inside the container
- headway:dave/eye-legend-stay — ~8 min: the --release snapshot build OOM-killed again (two snapshot containers building at once), then failed to link on the half-written objects until a `cargo clean --release -p notedeck_dave -p egui_kittest` inside the container and a rerun with CARGO_BUILD_JOBS=3
- headway:dave/group-angry-try — ~12 min across four --release snapshot-test runs (two before renders, two after) for one composer redesign; another lane's concurrent snapshot build doubled the wait
- headway:dave/hunt-mixed-venue — two --release snapshot-test runs died with the linker OOM-killed (signal 9) linking all six dave test binaries at once; rendering the mockups needed only `--lib`, run by hand via `docker exec` on the container
- headway:notedeck/when-exercise-arch — ~45-min session: ~12 min of --release snapshot build for a test-harness port, a second --release build of the base commit to compare, and a stale-rlib rebuild after the two shared one target dir
