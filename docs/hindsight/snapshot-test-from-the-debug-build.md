# scripts/snapshot-test can render from the debug build a session already has

`scripts/snapshot-test` always runs `cargo test --release`, so a fresh worktree
pays a cold release build of the crate's whole dependency tree to update one png.

- headway:dave/okay-scale-wrap — 60-min session: ~10 min of a cold --release snapshot build, killed; `VK_ICD_FILENAMES=$LAVAPIPE_ICD UPDATE_SNAPSHOTS=1 cargo test -p notedeck_dave --lib -- --ignored <name>` off the cached debug build rendered in 1s
- headway:dave/adapt-toss-tattoo — 47-min session: ~8 min across two --release snapshot-test runs to render one png (and re-render it after a layout fix)
- headway:dave/february-album-decrease — ~6 min of a cold --release build to re-record six session-list pngs after a 29-line row change
