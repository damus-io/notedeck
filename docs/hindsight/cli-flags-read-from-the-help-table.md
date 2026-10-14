# A CLI's flags are read from its help table

Card: headway:notedeck/together-clog-cattle
Every flag parses into one global bag, so a help page's OPTIONS list is written
by hand from reading the parser, and nothing fails when it is wrong.

- headway:notedeck/amazing-galaxy-impulse — ~10 min: derived each of 18 commands' option set by tracing `Cli::parse`'s bag through `parse_command`, the inline spawn/send/approve/config arms and each `cmd_*`'s use of `cli.json`/filters/`list_scope`, with no check that the 18 OPTIONS lists match
