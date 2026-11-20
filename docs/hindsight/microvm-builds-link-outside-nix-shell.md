# A `sandbox`/`microvm` build links outside nix-shell

msb_krun links libcap-ng, which shell.nix has and an agent's shell doesn't;
finding an x86-64 copy in /nix/store for LIBRARY_PATH is a hunt every time.

- headway:dave/gesture-cage-west — ~3 min: the `--features sandbox` test failed to link twice (no lib, then an aarch64 copy) before an x86-64 libcap-ng was found
