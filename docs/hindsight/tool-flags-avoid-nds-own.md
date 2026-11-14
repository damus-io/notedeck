# A tool's flag can't shadow one of nd's own

nd reads `--json`, `--color`, `--pager` and `--no-pager` before a tool's flags,
so a tool argument spelled the same is unreachable, and no spec check says so.

- headway:notedeck/brief-bar-number — `notebook_color_node`'s `color` took `.long("color")` to match the CLI's `--color <c>`, which nd's `--color <when>` swallowed; caught only by smoke-running `nd notebook color --help` against a headless notedeck, and made positional-only
