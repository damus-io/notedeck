# bin_testing walks the resolved dependency graph with one shared helper

Three graph tests each run `cargo metadata` and hand-roll the same edge walk.
A shared graph (target, features, `reachable(roots)`) makes a new rule ~10 lines.

- headway:dave/patch-salad-february — ~5 min: copied guest_no_c's metadata call and walk into sandbox_desktop_only, then grew it features and members
