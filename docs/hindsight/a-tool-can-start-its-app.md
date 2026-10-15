# A tool can start its app

Only running apps' tools are registered, so `dave_spawn_session`, which must
start Dave, is a name check in the chrome's socket `call_tool` (ipc.rs).

- headway:notedeck/shiver-search-pyramid — a name check on `dave_spawn_session` in the chrome's socket dispatch, and `nd spawn` can't look the tool up first
