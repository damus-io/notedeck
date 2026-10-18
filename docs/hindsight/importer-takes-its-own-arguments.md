# An importer takes its own arguments

`ImportInput` has only a source and paths, so a month must ride as a path,
which `import --help` can't list and a typo reads as a missing file.

- headway:cashflow/brick-hospital-race — ~4 min: `appstore`'s months ride as path arguments (`2026-03`, `2025-10..2026-03`), told apart from files by whether the path exists, instead of a `--month` the CLI parses and the importer receives typed
