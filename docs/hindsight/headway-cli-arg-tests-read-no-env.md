# headway_cli's argument tests read no environment

`args::tests` parse through `Cli::parse`, which reads `$HEADWAY_COMMENT_NSEC`
from the process, so the suite fails inside any agent session that sets it.

- headway:notedeck/reopen-budget-accident — ~2 min: `comment_key_parses_beside_the_signing_key` failed only because the session exports `HEADWAY_COMMENT_NSEC`; reran under `env -u` to tell it from a regression
