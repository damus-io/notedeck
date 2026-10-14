# cli_term resolves which help page a command line asks for

headway and agentium each fold `help [cmd]` and `help options` in their own
`Cli::parse`, so every help-surface change is the same edit made twice.

- headway:notedeck/salmon-define-salt — `help options` had to be wired into both parsers, with the same test copied into each
