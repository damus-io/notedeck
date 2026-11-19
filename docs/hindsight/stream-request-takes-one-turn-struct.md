# AiBackend::stream_request takes one turn struct

`stream_request` takes ten positional arguments, so every backend re-spells
them, and a recording fake copies a few into its own struct, not the request.

- headway:notedeck/simple-race-leg — ~3 min: `RecordedTurn` re-declares five of `stream_request`'s ten arguments (session id, tools, model, env, last user text) because there is no `TurnRequest` to clone; a test that wants `cwd`, `resume_session_id` or `permission_mode` has to grow the fake again
