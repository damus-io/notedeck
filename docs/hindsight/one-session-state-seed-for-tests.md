# One kind-31988 session-state seed for every test that draws an agentium chip

Each test that needs an `agentium:` chip to resolve hand-builds and ingests
its own kind-31988 note, so seven copies drift in which tags they set.

- headway:notedeck/when-exercise-arch — porting the chip tests onto TestHost left four copies of the same NoteBuilder seed in chip_click, reference_chip, from_session_card and session_list (plus reference.rs, session_cache.rs, chrome actions.rs); TestHost couldn't own it because notedeck_testing doesn't know agentium kinds
- headway:notedeck/story-chimney-pizza — porting `agentium list/show/log/grep` to Dave's data tools copied the state + kind-1988 message + kind-1989 source-line NoteBuilder seeds a fifth time (tools/tests.rs), from the four agentium_cli e2e files that each carry their own
