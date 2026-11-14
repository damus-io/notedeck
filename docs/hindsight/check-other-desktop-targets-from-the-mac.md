# Check the Linux and Windows cfg branches from the Mac

`cargo check --target` for Linux or Windows dies on the Mac in ring's C
build, so `cfg(target_os)` branches for other desktops ship unbuilt until CI.

- headway:notedeck/engine-amateur-sail — the updater's install path split three ways (macOS, other desktops, Android); the Linux/Windows branches were read, not compiled, because `cargo check --target x86_64-unknown-linux-musl` died in ring's C build
