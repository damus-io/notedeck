# cli_term resolves which help page a command line asks for

headway, agentium and nd each fold `help [cmd]` and `help options` in their own
`Cli::parse`, so every help-surface change is the same edit made three times.

- headway:notedeck/salmon-define-salt — `help options` had to be wired into both parsers, with the same test copied into each
- headway:notedeck/balcony-warm-action — nd's parser re-derived `help`/`help options`/`<cmd> --help` and which flags take a value, a third copy
- headway:cashflow/love-round-vendor — cashflow_cli's args.rs is a fourth copy of nd's parse loop (`help`/`help options`/`<cmd> --help`, globals before and after the command) with its tests copied too
