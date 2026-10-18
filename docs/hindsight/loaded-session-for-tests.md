# A LoadedSession a unit test can build

`session_loader::LoadedSession` has no `Default`, so a unit test of what consumes
a load has to fold a real session out of nostrdb or go untested.

- headway:notedeck/hammer-evolve-knee — dropped `NoteLedger::claim`'s own unit test (it takes `&LoadedSession`) and left it to the restore and convergence tests; a `LoadedSession { note_ids, max_key, ..Default::default() }` would have been three lines
- headway:notedeck/cushion-audit-embark — `NoteLedger::install_history`'s unit test spells out all nine fields to set two
