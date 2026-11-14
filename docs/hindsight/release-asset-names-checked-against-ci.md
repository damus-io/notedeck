# release asset names checked against CI

Nothing ties `target_asset_name()` to the names rust.yml and macos_build.sh upload.
A rename on either side makes the updater find nothing, silently.

- headway:notedeck/mask-dance-meadow — moved the macOS tarball from a rust.yml step into macos_build.sh and could only check by reading both files that the name still matched `target_asset_name()`
