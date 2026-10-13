# A tool argument's advertised values come from its serde enum

Each enum argument of a UI tool is written twice: a serde enum to parse it and
a `&[&str]` of the same names for its spec, kept in step by hand.

- headway:dave/hat-random-call — `dave_set_permission_mode` needed a `ModeArg` enum, a `MODES` list mirroring its snake_case names, an args struct and a match onto `PermissionMode`, beside the six pairs already in ui_tools.rs (`BlockTo`/`BLOCK_TOS`, …); a rename in one and not the other parses fine and advertises a value that errors
