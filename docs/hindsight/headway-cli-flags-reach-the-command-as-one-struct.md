# headway_cli's flags reach the command builder as one struct

`args::parse_command` takes each parsed flag as a positional argument (20 of them),
so one new flag is a local, a parameter, a call argument and a `None` in its test.

- headway:personal/ribbon-dance-garbage — ~3 min: `diff --note` touched four places in args.rs to carry one value from the flag loop to `Command::Diff`, and the test's positional call broke on the count
