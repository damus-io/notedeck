# nostrdb_net sync warnings go through the progress observer

The fork's reconcile paths `eprintln!` their own warnings with no event first,
so a CLI's live progress line can't erase itself and the warning splices onto it.

- headway:notedeck/dirt-dress-copper — ~5 min: split the fork's `connect_and_sync` open in `headway_cli/src/sync.rs` just to clear the line before its offline warning, and cleared on `SyncEvent::Pushed` to beat "flushed"; the stall/fallback warnings inside `reconcile_sync` still splice on a tty
