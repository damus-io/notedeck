# cli_term pages a help page taller than the terminal

Help goes to stderr but `cli_term::emit` pages stdout only, on a flag, so a CLI
whose help outgrows the screen measures the terminal and switches streams itself.

- headway:notedeck/maximum-win-busy — ~3 min: `nd --help`'s APPS section runs ~90 lines, so nd grew a TIOCGWINSZ `stdout_size` in `nd_cli/src/output.rs` and a stderr-or-pager split in `main.rs::usage`
